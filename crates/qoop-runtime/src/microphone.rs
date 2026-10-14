//! A microphone for programs in a tab, lent by the interface a person types into.
//!
//! Claude Code's voice records with `arecord` when the machine has no sound card, which is
//! every server. On such a machine `arecord` in a tab is qoop (`qoop_cli`'s shim): it asks
//! here to `Listen`, the daemon asks the interfaces to lend a microphone (`Listen` events,
//! repeated while it lasts), and the interface on the person's machine sends what its
//! microphone hears as `Audio`, which reaches the shim as `Heard`.
//!
//! One recording at a time, and nobody takes over a live one. Audio is taken from the
//! first connection that sends it, so two windows on the same tab never mix. Whether to
//! lend the microphone at all is the interface's call: it does so only right after a key
//! the person pressed in that tab.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};

use qoop_domain::ClientId;

/// How often a live recording says so. An interface that hears nothing for a few of these
/// stops its microphone, so a lost "off" never leaves it on.
pub const BEAT: Duration = Duration::from_secs(1);

/// Chunks of audio held for a slow reader before newer ones are dropped: a few seconds,
/// and never a wait on the connection that carries every keystroke.
const HELD: usize = 64;

/// `Heard` data back to samples.
pub fn decode(data: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(data).ok()
}

/// A change in who listens: the terminal, the recording's number, and whether it is on.
pub type Change = (u32, u64, bool);

struct Live {
    serial: u64,
    terminal: u32,
    audio: mpsc::Sender<Vec<u8>>,
    /// The connection whose audio is taken, once one has sent some.
    from: Option<ClientId>,
}

pub struct Hub {
    live: Mutex<Option<Live>>,
    next: AtomicU64,
    changes: broadcast::Sender<Change>,
}

impl Default for Hub {
    fn default() -> Self {
        Self {
            live: Mutex::new(None),
            next: AtomicU64::new(0),
            changes: broadcast::channel(16).0,
        }
    }
}

/// A recording this hub holds; letting it go says so to the interfaces.
pub struct Recording {
    hub: Arc<Hub>,
    pub serial: u64,
    pub terminal: u32,
    pub audio: mpsc::Receiver<Vec<u8>>,
}

impl Hub {
    pub fn subscribe(&self) -> broadcast::Receiver<Change> {
        self.changes.subscribe()
    }

    /// Start recording for `terminal`, or `None` while another recording is live.
    pub fn claim(self: &Arc<Self>, terminal: u32) -> Option<Recording> {
        let mut live = self.live.lock().unwrap();
        if live.is_some() {
            return None;
        }
        let serial = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let (audio, rx) = mpsc::channel(HELD);
        *live = Some(Live {
            serial,
            terminal,
            audio,
            from: None,
        });
        Some(Recording {
            hub: self.clone(),
            serial,
            terminal,
            audio: rx,
        })
    }

    /// Audio `client` sent for `terminal`: passed on when it is for the live recording and
    /// from the first connection that sent any; dropped otherwise, and when the reader is
    /// behind.
    pub fn hear(&self, client: &ClientId, terminal: u32, bytes: Vec<u8>) {
        let mut live = self.live.lock().unwrap();
        let Some(live) = live.as_mut().filter(|l| l.terminal == terminal) else {
            return;
        };
        if live.from.get_or_insert_with(|| client.clone()) != client {
            return;
        }
        let _ = live.audio.try_send(bytes);
    }

    /// Say the recording is still on: sent at once and then every `BEAT`.
    pub fn beat(&self, recording: &Recording) {
        let _ = self
            .changes
            .send((recording.terminal, recording.serial, true));
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        let mut live = self.hub.live.lock().unwrap();
        if live.as_ref().is_some_and(|l| l.serial == self.serial) {
            *live = None;
        }
        drop(live);
        let _ = self.hub.changes.send((self.terminal, self.serial, false));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(name: &str) -> ClientId {
        ClientId::new(name)
    }

    #[tokio::test]
    async fn one_recording_at_a_time_and_its_end_is_said() {
        let hub = Arc::new(Hub::default());
        let mut changes = hub.subscribe();
        let recording = hub.claim(4).expect("free");
        assert!(hub.claim(5).is_none(), "a live recording was taken over");
        hub.beat(&recording);
        assert_eq!(changes.recv().await.unwrap(), (4, recording.serial, true));
        let serial = recording.serial;
        drop(recording);
        assert_eq!(changes.recv().await.unwrap(), (4, serial, false));
        assert!(hub.claim(5).is_some(), "the slot stayed taken");
    }

    #[tokio::test]
    async fn audio_comes_from_the_first_sender_for_the_live_terminal_only() {
        let hub = Arc::new(Hub::default());
        let mut recording = hub.claim(4).expect("free");
        hub.hear(&client("a"), 4, vec![1]);
        hub.hear(&client("b"), 4, vec![2]);
        hub.hear(&client("a"), 9, vec![3]);
        hub.hear(&client("a"), 4, vec![4]);
        assert_eq!(recording.audio.recv().await, Some(vec![1]));
        assert_eq!(recording.audio.recv().await, Some(vec![4]));
        assert!(recording.audio.try_recv().is_err());
    }

    /// A reader that is behind loses the newest audio; the sender never waits.
    #[test]
    fn a_full_reader_drops_rather_than_waits() {
        let hub = Arc::new(Hub::default());
        let mut recording = hub.claim(4).expect("free");
        for n in 0..HELD + 10 {
            hub.hear(&client("a"), 4, vec![n as u8]);
        }
        let mut got = 0;
        while recording.audio.try_recv().is_ok() {
            got += 1;
        }
        assert_eq!(got, HELD);
    }
}
