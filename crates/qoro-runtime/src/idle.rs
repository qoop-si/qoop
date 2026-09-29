//! Idle sleep: a space whose tabs have all been idle past a time goes to sleep by itself.
//!
//! Off by default, and turned on by `[sleep] idle_minutes` in `config.toml`. Off means the
//! task is never spawned: an idle daemon then pays zero wakeups for this feature, which is
//! what keeps ADR-009's macOS literal-zero-idle claim and the latency gate intact.
//!
//! Eligibility is fail closed throughout: an unknown program, a boundary identity, a setup
//! gate, a watched terminal, or a wait between tabs all veto, and a space sleeps only when
//! every one of its tabs is ready. The sleep itself is the one a person asks for, which
//! vets each tab again, process tree included, and keeps every row so the space comes
//! back as it was.

use std::collections::{BTreeMap, BTreeSet};

/// What armed the old idle reaper. No longer read; a daemon that finds it set says where
/// the setting moved.
pub const IDLE_TIMEOUT_ENV: &str = "QORO_IDLE_TIMEOUT_MINUTES";

/// How often idle sleep looks while on: a quarter of the timeout,
/// floored at a minute and capped at five, so a 30 minute timeout wakes
/// every 5 minutes and a 1 minute trial timeout every minute.
pub fn reap_tick(timeout: std::time::Duration) -> std::time::Duration {
    let quarter = timeout.as_secs().div_ceil(4).max(1);
    std::time::Duration::from_secs(quarter.clamp(60, 300))
}

/// Shells a person sits in, by basename, lowercase.
///
/// Compared against what `identify()` returns, which is already the name a
/// person would say rather than the runtime underneath it. Anything not on
/// this list and not a known agent is a task holding the tab.
pub const SHELL_PROGRAMS: &[&str] = &[
    "sh",
    "dash",
    "bash",
    "zsh",
    "fish",
    "nu",
    "powershell",
    "pwsh",
    "cmd",
];

/// Whether a program name is a plain interactive shell.
pub fn is_shell_program(name: &str) -> bool {
    let base = name.rsplit('/').next().unwrap_or(name).to_lowercase();
    SHELL_PROGRAMS.contains(&base.as_str())
}

/// What the foreground of a terminal holds, for the reap decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForegroundKind {
    /// No process: exited, settled to idle.
    Absent,
    /// A plain shell at its prompt.
    Shell,
    /// A known agent binary.
    Agent,
    /// Anything else: a build, a sleep, an unreadable wrapper. Vetoes.
    Other,
}

/// Classify a foreground program for the reap decision.
///
/// `agent` is the bit carried on the identity, decided where the argv was read: the name
/// alone cannot say, because `pi` is an agent under node and a stranger's tool as its own
/// executable, and a tool read as an agent is a terminal the reaper may close.
pub fn classify_program(program: Option<&str>, agent: bool) -> ForegroundKind {
    match program {
        None => ForegroundKind::Absent,
        Some(name) if is_shell_program(name) => ForegroundKind::Shell,
        Some(_) if agent => ForegroundKind::Agent,
        Some(_) => ForegroundKind::Other,
    }
}

/// Whether a terminal may be reaped. Pure, table-tested.
///
/// Fail closed: only `Idle`/`Done` with an absent, shell or agent foreground
/// (plus `Unknown` with an absent or shell foreground: a shell that never
/// produced an accepted observation sits at `Unknown` forever, and excluding
/// it would make every plain shell tab immortal) may close. `Failed` is
/// excluded: closing it destroys the error text a person needs to read, and
/// `forget()` drops the focus log with it. A setup gate, a boundary
/// identity, a live watcher, or idleness below the timeout all veto;
/// idleness exactly at the timeout is eligible.
pub fn eligible_for_reap(
    state: qoro_domain::AgentRuntimeState,
    foreground: ForegroundKind,
    boundary: bool,
    setup_gate: bool,
    watched: bool,
    idle_secs: i64,
    timeout_secs: i64,
) -> bool {
    use qoro_domain::AgentRuntimeState as S;
    if boundary || setup_gate || watched {
        return false;
    }
    if idle_secs < timeout_secs {
        return false;
    }
    match state {
        S::Idle | S::Done => matches!(
            foreground,
            ForegroundKind::Absent | ForegroundKind::Shell | ForegroundKind::Agent
        ),
        // A shell at its prompt that nothing ever observed: the only
        // `Unknown` that may close. An unknown agent-shaped or
        // task-shaped foreground stays.
        S::Unknown => matches!(foreground, ForegroundKind::Absent | ForegroundKind::Shell),
        S::Starting | S::Working | S::Blocked | S::Failed | S::Uncertain => false,
    }
}

/// Whether the terminal's foreground group holds a task the snapshot cannot
/// see: a `cargo`, `sleep` or `python` child under a shell leader, which
/// `program_of` alone reports as the shell.
///
/// Runs only on reap candidates once per reap tick, never on the hot path.
/// `true` means veto. Fail closed: an unreadable member vetoes (it only
/// skips one round; the next tick retries), while no foreground group at
/// all means nothing to protect. Shell and known-agent members are fine;
/// backgrounded (`&`) jobs outside the foreground group are NOT seen here,
/// which is a stated residual of v1.
pub fn group_holds_task(master: std::os::fd::RawFd) -> bool {
    let Some(leader) = qoro_pty::sys::foreground_leader(master) else {
        return false;
    };
    let members = qoro_pty::sys::group_members(qoro_pty::Pgid(leader.0));
    for member in members {
        if member.0 == leader.0 {
            continue;
        }
        let program_path: Option<String> = qoro_pty::sys::program_of(member);
        let Some(path) = program_path else {
            return true;
        };
        let argv = qoro_pty::sys::argv_of(member);
        let program = qoro_domain::program::identify(&path, &argv);
        if program.agent {
            continue;
        }
        if is_shell_program(&program.name) {
            continue;
        }
        return true;
    }
    false
}

/// What in a terminal's process tree would outlive, or be lost to, putting it to sleep.
///
/// Walked from the terminal's leader by parent. Two kinds block, named by program:
/// a descendant that is no longer on the terminal (`setsid`, the way Claude Code starts a
/// background shell), which teardown cannot reach, so the space would be called asleep
/// while it still ran; and a group the shell started that is neither its own nor the
/// foreground's (`postgres &`), a job that would end without coming back. Everything under
/// the foreground that is still on the terminal is the program's own, MCP servers in groups
/// of their own included (measured under codex): teardown ends them and the program starts
/// them again when it resumes. Pure, so the measured trees are its tests.
pub fn sleep_blockers(
    table: &[qoro_pty::sys::ProcRow],
    leader: i32,
    foreground: Option<i32>,
    tty: u64,
) -> Vec<String> {
    let mut children: BTreeMap<i32, Vec<&qoro_pty::sys::ProcRow>> = BTreeMap::new();
    for row in table {
        children.entry(row.ppid).or_default().push(row);
    }
    let shell_group = table
        .iter()
        .find(|r| r.pid == leader)
        .map_or(leader, |r| r.pgid);
    let mut blockers = Vec::new();
    // (pid, whether it is under the foreground program)
    let mut stack = vec![(leader, false)];
    while let Some((pid, under_foreground)) = stack.pop() {
        for child in children.get(&pid).map(Vec::as_slice).unwrap_or_default() {
            if child.tty != Some(tty) {
                blockers.push(child.name.clone());
                continue;
            }
            let foreground_here =
                under_foreground || (Some(child.pgid) == foreground && child.pgid != shell_group);
            if !foreground_here && child.pgid != shell_group {
                blockers.push(child.name.clone());
                continue;
            }
            stack.push((child.pid, foreground_here));
        }
    }
    blockers.sort();
    blockers.dedup();
    blockers
}

/// Whether input from this sender counts as live traffic.
///
/// Restore replay arrives as `Input` from `qoro-restore`: transcript bytes
/// re-typed after a restart, not a person doing something. Stamping it
/// would hand every restored tab a fresh full timeout on every restart.
pub fn is_live_input(from: &str) -> bool {
    from != "qoro-restore"
}

/// Apply waiter→target edges to a snapshot eligibility map.
///
/// `eligible` is the pure per-stream verdict at pass start, before any
/// reap in the pass runs: evaluating against the snapshot keeps the
/// outcome independent of pass order. A waiter is vetoed while its target
/// is tracked and staying awake this pass, whether for its own reasons or
/// because it waits on someone busy in turn: vetoes run to a fixed point,
/// so the head of a chain `A -> B -> C` stays while `C` works. Both idle
/// together sleep together, cycles included; a target that is gone vetoes
/// nothing, because nothing is being waited on any more.
///
/// The waiter's own activity since asking does not end the wait: an agent
/// always renders after its `qoro send/prompt` returns, so that clock moves
/// past the ask in every real delegation. The edge ends when the target is
/// reaped or closed, when the waiter asks someone else (the edge moves), or
/// at the edge TTL.
///
/// ponytail: "staying awake" is any reason the target is not reapable,
/// including a person watching it, so a waiter on a tab someone keeps open
/// on screen stays up to the edge TTL. Split busy from watched if that
/// ever costs real memory.
pub fn apply_edge_vetoes(
    eligible: &BTreeMap<u32, bool>,
    edges: &[(u32, u32)],
) -> BTreeMap<u32, bool> {
    let mut out = eligible.clone();
    // Vetoes only ever turn true into false, so this ends within one round
    // per edge, and the fixed point does not depend on edge order.
    loop {
        let mut changed = false;
        for (waiter, target) in edges {
            // The target stays awake: the wait is alive, the waiter stays.
            // Reapable target: both sleep together. Absent target: gone,
            // and a wait on nothing is not a wait.
            if out.get(waiter).copied().unwrap_or(false) && out.get(target).is_some_and(|e| !e) {
                out.insert(*waiter, false);
                changed = true;
            }
        }
        if !changed {
            return out;
        }
    }
}

/// Per-terminal watcher refcounts: how many live connections hold a render
/// subscription on each terminal.
///
/// The reaper skips watched terminals: closing a tab somebody is looking at
/// is the one outcome this feature must never produce. Counts, not flags,
/// because two connections may watch the same tab.
///
/// Entries for closed terminals are removed on unsubscribe and disconnect,
/// which always follow a close for a watched tab. A number does come back
/// within a run (a woken space keeps its tabs' numbers), so a subscription
/// made for an earlier life is dropped, count and all, when the same
/// connection subscribes to the new one.
#[derive(Clone, Default, Debug)]
pub struct WatchCounts {
    inner: std::sync::Arc<std::sync::Mutex<BTreeMap<u32, usize>>>,
}

impl WatchCounts {
    /// One more connection watches this terminal.
    pub fn watch(&self, stream: u32) {
        *self.inner.lock().unwrap().entry(stream).or_default() += 1;
    }

    /// One connection stopped watching. Counts saturate at zero rather than
    /// underflow: an unbalanced call must not poison every later decision.
    pub fn unwatch(&self, stream: u32) {
        let mut held = self.inner.lock().unwrap();
        match held.get_mut(&stream) {
            Some(count) if *count > 1 => *count -= 1,
            Some(_) => {
                held.remove(&stream);
            }
            None => {}
        }
    }

    /// Forget every subscription one connection held, on disconnect.
    pub fn unwatch_all(&self, streams: &BTreeSet<u32>) {
        for stream in streams {
            self.unwatch(*stream);
        }
    }

    /// Whether anybody currently watches this terminal.
    pub fn watched(&self, stream: u32) -> bool {
        self.inner
            .lock()
            .unwrap()
            .get(&stream)
            .is_some_and(|count| *count > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qoro_domain::AgentRuntimeState as S;

    #[test]
    fn the_tick_is_a_quarter_of_the_timeout_within_a_minute_and_five() {
        // The trial timeout ticks every minute; the recommended one every five.
        assert_eq!(
            reap_tick(std::time::Duration::from_secs(60)),
            std::time::Duration::from_secs(60)
        );
        assert_eq!(
            reap_tick(std::time::Duration::from_secs(1800)),
            std::time::Duration::from_secs(300)
        );
        assert_eq!(
            reap_tick(std::time::Duration::from_secs(3600 * 4)),
            std::time::Duration::from_secs(300),
            "long timeouts cap at five minutes"
        );
    }

    #[test]
    fn shells_are_shells_and_tasks_are_not() {
        for shell in ["sh", "bash", "zsh", "fish", "/bin/zsh", "nu", "pwsh"] {
            assert!(is_shell_program(shell), "{shell}");
        }
        for task in [
            "claude", "cargo", "sleep", "python3", "node", "docker", "tail", "watch",
        ] {
            assert!(!is_shell_program(task), "{task}");
        }
        assert_eq!(classify_program(None, false), ForegroundKind::Absent);
        assert_eq!(classify_program(Some("zsh"), false), ForegroundKind::Shell);
        assert_eq!(
            classify_program(Some("claude"), true),
            ForegroundKind::Agent
        );
        assert_eq!(
            classify_program(Some("cargo"), false),
            ForegroundKind::Other
        );
        // The carried bit decides, not the name: `pi` as its own executable is somebody
        // else's tool and vetoes, `pi` under node is the agent.
        assert_eq!(classify_program(Some("pi"), false), ForegroundKind::Other);
        assert_eq!(classify_program(Some("pi"), true), ForegroundKind::Agent);
    }

    /// The eligibility table: every state against every foreground, plus
    /// every veto. `Failed` never closes; `Unknown` closes only for a shell
    /// or an exited tab; the timeout boundary is inclusive.
    #[test]
    fn only_idle_done_and_unknown_shells_past_the_timeout_close() {
        let t = 1800;
        // Idle and done close with an exited, shell or idle-agent foreground.
        for state in [S::Idle, S::Done] {
            for fg in [
                ForegroundKind::Absent,
                ForegroundKind::Shell,
                ForegroundKind::Agent,
            ] {
                assert!(
                    eligible_for_reap(state, fg, false, false, false, t, t),
                    "{state:?} + {fg:?} at the boundary closes"
                );
                assert!(
                    !eligible_for_reap(state, fg, false, false, false, t - 1, t),
                    "{state:?} + {fg:?} a second early stays"
                );
            }
            // A task in the foreground vetoes even a finished agent.
            assert!(!eligible_for_reap(
                state,
                ForegroundKind::Other,
                false,
                false,
                false,
                t * 10,
                t
            ));
        }
        // Unknown closes only for shells and exited tabs: otherwise plain
        // shell tabs, which nothing ever observes, would live forever.
        assert!(eligible_for_reap(
            S::Unknown,
            ForegroundKind::Shell,
            false,
            false,
            false,
            t,
            t
        ));
        assert!(eligible_for_reap(
            S::Unknown,
            ForegroundKind::Absent,
            false,
            false,
            false,
            t,
            t
        ));
        assert!(!eligible_for_reap(
            S::Unknown,
            ForegroundKind::Agent,
            false,
            false,
            false,
            t * 10,
            t
        ));
        assert!(!eligible_for_reap(
            S::Unknown,
            ForegroundKind::Other,
            false,
            false,
            false,
            t * 10,
            t
        ));
        // Every other state never closes, however idle.
        for state in [S::Starting, S::Working, S::Blocked, S::Failed, S::Uncertain] {
            assert!(
                !eligible_for_reap(state, ForegroundKind::Shell, false, false, false, t * 10, t),
                "{state:?} never closes"
            );
        }
        // Every veto holds even for a long-idle finished agent.
        assert!(
            !eligible_for_reap(
                S::Idle,
                ForegroundKind::Shell,
                true,
                false,
                false,
                t * 10,
                t
            ),
            "boundary"
        );
        assert!(
            !eligible_for_reap(
                S::Done,
                ForegroundKind::Agent,
                false,
                true,
                false,
                t * 10,
                t
            ),
            "setup gate"
        );
        assert!(
            !eligible_for_reap(
                S::Idle,
                ForegroundKind::Shell,
                false,
                false,
                true,
                t * 10,
                t
            ),
            "watched"
        );
    }

    #[test]
    fn only_restore_replay_is_not_live_input() {
        assert!(!is_live_input("qoro-restore"));
        for live in ["c1", "", "qoro-restore2", "QORO-RESTORE"] {
            assert!(is_live_input(live), "{live:?} is live");
        }
    }

    #[test]
    fn waiter_edges_veto_only_while_the_target_is_busy() {
        // X waits on Y. Y busy: X stays. Y idle too: both sleep together.
        let live = BTreeMap::from([(1, true), (2, false)]);
        let out = apply_edge_vetoes(&live, &[(1, 2)]);
        assert!(!out[&1], "waiter vetoed while target busy");
        assert!(!out[&2]);
        let both_idle = BTreeMap::from([(1, true), (2, true)]);
        let out = apply_edge_vetoes(&both_idle, &[(1, 2)]);
        assert!(out[&1], "both idle sleep together");
        // A cycle idle on both ends sleeps together, never immortal.
        let out = apply_edge_vetoes(&both_idle, &[(1, 2), (2, 1)]);
        assert!(out[&1] && out[&2], "an idle cycle sleeps together");
        // A chain whose tail works keeps its head: A waits on B, B on C.
        let chain = BTreeMap::from([(1, true), (2, true), (3, false)]);
        for edges in [[(1, 2), (2, 3)], [(2, 3), (1, 2)]] {
            let out = apply_edge_vetoes(&chain, &edges);
            assert!(!out[&1] && !out[&2], "whole chain stays, any order");
        }
        // Target gone: nothing waited on, waiter eligible.
        let out = apply_edge_vetoes(&both_idle, &[(1, 9)]);
        assert!(out[&1]);
        // An ineligible waiter stays ineligible: edges only veto.
        let ineligible = BTreeMap::from([(1, false), (2, false)]);
        let out = apply_edge_vetoes(&ineligible, &[(1, 2)]);
        assert!(!out[&1]);
    }

    #[test]
    fn watcher_counts_track_two_connections_on_one_tab() {
        let counts = WatchCounts::default();
        assert!(!counts.watched(1));
        counts.watch(1);
        counts.watch(1);
        assert!(counts.watched(1));
        counts.unwatch(1);
        assert!(counts.watched(1), "one watcher left");
        counts.unwatch(1);
        assert!(!counts.watched(1));
        // Unbalanced calls saturate rather than poison.
        counts.unwatch(1);
        assert!(!counts.watched(1));
        counts.watch(2);
        counts.unwatch_all(&BTreeSet::from([1, 2]));
        assert!(!counts.watched(2));
    }

    fn row(pid: i32, ppid: i32, pgid: i32, tty: Option<u64>, name: &str) -> qoro_pty::sys::ProcRow {
        qoro_pty::sys::ProcRow {
            pid,
            ppid,
            pgid,
            tty,
            name: name.into(),
        }
    }

    #[test]
    fn a_shell_at_its_prompt_has_nothing_in_the_way() {
        let t = [row(10, 1, 10, Some(5), "zsh")];
        assert!(sleep_blockers(&t, 10, Some(10), 5).is_empty());
    }

    #[test]
    fn claude_idle_with_its_caffeinate_may_sleep() {
        // Measured: Claude Code 2.1.280, idle, one `caffeinate` in its own group.
        let t = [
            row(10, 1, 10, Some(5), "zsh"),
            row(20, 10, 20, Some(5), "claude"),
            row(21, 20, 20, Some(5), "caffeinate"),
        ];
        assert!(sleep_blockers(&t, 10, Some(20), 5).is_empty());
    }

    #[test]
    fn claudes_background_shell_left_the_terminal_and_blocks() {
        // Measured: `run_in_background` starts a setsid shell, tty `??`.
        let t = [
            row(10, 1, 10, Some(5), "zsh"),
            row(20, 10, 20, Some(5), "claude"),
            row(30, 20, 30, None, "zsh"),
            row(31, 30, 30, None, "sleep"),
        ];
        assert_eq!(sleep_blockers(&t, 10, Some(20), 5), vec!["zsh".to_string()]);
    }

    #[test]
    fn codex_mcp_servers_in_their_own_groups_are_its_own() {
        // Measured: codex 0.154, every MCP server in a group of its own, all on the tty.
        let t = [
            row(10, 1, 10, Some(5), "zsh"),
            row(20, 10, 20, Some(5), "codex"),
            row(21, 20, 21, Some(5), "npm"),
            row(22, 21, 21, Some(5), "node"),
            row(23, 20, 23, Some(5), "app-mcp"),
            row(24, 23, 24, Some(5), "Google Chrome"),
        ];
        assert!(sleep_blockers(&t, 10, Some(20), 5).is_empty());
    }

    #[test]
    fn a_job_the_shell_put_in_the_background_blocks() {
        let t = [
            row(10, 1, 10, Some(5), "zsh"),
            row(40, 10, 40, Some(5), "postgres"),
        ];
        assert_eq!(
            sleep_blockers(&t, 10, Some(10), 5),
            vec!["postgres".to_string()]
        );
    }

    #[test]
    fn a_nested_multiplexer_server_blocks() {
        // A nested multiplexer's server, started from a shell, leaves the terminal.
        let t = [
            row(10, 1, 10, Some(5), "zsh"),
            row(20, 10, 20, Some(5), "muxd"),
            row(21, 20, 21, None, "muxd"),
        ];
        assert_eq!(
            sleep_blockers(&t, 10, Some(20), 5),
            vec!["muxd".to_string()]
        );
    }
}
