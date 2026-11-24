//! How qass stands and its recorded acts in words, shared with the desktop's qass page. The
//! terminal interface shows qass on its own framed screen (`qass_screen`), not here.

/// How qass stands on This Mac.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stand {
    /// Nothing of it was ever set up here.
    None,
    /// Its space is there and its agent sleeps.
    Asleep,
    /// Its space is there and it does not run.
    Stopped,
    /// It runs and the watching switch is off.
    Paused,
    /// It runs and watches `agents` agents on `on` of `of` computers.
    Watching { agents: usize, on: usize, of: usize },
}

impl Stand {
    /// Whether qass runs (it can be paused, asked and opened).
    pub fn runs(&self) -> bool {
        matches!(self, Stand::Paused | Stand::Watching { .. })
    }
}

/// An act as the daemon recorded it ("wrote to T4"), its first letter raised.
pub fn act_words(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |c| {
        c.to_uppercase().chain(chars).collect::<String>()
    })
}
