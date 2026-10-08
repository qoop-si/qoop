//! The teardown state machine, as a pure function of observations.
//!
//! Everything here was measured before it was written, and ADR-014 carries the numbers.
//! The three facts that shape it:
//!
//! 1. `kill(-pgid)` does not reach a job that put itself in another process group, which is
//!    what every job-control shell does for every job. Measured: leader dead, `nohup` job
//!    still running after the full escalation.
//! 2. The containment set is **empty** once the leader dies, because POSIX disassociates a
//!    controlling terminal when the controlling process terminates. Measured: five members
//!    before teardown, zero after. So the set must be captured before the first signal and
//!    unioned across rounds, never replaced.
//! 3. An exiting child is not reapable until its pending tty output is drained. Closing the
//!    master appears to fix it only because closing discards that output, which is the
//!    shell's logout message or an agent's final summary.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// One escalation step's grace period.
pub const GRACE: Duration = Duration::from_millis(250);
/// Total budget from entering `Closing` to reaped, referenced by gate G1.19 rather than
/// left as an implied sum of per-step waits.
pub const TEARDOWN_DEADLINE: Duration = Duration::from_secs(2);
/// How much of a still-writing child's final output is retained. Reading must not stop,
/// because stopping it is what blocks the reap; retaining everything is a separate decision
/// and this is it.
pub const RETAINED_TAIL: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pid(pub i32);
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pgid(pub i32);

/// A member of the containment set.
///
/// `start_time` is the pid-reuse guard. The set never shrinks, so a group whose members all
/// died during the first round is still signalled 500 ms later, and pids are allocated
/// sequentially on a machine that can turn over hundreds per second. `e_tdev` cannot be the
/// re-verification key because it is exactly what the kernel revokes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Member {
    pub pid: Pid,
    pub pgid: Pgid,
    pub start_time: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    Hup,
    Term,
    Kill,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Signal these groups with this signal, then wait `GRACE`, draining throughout.
    SignalGroups { signal: Signal, groups: Vec<Pgid> },
    /// Poll for exit, still draining.
    Reap,
    /// Close the master. Last, and only now.
    CloseMaster,
    /// The deadline expired with the child still alive. Reported, never silently ignored.
    Failed { reason: &'static str },
}

/// The containment set: a monotonic union that can only grow.
#[derive(Clone, Debug, Default)]
pub struct Containment {
    members: BTreeMap<Pid, Member>,
    /// Set by the first `absorb`. The ordering requirement, that the set is captured before
    /// the first signal, was measured to be the difference between containing the
    /// terminal's processes and containing nothing, so it is state rather than a comment.
    captured: bool,
}

impl Containment {
    pub fn new() -> Self {
        Self::default()
    }

    /// Merge an enumeration into the set. Members are never removed, because the kernel
    /// removing them from the enumeration is the exact case this exists to survive.
    pub fn absorb(&mut self, enumerated: impl IntoIterator<Item = Member>) {
        self.captured = true;
        for m in enumerated {
            // The same process read again keeps its place but takes its current group: a
            // job that moved group was otherwise signalled at the old one, which the
            // membership check refuses, and teardown ran to its deadline with it alive.
            // A different start time is a different process, and the recorded one stays.
            let entry = self.members.entry(m.pid).or_insert(m);
            if entry.start_time == m.start_time {
                entry.pgid = m.pgid;
            }
        }
    }

    /// Has an enumeration happened at all? Round 0 signalling without one contains nothing,
    /// and the set is empty from then on because the kernel has revoked the tty association.
    pub fn was_captured(&self) -> bool {
        self.captured
    }

    pub fn len(&self) -> usize {
        self.members.len()
    }
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }
    pub fn contains(&self, pid: Pid) -> bool {
        self.members.contains_key(&pid)
    }

    /// Forget members that are gone.
    ///
    /// `absorb` never removes, which is what makes the set monotonic while the terminal
    /// runs, and unbounded if nothing ever prunes it. A terminal that has run a day of
    /// builds reaches ten thousand recorded members, which is 2.6 milliseconds of syscalls
    /// per teardown loop iteration and ten thousand chances to signal a recycled group.
    /// Pruning at the point where they are probed anyway costs nothing.
    pub fn prune(&mut self, still_alive: &dyn Fn(Pid, u64) -> bool) -> usize {
        let before = self.members.len();
        self.members.retain(|_, m| still_alive(m.pid, m.start_time));
        before - self.members.len()
    }

    /// Whether any recorded member still exists with the start time it was recorded with.
    ///
    /// This is what "the terminal is gone" means. The child being reaped is a different
    /// and weaker fact: a job that left the process group outlives the shell that started
    /// it, and stopping at the reap leaves it running forever with nobody to attribute it
    /// to.
    pub fn any_alive(&self, still_alive: &dyn Fn(Pid, u64) -> bool) -> bool {
        self.members
            .values()
            .any(|m| still_alive(m.pid, m.start_time))
    }

    /// The groups to signal this round.
    ///
    /// A group is skipped unless at least one of its recorded members still exists with the
    /// start time it was recorded with. That check used to be skipped on the first round,
    /// justified by the set being freshly enumerated. It is not fresh any more: the set is
    /// absorbed every fifty milliseconds for the terminal's whole life, so the first round,
    /// the one that signalled without the guard, was the round with the stalest data. A
    /// terminal open all day would record a build's pid in the morning, and in the evening
    /// signal whatever unrelated process group had inherited that number.
    ///
    /// The probe costs 248 nanoseconds, measured, so there was never anything to buy.
    ///
    /// Alive is not enough either: the member has to still be in the group. One that left
    /// it (`setsid`, `setpgid`, as daemonizing tools do) kept a group "alive" after its
    /// number was free, and teardown signalled whatever group had the number by then.
    /// `in_group` is that check, `qoop_pty::sys::in_group` in production.
    pub fn groups_to_signal(
        &self,
        round: usize,
        still_alive: &dyn Fn(Pid, u64) -> bool,
        in_group: &dyn Fn(Pid, u64, Pgid) -> bool,
    ) -> Vec<Pgid> {
        let _ = round;
        // Group once and probe each pid at most once. The obvious nested version is
        // quadratic in the number of members, and a terminal running a build can have
        // hundreds, on a path that runs while a user is waiting for a terminal to close.
        let mut by_group: BTreeMap<Pgid, Vec<&Member>> = BTreeMap::new();
        for m in self.members.values() {
            by_group.entry(m.pgid).or_default().push(m);
        }
        by_group
            .into_iter()
            .filter(|(pgid, members)| {
                members.iter().any(|o| {
                    still_alive(o.pid, o.start_time) && in_group(o.pid, o.start_time, *pgid)
                })
            })
            .map(|(pgid, _)| pgid)
            .collect()
    }
}

/// The ordered plan. Written as a sequence rather than as prose because step 3 before step
/// 5 is the difference between containing the terminal's processes and containing nothing.
pub fn plan(round: usize) -> Option<Signal> {
    match round {
        0 => Some(Signal::Hup),
        1 => Some(Signal::Term),
        2 => Some(Signal::Kill),
        _ => None,
    }
}

/// Decide the next step. `started` is when `Closing` was entered.
pub fn next_step(
    round: usize,
    now: Instant,
    started: Instant,
    child_reaped: bool,
    containment: &Containment,
    still_alive: &dyn Fn(Pid, u64) -> bool,
    in_group: &dyn Fn(Pid, u64, Pgid) -> bool,
) -> Step {
    // Reaping the child is necessary but not sufficient. Closing here was the first
    // version, and it meant that the ordinary case, a user typing `exit`, signalled nothing
    // at all: the shell reaps immediately and every job it left behind survived. The
    // containment set exists for exactly those processes, so it is what decides when
    // teardown is done.
    if child_reaped && !containment.any_alive(still_alive) {
        return Step::CloseMaster;
    }
    if now.duration_since(started) > TEARDOWN_DEADLINE {
        return Step::Failed {
            reason: "teardown deadline expired with the child still alive",
        };
    }
    if round == 0 && !containment.was_captured() {
        // Unrepresentable beats documented. Signalling before any enumeration means the set
        // is empty now and stays empty, because the kernel revokes the tty association when
        // the leader dies.
        return Step::Failed {
            reason: "containment set was not captured before the first signal",
        };
    }
    match plan(round) {
        Some(signal) => {
            let groups = containment.groups_to_signal(round, still_alive, in_group);
            if groups.is_empty() {
                Step::Reap
            } else {
                Step::SignalGroups { signal, groups }
            }
        }
        None => Step::Reap,
    }
}

/// Keep only the last `RETAINED_TAIL` bytes of what teardown drained.
pub fn retain_tail(buf: &mut Vec<u8>, incoming: &[u8]) -> usize {
    buf.extend_from_slice(incoming);
    if buf.len() > RETAINED_TAIL {
        let drop = buf.len() - RETAINED_TAIL;
        buf.drain(..drop);
        drop
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pid: i32, pgid: i32, start: u64) -> Member {
        Member {
            pid: Pid(pid),
            pgid: Pgid(pgid),
            start_time: start,
        }
    }
    fn all_alive(_: Pid, _: u64) -> bool {
        true
    }
    fn none_alive(_: Pid, _: u64) -> bool {
        false
    }
    fn in_all(_: Pid, _: u64, _: Pgid) -> bool {
        true
    }

    #[test]
    fn the_containment_set_never_shrinks() {
        // The measured case: enumeration finds five members before teardown and zero after
        // the leader dies, because the kernel disassociates the controlling terminal.
        let mut c = Containment::new();
        c.absorb([m(10, 10, 1), m(11, 11, 2)]);
        assert_eq!(c.len(), 2);
        c.absorb([]); // the post-death enumeration, which finds nothing
        assert_eq!(c.len(), 2, "a member must never leave the set");
        assert!(c.contains(Pid(11)));
    }

    #[test]
    fn a_job_in_its_own_group_is_signalled() {
        // The measured escape: a nohup job in the leader's session with its own pgid
        // survives kill(-leader_pgid). It must appear as a separate group here.
        let mut c = Containment::new();
        c.absorb([m(10, 10, 1), m(11, 11, 2)]);
        let groups = c.groups_to_signal(0, &all_alive, &in_all);
        assert!(groups.contains(&Pgid(10)));
        assert!(groups.contains(&Pgid(11)));
    }

    #[test]
    fn a_group_whose_members_are_all_gone_is_not_signalled_again() {
        // The pid-reuse guard. The set never shrinks, so without this a recycled group id
        // is signalled 500 ms later and hits an unrelated process's whole group.
        let mut c = Containment::new();
        c.absorb([m(10, 10, 1)]);
        assert_eq!(
            c.groups_to_signal(1, &none_alive, &in_all),
            Vec::<Pgid>::new()
        );
        assert_eq!(c.groups_to_signal(1, &all_alive, &in_all), vec![Pgid(10)]);
    }

    #[test]
    fn a_group_whose_members_are_all_gone_is_not_signalled_in_any_round() {
        // Including round zero. The set is maintained for the terminal's whole life, so the
        // first round is the one with the oldest entries, not the freshest.
        let mut c = Containment::new();
        c.absorb([m(10, 10, 1)]);
        assert!(c.groups_to_signal(0, &none_alive, &in_all).is_empty());
        assert_eq!(c.groups_to_signal(0, &all_alive, &in_all), vec![Pgid(10)]);
    }

    /// A member that left its group (`setsid`, `setpgid`, as daemonizing tools do) does
    /// not keep that group alive: the number may belong to a stranger's group by now.
    #[test]
    fn a_member_that_left_its_group_does_not_keep_the_group_signalled() {
        let mut c = Containment::new();
        c.absorb([m(20, 10, 1)]);
        let left = |_: Pid, _: u64, _: Pgid| false;
        assert!(c.groups_to_signal(0, &all_alive, &left).is_empty());
        assert_eq!(c.groups_to_signal(0, &all_alive, &in_all), vec![Pgid(10)]);
    }

    /// Read again after it moved group, the same process is signalled at its new group;
    /// a different process under a reused pid does not replace the recorded one.
    #[test]
    fn a_member_read_again_in_a_new_group_is_signalled_there() {
        let mut c = Containment::new();
        c.absorb([m(20, 10, 1)]);
        c.absorb([m(20, 30, 1)]);
        let in_thirty = |_: Pid, _: u64, g: Pgid| g == Pgid(30);
        assert_eq!(
            c.groups_to_signal(0, &all_alive, &in_thirty),
            vec![Pgid(30)]
        );
        c.absorb([m(20, 40, 2)]);
        assert_eq!(
            c.groups_to_signal(0, &all_alive, &in_thirty),
            vec![Pgid(30)]
        );
    }

    #[test]
    fn pruning_drops_members_that_are_gone_and_keeps_the_rest() {
        let mut c = Containment::new();
        c.absorb([m(10, 10, 1), m(11, 11, 2)]);
        let alive = |p: Pid, _: u64| p == Pid(11);
        assert_eq!(c.prune(&alive), 1);
        assert_eq!(c.len(), 1);
        assert!(c.contains(Pid(11)));
    }

    #[test]
    fn a_recorded_start_time_mismatch_drops_the_group() {
        let mut c = Containment::new();
        c.absorb([m(10, 10, 111)]);
        let recycled = |_: Pid, start: u64| start == 999;
        assert_eq!(
            c.groups_to_signal(1, &recycled, &in_all),
            Vec::<Pgid>::new()
        );
    }

    #[test]
    fn signalling_before_any_enumeration_is_refused() {
        let t = Instant::now();
        let empty = Containment::new();
        assert!(matches!(
            next_step(0, t, t, false, &empty, &all_alive, &in_all),
            Step::Failed { .. }
        ));
        // After an enumeration, even one that found nothing, the machine proceeds.
        let mut captured = Containment::new();
        captured.absorb([]);
        assert_eq!(
            next_step(0, t, t, false, &captured, &all_alive, &in_all),
            Step::Reap
        );
    }

    #[test]
    fn grouping_probes_each_pid_at_most_once() {
        use std::cell::RefCell;
        let mut c = Containment::new();
        // Many members sharing few groups, which is what a build in a terminal looks like.
        c.absorb((0..500).map(|i| m(1000 + i, 1000 + (i % 3), i as u64)));
        let probes = RefCell::new(0usize);
        let counting = |_: Pid, _: u64| {
            *probes.borrow_mut() += 1;
            true
        };
        let groups = c.groups_to_signal(1, &counting, &in_all);
        assert_eq!(groups.len(), 3);
        assert!(
            *probes.borrow() <= 500,
            "each pid must be probed at most once, saw {}",
            probes.borrow()
        );
    }

    #[test]
    fn the_escalation_order_is_hup_term_kill_then_reap() {
        assert_eq!(plan(0), Some(Signal::Hup));
        assert_eq!(plan(1), Some(Signal::Term));
        assert_eq!(plan(2), Some(Signal::Kill));
        assert_eq!(plan(3), None);
    }

    #[test]
    fn the_master_closes_last_and_only_after_the_reap() {
        let t = Instant::now();
        let mut c = Containment::new();
        c.absorb([m(10, 10, 1)]);
        // Not reaped yet: never close.
        assert!(!matches!(
            next_step(0, t, t, false, &c, &all_alive, &in_all),
            Step::CloseMaster
        ));
        // Reaped, but a member of the containment set is still running: not done either.
        assert!(!matches!(
            next_step(1, t, t, true, &c, &all_alive, &in_all),
            Step::CloseMaster
        ));
        // Reaped and nothing left on the terminal: close, and only now.
        assert_eq!(
            next_step(3, t, t, true, &c, &none_alive, &in_all),
            Step::CloseMaster
        );
    }

    #[test]
    fn a_reaped_child_does_not_end_teardown_while_an_escaped_job_runs() {
        // The regression this is shaped around: the user types `exit`, the shell is reaped
        // within milliseconds, and a `nohup` job in its own process group keeps running.
        let t = Instant::now();
        let mut c = Containment::new();
        c.absorb([m(10, 10, 1), m(11, 11, 2)]);
        let step = next_step(1, t, t, true, &c, &all_alive, &in_all);
        assert_eq!(
            step,
            Step::SignalGroups {
                signal: Signal::Term,
                groups: vec![Pgid(10), Pgid(11)],
            }
        );
    }

    #[test]
    fn the_deadline_produces_a_reported_failure_not_a_silent_give_up() {
        let t = Instant::now();
        let c = Containment::new();
        let step = next_step(
            0,
            t + TEARDOWN_DEADLINE * 2,
            t,
            false,
            &c,
            &all_alive,
            &in_all,
        );
        assert!(matches!(step, Step::Failed { .. }));
    }

    #[test]
    fn the_retained_tail_is_bounded_and_keeps_the_end() {
        // A child that ignores SIGTERM and keeps writing is drained at full speed. What is
        // retained is bounded, and it is the end, because the end is what a user reads.
        let mut buf = Vec::new();
        let mut dropped = 0usize;
        for i in 0..100u32 {
            let chunk = vec![(i % 256) as u8; 4096];
            dropped += retain_tail(&mut buf, &chunk);
        }
        assert_eq!(buf.len(), RETAINED_TAIL);
        assert!(dropped > 0, "the discarded remainder must be counted");
        assert_eq!(*buf.last().unwrap(), 99u8);
    }
}
