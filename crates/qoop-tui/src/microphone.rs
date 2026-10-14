//! This machine's microphone, lent to a program in a tab that has none (a remote tab's
//! Claude Code voice, through qoop's `arecord` there). See `qoop_runtime::microphone`.
//!
//! The daemon says a recording is on (`Listen`), every second while it lasts. Whether to
//! lend the microphone is decided here, once per recording, and only yes right after a key
//! the person pressed in that tab: a program cannot press keys in this window, so nothing
//! running there can start listening on its own. A key starts at most one recording, and a
//! recording stops when it is said to, when it stops being said, when the person looks
//! elsewhere, or at `LONGEST`.
//!
//! The recording itself is SoX's `rec`, the same tool Claude Code falls back on.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// How recent the key that starts a recording must be.
const KEY: Duration = Duration::from_secs(1);
/// How long a recording lasts without being said to be on.
const QUIET: Duration = Duration::from_secs(3);
/// The longest a recording runs.
const LONGEST: Duration = Duration::from_secs(120);

pub const NO_SOX: &str = "voice in remote tabs needs SoX here: brew install sox";

/// Said when the recorder ends by itself: most often a microphone the terminal app may not
/// use.
pub const STOPPED: &str =
    "the microphone stopped: check that this terminal app may use it (Privacy, Microphone)";

/// Shown while the microphone is lent, so a recording is never unseen.
pub const ON: &str = "microphone on: this tab is listening";

/// What a `Listen` event asks of this window.
#[derive(Debug, PartialEq)]
pub enum Step {
    Start,
    Stop,
    Nothing,
}

#[derive(Debug, Default)]
pub struct Microphone {
    /// The recording last decided on, by terminal: its later beats decide nothing.
    decided: BTreeMap<u32, u64>,
    /// The key last sent to a terminal: where, when, and whether a recording took it.
    key: Option<(u32, Instant, bool)>,
    /// The recording this window lends to: terminal, recording, since, last beat.
    live: Option<(u32, u64, Instant, Instant)>,
}

impl Microphone {
    /// A key went to `terminal`.
    pub fn key(&mut self, terminal: u32, now: Instant) {
        self.key = Some((terminal, now, false));
    }

    /// `Listen` for `terminal` said recording `listener` is `on`, while `focused` has the
    /// keyboard; `agent` says an agent runs there, the one kind of program lent one.
    #[allow(clippy::too_many_arguments)]
    pub fn heard(
        &mut self,
        terminal: u32,
        listener: u64,
        on: bool,
        focused: u32,
        agent: bool,
        now: Instant,
    ) -> Step {
        let ours = self
            .live
            .is_some_and(|(t, l, ..)| t == terminal && l == listener);
        if !on {
            // Only its own end: a new recording's start may arrive before the old one's end.
            if self.decided.get(&terminal) == Some(&listener) {
                self.decided.remove(&terminal);
            }
            return if ours { Step::Stop } else { Step::Nothing };
        }
        if ours {
            if let Some(live) = self.live.as_mut() {
                live.3 = now;
            }
            return Step::Nothing;
        }
        if self.decided.insert(terminal, listener) == Some(listener) {
            return Step::Nothing;
        }
        let fresh = match self.key.as_mut() {
            Some((t, at, spent)) if *t == terminal && !*spent && now.duration_since(*at) <= KEY => {
                *spent = true;
                true
            }
            _ => false,
        };
        if !fresh || !agent || focused != terminal || self.live.is_some() {
            return Step::Nothing;
        }
        self.live = Some((terminal, listener, now, now));
        Step::Start
    }

    /// The terminal a recording is lent to now.
    pub fn lending(&self) -> Option<u32> {
        self.live.map(|(t, ..)| t)
    }

    /// Whether the recording must stop now.
    pub fn due(&self, focused: u32, now: Instant) -> bool {
        self.live.is_some_and(|(t, _, since, beat)| {
            t != focused || now.duration_since(beat) > QUIET || now.duration_since(since) > LONGEST
        })
    }

    /// The recording stopped.
    pub fn stopped(&mut self) {
        self.live = None;
    }
}

/// Start `rec` number `n` for `terminal`: its samples arrive on `tx` in the format the
/// daemon passes on (16 kHz mono signed 16-bit little endian), and nothing when it ends.
/// Dropped, the child is killed.
pub fn record(
    n: u64,
    terminal: u32,
    tx: tokio::sync::mpsc::UnboundedSender<(u64, u32, Vec<u8>)>,
) -> std::io::Result<tokio::process::Child> {
    use tokio::io::AsyncReadExt as _;
    let mut child = tokio::process::Command::new("rec")
        .args(["-q", "--buffer", "1024", "-t", "raw", "-r", "16000"])
        .args(["-e", "signed", "-b", "16", "-c", "1", "-L", "-"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut out = child.stdout.take().expect("piped");
    tokio::spawn(async move {
        let mut buf = vec![0u8; 3200];
        // Nothing read is the recorder's end, said once so the window can say why.
        loop {
            let got = out.read(&mut buf).await.unwrap_or(0);
            if tx.send((n, terminal, buf[..got].to_vec())).is_err() || got == 0 {
                return;
            }
        }
    });
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64, start: Instant) -> Instant {
        start + Duration::from_millis(ms)
    }

    #[test]
    fn a_recording_starts_only_right_after_a_key_in_that_tab() {
        let t0 = Instant::now();
        let mut m = Microphone::default();
        // No key at all: a program cannot start listening on its own.
        assert_eq!(m.heard(4, 1, true, 4, true, t0), Step::Nothing);
        // A key, then its recording.
        m.key(4, at(10, t0));
        assert_eq!(m.heard(4, 2, true, 4, true, at(200, t0)), Step::Start);
        assert_eq!(m.lending(), Some(4));
    }

    #[test]
    fn a_declined_recording_stays_declined_whatever_is_typed_later() {
        let t0 = Instant::now();
        let mut m = Microphone::default();
        assert_eq!(m.heard(4, 1, true, 4, true, t0), Step::Nothing);
        m.key(4, at(500, t0));
        assert_eq!(
            m.heard(4, 1, true, 4, true, at(1000, t0)),
            Step::Nothing,
            "its beat"
        );
        // A new recording right after the key is the person's.
        assert_eq!(m.heard(4, 2, true, 4, true, at(1100, t0)), Step::Start);
    }

    #[test]
    fn a_key_starts_one_recording_and_only_in_its_own_tab() {
        let t0 = Instant::now();
        let mut m = Microphone::default();
        m.key(4, t0);
        assert_eq!(
            m.heard(5, 1, true, 5, true, t0),
            Step::Nothing,
            "another tab"
        );
        assert_eq!(
            m.heard(4, 2, true, 6, true, t0),
            Step::Nothing,
            "not focused"
        );
        m.key(4, t0);
        assert_eq!(m.heard(4, 3, true, 4, true, t0), Step::Start);
        assert_eq!(m.heard(4, 3, false, 4, true, t0), Step::Stop);
        m.stopped();
        assert_eq!(
            m.heard(4, 4, true, 4, true, t0),
            Step::Nothing,
            "the key was spent"
        );
        m.key(4, t0);
        assert_eq!(
            m.heard(4, 5, true, 4, true, at(1500, t0)),
            Step::Nothing,
            "a stale key"
        );
    }

    /// A shell's program is never lent one, however fresh the key.
    #[test]
    fn only_an_agent_tab_is_lent_the_microphone() {
        let t0 = Instant::now();
        let mut m = Microphone::default();
        m.key(4, t0);
        assert_eq!(m.heard(4, 1, true, 4, false, t0), Step::Nothing);
    }

    /// An old recording's end arriving after the next one's start leaves the new decision.
    #[test]
    fn a_late_end_does_not_undo_the_next_decision() {
        let t0 = Instant::now();
        let mut m = Microphone::default();
        assert_eq!(m.heard(4, 2, true, 4, true, t0), Step::Nothing, "declined");
        assert_eq!(m.heard(4, 1, false, 4, true, t0), Step::Nothing);
        m.key(4, t0);
        assert_eq!(
            m.heard(4, 2, true, 4, true, t0),
            Step::Nothing,
            "decided again"
        );
    }

    #[test]
    fn a_recording_stops_on_silence_on_a_look_elsewhere_and_at_its_longest() {
        let t0 = Instant::now();
        let mut m = Microphone::default();
        m.key(4, t0);
        assert_eq!(m.heard(4, 1, true, 4, true, t0), Step::Start);
        assert!(!m.due(4, at(2000, t0)));
        assert!(m.due(5, at(10, t0)), "the person looked elsewhere");
        assert!(m.due(4, at(3500, t0)), "no beat");
        let mut now = t0;
        while now < at(119_000, t0) {
            now += Duration::from_secs(1);
            m.heard(4, 1, true, 4, true, now);
            assert!(!m.due(4, now));
        }
        assert!(m.due(4, at(121_000, t0)), "past the longest");
    }
}
