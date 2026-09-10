//! Deciding, byte by byte, what the user meant.
//!
//! A read from a terminal is not a keystroke, it is however many bytes happened to have
//! arrived. It can end anywhere: between the two bytes of a command, or in the middle of a
//! multi byte character. So the decoder is a struct rather than a function, and the only
//! state it needs is whether the previous read ended on the prefix.
//!
//! The prefix is ctrl-a, as in screen and tmux, and the byte after it selects a command.
//! Holding the prefix back matters for correctness, not tidiness: forwarding it and then
//! reconsidering would put a stray ctrl-a into whatever the user is running, which in a
//! shell moves the cursor to the start of the line.

/// The prefix that turns the next byte into a command.
const PREFIX: u8 = 0x01;

/// ctrl-q, documented as detach since before there was a prefix key, so it stays a command
/// on its own rather than becoming `ctrl-a d` only.
const DETACH: u8 = 0x11;

/// What the interface should do with what was typed.
///
/// `Input` carries owned bytes because the run it describes can be stitched from more than
/// one place in the read, so there is no single slice of the input to borrow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Send these bytes to the focused terminal.
    Input(Vec<u8>),
    /// ctrl-a n
    Next,
    /// ctrl-a p
    Previous,
    /// ctrl-a c
    Create,
    /// ctrl-a x
    Close,
    /// ctrl-a d, and also a bare ctrl-q.
    Detach,
    /// ctrl-a l
    Redraw,
}

/// The decoder. Cheap to hold, and one per attached client.
#[derive(Debug, Default)]
pub struct Keys {
    /// The last byte of the previous read was the prefix, so the next byte is a command.
    after_prefix: bool,
}

impl Keys {
    /// Decode one read. May return several actions, and may keep state for the next read.
    ///
    /// Ordinary bytes are coalesced into as few `Input` actions as the commands between
    /// them allow: a read is usually a paste or a burst of typing, and one write to the
    /// terminal is worth a great deal more than one action per byte. Order is preserved
    /// exactly, which is what makes a character split across reads arrive intact.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Action> {
        let mut actions = Vec::new();
        let mut pending = Vec::new();

        for &byte in bytes {
            if self.after_prefix {
                self.after_prefix = false;
                let command = match byte {
                    // Twice over is how the user types the prefix itself for the program
                    // inside, so it is input rather than a command.
                    PREFIX => {
                        pending.push(PREFIX);
                        continue;
                    }
                    b'n' => Action::Next,
                    b'p' => Action::Previous,
                    b'c' => Action::Create,
                    b'x' => Action::Close,
                    b'd' => Action::Detach,
                    b'l' => Action::Redraw,
                    // Anything else, an uppercase letter included, is a mistyped command
                    // and is dropped. Passing it through would send it to a shell, where a
                    // slip of the finger could run something.
                    _ => continue,
                };
                flush(&mut pending, &mut actions);
                actions.push(command);
                continue;
            }

            match byte {
                PREFIX => self.after_prefix = true,
                DETACH => {
                    flush(&mut pending, &mut actions);
                    actions.push(Action::Detach);
                }
                _ => pending.push(byte),
            }
        }

        flush(&mut pending, &mut actions);
        actions
    }
}

/// Emit the run of ordinary bytes collected so far, if there is one, so that it lands
/// before the action that interrupted it.
fn flush(pending: &mut Vec<u8>, actions: &mut Vec<Action>) {
    if !pending.is_empty() {
        actions.push(Action::Input(std::mem::take(pending)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(bytes: &[u8]) -> Action {
        Action::Input(bytes.to_vec())
    }

    #[test]
    fn ordinary_bytes_arrive_as_one_action_per_read() {
        let mut keys = Keys::default();
        assert_eq!(keys.feed(b"hello"), vec![input(b"hello")]);
    }

    #[test]
    fn a_command_splits_the_run_around_it_in_order() {
        let mut keys = Keys::default();
        assert_eq!(
            keys.feed(b"ab\x01n cd"),
            vec![input(b"ab"), Action::Next, input(b" cd")]
        );
    }

    #[test]
    fn a_prefix_at_the_end_of_a_read_waits_for_the_next_one() {
        let mut keys = Keys::default();
        assert_eq!(
            keys.feed(b"ab\x01"),
            vec![input(b"ab")],
            "the prefix leaked"
        );
        assert_eq!(keys.feed(b"ncd"), vec![Action::Next, input(b"cd")]);
    }

    #[test]
    fn every_command_letter_is_recognised() {
        let mut keys = Keys::default();
        assert_eq!(
            keys.feed(b"\x01n\x01p\x01c\x01x\x01d\x01l"),
            vec![
                Action::Next,
                Action::Previous,
                Action::Create,
                Action::Close,
                Action::Detach,
                Action::Redraw,
            ]
        );
    }

    #[test]
    fn the_prefix_twice_sends_one_literal_prefix_byte() {
        let mut keys = Keys::default();
        assert_eq!(keys.feed(b"\x01\x01"), vec![input(b"\x01")]);
    }

    #[test]
    fn the_prefix_twice_across_reads_still_sends_one_literal_byte() {
        let mut keys = Keys::default();
        assert_eq!(keys.feed(b"\x01"), vec![]);
        assert_eq!(keys.feed(b"\x01"), vec![input(b"\x01")]);
    }

    #[test]
    fn an_unknown_command_byte_is_discarded_and_never_reaches_the_terminal() {
        let mut keys = Keys::default();
        assert_eq!(keys.feed(b"\x01z"), vec![]);
        assert_eq!(keys.feed(b"ok"), vec![input(b"ok")], "the decoder is stuck");
    }

    /// The mistyped command that matters most: a newline after the prefix would otherwise
    /// submit whatever is on the shell's line.
    #[test]
    fn a_mistyped_command_does_not_leave_a_stray_byte_in_the_stream() {
        let mut keys = Keys::default();
        assert_eq!(keys.feed(b"rm -rf /\x01\n"), vec![input(b"rm -rf /")]);
    }

    #[test]
    fn an_uppercase_command_letter_is_not_a_command() {
        let mut keys = Keys::default();
        assert_eq!(keys.feed(b"\x01N"), vec![], "uppercase acted as a command");
        assert_eq!(keys.feed(b"\x01n"), vec![Action::Next]);
    }

    #[test]
    fn a_bare_ctrl_q_detaches_wherever_it_appears() {
        let mut keys = Keys::default();
        assert_eq!(keys.feed(b"\x11"), vec![Action::Detach]);
        assert_eq!(
            keys.feed(b"ab\x11cd"),
            vec![input(b"ab"), Action::Detach, input(b"cd")]
        );
    }

    /// Byte for byte, in order, however the reads are cut. The decoder does not know what a
    /// character is, and that is exactly why this holds.
    #[test]
    fn a_multi_byte_character_split_across_reads_arrives_intact() {
        let original = "günaydın, çalışıyor mu?".as_bytes();
        let mut keys = Keys::default();
        let mut received = Vec::new();
        for &byte in original {
            for action in keys.feed(&[byte]) {
                match action {
                    Action::Input(bytes) => received.extend_from_slice(&bytes),
                    other => panic!("plain text produced {other:?}"),
                }
            }
        }
        assert_eq!(received, original);
    }

    #[test]
    fn an_empty_read_produces_nothing_and_keeps_a_pending_prefix() {
        let mut keys = Keys::default();
        assert_eq!(keys.feed(b"\x01"), vec![]);
        assert_eq!(keys.feed(b""), vec![]);
        assert_eq!(keys.feed(b"n"), vec![Action::Next]);
    }
}
