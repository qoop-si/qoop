//! Idle auto-sleep: close terminals idle past a configurable timeout.
//!
//! Disabled by default and armed by one environment variable,
//! `QORO_IDLE_TIMEOUT_MINUTES`. `0`, unset, empty or invalid means the reaper
//! task is never spawned: an idle daemon then pays zero wakeups for this
//! feature, which is what keeps ADR-009's macOS literal-zero-idle claim and
//! the latency gate intact. `30` is the documented recommended opt-in value.
//!
//! Eligibility is fail closed throughout: an unknown program, a boundary
//! identity, a setup gate, a watched terminal, or a full command queue all
//! veto. A reaped tab closes through the existing `CloseTerminal` path, so
//! the agent's own conversation survives in the agent's own store and the
//! scrollback files stay on disk, but the durable qoro row (including the
//! recorded `agent_session`) is removed exactly like a manual close, and a
//! later restore does not bring the tab back.

use std::collections::{BTreeMap, BTreeSet};

/// The environment variable that arms the reaper, in whole minutes.
pub const IDLE_TIMEOUT_ENV: &str = "QORO_IDLE_TIMEOUT_MINUTES";

/// The recommended opt-in value, in minutes. Not a default: the default is
/// disabled. Thirty minutes is what the feature was asked for.
pub const RECOMMENDED_IDLE_TIMEOUT_MINUTES: u64 = 30;

/// Parse the timeout from an environment value.
///
/// `None` (unset) and `"0"` mean disabled. Anything that is not a plain
/// non-negative integer (floats, suffixes like `30m`, signs, whitespace
/// around digits is trimmed then must be all digits, overflow) is disabled
/// too: a misconfigured reaper must never close tabs, and the caller logs
/// the raw value once. Minutes convert to seconds saturating, never wrapping.
pub fn parse_idle_timeout(env: Option<&str>) -> Option<std::time::Duration> {
    let raw = env?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let minutes: u64 = trimmed.parse().ok()?;
    if minutes == 0 {
        return None;
    }
    Some(std::time::Duration::from_secs(minutes.saturating_mul(60)))
}

/// How often the reaper wakes while armed: a quarter of the timeout,
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

/// Classify a foreground program name for the reap decision.
pub fn classify_program(program: Option<&str>) -> ForegroundKind {
    match program {
        None => ForegroundKind::Absent,
        Some(name) if is_shell_program(name) => ForegroundKind::Shell,
        Some(name) if qoro_domain::program::identify(name, &[]).agent => ForegroundKind::Agent,
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
/// outcome independent of pass order. A waiter is vetoed while its wait is
/// observably alive: it has shown no activity since asking
/// (`idle_since <= since`) and its target is tracked and not itself
/// eligible. Both idle together sleep together, cycles included; a target
/// that is gone vetoes nothing, because nothing is being waited on any
/// more.
pub fn apply_edge_vetoes(
    eligible: &BTreeMap<u32, bool>,
    edges: &[(u32, u32, i64)],
    idle_since: &BTreeMap<u32, i64>,
) -> BTreeMap<u32, bool> {
    let mut out = eligible.clone();
    for (waiter, target, since) in edges {
        if !eligible.get(waiter).copied().unwrap_or(false) {
            continue;
        }
        let quiet_since_asking = idle_since.get(waiter).is_some_and(|idle| *idle <= *since);
        if !quiet_since_asking {
            continue;
        }
        // The target is alive but busy: the wait is alive, the waiter stays.
        // Eligible target: both sleep together. Absent target: gone, and a
        // wait on nothing is not a wait.
        if eligible.get(target).is_some_and(|e| !e) {
            out.insert(*waiter, false);
        }
    }
    out
}

/// Per-terminal watcher refcounts: how many live connections hold a render
/// subscription on each terminal.
///
/// The reaper skips watched terminals: closing a tab somebody is looking at
/// is the one outcome this feature must never produce. Counts, not flags,
/// because two connections may watch the same tab.
///
/// Entries for closed terminals are removed on unsubscribe and disconnect,
/// which always follow a close for a watched tab. A lingering entry is
/// harmless in any case: stream numbers are never reused within a run, so
/// it can never match a later terminal.
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
    fn the_timeout_parses_plain_minutes_and_nothing_else() {
        assert_eq!(parse_idle_timeout(None), None, "unset is disabled");
        assert_eq!(parse_idle_timeout(Some("0")), None);
        assert_eq!(parse_idle_timeout(Some("")), None);
        assert_eq!(parse_idle_timeout(Some("   ")), None);
        assert_eq!(
            parse_idle_timeout(Some("30")),
            Some(std::time::Duration::from_secs(1800))
        );
        assert_eq!(
            parse_idle_timeout(Some("  30  ")),
            Some(std::time::Duration::from_secs(1800)),
            "surrounding whitespace is trimmed"
        );
        // Everything else is disabled, never a panic and never seconds.
        for bad in ["-1", "30m", "30.5", "+30", "3 0", "abc", "0x1e"] {
            assert_eq!(parse_idle_timeout(Some(bad)), None, "{bad}");
        }
        // Overflow saturates instead of wrapping: parses as minutes but
        // exceeds seconds range once multiplied.
        let huge = parse_idle_timeout(Some("307445734561825861")).expect("saturates");
        assert_eq!(huge, std::time::Duration::from_secs(u64::MAX));
        // Beyond u64 entirely: disabled, never a panic.
        assert_eq!(parse_idle_timeout(Some("99999999999999999999")), None);
    }

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
        assert_eq!(classify_program(None), ForegroundKind::Absent);
        assert_eq!(classify_program(Some("zsh")), ForegroundKind::Shell);
        assert_eq!(classify_program(Some("claude")), ForegroundKind::Agent);
        assert_eq!(classify_program(Some("cargo")), ForegroundKind::Other);
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
    fn waiter_edges_veto_only_while_the_wait_is_observably_alive() {
        // X waits on Y. Y busy: X stays. Y idle too: both sleep together.
        let live = BTreeMap::from([(1, true), (2, false)]);
        let idle = BTreeMap::from([(1, 100), (2, 50)]);
        let out = apply_edge_vetoes(&live, &[(1, 2, 100)], &idle);
        assert!(!out[&1], "waiter vetoed while target busy");
        assert!(!out[&2]);
        let both_idle = BTreeMap::from([(1, true), (2, true)]);
        let out = apply_edge_vetoes(&both_idle, &[(1, 2, 100)], &idle);
        assert!(out[&1], "both idle sleep together");
        // A cycle idle on both ends sleeps together, never immortal.
        let out = apply_edge_vetoes(&both_idle, &[(1, 2, 100), (2, 1, 100)], &idle);
        assert!(out[&1] && out[&2], "an idle cycle sleeps together");
        // Target gone: nothing waited on, waiter eligible.
        let out = apply_edge_vetoes(&both_idle, &[(1, 9, 100)], &idle);
        assert!(out[&1]);
        // Waiter active since asking: the wait is over, edge inert.
        let busy_waiter = BTreeMap::from([(1, 200), (2, 50)]);
        let out = apply_edge_vetoes(&both_idle, &[(1, 2, 100)], &busy_waiter);
        assert!(out[&1], "activity since asking ends the wait");
        // An ineligible waiter stays ineligible: edges only veto.
        let ineligible = BTreeMap::from([(1, false), (2, false)]);
        let out = apply_edge_vetoes(&ineligible, &[(1, 2, 100)], &idle);
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
}
