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
/// see: a `cargo`, `sleep` or `python` in front, which a snapshot taken before
/// it started, or one a silent command never refreshed, still reports as the
/// shell.
///
/// Every process of the group is judged the same way, its leader included. A
/// shell with job control (every tab's, started `-i`) gives each command it
/// runs a group of its own, so `sleep 75` typed at the prompt is that group's
/// leader and its only member: skipping the leader read it as nothing.
/// Measured on a Linux remote, where the snapshot stayed `bash, idle` and a
/// move went ahead under a running `sleep`. A shell at its prompt leads the
/// foreground group itself and reads as no task, and so does a known agent.
///
/// Runs only on reap candidates once per reap tick, never on the hot path.
/// `true` means veto. Fail closed: an unreadable process vetoes (it only
/// skips one round; the next tick retries), while no foreground group at
/// all means nothing to protect. Backgrounded (`&`) jobs outside the
/// foreground group are NOT seen here, which is a stated residual of v1.
pub fn group_holds_task(master: std::os::fd::RawFd) -> bool {
    let Some(leader) = qoro_pty::sys::foreground_leader(master) else {
        return false;
    };
    let members = qoro_pty::sys::group_members(qoro_pty::Pgid(leader.0));
    for member in members {
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
/// them again when it resumes. A process that says it may be lost (`sleep_safe`: the
/// `QORO_SLEEP_SAFE=1` mark on its command line, which a watcher that ends with its agent
/// carries) blocks nothing, nor does anything under it. Pure, so the measured trees are its
/// tests.
pub fn sleep_blockers(
    table: &[qoro_pty::sys::ProcRow],
    leader: i32,
    foreground: Option<i32>,
    tty: u64,
    sleep_safe: &dyn Fn(i32) -> bool,
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
            let foreground_here =
                under_foreground || (Some(child.pgid) == foreground && child.pgid != shell_group);
            if child.tty != Some(tty) || (!foreground_here && child.pgid != shell_group) {
                if !sleep_safe(child.pid) {
                    blockers.push(child.name.clone());
                }
                continue;
            }
            stack.push((child.pid, foreground_here));
        }
    }
    blockers.sort();
    blockers.dedup();
    blockers
}

/// How many background agents Claude Code is running inside this screen's tab.
///
/// A subagent lives in Claude's own process, so `sleep_blockers` never sees it, and with the
/// main turn over the tab reads idle: a sleep would end the agent's work with it. Claude
/// draws a panel below the prompt box while any exist: a footer line that ends
/// `↓ to manage`, then the lead's own row (`⏺ main`), then one `◯` row per agent. A running
/// agent's row ends in its counter (`4s · ↓ 35.8k tokens`); a finished teammate that stays
/// listed says `idle` instead and holds nothing. The shell and monitor counts in the footer
/// are left out on purpose: they are processes, judged by `sleep_blockers`, which lets a
/// `QORO_SLEEP_SAFE` watcher go. The footer, the lead row and the glyph are all required,
/// so a panel quoted in an answer (a `qoro read` of another tab) is not mistaken for one.
/// Measured on Claude Code 2.1.284. Pure, so the measured screens are its tests.
pub fn running_background_agents(screen: &[String]) -> usize {
    let Some(footer) = screen
        .iter()
        .rposition(|row| row.trim_end().ends_with("↓ to manage"))
    else {
        return 0;
    };
    let mut rows = screen[footer + 1..]
        .iter()
        .map(|row| row.trim())
        .filter(|row| !row.is_empty());
    if !rows.next().is_some_and(|lead| lead.starts_with('⏺')) {
        return 0;
    }
    rows.filter(|row| {
        row.starts_with('◯')
            && row.ends_with(" tokens")
            && (row.contains(" · ↑ ") || row.contains(" · ↓ "))
    })
    .count()
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

/// The wait that keeps a person's sleep of `sleeping` from going ahead, if any: a tab in
/// that set waiting on one that stays awake. The same rule as [`apply_edge_vetoes`]: only
/// the waiter is held, never the tab it waits on, which can be slept and woken again; and a
/// waiter sleeping together with its target, or after it, cuts nothing.
pub fn holding_wait(
    edges: &[(u32, u32)],
    sleeping: &[u32],
    asleep: impl Fn(u32) -> bool,
) -> Option<(u32, u32)> {
    edges.iter().copied().find(|(waiter, target)| {
        sleeping.contains(waiter) && !sleeping.contains(target) && !asleep(*target)
    })
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
    /// When each terminal's last watcher left, unix seconds: a tab read until a moment ago
    /// has been idle for that moment, not for as long as its agent has.
    left: std::sync::Arc<std::sync::Mutex<BTreeMap<u32, i64>>>,
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
                self.left
                    .lock()
                    .unwrap()
                    .insert(stream, crate::agents::now_unix());
            }
            None => {}
        }
    }

    /// When the last watcher of this terminal left, if one ever did.
    pub fn left_at(&self, stream: u32) -> Option<i64> {
        self.left.lock().unwrap().get(&stream).copied()
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
    fn a_sleep_is_held_only_by_a_waiter_whose_target_stays_awake() {
        // 1 waits on 2.
        let edges = [(1, 2)];
        let awake = |_| false;
        assert_eq!(
            holding_wait(&edges, &[2], awake),
            None,
            "the tab waited on sleeps"
        );
        assert_eq!(
            holding_wait(&edges, &[1], awake),
            Some((1, 2)),
            "the waiter is held"
        );
        assert_eq!(
            holding_wait(&edges, &[1, 2], awake),
            None,
            "both sleep together"
        );
        assert_eq!(
            holding_wait(&edges, &[1], |t| t == 2),
            None,
            "its target already sleeps"
        );
        assert_eq!(holding_wait(&edges, &[3], awake), None, "an unrelated tab");
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
        assert!(
            counts.left_at(1).is_some(),
            "the last watcher leaving is written down"
        );
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
        assert!(sleep_blockers(&t, 10, Some(10), 5, &|_| false).is_empty());
    }

    #[test]
    fn claude_idle_with_its_caffeinate_may_sleep() {
        // Measured: Claude Code 2.1.280, idle, one `caffeinate` in its own group.
        let t = [
            row(10, 1, 10, Some(5), "zsh"),
            row(20, 10, 20, Some(5), "claude"),
            row(21, 20, 20, Some(5), "caffeinate"),
        ];
        assert!(sleep_blockers(&t, 10, Some(20), 5, &|_| false).is_empty());
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
        assert_eq!(
            sleep_blockers(&t, 10, Some(20), 5, &|_| false),
            vec!["zsh".to_string()]
        );
    }

    #[test]
    fn a_background_shell_marked_sleep_safe_lets_it_sleep() {
        // qass's watcher: a setsid shell that ends by itself once its agent is gone.
        let t = [
            row(10, 1, 10, Some(5), "zsh"),
            row(20, 10, 20, Some(5), "claude"),
            row(30, 20, 30, None, "zsh"),
            row(31, 30, 30, None, "bash"),
            row(40, 20, 40, None, "zsh"),
        ];
        assert_eq!(
            sleep_blockers(&t, 10, Some(20), 5, &|pid| pid == 30),
            vec!["zsh".to_string()],
            "only the unmarked one blocks"
        );
        assert!(sleep_blockers(&t, 10, Some(20), 5, &|pid| pid == 30 || pid == 40).is_empty());
    }

    fn screen(rows: &[&str]) -> Vec<String> {
        rows.iter().map(|r| (*r).to_string()).collect()
    }

    #[test]
    fn a_running_background_agent_row_is_counted() {
        // Measured: Claude Code 2.1.284 with one subagent in the background, main turn over.
        let s = screen(&[
            "❯",
            "────────────────────────────────────────",
            "  ⏵⏵ bypass permissions on · 1 shell · ← 5 agents · ↓ to manage",
            "",
            "  ⏺ main",
            "  ◯ sleep-agent  Run this shell command and report done          4s · ↓ 35.8k tokens",
            "  ◯ rs-reader    Find the 20 largest .rs files under /Users/u...  1m 5s · ↑ 50 tokens",
        ]);
        assert_eq!(running_background_agents(&s), 2);
    }

    #[test]
    fn a_finished_teammate_and_a_plain_footer_hold_nothing() {
        let finished = screen(&[
            "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← 5 agents · ↓ to manage",
            "  ⏺ main",
            "  ◯ hi-agent-2  Answer with the word hi and nothing else.  idle",
        ]);
        assert_eq!(running_background_agents(&finished), 0);
        let none = screen(&[
            "❯",
            "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← 5 agents",
        ]);
        assert_eq!(running_background_agents(&none), 0);
    }

    #[test]
    fn a_whole_panel_quoted_in_an_answer_is_not_a_running_agent() {
        // A `qoro read` of another tab, pasted into an answer: the footer is quoted too,
        // but no lead row follows it, and the real footer below has no panel.
        let s = screen(&[
            "⏺ the other tab shows:",
            "  ⏵⏵ bypass permissions on · 1 shell · ← 5 agents · ↓ to manage",
            "  ◯ rs-reader  Find the largest files    4s · ↓ 35.8k tokens",
            "❯",
            "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← 5 agents",
        ]);
        assert_eq!(running_background_agents(&s), 0);
        // A footer quoted mid-line does not end the way the real one does.
        let mid = screen(&[
            "⏺ it says `↓ to manage` when something runs",
            "  ⏺ main",
            "  ◯ x  y    4s · ↓ 1k tokens",
        ]);
        assert_eq!(running_background_agents(&mid), 0);
    }

    #[test]
    fn a_counter_quoted_above_the_footer_is_not_a_running_agent() {
        let s = screen(&[
            "⏺ the row reads `◯ x  desc  4s · ↓ 1k tokens` while it runs",
            "  ◯ old  desc  4s · ↓ 1k tokens",
            "  ⏵⏵ bypass permissions on · ← 5 agents · ↓ to manage",
            "  ⏺ main",
        ]);
        assert_eq!(running_background_agents(&s), 0);
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
        assert!(sleep_blockers(&t, 10, Some(20), 5, &|_| false).is_empty());
    }

    #[test]
    fn a_job_the_shell_put_in_the_background_blocks() {
        let t = [
            row(10, 1, 10, Some(5), "zsh"),
            row(40, 10, 40, Some(5), "postgres"),
        ];
        assert_eq!(
            sleep_blockers(&t, 10, Some(10), 5, &|_| false),
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
            sleep_blockers(&t, 10, Some(20), 5, &|_| false),
            vec!["muxd".to_string()]
        );
    }

    /// The foreground of a real job-control shell, step by step: at its prompt, running a
    /// silent `sleep` that leads a group of its own and is its only member, and running a
    /// known agent. Only the `sleep` is a task.
    #[test]
    fn a_command_leading_its_own_foreground_group_is_a_task() {
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;
        use std::time::{Duration, Instant};

        let pty = qoro_pty::sys::open_pty(80, 24).expect("pty");
        let master = pty.master.as_raw_fd();
        let slave = pty.slave.as_raw_fd();
        let mut command = std::process::Command::new("/bin/sh");
        // Interactive, as every tab's shell is started, so it gives each command a group.
        command.arg("-i").env("PS1", "$ ").env_remove("ENV");
        // SAFETY: post-fork, only async-signal-safe calls and no allocation: the session
        // setup the daemon performs for a tab.
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                for target in 0..3 {
                    if libc::dup2(slave, target) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut shell = command.spawn().expect("spawn");
        drop(pty.slave);
        qoro_pty::sys::set_nonblocking(master).expect("nonblocking");
        let shell_pid = shell.id() as i32;
        let agent_dir = tempfile::tempdir().expect("tempdir");
        let agent = agent_dir.path().join("droid");
        std::fs::write(&agent, "#!/bin/sh\nread line\n").expect("agent");
        std::fs::set_permissions(&agent, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod");

        // Drains what the shell prints while waiting for the foreground to be `program`.
        let front_is = |program: &str| {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut sink = [0u8; 4096];
            loop {
                // SAFETY: master is open and sink is a writable buffer of its length.
                let _ = unsafe {
                    libc::read(master, sink.as_mut_ptr() as *mut libc::c_void, sink.len())
                };
                let leader = qoro_pty::sys::foreground_leader(master);
                let name = leader.and_then(qoro_pty::sys::program_of).map(|path| {
                    let argv = qoro_pty::sys::argv_of(leader.expect("leader"));
                    qoro_domain::program::identify(&path, &argv).name
                });
                let at_prompt = program == "shell" && leader.map(|p| p.0) == Some(shell_pid);
                if at_prompt || (program != "shell" && name.as_deref() == Some(program)) {
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "{program} never came to the front: {name:?}"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        let type_in = |line: &str| {
            // SAFETY: master is open and the buffer is the line's bytes.
            unsafe { libc::write(master, line.as_ptr() as *const libc::c_void, line.len()) };
        };

        front_is("shell");
        assert!(!group_holds_task(master), "a shell at its prompt is a task");

        type_in("sleep 30\n");
        front_is("sleep");
        let group = qoro_pty::sys::foreground_leader(master).expect("a foreground group");
        assert_ne!(
            group.0, shell_pid,
            "the shell ran the command without job control"
        );
        assert_eq!(
            qoro_pty::sys::group_members(qoro_pty::Pgid(group.0)),
            vec![group],
            "the command is not alone in its group, so this is not the case under test"
        );
        assert!(
            group_holds_task(master),
            "a lone sleep leading its group was missed"
        );

        // SAFETY: a signal to the group this test's shell started.
        unsafe { libc::kill(-group.0, libc::SIGKILL) };
        front_is("shell");
        type_in(&format!("{}\n", agent.display()));
        front_is("droid");
        assert!(!group_holds_task(master), "an agent in front is a task");

        if let Some(group) = qoro_pty::sys::foreground_leader(master) {
            // SAFETY: as above.
            unsafe { libc::kill(-group.0, libc::SIGKILL) };
        }
        let _ = shell.kill();
        let _ = shell.wait();
    }
}
