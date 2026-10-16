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

/// The prefix a decoder starts with when nobody chose another: ctrl-a, as in screen.
pub(crate) const PREFIX: u8 = 0x01;

/// The prefix in effect, for the words on the screen.
///
/// Held apart from the decoder on purpose. The decoder is a value, one per client, and its
/// tests build as many as they like; the text that names the key ("ctrl-a c starts a
/// terminal") is spread across every painter and message in the interface, and threading a
/// byte through all of them to spell one word is a larger change than the word.
static SHOWN_PREFIX: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(PREFIX);

/// Say which prefix the screen should name. Set where the interface starts and again
/// whenever it reads `config.toml` again.
pub fn show_prefix(byte: u8) {
    SHOWN_PREFIX.store(byte, std::sync::atomic::Ordering::Relaxed);
}

/// The prefix byte the screen names.
pub fn shown_prefix() -> u8 {
    SHOWN_PREFIX.load(std::sync::atomic::Ordering::Relaxed)
}

/// The prefix as a person writes it: `ctrl-a`.
pub fn prefix_name() -> String {
    name_of(SHOWN_PREFIX.load(std::sync::atomic::Ordering::Relaxed))
}

/// Text written with `ctrl-a` for the prefix, as the screen should say it.
///
/// For the long fixed lists (every key on the status bar, the keybinds box), which are
/// written once with the default and named on the way out. Every prefix a person can
/// choose is spelled `ctrl-` and one letter, the same width as `ctrl-a`, so nothing
/// measured against the written text moves.
pub fn named(text: &str) -> String {
    text.replace("ctrl-a", &prefix_name())
}

/// A control byte as a person writes it: `0x02` is `ctrl-b`.
pub fn name_of(byte: u8) -> String {
    match byte {
        0x01..=0x1a => format!("ctrl-{}", (byte + b'a' - 1) as char),
        other => format!("0x{other:02x}"),
    }
}

/// ctrl-q, documented as detach since before there was a prefix key, so it stays a command
/// on its own rather than becoming `ctrl-a d` only.
pub(crate) const DETACH: u8 = 0x11;

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
    /// A mouse press, in one based screen coordinates.
    ///
    /// Reported rather than acted on here, because what a click means depends on where the
    /// interface drew things and this decoder does not know that. It knows what the
    /// terminal said.
    Click {
        row: u16,
        col: u16,
        /// Shift was held. The universal way to say "this one is mine, not the
        /// program's": every terminal emulator gives a person their own selection back
        /// this way when the program in front of them has taken the mouse.
        shift: bool,
    },
    /// The pointer moved with the button held. What a selection follows.
    Drag {
        row: u16,
        col: u16,
        shift: bool,
    },
    /// The button came up. What ends a selection, and what copies it.
    Release {
        row: u16,
        col: u16,
        shift: bool,
    },
    /// The wheel turned, in one based screen coordinates.
    ///
    /// Reported rather than acted on, like a press: whether a wheel belongs to the program
    /// in the pane or to the interface depends on what that program asked for, which this
    /// decoder does not know.
    Wheel {
        row: u16,
        col: u16,
        up: bool,
        shift: bool,
    },
    /// ctrl-a l
    Redraw,
    /// ctrl-a s, and ctrl-a S for the other direction.
    ///
    /// A space and not a terminal: the two lists on the left are navigated separately
    /// because they answer separate questions, and one pair of keys for both would make
    /// "next" mean whichever list the interface last thought was interesting.
    NextSpace,
    PreviousSpace,
    /// ctrl-a w, a new space.
    CreateSpace,
    /// ctrl-a W, close the space on screen when it holds nothing.
    ///
    /// Uppercase on purpose, like the rename pair: lowercase `w` makes the space this
    /// one removes. A space that still holds terminals is refused rather than emptied:
    /// closing a tab is one keystroke each, and a bulk close behind one letter would end
    /// sessions somebody is still looking at.
    CloseSpace,
    /// ctrl-a r, rename the tab; ctrl-a R, the space's settings, which rename it.
    ///
    /// A name given at creation is a guess: the terminal a person opened to "look at
    /// something" is the one they are still in an hour later, doing the thing it should
    /// have been called after.
    Rename,
    SpaceSettings,
    /// ctrl-a Q, qass in its own space, or back from it.
    ///
    /// Uppercase, like the other pages and boxes on the capitals; the lowercase letter stays
    /// free.
    Qass,
    /// ctrl-a i, the question inbox beside qass's terminal: open and focus it, or put it away.
    Asks,
    /// ctrl-a T, on qass's tab with the board: fold the pane with the keyboard to its strip,
    /// or open the one folded.
    Fold,
    /// ctrl-a b, fold the left column away and back.
    Collapse,
    /// ctrl-a U, say whether a newer release is waiting and how it gets installed.
    ///
    /// Said, not fetched: the interface never touches the network, it only reads the
    /// notice `qoop update --check` left, and the install itself runs outside, as
    /// `qoop update`.
    Update,
    /// ctrl-a m, the menu.
    Menu,
    /// ctrl-a ?: every key, without going through the menu.
    ///
    /// Only `?`: `h` and `/` stay unbound, so the pane-left `H` keeps its
    /// neighbourhood to itself.
    Keybinds,
    /// ctrl-a 1 through ctrl-a 9, the nth tab of the current space, counted from one.
    ///
    /// Counted from one because that is what is written on the tab. Zero is not a command:
    /// there is no tab zero to go to, and silently sending it somewhere would be worse than
    /// doing nothing.
    Tab(usize),
    /// ctrl-a space: the search palette over spaces, agents, tabs and actions.
    Palette,
    /// ctrl-a a: the keyboard on the agents column: move over its rows, open, sleep, search.
    Agents,
    /// ctrl-a g: the column as the tree of spaces or by urgency.
    ToggleView,
    /// ctrl-a f: fold the focused space's agents under its rule, or open them again.
    FoldSpace,
    /// ctrl-a M: the focused agent's last answer, read from its transcript, as a page.
    Answer,
    /// ctrl-a tab: the agent that needs the person next, the first of the next row's queue.
    UpNext,
    /// ctrl-a F: the focus view over the focused terminal: the user's last
    /// prompt, the agent's last turns, and one row per tool call.
    ///
    /// Uppercase on purpose, like the pane moves.
    Focus,
    /// ctrl-a v: split the focused pane side by side.
    SplitV,
    /// ctrl-a -: split the focused pane stacked.
    SplitH,
    /// ctrl-a H/J/K/L: focus the pane left/down/up/right.
    ///
    /// Uppercase on purpose: lowercase `l` is redraw, and the four directions
    /// stay together rather than splitting one of them onto another letter.
    PaneMove(crate::layout::Move),
    /// ctrl-a o: focus the next pane in tree order.
    PaneNext,
    /// ctrl-a z: zoom the focused pane, or go back.
    Zoom,
    /// ctrl-a Z: put the tab the keyboard is in to sleep, or wake the sleeping tab it
    /// rests on.
    ///
    /// Uppercase on purpose, like the space close: lowercase `z` is the zoom, pressed
    /// often, and a slip of the shift key must not end the processes of a tab.
    Sleep,
    /// ctrl-a X: drop the focused pane from the tree, keeping its terminal
    /// alive as a tab. Lowercase `x` keeps killing the terminal.
    DetachPane,
    /// ctrl-a e: resize mode, where `h j k l` move the nearest divider until `esc`.
    Resize,
    /// ctrl-a =: every pane the same share of its row or column.
    Equalize,
    /// ctrl-a [: copy mode over the focused terminal, as in tmux: move through the whole
    /// history with the keyboard, select, search, copy.
    CopyMode,
    /// ctrl-a t: the focused agent's session as a chat, or the terminal as it is.
    Chat,
}

impl Action {
    /// Whether a person moved on to something: a command, or a click. Typing for the pane and
    /// the rest of a mouse gesture (a drag, a release, the wheel) are not.
    pub fn moves_on(&self) -> bool {
        !matches!(
            self,
            Action::Input(_) | Action::Drag { .. } | Action::Release { .. } | Action::Wheel { .. }
        )
    }
}

/// The decoder. Cheap to hold, and one per attached client.
#[derive(Debug)]
pub struct Keys {
    /// The byte that turns the next one into a command.
    prefix: u8,
    /// The last byte of the previous read was the prefix, so the next byte is a command.
    after_prefix: bool,
    /// The start of a mouse report that the read ended in the middle of.
    pending_mouse: Vec<u8>,
}

/// What the bytes from an `ESC [ <` hold.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Report {
    /// A whole report, its length, and what it means (`None` for what nothing here uses).
    Whole(usize, Option<Action>),
    /// Digits and separators up to the end of the read: the rest is still coming.
    Partial,
    /// Not a report after all. This many bytes are dropped and the byte after them is
    /// ordinary input, so a `ctrl-q` right behind a mangled report still detaches.
    Broken(usize),
}

/// Read what starts at an `ESC [ <`.
///
/// The report ends at the first byte that is not a digit or `;`. Searching for the next
/// `M` or `m` anywhere was the first version, and a report that never got its terminator
/// held everything typed after it, `ctrl-q` included, until the person happened to type
/// an `m`.
pub(crate) fn report(bytes: &[u8]) -> Report {
    // Three numbers of at most five digits and the two separators between them.
    const LONGEST_BODY: usize = 17;
    let body = bytes
        .iter()
        .skip(3)
        .take_while(|b| b.is_ascii_digit() || **b == b';')
        .count();
    let end = 3 + body;
    if body > LONGEST_BODY {
        return Report::Broken(end);
    }
    match bytes.get(end) {
        None => Report::Partial,
        Some(b'M' | b'm') => match mouse(&bytes[..=end]) {
            Some(action) => Report::Whole(end + 1, action),
            None => Report::Broken(end + 1),
        },
        Some(_) => Report::Broken(end),
    }
}

/// The reply to `CSI ? 1006 h`, which is how a terminal reports a mouse press with
/// coordinates that are not limited to 223 columns.
///
/// `\x1b[<button;col;rowM` for a press and `m` for a release, the terminator being the
/// last byte of `bytes`. Only the press is turned into an action: acting on both would
/// switch twice for one click.
pub(crate) fn mouse(bytes: &[u8]) -> Option<Option<Action>> {
    let end = bytes.len() - 1;
    let body = std::str::from_utf8(&bytes[3..end]).ok()?;
    let mut parts = body.split(';');
    let button: u16 = parts.next()?.parse().ok()?;
    let col: u16 = parts.next()?.parse().ok()?;
    let row: u16 = parts.next()?.parse().ok()?;
    let press = bytes[end] == b'M';
    // The button field is a set of flags around a two bit button number: 4 shift, 8 meta,
    // 16 control, 32 the pointer moving with a button held, 64 the wheel.
    let wheel = button & 64 != 0;
    let motion = button & 32 != 0;
    let shift = button & 4 != 0;
    let plain = button & 0b0000_0011;
    let action = if wheel {
        // 64 is up and 65 is down; the sideways wheel (66, 67) is not a scroll of anything
        // this draws and is dropped rather than read as a vertical one.
        match plain {
            0 | 1 => Some(Action::Wheel {
                row,
                col,
                up: plain == 0,
                shift,
            }),
            _ => None,
        }
    } else if plain != 0 {
        // A button other than the left one. Nothing here uses them, and treating one as a
        // left click would move the focus on a middle click paste.
        None
    } else if motion {
        press.then_some(Action::Drag { row, col, shift })
    } else if press {
        Some(Action::Click { row, col, shift })
    } else {
        Some(Action::Release { row, col, shift })
    };
    Some(action)
}

impl Default for Keys {
    fn default() -> Self {
        Keys::with_prefix(PREFIX)
    }
}

impl Keys {
    /// A decoder whose commands follow `prefix` rather than ctrl-a.
    pub fn with_prefix(prefix: u8) -> Self {
        Keys {
            prefix,
            after_prefix: false,
            pending_mouse: Vec::new(),
        }
    }

    /// The byte that starts a command.
    pub fn prefix(&self) -> u8 {
        self.prefix
    }

    /// Whether the prefix came last and the next key is a command.
    pub fn prefix_pending(&self) -> bool {
        self.after_prefix
    }

    /// Forget a `ctrl-a` still waiting for its letter: something else took the keyboard
    /// first, and the next key must not arrive as a command it was never typed as.
    pub fn clear_prefix(&mut self) {
        self.after_prefix = false;
    }

    /// Decode one read. May return several actions, and may keep state for the next read.
    ///
    /// Ordinary bytes are coalesced into as few `Input` actions as the commands between
    /// them allow: a read is usually a paste or a burst of typing, and one write to the
    /// terminal is worth a great deal more than one action per byte. Order is preserved
    /// exactly, which is what makes a character split across reads arrive intact.
    /// Whether a half-read report is being held, waiting for its tail.
    ///
    /// The run loop reads the input before it feeds the decoder, so anything that decides
    /// what a read *is* has to ask: a read that ends half way through a report is the tail
    /// of one the decoder already has the head of, and both halves belong to the mouse.
    pub fn mouse_pending(&self) -> bool {
        !self.pending_mouse.is_empty()
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Action> {
        let mut actions = Vec::new();
        let mut pending = Vec::new();

        // A mouse report is a sequence, so it is read whole where it starts, in the same
        // pass as everything else: taken off first, the bytes before it skipped the prefix
        // decoder, and `ctrl-a n` typed while the trackpad scrolled reached the shell.
        // Carried across reads for the same reason the prefix is: a report can be split,
        // and half of one passed through to a shell would type `[<0;5;3M` at a prompt.
        let mut bytes = bytes;
        let mut carried;
        if !self.pending_mouse.is_empty() {
            carried = std::mem::take(&mut self.pending_mouse);
            carried.extend_from_slice(bytes);
            bytes = &carried[..];
        }

        let mut at = 0;
        while at < bytes.len() {
            if bytes[at..].starts_with(b"\x1b[<") {
                match report(&bytes[at..]) {
                    Report::Whole(used, action) => {
                        flush(&mut pending, &mut actions);
                        actions.extend(action);
                        at += used;
                    }
                    Report::Partial => {
                        self.pending_mouse = bytes[at..].to_vec();
                        break;
                    }
                    Report::Broken(dropped) => at += dropped,
                }
                continue;
            }
            let byte = bytes[at];
            at += 1;
            if self.after_prefix {
                self.after_prefix = false;
                let command = match byte {
                    // Twice over is how the user types the prefix itself for the program
                    // inside, so it is input rather than a command.
                    byte if byte == self.prefix => {
                        pending.push(self.prefix);
                        continue;
                    }
                    b'n' => Action::Next,
                    b'p' => Action::Previous,
                    b'c' => Action::Create,
                    b'x' => Action::Close,
                    b'd' => Action::Detach,
                    b'l' => Action::Redraw,
                    b's' => Action::NextSpace,
                    b'S' => Action::PreviousSpace,
                    b'w' => Action::CreateSpace,
                    b'W' => Action::CloseSpace,
                    b'r' => Action::Rename,
                    b'b' => Action::Collapse,
                    b'U' => Action::Update,
                    b'm' => Action::Menu,
                    b'?' => Action::Keybinds,
                    b'R' => Action::SpaceSettings,
                    b'Q' => Action::Qass,
                    b'i' => Action::Asks,
                    b'T' => Action::Fold,
                    b'1'..=b'9' => Action::Tab((byte - b'0') as usize),
                    // The space bar: the palette is the front door, so it gets the
                    // biggest key.
                    b' ' => Action::Palette,
                    b'a' => Action::Agents,
                    b'g' => Action::ToggleView,
                    b'f' => Action::FoldSpace,
                    b'M' => Action::Answer,
                    b'\t' => Action::UpNext,
                    b'F' => Action::Focus,
                    b'v' => Action::SplitV,
                    b'-' => Action::SplitH,
                    b'H' => Action::PaneMove(crate::layout::Move::Left),
                    b'J' => Action::PaneMove(crate::layout::Move::Down),
                    b'K' => Action::PaneMove(crate::layout::Move::Up),
                    b'L' => Action::PaneMove(crate::layout::Move::Right),
                    b'o' => Action::PaneNext,
                    b'z' => Action::Zoom,
                    b'Z' => Action::Sleep,
                    b'X' => Action::DetachPane,
                    b'e' => Action::Resize,
                    b'=' => Action::Equalize,
                    b'[' => Action::CopyMode,
                    b't' => Action::Chat,
                    // Anything else is a mistyped command and is dropped. Passing it through would send it to a shell, where a
                    // slip of the finger could run something.
                    _ => continue,
                };
                flush(&mut pending, &mut actions);
                actions.push(command);
                continue;
            }

            match byte {
                byte if byte == self.prefix => self.after_prefix = true,
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
    fn a_command_or_a_click_moves_on_and_typing_or_the_wheel_does_not() {
        let mut keys = Keys::default();
        assert!(keys.feed(b"\x01c").iter().all(Action::moves_on));
        assert!(
            Action::Click {
                row: 1,
                col: 5,
                shift: false
            }
            .moves_on()
        );
        assert!(!input(b"ls").moves_on());
        assert!(
            !Action::Wheel {
                row: 1,
                col: 1,
                up: true,
                shift: false
            }
            .moves_on()
        );
    }

    #[test]
    fn another_prefix_takes_over_every_command_and_ctrl_a_becomes_typing() {
        let mut keys = Keys::with_prefix(0x02);
        assert_eq!(keys.feed(b"\x02c"), vec![Action::Create]);
        assert_eq!(keys.feed(b"\x02\x02"), vec![input(b"\x02")]);
        assert_eq!(
            keys.feed(b"\x01c"),
            vec![input(b"\x01c")],
            "ctrl-a is the program's own key now"
        );
    }

    #[test]
    fn a_prefix_is_named_the_way_a_person_writes_it() {
        assert_eq!(name_of(0x01), "ctrl-a");
        assert_eq!(name_of(0x02), "ctrl-b");
        assert_eq!(name_of(0x1a), "ctrl-z");
        assert_eq!(prefix_name(), "ctrl-a", "nobody chose another here");
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

    /// `t` is the chat switch, and `v` stays the split it always was.
    #[test]
    fn ctrl_a_t_is_the_chat_switch_and_v_is_still_the_split() {
        let mut keys = Keys::default();
        assert_eq!(keys.feed(b"\x01t\x01v"), vec![Action::Chat, Action::SplitV]);
        assert_eq!(keys.feed(b"t"), vec![input(b"t")], "a bare t is typing");
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
        assert_eq!(keys.feed(b"\x01q"), vec![]);
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
    fn split_and_pane_commands_decode() {
        use crate::layout::Move::{Down, Left, Right, Up};
        let mut keys = Keys::default();
        assert_eq!(
            keys.feed(b"\x01v\x01-\x01H\x01J\x01K\x01L\x01o\x01z\x01X"),
            vec![
                Action::SplitV,
                Action::SplitH,
                Action::PaneMove(Left),
                Action::PaneMove(Down),
                Action::PaneMove(Up),
                Action::PaneMove(Right),
                Action::PaneNext,
                Action::Zoom,
                Action::DetachPane,
            ]
        );
    }

    #[test]
    fn resize_equalize_and_copy_mode_decode_after_any_prefix() {
        let mut keys = Keys::default();
        assert_eq!(
            keys.feed(b"\x01e\x01=\x01["),
            vec![Action::Resize, Action::Equalize, Action::CopyMode]
        );
        let mut keys = Keys::with_prefix(0x02);
        assert_eq!(keys.feed(b"\x02["), vec![Action::CopyMode]);
        assert_eq!(
            keys.feed(b"e=["),
            vec![input(b"e=[")],
            "no prefix, just typing"
        );
    }

    #[test]
    fn uppercase_w_closes_the_space_while_lowercase_makes_one() {
        let mut keys = Keys::default();
        assert_eq!(keys.feed(b"\x01W"), vec![Action::CloseSpace]);
        assert_eq!(keys.feed(b"\x01w"), vec![Action::CreateSpace]);
    }

    #[test]
    fn uppercase_z_sleeps_the_space_while_lowercase_zooms() {
        let mut keys = Keys::default();
        assert_eq!(keys.feed(b"\x01Z"), vec![Action::Sleep]);
        assert_eq!(keys.feed(b"\x01z"), vec![Action::Zoom]);
    }

    #[test]
    fn uppercase_f_opens_the_focus_view_while_lowercase_folds_the_space() {
        let mut keys = Keys::default();
        assert_eq!(keys.feed(b"\x01F"), vec![Action::Focus]);
        assert_eq!(keys.feed(b"\x01f"), vec![Action::FoldSpace]);
    }

    #[test]
    fn lowercase_l_stays_redraw_while_uppercase_l_moves_focus() {
        use crate::layout::Move::Right;
        let mut keys = Keys::default();
        assert_eq!(keys.feed(b"\x01l"), vec![Action::Redraw]);
        assert_eq!(keys.feed(b"\x01L"), vec![Action::PaneMove(Right)]);
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

    #[test]
    fn the_space_keys_are_separate_from_the_terminal_keys() {
        let mut keys = Keys::default();
        assert_eq!(keys.feed(b"\x01s"), vec![Action::NextSpace]);
        assert_eq!(keys.feed(b"\x01S"), vec![Action::PreviousSpace]);
        assert_eq!(keys.feed(b"\x01w"), vec![Action::CreateSpace]);
        // And the terminal keys still mean terminals.
        assert_eq!(keys.feed(b"\x01n"), vec![Action::Next]);
        assert_eq!(keys.feed(b"\x01c"), vec![Action::Create]);
    }

    #[test]
    fn space_opens_the_palette_and_g_toggles_the_agents_view() {
        let mut keys = Keys::default();
        assert_eq!(keys.feed(b"\x01 "), vec![Action::Palette]);
        assert_eq!(keys.feed(b"\x01\t"), vec![Action::UpNext]);
        assert_eq!(keys.feed(b"\x01g"), vec![Action::ToggleView]);
    }

    #[test]
    fn uppercase_u_names_the_waiting_update_while_lowercase_is_dropped() {
        let mut keys = Keys::default();
        assert_eq!(keys.feed(b"\x01U"), vec![Action::Update]);
        assert_eq!(keys.feed(b"\x01u"), Vec::<Action>::new());
    }

    #[test]
    fn the_rename_keys_are_separate_for_a_tab_and_for_a_space() {
        let mut keys = Keys::default();
        assert_eq!(keys.feed(b"\x01r"), vec![Action::Rename]);
        assert_eq!(keys.feed(b"\x01R"), vec![Action::SpaceSettings]);
        assert_eq!(keys.feed(b"\x01Q"), vec![Action::Qass]);
        assert_eq!(keys.feed(b"\x01i"), vec![Action::Asks]);
    }

    #[test]
    fn question_mark_opens_every_key_without_the_menu() {
        let mut keys = Keys::default();
        assert_eq!(keys.feed(b"\x01?"), vec![Action::Keybinds]);
    }

    #[test]
    fn a_digit_picks_the_tab_that_carries_that_number() {
        let mut keys = Keys::default();
        for n in 1..=9usize {
            let typed = [0x01, b'0' + n as u8];
            assert_eq!(keys.feed(&typed), vec![Action::Tab(n)], "ctrl-a {n}");
        }
        // Zero is not a tab, so it does nothing rather than something arbitrary, and it
        // must not reach the shell either.
        assert_eq!(keys.feed(b"\x010"), vec![]);
    }

    /// Every shape of mouse report this interface asks the terminal for.
    ///
    /// Written out as the bytes a terminal actually sends, because the decoder's job is to
    /// read those and a test that built its input from the decoder's own idea of them
    /// would agree with any mistake in it.
    #[test]
    fn the_mouse_decoder_tells_a_press_a_drag_a_release_and_a_wheel_apart() {
        let mut keys = Keys::default();
        // Left button down at column 10, row 5.
        assert_eq!(
            keys.feed(b"\x1b[<0;10;5M"),
            vec![Action::Click {
                row: 5,
                col: 10,
                shift: false
            }]
        );
        // The same button, moving: 32 is the motion flag.
        assert_eq!(
            keys.feed(b"\x1b[<32;12;7M"),
            vec![Action::Drag {
                row: 7,
                col: 12,
                shift: false
            }]
        );
        // Up again, which a terminal marks with a lowercase terminator.
        assert_eq!(
            keys.feed(b"\x1b[<0;12;7m"),
            vec![Action::Release {
                row: 7,
                col: 12,
                shift: false
            }]
        );
        // The wheel: 64 up, 65 down.
        assert_eq!(
            keys.feed(b"\x1b[<64;3;3M"),
            vec![Action::Wheel {
                row: 3,
                col: 3,
                up: true,
                shift: false
            }]
        );
        assert_eq!(
            keys.feed(b"\x1b[<65;3;3M"),
            vec![Action::Wheel {
                row: 3,
                col: 3,
                up: false,
                shift: false
            }]
        );
        // Shift is bit 4, and it is what says a drag belongs to the person rather than to
        // the program that took the mouse.
        assert_eq!(
            keys.feed(b"\x1b[<36;12;7M"),
            vec![Action::Drag {
                row: 7,
                col: 12,
                shift: true
            }]
        );
        // A button this interface has nothing for is dropped rather than read as the left
        // one: a middle click paste would otherwise move the focus.
        assert_eq!(keys.feed(b"\x1b[<1;4;4M"), vec![]);
        assert_eq!(keys.feed(b"\x1b[<2;4;4M"), vec![]);
        // And the sideways wheel scrolls nothing here, rather than scrolling vertically.
        assert_eq!(keys.feed(b"\x1b[<66;4;4M"), vec![]);
    }

    /// A mouse report split across two reads is still one report, and never typed.
    #[test]
    fn half_a_drag_report_is_carried_rather_than_passed_through_as_text() {
        let mut keys = Keys::default();
        assert_eq!(keys.feed(b"\x1b[<32;12"), vec![]);
        assert_eq!(
            keys.feed(b";7M"),
            vec![Action::Drag {
                row: 7,
                col: 12,
                shift: false
            }]
        );
    }

    /// A report that never gets its terminator is dropped where it stops being one. Held
    /// until some later `m`, it swallowed everything typed after it, the detach included.
    #[test]
    fn a_mangled_report_does_not_swallow_what_comes_after_it() {
        let mut keys = Keys::default();
        assert_eq!(keys.feed(b"\x1b[<0;5ls\r"), vec![input(b"ls\r")]);
        assert_eq!(keys.feed(&[DETACH]), vec![Action::Detach]);
        // Longer than any real report, with no end in sight: not held either.
        let mut long = b"\x1b[<".to_vec();
        long.extend(std::iter::repeat_n(b'1', 40));
        assert!(keys.feed(&long).is_empty());
        assert_eq!(keys.feed(b"x"), vec![input(b"x")]);
    }

    /// The bytes in front of a report are keys like any others: the prefix still works.
    #[test]
    fn a_command_in_front_of_a_mouse_report_is_still_a_command() {
        let mut keys = Keys::default();
        assert_eq!(
            keys.feed(b"\x01n\x1b[<64;3;4M"),
            vec![
                Action::Next,
                Action::Wheel {
                    row: 4,
                    col: 3,
                    up: true,
                    shift: false
                }
            ]
        );
    }
}
