//! What the full-page screens share (`qoop sound`, `qoop settings`, `qoop onboard`): the
//! terminal held raw on the alternate screen, the keys read off it, the sound playing, and a
//! grid of cells painted in qoop's own inks with the logo and the pet drawn the way the
//! sidebar draws them.

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use qoop_runtime::avatar::{self, Avatar};
use qoop_runtime::client::TerminalGuard;
use qoop_tui::notify;
use qoop_tui::theme::{self, Role};

/// The terminal, raw, on the alternate screen with the cursor hidden, and the keys typed on
/// it. Dropped, the terminal is as it was; a panic restores it before its message prints.
pub struct Session {
    _guard: TerminalGuard,
    pub typed: mpsc::Receiver<Vec<u8>>,
    _reader: Reader,
}

impl Session {
    pub fn start() -> std::io::Result<Session> {
        qoop_tui::colorlevel::apply();
        let guard = TerminalGuard::enter_raw()?;
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            TerminalGuard::restore();
            previous_hook(info);
        }));
        // The guard's restore leaves the alternate screen and shows the cursor again.
        print!("\x1b[?1049h\x1b[?25l");
        // Its background, as the interface asks it, and to be told when it turns light or
        // dark (`CSI ? 2031 h`, which the guard turns off): in a qoop pane the daemon answers
        // with the one the person is looking at, and tells of a change, a first interface
        // after a daemon that started with none included. Both arrive with the keys
        // (`Key::Ground`).
        if qoop_tui::run::may_ask_terminal() {
            print!("\x1b[?2031h\x1b]11;?\x1b\\");
        }
        std::io::stdout().flush()?;
        let (typed, reader) = typing(libc::STDIN_FILENO);
        Ok(Session {
            _guard: guard,
            typed,
            _reader: reader,
        })
    }
}

/// The thread reading the terminal, stopped and waited for when the page ends, so what reads
/// standard input next (the sign-in question after onboarding, `qoop login`'s yes) gets every
/// key instead of a reader still blocked on it.
struct Reader {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The bytes of `fd`, read on a thread of their own so a screen can wait a moment for the
/// rest of an escape sequence without blocking on it. Read from the descriptor itself, not
/// through `std::io::stdin`, whose buffer could keep bytes a wait on the descriptor misses.
fn typing(fd: libc::c_int) -> (mpsc::Receiver<Vec<u8>>, Reader) {
    let (tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    debug_assert!(
        fd < libc::FD_SETSIZE as libc::c_int,
        "fd {fd} does not fit an fd_set"
    );
    let thread = std::thread::spawn(move || {
        let mut buf = [0u8; 64];
        // select, not poll: macOS's poll answers POLLNVAL at once for /dev/tty, which is what
        // install.sh hands `qoop onboard`. fd_set holds descriptors below FD_SETSIZE only.
        // ponytail: wakes every 50 ms to see the stop flag; a self-pipe if that ever shows.
        while fd < libc::FD_SETSIZE as libc::c_int && !stopped.load(Ordering::Relaxed) {
            // SAFETY: an all-zero fd_set is an empty one; it lives on this frame.
            let mut set: libc::fd_set = unsafe { std::mem::zeroed() };
            let mut wait = libc::timeval {
                tv_sec: 0,
                tv_usec: 50_000,
            };
            // SAFETY: `fd` is below FD_SETSIZE (the loop's condition), and `set` and `wait`
            // are valid for the call; the write and except sets are not asked for.
            let got = unsafe {
                libc::FD_SET(fd, &mut set);
                libc::select(
                    fd + 1,
                    &mut set,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut wait,
                )
            };
            if got < 0 && !interrupted() {
                break;
            }
            // SAFETY: `set` is the fd_set select just filled, and `fd` is below FD_SETSIZE.
            if got <= 0 || !unsafe { libc::FD_ISSET(fd, &set) } {
                continue;
            }
            // SAFETY: reads at most `buf.len()` bytes into `buf`, which outlives the call.
            let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 && interrupted() {
                continue;
            }
            if n <= 0 || tx.send(buf[..n as usize].to_vec()).is_err() {
                break;
            }
        }
    });
    let reader = Reader {
        stop,
        thread: Some(thread),
    };
    (rx, reader)
}

/// The last call failed in a way asking again fixes: a signal, or nothing to read yet.
fn interrupted() -> bool {
    matches!(
        std::io::Error::last_os_error().kind(),
        std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Key {
    Left,
    Right,
    Up,
    Down,
    Enter,
    PageUp,
    PageDown,
    Home,
    End,
    Tab,
    BackTab,
    Backspace,
    /// The escape key alone.
    Esc,
    /// Ctrl-C, ctrl-D, or the terminal gone.
    Stop,
    CtrlR,
    Char(char),
    /// The left button, in the cells of the page (from 0): pressed, moved while held
    /// (`held`), or let go. Only a page that asks for the mouse gets these.
    Mouse {
        down: bool,
        held: bool,
        col: u16,
        row: u16,
    },
    /// One notch of the wheel over a cell of the page, up or down. Only a page that asks for
    /// the mouse gets these.
    Wheel {
        up: bool,
        col: u16,
        row: u16,
    },
    /// What the interface says about a press held in this page and let go outside it: over
    /// qass's pane (`true`), or anywhere else.
    Dropped(bool),
    /// The terminal's answer to `OSC 11 ?`, or its report of a scheme change (`CSI ? 997 ;
    /// 2 n`): its background is a light one (`true`) or not.
    /// [`next_keys`] keeps it for the theme and hands it to no page.
    Ground(bool),
}

/// What the terminal said its background is: 0 nothing yet, 1 dark, 2 light.
static GROUND: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Whether the terminal said its background is light, once it said.
pub fn ground_light() -> Option<bool> {
    match GROUND.load(std::sync::atomic::Ordering::Relaxed) {
        0 => None,
        n => Some(n == 2),
    }
}

/// The keys in `bytes`, and how many bytes they used. An escape sequence is read to its
/// final byte; one cut off at the end (a lone `ESC`, or `ESC [` and parameters) is left
/// unused for the caller to wait on. Sequences and bytes that mean nothing here are skipped.
pub fn decode(bytes: &[u8]) -> (Vec<Key>, usize) {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let key = match bytes[i] {
            0x1b => {
                let Some(&next) = bytes.get(i + 1) else {
                    return (out, i);
                };
                if next == b']' {
                    // An OSC, to BEL or ST, read whole: never its letters as keys.
                    let body = &bytes[i + 2..];
                    let end = body.iter().enumerate().find_map(|(at, b)| match b {
                        0x07 => Some((at, 1)),
                        0x1b if body.get(at + 1) == Some(&b'\\') => Some((at, 2)),
                        _ => None,
                    });
                    let Some((at, len)) = end else {
                        return (out, i);
                    };
                    let key = match &body[..at] {
                        b"7701;drop=qass" => Some(Key::Dropped(true)),
                        b"7701;drop=none" => Some(Key::Dropped(false)),
                        [b'1', b'1', b';', rgb @ ..] => qoop_tui::input::parse_rgb(rgb)
                            .map(|rgb| Key::Ground(theme::is_light_background(rgb))),
                        _ => None,
                    };
                    out.extend(key);
                    i += 2 + at + len;
                    continue;
                }
                if !matches!(next, b'[' | b'O') {
                    // Alt and a key: the key alone.
                    i += 1;
                    continue;
                }
                // Parameters, then one final byte in `@` to `~`.
                let Some(end) = bytes[i + 2..]
                    .iter()
                    .position(|b| (0x40..=0x7e).contains(b))
                else {
                    return (out, i);
                };
                let last = bytes[i + 2 + end];
                let params = &bytes[i + 2..i + 2 + end];
                i += 2 + end;
                if let (b'M' | b'm', Some(sgr)) = (last, params.strip_prefix(b"<")) {
                    out.extend(mouse(sgr, last == b'M'));
                    i += 1;
                    continue;
                }
                match last {
                    b'D' => Some(Key::Left),
                    b'C' => Some(Key::Right),
                    b'A' => Some(Key::Up),
                    b'B' => Some(Key::Down),
                    b'Z' => Some(Key::BackTab),
                    b'H' => Some(Key::Home),
                    b'F' => Some(Key::End),
                    // A colour scheme report (`CSI ? 2031 h` asked for them): dark, light.
                    b'n' => match params {
                        b"?997;1" => Some(Key::Ground(false)),
                        b"?997;2" => Some(Key::Ground(true)),
                        _ => None,
                    },
                    b'~' => match params {
                        b"1" | b"7" => Some(Key::Home),
                        b"4" | b"8" => Some(Key::End),
                        b"5" => Some(Key::PageUp),
                        b"6" => Some(Key::PageDown),
                        _ => None,
                    },
                    _ => None,
                }
            }
            b'\r' | b'\n' => Some(Key::Enter),
            b'\t' => Some(Key::Tab),
            0x7f | 0x08 => Some(Key::Backspace),
            0x03 | 0x04 => Some(Key::Stop),
            0x12 => Some(Key::CtrlR),
            b @ 0x20..=0x7e => Some(Key::Char(char::from(b))),
            // A character past ASCII, in its UTF-8 bytes: cut off at the end, it waits too.
            b @ 0xc2..=0xf4 => {
                let len = match b {
                    0xc2..=0xdf => 2,
                    0xe0..=0xef => 3,
                    _ => 4,
                };
                // Only continuation bytes belong to it: at any other (an escape typed next),
                // the lead is dropped alone and that byte read for itself.
                let rest = &bytes[i + 1..bytes.len().min(i + len)];
                if rest.iter().any(|c| !(0x80..=0xbf).contains(c)) {
                    i += 1;
                    continue;
                }
                if rest.len() < len - 1 {
                    return (out, i);
                }
                let raw = &bytes[i..i + len];
                i += len - 1;
                // A C1 control (U+0080 to U+009F) is no character to type.
                std::str::from_utf8(raw)
                    .ok()
                    .and_then(|s| s.chars().next())
                    .filter(|c| !c.is_control())
                    .map(Key::Char)
            }
            _ => None,
        };
        out.extend(key);
        i += 1;
    }
    (out, i)
}

/// An SGR mouse report's `b;x;y`, for the left button and the plain wheel only: the other
/// buttons and a wheel with a modifier are nothing here.
fn mouse(params: &[u8], down: bool) -> Option<Key> {
    let text = std::str::from_utf8(params).ok()?;
    let mut parts = text.split(';').map(|p| p.parse::<u16>().ok());
    let (b, x, y) = (parts.next()??, parts.next()??, parts.next()??);
    let (col, row) = (x.saturating_sub(1), y.saturating_sub(1));
    if matches!(b, 64 | 65) {
        return down.then_some(Key::Wheel {
            up: b == 64,
            col,
            row,
        });
    }
    if b & 0b0100_0011 != 0 {
        return None;
    }
    Some(Key::Mouse {
        down,
        held: b & 32 != 0,
        col,
        row,
    })
}

/// The keys typed next: what arrives, and when it ends in a cut-off escape, what follows
/// within a moment. Nothing following, a lone escape is the escape key. Nothing typed for
/// 300 ms is no keys, so the caller redraws (the window may have been resized).
pub fn next_keys(typed: &mpsc::Receiver<Vec<u8>>) -> Vec<Key> {
    let mut found = typed_keys(typed);
    // The terminal's background, answered: kept, and the theme drawn for it.
    found.retain(|key| match key {
        Key::Ground(light) => {
            GROUND.store(1 + u8::from(*light), std::sync::atomic::Ordering::Relaxed);
            crate::settings::apply_theme();
            false
        }
        _ => true,
    });
    found
}

fn typed_keys(typed: &mpsc::Receiver<Vec<u8>>) -> Vec<Key> {
    let mut bytes = match typed.recv_timeout(Duration::from_millis(300)) {
        Ok(bytes) => bytes,
        Err(mpsc::RecvTimeoutError::Timeout) => return Vec::new(),
        Err(mpsc::RecvTimeoutError::Disconnected) => return vec![Key::Stop],
    };
    loop {
        let (mut found, used) = decode(&bytes);
        if used == bytes.len() {
            return found;
        }
        match typed.recv_timeout(Duration::from_millis(50)) {
            Ok(more) => bytes.extend(more),
            Err(_) => {
                if bytes[used..] == [0x1b] {
                    found.push(Key::Esc);
                }
                return found;
            }
        }
    }
}

/// The sound playing now, stopped when the next one starts so moving fast never stacks
/// them.
pub struct Player(Option<Child>);

impl Player {
    pub fn new() -> Player {
        Player(None)
    }

    pub fn play(&mut self, name: &str, macos: bool) {
        if let Some(mut was) = self.0.take() {
            let _ = was.kill();
            let _ = was.wait();
        }
        let Some(call) = notify::sound(false, macos, name, name) else {
            return;
        };
        self.0 = Command::new(call.program)
            .args(&call.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok();
    }
}

/// How a cell is painted: one escape, set in full (each starts with a reset).
pub fn ink(role: Role) -> String {
    theme::fg(role)
}

/// [`ink`], bold.
pub fn bold(role: Role) -> String {
    format!("{}\x1b[1m", theme::fg(role))
}

/// The raised background the selected row wears, in `role`'s ink.
pub fn raised(role: Role) -> String {
    theme::selected_in(role)
}

/// [`raised`], bold.
pub fn raised_bold(role: Role) -> String {
    format!("{}\x1b[1m", theme::selected_in(role))
}

/// A raw 256 ink, for the pet's own colours.
pub fn raw(ink: u8) -> String {
    format!("\x1b[0m{}", avatar::fg(ink))
}

#[derive(Clone)]
struct Cell {
    ch: char,
    esc: String,
    /// Zero-width marks (a combining accent) that follow `ch` in the same cell.
    extra: String,
}

/// The second cell of a wide character: painted as nothing.
const SPACER: char = '\0';

/// A frame of cells, painted in one write. Writing past an edge is clipped.
pub struct Screen {
    pub w: usize,
    pub h: usize,
    cells: Vec<Vec<Cell>>,
}

impl Screen {
    pub fn new(w: usize, h: usize) -> Screen {
        let blank = Cell {
            ch: ' ',
            esc: theme::RESET.to_string(),
            extra: String::new(),
        };
        Screen {
            w,
            h,
            cells: vec![vec![blank; w]; h],
        }
    }

    /// `text` from column `x` on row `y`, in `esc`; where the next column is. A wide
    /// character takes two columns (its second is a spacer), a zero-width mark joins the cell
    /// before it, and a wide character that does not fit the last column is a space.
    pub fn put(&mut self, x: usize, y: usize, text: &str, esc: &str) -> usize {
        let mut x = x;
        for ch in text.chars().filter(|c| qoop_tui::width::drawn(*c)) {
            let cols = qoop_tui::width::columns(ch);
            if cols == 0 {
                if let Some(cell) = x.checked_sub(1).and_then(|p| self.cell(p, y)) {
                    cell.extra.push(ch);
                }
                continue;
            }
            let wide = cols > 1;
            let ch = if wide && x + 1 >= self.w { ' ' } else { ch };
            self.set(x, y, ch, esc);
            if wide && ch != ' ' {
                self.set(x + 1, y, SPACER, esc);
            }
            x += cols.min(2);
        }
        x
    }

    fn cell(&mut self, x: usize, y: usize) -> Option<&mut Cell> {
        self.cells.get_mut(y)?.get_mut(x)
    }

    /// One cell written; the other half of a wide character it breaks is blanked.
    fn set(&mut self, x: usize, y: usize, ch: char, esc: &str) {
        let Some(old) = self.cell(x, y).map(|c| c.ch) else {
            return;
        };
        if old == SPACER && x > 0 {
            self.set_blank(x - 1, y);
        } else if qoop_tui::width::columns(old) > 1 {
            self.set_blank(x + 1, y);
        }
        if let Some(cell) = self.cell(x, y) {
            *cell = Cell {
                ch,
                esc: esc.to_string(),
                extra: String::new(),
            };
        }
    }

    fn set_blank(&mut self, x: usize, y: usize) {
        if let Some(cell) = self.cell(x, y) {
            cell.ch = ' ';
            cell.extra.clear();
        }
    }

    /// Pieces of text one after another; where the next column is.
    pub fn seg(&mut self, x: usize, y: usize, parts: &[(&str, String)]) -> usize {
        parts
            .iter()
            .fold(x, |x, (text, esc)| self.put(x, y, text, esc))
    }

    /// `text` centred on column `cx`.
    pub fn centre(&mut self, cx: usize, y: usize, text: &str, esc: &str) {
        let n = text.chars().count();
        self.put(cx.saturating_sub(n / 2), y, text, esc);
    }

    /// Pieces centred together on column `cx`.
    pub fn centre_seg(&mut self, cx: usize, y: usize, parts: &[(&str, String)]) {
        let n: usize = parts.iter().map(|(t, _)| t.chars().count()).sum();
        self.seg(cx.saturating_sub(n / 2), y, parts);
    }

    /// The word on the nine-dot grid (docs/brand): q, o, o, p, the q's tail under its right
    /// column and the p's tail under its left one, both in the brand ink. Four rows, 26
    /// columns: four rings of five cells on a pitch of seven.
    pub fn word(&mut self, x: usize, y: usize) {
        let ring = ["xxx", "x.x", "xxx"];
        let cream = ink(Role::Text);
        let brand = ink(Role::Brand);
        for (letter, at) in [('q', 0), ('o', 7), ('o', 14), ('p', 21)] {
            for (dy, row) in ring.iter().enumerate() {
                for (dx, dot) in row.chars().enumerate() {
                    if dot == 'x' {
                        self.put(x + at + dx * 2, y + dy, "●", &cream);
                    }
                }
            }
            if letter == 'q' {
                self.put(x + at + 4, y + 3, "●", &brand);
            }
            if letter == 'p' {
                self.put(x + at, y + 3, "●", &brand);
            }
        }
    }

    /// The pet in half blocks, one cell a pixel wide and two tall: 16 columns, 6 rows.
    pub fn pet(&mut self, x: usize, y: usize, pet: &Avatar) {
        for row in 0..avatar::GRID_H / 2 {
            for col in 0..avatar::GRID_W {
                let top = avatar::pixel(pet, col, row * 2);
                let bottom = avatar::pixel(pet, col, row * 2 + 1);
                let (ch, esc) = match (top, bottom) {
                    (Some(t), Some(b)) => ('▀', format!("{}{}", raw(t), avatar::bg(b))),
                    (Some(t), None) => ('▀', raw(t)),
                    (None, Some(b)) => ('▄', raw(b)),
                    (None, None) => continue,
                };
                self.put(x + col as usize, y + row as usize, &ch.to_string(), &esc);
            }
        }
    }

    /// The pet twice as big: two cells a pixel wide, one row a pixel tall. 32 columns, 12
    /// rows, the first and last two rows of the grid empty for every pet.
    pub fn pet2(&mut self, x: usize, y: usize, pet: &Avatar) {
        for row in 0..avatar::GRID_H {
            for col in 0..avatar::GRID_W {
                if let Some(p) = avatar::pixel(pet, col, row) {
                    let esc = format!("\x1b[0m{}", avatar::bg(p));
                    self.put(x + col as usize * 2, y + row as usize, "  ", &esc);
                }
            }
        }
    }

    /// The bottom row: the brand chip, then each key and what it does.
    pub fn status_bar(&mut self, keys: &[(&str, &str)]) {
        let y = self.h - 1;
        let bar = theme::status();
        self.put(0, y, &" ".repeat(self.w), &bar);
        let mut x = self.put(1, y, " qoop ", &theme::button(true));
        x += 2;
        for (key, what) in keys {
            x = self.put(x, y, key, &format!("{}\x1b[1m", theme::status_key()));
            x = self.put(x, y, &format!(" {what}   "), &bar);
        }
    }

    /// The whole frame as one write: each row placed by position, escapes only where the
    /// paint changes.
    pub fn frame(&self) -> String {
        let mut out = String::new();
        for (y, row) in self.cells.iter().enumerate() {
            out.push_str(&format!("\x1b[{};1H", y + 1));
            let mut last = "";
            for cell in row {
                if cell.esc != last {
                    out.push_str(&cell.esc);
                    last = &cell.esc;
                }
                if cell.ch != SPACER {
                    out.push(cell.ch);
                    out.push_str(&cell.extra);
                }
            }
        }
        out.push_str(theme::RESET);
        out
    }
}

/// The terminal's size, columns then rows.
pub fn size() -> (usize, usize) {
    let (w, h) = qoop_runtime::client::outer_size();
    (usize::from(w), usize::from(h))
}

/// Whether a `w` x `h` terminal is too small for a page that needs `need`.
pub fn too_small(w: usize, h: usize, need: (usize, usize)) -> bool {
    w < need.0 || h < need.1
}

/// `say` on a cleared terminal of `w` x `h`: broken at spaces, never in a word, a line to a
/// row, and no row below the last.
fn notice(say: &str, w: usize, h: usize) -> String {
    let mut text = format!("\x1b[H\x1b[2J{}", ink(Role::Text));
    for (row, line) in crate::qass_page::wrap(say, w).iter().enumerate().take(h) {
        text.push_str(&format!("\x1b[{};1H{line}", row + 1));
    }
    text.push_str(theme::RESET);
    text
}

/// Paint `screen`, or, when the terminal is smaller than `need`, say how big it must be (a
/// pane in a qoop terminal, else the window) and the page's own key that leaves it (`leave`).
pub fn show(screen: &Screen, need: (usize, usize), leave: &str) -> std::io::Result<()> {
    let (w, h) = size();
    let what = if qoop_runtime::legacy::var("QOOP_TERMINAL").is_ok() {
        "this pane"
    } else {
        "the window"
    };
    let say = format!(
        "make {what} at least {} x {} (it is {w} x {h}), {leave} leaves",
        need.0, need.1
    );
    show_said(screen, need, &say)
}

/// [`show`] with its own words for a terminal smaller than `need`.
pub fn show_said(screen: &Screen, need: (usize, usize), say: &str) -> std::io::Result<()> {
    let (w, h) = size();
    let mut out = std::io::stdout();
    if too_small(w, h, need) {
        out.write_all(notice(say, w, h).as_bytes())?;
    } else {
        out.write_all(screen.frame().as_bytes())?;
    }
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same on `/dev/tty`, what install.sh gives `qoop onboard`: a terminal of its own in a
    /// child (this test binary again, running [`dev_tty_child`]), whose page ends at once and
    /// leaves the next line typed to the read that follows. macOS's poll cannot wait on
    /// `/dev/tty`, so a reader built on it stays blocked in `read` and the page never ends.
    #[test]
    fn a_page_on_dev_tty_ends_at_once_and_leaves_the_next_line_to_the_next_read() {
        use std::os::unix::process::CommandExt;
        use std::sync::Mutex;
        use std::time::Instant;

        let (mut master, mut slave) = (0 as libc::c_int, 0 as libc::c_int);
        // SAFETY: openpty fills the two descriptors; name, termios and size are not asked for.
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(opened, 0, "openpty");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "page::tests::dev_tty_child", "--ignored"])
            .args(["--nocapture", "--test-threads=1"])
            .env("QOOP_DEV_TTY_CHILD", "1");
        // SAFETY: only async-signal-safe calls between fork and exec: close, and login_tty
        // (setsid, the terminal made the controlling one, dup2 onto 0, 1 and 2, close).
        unsafe {
            child.pre_exec(move || {
                libc::close(master);
                if libc::login_tty(slave) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        /// The child, killed and reaped however the test ends, a failed wait included.
        struct Reaped(std::process::Child);
        impl Drop for Reaped {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = Reaped(child.spawn().unwrap());
        // SAFETY: the parent's copy of the terminal side, opened above, closed once.
        unsafe { libc::close(slave) };
        let seen = Arc::new(Mutex::new(Vec::new()));
        let filled = seen.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 256];
            loop {
                // SAFETY: reads at most `buf.len()` bytes into `buf`, from the pty's master.
                let n = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
                if n <= 0 {
                    break;
                }
                filled.lock().unwrap().extend_from_slice(&buf[..n as usize]);
            }
        });
        let text = || String::from_utf8_lossy(&seen.lock().unwrap()).into_owned();
        let wait_for = |mark: &str| {
            let until = Instant::now() + Duration::from_secs(20);
            while !text().contains(mark) {
                assert!(Instant::now() < until, "no {mark}: {}", text());
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        let put = |bytes: &[u8]| {
            // SAFETY: writes `bytes`, which outlive the call, to the pty's master.
            let n = unsafe { libc::write(master, bytes.as_ptr().cast(), bytes.len()) };
            assert_eq!(n, bytes.len() as isize);
        };
        wait_for("READY");
        put(b"x\n");
        wait_for("ENDED");
        put(b"y\n");
        let until = Instant::now() + Duration::from_secs(20);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            if Instant::now() > until {
                panic!("the child never ended: {}", text());
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(status.success(), "{}", text());
        assert!(text().contains("GOT y"), "{}", text());
    }

    /// The child of the test above, on the terminal it was given.
    #[test]
    #[ignore = "run by a_page_on_dev_tty_ends_at_once_and_leaves_the_next_line_to_the_next_read"]
    fn dev_tty_child() {
        if std::env::var_os("QOOP_DEV_TTY_CHILD").is_none() {
            return;
        }
        // SAFETY: a NUL-terminated path, opened for reading and writing.
        let fd = unsafe { libc::open(c"/dev/tty".as_ptr(), libc::O_RDWR) };
        assert!(fd >= 0, "{}", std::io::Error::last_os_error());
        let (typed, reader) = typing(fd);
        println!("READY");
        let got = typed.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(got, b"x\n");
        let (done, ended) = mpsc::channel();
        std::thread::spawn(move || {
            drop(reader);
            let _ = done.send(());
        });
        assert!(
            ended.recv_timeout(Duration::from_secs(5)).is_ok(),
            "the reader is still blocked on /dev/tty"
        );
        println!("ENDED");
        let mut buf = [0u8; 8];
        // SAFETY: reads at most `buf.len()` bytes into `buf`, from the terminal opened above.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        assert_eq!(&buf[..n.max(0) as usize], b"y\n");
        println!("GOT y");
    }

    /// A page that ends stops reading: the next reader of the same descriptor (the sign-in
    /// question after onboarding) gets the very next bytes, none taken by a reader left behind.
    #[test]
    fn an_ended_page_leaves_every_later_key_to_the_next_reader() {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: pipe fills the two descriptors of this array.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let [from, to] = fds;
        let put = |bytes: &[u8]| {
            // SAFETY: writes `bytes`, which outlive the call, to the pipe this test owns.
            let n = unsafe { libc::write(to, bytes.as_ptr().cast(), bytes.len()) };
            assert_eq!(n, bytes.len() as isize);
        };
        let (typed, reader) = typing(from);
        put(b"x");
        assert_eq!(typed.recv_timeout(Duration::from_secs(5)).unwrap(), b"x");
        let (done, ended) = mpsc::channel();
        std::thread::spawn(move || {
            drop(reader);
            let _ = done.send(());
        });
        assert!(
            ended.recv_timeout(Duration::from_secs(5)).is_ok(),
            "the reader is still blocked on the descriptor"
        );
        put(b"y\n");
        let mut buf = [0u8; 8];
        // SAFETY: reads at most `buf.len()` bytes into `buf`, from the pipe this test owns.
        let n = unsafe { libc::read(from, buf.as_mut_ptr().cast(), buf.len()) };
        assert_eq!(&buf[..n as usize], b"y\n");
        // SAFETY: closes the two descriptors this test opened, once each.
        unsafe {
            libc::close(from);
            libc::close(to);
        }
    }

    /// The left button's reports are read whole, the wheel and other buttons are nothing, and
    /// the interface's drop arrives as one key, never as its letters; a report or a drop cut
    /// anywhere waits for the rest.
    #[test]
    fn mouse_reports_and_the_drop_are_read_whole() {
        let only = |bytes: &[u8]| decode(bytes).0;
        let at = |down, held, col, row| Key::Mouse {
            down,
            held,
            col,
            row,
        };
        assert_eq!(
            only(b"\x1b[<0;6;5M\x1b[<32;7;5M\x1b[<0;7;5m"),
            [
                at(true, false, 5, 4),
                at(true, true, 6, 4),
                at(false, false, 6, 4)
            ]
        );
        // The plain wheel is a key; a wheel with a modifier and the other buttons are nothing.
        assert_eq!(
            only(b"\x1b[<64;6;5M\x1b[<65;6;5M\x1b[<2;6;5M\x1b[<68;6;5M\x1b[<80;6;5M"),
            [
                Key::Wheel {
                    up: true,
                    col: 5,
                    row: 4
                },
                Key::Wheel {
                    up: false,
                    col: 5,
                    row: 4
                },
            ]
        );
        assert_eq!(only(b"\x1b]7701;drop=qass\x07"), [Key::Dropped(true)]);
        assert_eq!(only(b"\x1b]7701;drop=none\x1b\\"), [Key::Dropped(false)]);
        assert_eq!(only(b"\x1b]0;a title\x07q"), [Key::Char('q')]);
        for whole in [
            &b"\x1b]7701;drop=qass\x07"[..],
            &b"\x1b[<0;6;5M"[..],
            &b"\x1b[<65;6;5M"[..],
        ] {
            for cut in 1..whole.len() {
                assert_eq!(decode(&whole[..cut]), (Vec::new(), 0), "{cut}");
            }
        }
    }

    #[test]
    fn keys_arrows_letters_and_cut_sequences() {
        let only = |bytes: &[u8]| decode(bytes).0;
        assert_eq!(
            only(b"\x1b[C\x1b[D\x1b[A\x1b[B\x1bOC\x1b[Z"),
            [
                Key::Right,
                Key::Left,
                Key::Up,
                Key::Down,
                Key::Right,
                Key::BackTab
            ]
        );
        assert_eq!(
            only(b"ab \r\t\x7f\x03\x12"),
            [
                Key::Char('a'),
                Key::Char('b'),
                Key::Char(' '),
                Key::Enter,
                Key::Tab,
                Key::Backspace,
                Key::Stop,
                Key::CtrlR
            ]
        );
        // A modified arrow reads to its final byte; an unknown sequence is skipped whole.
        assert_eq!(only(b"\x1b[1;5Cx"), [Key::Right, Key::Char('x')]);
        assert_eq!(only(b"\x1b[200~q"), [Key::Char('q')]);
        // Alt and a key is the key.
        assert_eq!(only(b"\x1bx"), [Key::Char('x')]);
        // Cut off at the end: left for the next read.
        assert_eq!(decode(b"\x1b"), (vec![], 0));
        assert_eq!(decode(b"a\x1b[1;"), (vec![Key::Char('a')], 1));
        // Past ASCII, whole or waiting for its rest.
        assert_eq!(
            only("çş€".as_bytes()),
            [Key::Char('ç'), Key::Char('ş'), Key::Char('€')]
        );
        assert_eq!(decode(&"aş".as_bytes()[..2]), (vec![Key::Char('a')], 1));
        // A C1 control is dropped; a lead byte cut short by an escape keeps the escape.
        assert_eq!(only(b"\xc2\x9bx"), [Key::Char('x')]);
        assert_eq!(only(b"\xc3\x1b[A"), [Key::Up]);
        assert_eq!(decode(b"\xe2\x82\x1b"), (vec![], 2));
    }

    #[test]
    fn a_lone_escape_is_esc_and_a_cut_arrow_waits_for_its_rest() {
        let (tx, rx) = mpsc::channel();
        tx.send(b"\x1b".to_vec()).unwrap();
        assert_eq!(next_keys(&rx), [Key::Esc]);
        tx.send(b"l\x1b[".to_vec()).unwrap();
        tx.send(b"D".to_vec()).unwrap();
        assert_eq!(next_keys(&rx), [Key::Char('l'), Key::Left]);
        drop(tx);
        assert_eq!(next_keys(&rx), [Key::Stop]);
    }

    #[test]
    fn a_frame_places_every_row_and_clips_at_the_edge() {
        let mut s = Screen::new(6, 2);
        s.put(4, 0, "abcd", "\x1b[0m");
        s.put(0, 5, "gone", "\x1b[0m");
        let f = s.frame();
        assert!(f.contains("\x1b[1;1H"), "{f:?}");
        assert!(f.contains("\x1b[2;1H"), "{f:?}");
        assert!(f.contains("ab"), "{f:?}");
        assert!(!f.contains("abc"), "{f:?}");
        assert!(!f.contains("gone"), "{f:?}");
    }

    #[test]
    fn the_word_has_every_dot_and_both_tails_under_the_line() {
        let mut s = Screen::new(30, 4);
        s.word(0, 0);
        let dots = s.cells.iter().flatten().filter(|c| c.ch == '●').count();
        // Four rings of 8, and the q's tail and the p's.
        assert_eq!(dots, 34);
        let brand = ink(Role::Brand);
        let embers: Vec<(usize, usize)> = (0..4)
            .flat_map(|y| (0..30).map(move |x| (y, x)))
            .filter(|&(y, x)| s.cells[y][x].ch == '●' && s.cells[y][x].esc == brand)
            .collect();
        // q's under its right column, p's under its left one: nothing else is ember.
        assert_eq!(embers, vec![(3, 4), (3, 21)]);
        // The letters read q o o p: a ring each, empty in the middle.
        for at in [0, 7, 14, 21] {
            assert_eq!(s.cells[1][at + 2].ch, ' ', "ring at {at} is hollow");
            assert_eq!(s.cells[0][at].ch, '●');
            assert_eq!(s.cells[2][at + 4].ch, '●');
        }
    }

    #[test]
    fn a_wide_character_takes_two_cells_and_a_mark_joins_its_letter() {
        let mut s = Screen::new(6, 1);
        assert_eq!(s.put(0, 0, "中a", "E"), 3);
        assert_eq!(
            (s.cells[0][0].ch, s.cells[0][1].ch, s.cells[0][2].ch),
            ('中', SPACER, 'a')
        );
        // Written over its second half, the character is blanked whole.
        s.put(1, 0, "x", "E");
        assert_eq!((s.cells[0][0].ch, s.cells[0][1].ch), (' ', 'x'));
        s.put(0, 0, "中", "E");
        s.put(0, 0, "y", "E");
        assert_eq!((s.cells[0][0].ch, s.cells[0][1].ch), ('y', ' '));
        // One that does not fit the last column is a space, a mark rides the cell before it.
        s.put(5, 0, "中", "E");
        assert_eq!(s.cells[0][5].ch, ' ');
        s.put(3, 0, "u\u{0308}", "E");
        assert_eq!(s.cells[0][3].extra, "\u{0308}");
        assert!(s.frame().contains("u\u{0308}"));
        assert!(!s.frame().contains(SPACER));
    }

    /// The too-small notice keeps to the terminal: no row below its last, at any height.
    #[test]
    fn the_too_small_notice_writes_no_row_below_the_last() {
        let say = "make this pane at least 60 x 14 (it is 12 x 4), q leaves";
        for (w, h) in [(12, 4), (8, 3), (80, 1)] {
            let text = notice(say, w, h);
            assert!(text.contains(&format!("\x1b[{h};1H")), "{w}x{h}: {text:?}");
            assert!(
                !text.contains(&format!("\x1b[{};1H", h + 1)),
                "{w}x{h}: {text:?}"
            );
        }
    }

    #[test]
    fn the_terminal_s_background_is_read_as_light_or_dark_never_as_keys() {
        assert_eq!(
            decode(b"\x1b]11;rgb:fafa/fafa/fafa\x1b\\q"),
            (vec![Key::Ground(true), Key::Char('q')], 26)
        );
        assert_eq!(
            decode(b"\x1b]11;rgb:1e/1e/1e\x07"),
            (vec![Key::Ground(false)], 18)
        );
        // Cut before its end, it waits for the rest.
        assert_eq!(decode(b"\x1b]11;rgb:fa"), (vec![], 0));
        // A scheme change the terminal reports, as `CSI ? 2031 h` asked.
        assert_eq!(
            decode(b"\x1b[?997;2n\x1b[?997;1n\x1b[?997;3n"),
            (vec![Key::Ground(true), Key::Ground(false)], 27)
        );
    }

    /// The answer reaches no page as a key: it is kept, for the theme drawn next. Dark last,
    /// so the other tests go on drawing for the dark ground they expect.
    #[test]
    fn the_background_answered_is_kept_and_typed_by_no_one() {
        let (tx, rx) = mpsc::channel();
        tx.send(b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\".to_vec())
            .unwrap();
        assert_eq!(next_keys(&rx), []);
        assert_eq!(ground_light(), Some(true));
        tx.send(b"\x1b[?997;1n".to_vec()).unwrap();
        assert_eq!(next_keys(&rx), []);
        assert_eq!(ground_light(), Some(false));
    }

    #[test]
    fn paging_keys_are_read_in_both_encodings() {
        let only = |bytes: &[u8]| decode(bytes).0;
        assert_eq!(
            only(b"\x1b[5~\x1b[6~\x1b[H\x1b[F\x1bOH\x1bOF\x1b[1~\x1b[4~\x1b[7~\x1b[8~"),
            [
                Key::PageUp,
                Key::PageDown,
                Key::Home,
                Key::End,
                Key::Home,
                Key::End,
                Key::Home,
                Key::End,
                Key::Home,
                Key::End
            ]
        );
        // Bracketed paste markers stay nothing.
        assert!(only(b"\x1b[200~").is_empty());
    }
}
