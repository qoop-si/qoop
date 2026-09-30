//! Milestone 12: the gates for the terminal interface.
//!
//! Driven the way a person drives it, which for a full screen program means a real pty:
//! the binary is spawned with its own terminal, keystrokes are written to the master and
//! what it drew is read back from it. Asserting on the bytes it painted rather than on its
//! own idea of what it painted is the point. An interface that believes it drew the index
//! and did not is exactly the failure this is for.
//!
//! The pty comes from `qoro-pty`, which is this workspace's own. That is a circular
//! dependency only for a test about `qoro-pty` itself; here it is a test about what the
//! interface paints, and the pty is the same one every other program on the machine gets.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A running interface, with the terminal it is drawing to.
struct Interface {
    child: Child,
    /// Started here rather than by the interface, so that its stderr is a file this test
    /// can read when something goes wrong. An interface that dies because the daemon said
    /// something is a failure whose explanation is on the daemon's stderr, and
    /// `ensure_daemon` gives that to `/dev/null`.
    daemon: Child,
    master: OwnedFd,
    /// The runtime directory. Owned here when this interface started the
    /// session, borrowed when it attached to another interface's: the owner
    /// deletes it on drop, so an attacher must be dropped first.
    _dir: Option<tempfile::TempDir>,
    runtime: std::path::PathBuf,
    cols: u16,
    rows: u16,
    /// Everything it has painted so far. The screen is a stream here, not a grid: a test
    /// asks whether something was ever drawn, and the last frame alone would lose an
    /// answer that was correct a moment ago.
    ///
    /// The bytes as they came, each chunk followed by the whole text of every row that
    /// chunk wrote to. The interface writes only what changed, often the middle of a row,
    /// so a word that was already on a row is not in the bytes again; the row's text is.
    painted: String,
    /// What a terminal shows after those bytes, which is what the row readers read.
    screen: Screen,
}

/// A terminal as far as this interface drives one: placement, colour and text.
///
/// The interface writes only the rows, and the parts of rows, that changed. Reading the
/// newest full write of a row out of the byte stream reads a row as it was frames ago, so
/// the rows are read the way a terminal would show them instead.
struct Screen {
    /// Each cell's character and the colour sequences in effect when it was drawn. The
    /// second column of a wide character holds `\0`.
    cells: Vec<Vec<(char, String)>>,
    row: usize,
    col: usize,
    style: String,
    /// The start of a sequence or a character a read cut in half, for the next read.
    pending: Vec<u8>,
}

impl Screen {
    fn new(cols: u16, rows: u16) -> Self {
        Self {
            cells: vec![vec![(' ', String::new()); cols as usize]; rows as usize],
            row: 0,
            col: 0,
            style: String::new(),
            pending: Vec::new(),
        }
    }

    /// Apply bytes, and say which rows they wrote to.
    fn feed(&mut self, bytes: &[u8]) -> Vec<usize> {
        self.pending.extend_from_slice(bytes);
        let data = std::mem::take(&mut self.pending);
        let mut touched = Vec::new();
        let mut i = 0;
        while i < data.len() {
            if data[i] == 0x1b {
                match data.get(i + 1) {
                    None => break,
                    Some(b'[') => {
                        let Some(len) =
                            data[i + 2..].iter().position(|b| (0x40..=0x7e).contains(b))
                        else {
                            break;
                        };
                        let end = i + 2 + len;
                        let params = String::from_utf8_lossy(&data[i + 2..end]).into_owned();
                        match data[end] {
                            b'H' => {
                                let mut n = params
                                    .split(';')
                                    .map(|p| p.parse::<usize>().unwrap_or(1).max(1));
                                self.row = n.next().unwrap_or(1) - 1;
                                self.col = n.next().unwrap_or(1) - 1;
                            }
                            b'm' => {
                                if params.is_empty() || params == "0" || params.starts_with("0;") {
                                    self.style.clear();
                                }
                                if !(params.is_empty() || params == "0") {
                                    self.style
                                        .push_str(&String::from_utf8_lossy(&data[i..=end]));
                                }
                            }
                            _ => {}
                        }
                        i = end + 1;
                    }
                    Some(b']') => {
                        // An operating system command runs to a bell or a string terminator.
                        let Some(len) = data[i + 2..]
                            .windows(2)
                            .position(|w| w[0] == 0x07 || w == b"\x1b\\")
                        else {
                            break;
                        };
                        let at = i + 2 + len;
                        i = at + if data[at] == 0x07 { 1 } else { 2 };
                    }
                    Some(_) => i += 2,
                }
                continue;
            }
            let Some(c) = std::str::from_utf8(&data[i..(i + 4).min(data.len())])
                .or_else(|e| std::str::from_utf8(&data[i..i + e.valid_up_to()]))
                .ok()
                .and_then(|s| s.chars().next())
            else {
                if data.len() - i < 4 {
                    break;
                }
                i += 1;
                continue;
            };
            i += c.len_utf8();
            if c.is_control() {
                continue;
            }
            let w = qoro_tui::width::columns(c);
            if let Some(line) = self.cells.get_mut(self.row)
                && self.col < line.len()
            {
                line[self.col] = (c, self.style.clone());
                if w == 2
                    && let Some(next) = line.get_mut(self.col + 1)
                {
                    *next = ('\0', self.style.clone());
                }
                if !touched.contains(&self.row) {
                    touched.push(self.row);
                }
            }
            self.col += w;
        }
        self.pending = data[i..].to_vec();
        touched
    }

    /// One screen row's text, one based, the way a person reads it.
    fn text(&self, row: usize) -> Option<String> {
        let line = self.cells.get(row.checked_sub(1)?)?;
        Some(
            line.iter()
                .map(|(c, _)| *c)
                .filter(|c| *c != '\0')
                .collect(),
        )
    }

    /// The cells of one row, one based.
    fn cells(&self, row: usize) -> Option<&[(char, String)]> {
        self.cells.get(row.checked_sub(1)?).map(Vec::as_slice)
    }
}

impl std::fmt::Display for Screen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for row in 1..=self.cells.len() {
            writeln!(f, "{}", self.text(row).unwrap_or_default().trim_end())?;
        }
        Ok(())
    }
}

impl Drop for Interface {
    fn drop(&mut self) {
        // This one's daemon, by its own handle. Never `pkill qoro daemon`: the first
        // version did exactly that, and every interface test that finished killed every
        // other test's daemon on the machine. Three of five of these gates failed with a
        // broken pipe as soon as the suite ran in parallel, and the daemon they were
        // talking to had been sent a SIGTERM by a test that was already over.
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

const COLS: u16 = 100;
const ROWS: u16 = 30;

/// The screen row the column's title is drawn on: under the one next row, which is there
/// whether or not anybody waits. Taken from the interface rather than written as `2`, so this
/// test cannot disagree with the layout the next time the layout changes. The tab bar is over
/// the pane, not over this column, so the column starts at the very top.
const SPACES_TITLE_ROW: usize = qoro_tui::screen::SIDEBAR_TOP + 1;

/// The screen row the first space's rule is drawn on, directly under that title. The next
/// space's rule follows it directly while the first space holds no agent.
const FIRST_SPACE_ROW: usize = SPACES_TITLE_ROW + 1;

/// Every row of the list under the column's title, from its first row down to the bottom
/// of the column: the space rules and the agents under them.
///
/// Read off the screen rather than worked out from the layout: a row number computed in a
/// test would be a second copy of that arithmetic, and a wrong one the first time either
/// changed.
fn agents_panel(screen: &Screen) -> Vec<String> {
    let Some(first) = agents_title_row(screen).map(|row| row + 1) else {
        return Vec::new();
    };
    // The column stops where the status rows start. Reading into them picked up the notice,
    // which names the focused terminal, so a search for an agent's name found it there and
    // said the panel had it.
    (first..=(ROWS as usize).saturating_sub(qoro_tui::screen::STATUS))
        .map(|row| {
            screen
                .text(row)
                .map(|line| unbox(&line.chars().take(index_width()).collect::<String>()))
                .unwrap_or_default()
        })
        .collect()
}

/// Whether `line` is a space's rule: one blank, then the name straight after it, where an
/// agent's row has its braille mark. Two other rows also open with a blank and something
/// that is neither: the foot's thick logo, whose rows open with a dot (`●`), and its menu
/// row, which opens with the version and ends in the menu's mark. Neither is a rule. There
/// is no fold arrow: `ctrl-a f` folds, and so does a click on the blank before the name.
fn is_rule_line(line: &str) -> bool {
    line.strip_prefix(' ')
        .and_then(|rest| rest.chars().next())
        .is_some_and(|c| c != ' ' && c != '\u{25cf}' && !('\u{2800}'..='\u{28ff}').contains(&c))
        && !line.trim_end().ends_with('\u{ab}')
}

/// A row inside a space's box with the box's sides blanked, column for column, so what it
/// holds reads at the columns it is drawn at: ` │ ● claude  │` is `   ● claude   `. The
/// edges of the box, and every other row, as they are.
fn unbox(line: &str) -> String {
    match line.strip_prefix(" \u{2502} ") {
        Some(inside) => {
            let inside = match inside.trim_end().strip_suffix('\u{2502}') {
                Some(inner) => {
                    let pad = inside.chars().count() - inner.chars().count();
                    format!("{inner}{}", " ".repeat(pad))
                }
                None => inside.to_string(),
            };
            format!("   {inside}")
        }
        None => line.to_string(),
    }
}

/// The screen row the list under the title starts on: [`agents_panel`]'s first row.
fn first_agent_row_of(screen: &Screen) -> usize {
    agents_title_row(screen)
        .map(|row| row + 1)
        .unwrap_or_else(|| panic!("the column title is not on screen:\n{screen}"))
}

/// Where the column's ` agents` title is, if a whole frame of it has arrived.
///
/// An answer rather than a panic, because the caller is usually waiting rather than
/// asserting: what has been read so far can be half a frame, with the title in the part
/// that has not arrived, and a helper that panicked there failed one run in three for a
/// reason that had nothing to do with what was being tested.
fn agents_title_row(screen: &Screen) -> Option<usize> {
    (SPACES_TITLE_ROW..=ROWS as usize).find(|&row| {
        screen.text(row).is_some_and(|line| {
            line.chars()
                .take(index_width())
                .collect::<String>()
                .trim_end()
                .starts_with(" agents")
        })
    })
}

/// The interface's own idea of how wide the left column is, so this test cannot drift from
/// it.
fn index_width() -> usize {
    qoro_tui::sidebar::WIDTH
}

impl Interface {
    fn start() -> Self {
        Self::start_impl(false)
    }

    /// A daemon launched the way production launches one: backgrounded, with
    /// SIGINT and SIGQUIT ignored. G12.16 needs that condition to exist, and
    /// building it here rather than assuming it is what makes the gate about
    /// the daemon instead of about the machine running the test.
    fn start_backgrounded() -> Self {
        Self::start_impl(true)
    }

    fn start_impl(backgrounded: bool) -> Self {
        Self::start_impl_sized(backgrounded, COLS, ROWS)
    }

    fn start_impl_sized(backgrounded: bool, cols: u16, rows: u16) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::set_permissions(
            dir.path(),
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .expect("chmod");
        let runtime = dir.path().to_path_buf();
        let (child, daemon, master) = Self::start_in(dir.path(), backgrounded, cols, rows);
        Self {
            child,
            daemon,
            master,
            _dir: Some(dir),
            runtime,
            cols,
            rows,
            painted: String::new(),
            screen: Screen::new(cols, rows),
        }
    }

    /// Kill only the client, leaving the daemon and its terminals: the
    /// next attach finds a live session with an unowned size.
    fn disconnect_client(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// A second screen with its own window size on another interface's
    /// session: no new daemon (a second bind fails and the spare exits on
    /// its own), just another client, for testing what a fresh attach
    /// adopts and sizes. Drop this before the interface that owns the
    /// directory.
    fn attach_sized(dir: &std::path::Path, cols: u16, rows: u16) -> Self {
        let (child, daemon, master) = Self::start_in(dir, false, cols, rows);
        Self {
            child,
            daemon,
            master,
            _dir: None,
            runtime: dir.to_path_buf(),
            cols,
            rows,
            painted: String::new(),
            screen: Screen::new(cols, rows),
        }
    }

    fn start_in(
        dir: &std::path::Path,
        backgrounded: bool,
        cols: u16,
        rows: u16,
    ) -> (Child, Child, OwnedFd) {
        let mut daemon_cmd = Command::new(env!("CARGO_BIN_EXE_qoro"));
        daemon_cmd
            .arg("daemon")
            .env("QORO_RUNTIME_DIR", dir)
            .env("XDG_CONFIG_HOME", dir.join("config"))
            // A test run from inside a qoro terminal inherits that terminal's session, which
            // moves the socket to a path this harness never looks at.
            .env_remove("QORO_SESSION")
            .env("SHELL", "/bin/sh")
            .stdout(Stdio::null())
            .stderr(Stdio::from(
                std::fs::File::create(dir.join("daemon.log")).expect("log"),
            ));
        if backgrounded {
            // SAFETY: `pre_exec` runs post fork with async-signal-safe calls and no
            // allocation, and `signal` only sets a disposition. A shell backgrounds
            // a daemon this way with `&`.
            unsafe {
                use std::os::unix::process::CommandExt;
                daemon_cmd.pre_exec(|| {
                    libc::signal(libc::SIGINT, libc::SIG_IGN);
                    libc::signal(libc::SIGQUIT, libc::SIG_IGN);
                    Ok(())
                });
            }
        }
        let daemon = daemon_cmd.spawn().expect("spawn the daemon");
        let socket = qoro_testkit::socket_in(dir);
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && !socket.exists() {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(socket.exists(), "the daemon never bound its socket");

        let pty = qoro_pty::sys::open_pty(cols, rows).expect("a pty");
        let slave = pty.slave;
        let stdin = slave.try_clone().expect("dup");
        let stdout = slave.try_clone().expect("dup");
        let stderr = slave.try_clone().expect("dup");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_qoro"));
        cmd.env("QORO_RUNTIME_DIR", dir)
            .env_remove("QORO_SESSION")
            .env_remove("QORO_TERMINAL")
            .env_remove("QORO_TOKEN")
            // No remote from the machine running the tests: a real one would put ssh
            // connections under every gate here.
            .env("QORO_REMOTES", dir.join("remotes.json"))
            .env("XDG_CONFIG_HOME", dir.join("config"))
            // Pinned for the same reason the scale gates pin it: an interactive shell
            // with a configured prompt is somebody's machine, not this program.
            .env("SHELL", "/bin/sh")
            .env("TERM", "xterm-256color");
        // SAFETY: the only unsafe part is `pre_exec` below, whose closure runs post fork
        // and calls two async signal safe syscalls with no allocation.
        let child = unsafe {
            use std::os::unix::process::CommandExt;
            cmd.stdin(Stdio::from(stdin))
                .stdout(Stdio::from(stdout))
                .stderr(Stdio::from(stderr))
                // SAFETY: post fork, async signal safe, no allocation. The interface needs
                // its own session with this pty as its controlling terminal, or the ioctl
                // that asks for the window size answers about somebody else's.
                .pre_exec(|| {
                    if libc::setsid() < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                })
                .spawn()
                .expect("spawn the interface")
        };
        drop(slave);
        // Non blocking once, here, so that a read from a quiet interface returns instead of
        // hanging the test. Done per read in the first version, which was two syscalls per
        // poll for a flag that never changes.
        //
        // SAFETY: `fcntl` on a descriptor this function owns, with flags it accepts.
        unsafe {
            let fd = pty.master.as_raw_fd();
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        (child, daemon, pty.master)
    }

    fn type_in(&mut self, bytes: &[u8]) {
        // SAFETY: the descriptor is owned by `self.master` and outlives this borrow, and
        // `ManuallyDrop` is what stops the `File` from closing a descriptor it does not own.
        let mut file = unsafe {
            use std::os::fd::FromRawFd;
            std::mem::ManuallyDrop::new(std::fs::File::from_raw_fd(self.master.as_raw_fd()))
        };
        // Retried, because the master is non blocking for the sake of reads and a write to
        // a full pty buffer therefore answers `WouldBlock` rather than waiting. Dropping
        // that byte silently is how a keystroke goes missing under load and the test then
        // reports the feature as broken: measured, three of five of these gates failed that
        // way when the suite ran in parallel, with the daemon reporting nothing wrong
        // because nothing was.
        //
        // EIO is different and is not retried: it means the child is gone, and the
        // assertion that follows explains that better than a panic here would.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut written = 0;
        while written < bytes.len() && Instant::now() < deadline {
            match file.write(&bytes[written..]) {
                Ok(0) => break,
                Ok(n) => written += n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(_) => break,
            }
        }
        let _ = file.flush();
    }

    /// Read until `needle` has been painted, or give up.
    ///
    /// Returns how long it took, because two of these gates are about that number rather
    /// than about the text.
    fn wait_for(&mut self, needle: &str, within: Duration) -> Option<Duration> {
        let start = Instant::now();
        // SAFETY: as in `type_in`, a borrowed descriptor that this wrapper must not close.
        let mut file = unsafe {
            use std::os::fd::FromRawFd;
            std::mem::ManuallyDrop::new(std::fs::File::from_raw_fd(self.master.as_raw_fd()))
        };
        let mut buf = [0u8; 64 * 1024];
        while start.elapsed() < within {
            if self.painted.contains(needle) {
                return Some(start.elapsed());
            }
            match file.read(&mut buf) {
                Ok(0) => return None,
                Ok(n) => {
                    self.painted.push_str(&String::from_utf8_lossy(&buf[..n]));
                    for row in self.screen.feed(&buf[..n]) {
                        self.painted.push('\n');
                        self.painted
                            .push_str(&self.screen.text(row + 1).unwrap_or_default());
                    }
                }
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        self.painted.contains(needle).then(|| start.elapsed())
    }

    /// What the daemon said about itself, for a failure the screen cannot explain.
    fn runtime_dir(&self) -> std::path::PathBuf {
        self.runtime.clone()
    }

    fn daemon_log(&mut self) -> String {
        let alive = match self.daemon.try_wait() {
            Ok(None) => "alive".to_string(),
            Ok(Some(status)) => format!("gone with {status}"),
            Err(e) => format!("unknown: {e}"),
        };
        let said = std::fs::read_to_string(self.runtime.join("daemon.log")).unwrap_or_default();
        let interface = match self.child.try_wait() {
            Ok(None) => "alive".to_string(),
            Ok(Some(status)) => format!("gone with {status}"),
            Err(e) => format!("unknown: {e}"),
        };
        format!("daemon {alive}, interface {interface}, log: {said}")
    }

    fn saw(&self, needle: &str) -> bool {
        self.painted.contains(needle)
    }

    /// The index column of one screen row, as it was painted most recently.
    ///
    /// Read out of the byte stream rather than asserted as a whole line, because the
    /// program in a terminal is whatever the operating system calls it: `/bin/sh` is bash
    /// on this machine and dash on another, and a test that hardcodes one of them is a
    /// test about the machine.
    fn panel_line(&self, row: usize) -> Option<String> {
        Some(self.screen.text(row)?.chars().take(index_width()).collect())
    }

    /// One whole screen row, as it is on screen now.
    fn row(&self, row: usize) -> Option<String> {
        Some(self.screen.text(row)?.chars().take(COLS as usize).collect())
    }

    /// The tab bar, as painted most recently.
    fn tab_bar(&self) -> Option<String> {
        // The bar starts where the pane starts, past the column and its rule, and stops
        // where the pane stops.
        let (pane_cols, _) = qoro_tui::screen::pane_size(self.cols, self.rows);
        Some(
            self.screen
                .text(qoro_tui::screen::TABS)?
                .chars()
                .skip(qoro_tui::screen::left(self.cols))
                .take(pane_cols as usize)
                .collect(),
        )
    }

    /// Which tab owns the keyboard, counted along the bar: the one painted
    /// with the focused ink.
    ///
    /// A place on the bar and not a terminal number: the bar is numbered the way it is
    /// pressed, and terminal numbers are handed out across every space.
    ///
    /// Read off the bar rather than asked of the interface: what is being tested is what a
    /// person can see, and an interface that believes it moved the focus and did not paint
    /// it is exactly the failure this is for.
    fn focused_tab(&self) -> Option<u32> {
        let (pane_cols, _) = qoro_tui::screen::pane_size(self.cols, self.rows);
        let left = qoro_tui::screen::left(self.cols);
        let bar = self.screen.cells(qoro_tui::screen::TABS)?;
        let bar = &bar[left.min(bar.len())..(left + pane_cols as usize).min(bar.len())];
        let on = qoro_tui::theme::tab_on();
        let at = bar.iter().position(|(_, style)| style.contains(&on))?;
        bar[at..]
            .iter()
            .map(|(c, _)| *c)
            .skip_while(|c| *c == ' ')
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .ok()
    }

    /// Read until that terminal's tab is the focused one.
    /// Read until some tab owns the keyboard.
    fn wait_for_focused(&mut self, within: Duration) -> bool {
        self.wait_for_tabs(|_| false, Duration::ZERO);
        let start = Instant::now();
        while start.elapsed() < within {
            if self.focused_tab().is_some() {
                return true;
            }
            let _ = self.wait_for("this needle never appears", Duration::from_millis(120));
        }
        false
    }

    /// Read until that tab, counted along the bar, is the focused one.
    fn wait_for_tab(&mut self, want: u32, within: Duration) -> bool {
        self.wait_for_tabs(|_| false, Duration::ZERO);
        let start = Instant::now();
        while start.elapsed() < within {
            if self.focused_tab() == Some(want) {
                return true;
            }
            let _ = self.wait_for("this needle never appears", Duration::from_millis(120));
        }
        false
    }

    /// The one based screen column of the middle of a terminal's tab, read off the bar.
    ///
    /// Read rather than computed: the labels are as wide as the program names, so a column
    /// worked out here would be a copy of the bar's own layout arithmetic and would drift
    /// from it the first time a label changed.
    fn tab_column(&self, terminal: u32) -> Option<u16> {
        let bar = self.tab_bar()?;
        let at = bar.find(&format!(" {terminal} "))?;
        // The bar is read from its own left edge and a click is sent in screen columns, so
        // the column the pane starts at has to be added back.
        Some(at as u16 + 2 + qoro_tui::screen::left(self.cols) as u16)
    }

    /// One row of the left column exactly as it was written, escapes and all.
    ///
    /// The colour is the thing being asserted, so it cannot be stripped, and it has to be
    /// this row's colour: searching the whole painted stream for a yellow also finds a
    /// yellow the child process printed into the pane, which is a gate that passes when the
    /// wrong row is coloured or when nothing in the interface is.
    ///
    /// The whole row up to the reset the painter writes after its text, interior
    /// escapes included: the state dot and the space accent both repaint
    /// mid-row, and cutting at the first escape would return a prefix of the
    /// row rather than the row.
    fn raw_panel_line(&self, row: usize) -> Option<String> {
        let mut line = String::new();
        let mut ink = None;
        for (c, style) in self.screen.cells(row)?.iter().take(index_width()) {
            if ink != Some(style) {
                line.push_str(if style.is_empty() { "\x1b[0m" } else { style });
                ink = Some(style);
            }
            if *c != '\0' {
                line.push(*c);
            }
        }
        line.push_str("\x1b[0m");
        Some(line)
    }

    /// Read until some row of the agents panel says what is wanted.
    ///
    /// Across the whole panel and re-read on every poll, because where it starts and which
    /// row an agent lands on both move as agents arrive.
    fn wait_for_agents(&mut self, want: impl Fn(&str) -> bool, within: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < within {
            if agents_panel(&self.screen).iter().any(|row| want(row)) {
                return true;
            }
            let _ = self.wait_for("this needle never appears", Duration::from_millis(120));
        }
        false
    }

    /// Read until some row of the agents panel is painted as wanted, escapes and all.
    ///
    /// What a state gate uses: the state is the dot's ink at the start of the
    /// row, and the stripped text `wait_for_agents` reads never contains a
    /// state word. Same polling shape, so a row arriving mid-frame reads the
    /// same either way.
    fn wait_for_agents_raw(&mut self, want: impl Fn(&str) -> bool, within: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < within {
            let Some(first) = agents_title_row(&self.screen).map(|row| row + 1) else {
                let _ = self.wait_for("this needle never appears", Duration::from_millis(120));
                continue;
            };
            let last = (ROWS as usize).saturating_sub(qoro_tui::screen::STATUS);
            if (first..=last).any(|row| self.raw_panel_line(row).is_some_and(|l| want(&l))) {
                return true;
            }
            let _ = self.wait_for("this needle never appears", Duration::from_millis(120));
        }
        false
    }

    /// Read until the nth space, counted down the spaces panel from one, is the focused one.
    ///
    /// The background behind a row of its box is the whole marker: no glyph says where
    /// the keyboard is, so this reads the raw bytes (escapes intact) rather
    /// than the stripped text `panel_line` returns.
    fn wait_for_space(&mut self, nth: usize, within: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < within {
            if self
                .space_row(nth)
                .is_some_and(|row| self.space_selected(row))
            {
                return true;
            }
            let _ = self.wait_for("this needle never appears", Duration::from_millis(120));
        }
        false
    }

    /// The screen row the nth space's rule is drawn on, counted down the tree from one.
    ///
    /// Found by its shape rather than by arithmetic: a rule opens with a blank and the
    /// space's name, and agents move the rules down as they come and go.
    fn space_row(&self, nth: usize) -> Option<usize> {
        let first = agents_title_row(&self.screen)? + 1;
        let last = (ROWS as usize).saturating_sub(qoro_tui::screen::STATUS);
        (first..=last)
            .filter(|&row| self.panel_line(row).is_some_and(|line| is_rule_line(&line)))
            .nth(nth - 1)
    }

    /// Whether one index row carries the keyboard, read out of the raw bytes.
    ///
    /// The selected background (`48;5;236`) is the marker, and it is the first
    /// thing painted after the row's cursor move, so the first bytes decide.
    /// The stripped text cannot answer this: stripping takes the colour out.
    fn panel_selected(&self, row: usize) -> bool {
        self.screen
            .cells(row)
            .and_then(|cells| cells.first())
            .is_some_and(|(_, style)| style.contains("48;5;236"))
    }

    /// Whether the space whose rule is drawn on `rule` holds the keyboard: a row inside its
    /// box carries the raised background, or, when the keyboard is on a tab with no agent
    /// and so no row of its own, the rule itself does.
    fn space_selected(&self, rule: usize) -> bool {
        self.panel_selected(rule)
            || (rule + 1..ROWS as usize)
                .take_while(|&row| {
                    self.panel_line(row)
                        .is_some_and(|line| !is_rule_line(&line))
                })
                .any(|row| self.panel_selected(row))
    }

    /// Read until the space whose rule is drawn on `rule` carries the keyboard.
    fn wait_for_space_at(&mut self, rule: usize, within: Duration) -> Option<Duration> {
        let start = Instant::now();
        while start.elapsed() < within {
            if self.space_selected(rule) {
                return Some(start.elapsed());
            }
            let _ = self.wait_for("this needle never appears", Duration::from_millis(120));
        }
        None
    }

    /// Read until the tab bar says what is wanted.
    fn wait_for_tabs(&mut self, want: impl Fn(&str) -> bool, within: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < within {
            if self.tab_bar().is_some_and(|bar| want(&bar)) {
                return true;
            }
            let _ = self.wait_for("this needle never appears", Duration::from_millis(120));
        }
        false
    }

    /// The terminal's own column of one screen row, as painted most recently.
    fn pane_line(&self, row: usize) -> Option<String> {
        Some(
            self.screen
                .text(row)?
                .chars()
                .skip(index_width() + 1)
                .take(30)
                .collect(),
        )
    }

    /// Read until one index row says what is wanted.
    ///
    /// A repaint is not one event: the shell's prompt comes back before the daemon has
    /// noticed the process is gone, so the first frame after an interrupt still shows the
    /// command. Waiting for the row itself is what makes this a question about the index
    /// rather than about which frame happened to arrive first.
    fn wait_for_panel(
        &mut self,
        row: usize,
        want: impl Fn(&str) -> bool,
        within: Duration,
    ) -> Option<Duration> {
        let start = Instant::now();
        while start.elapsed() < within {
            if self.panel_line(row).is_some_and(|line| want(&line)) {
                return Some(start.elapsed());
            }
            let _ = self.wait_for("this needle never appears", Duration::from_millis(120));
        }
        None
    }

    /// A press and its release at a one based screen position, as a terminal reports them.
    fn click(&mut self, col: u16, row: u16) {
        self.type_in(format!("\x1b[<0;{col};{row}M").as_bytes());
        self.type_in(format!("\x1b[<0;{col};{row}m").as_bytes());
    }

    /// A press, a motion through each column, and the release: a drag.
    /// Buttons as a terminal reports them: plain left press, motion with
    /// the button held, plain release.
    fn drag(&mut self, col: u16, row: u16, through: &[u16]) {
        self.type_in(format!("\x1b[<0;{col};{row}M").as_bytes());
        for &at in through {
            self.type_in(format!("\x1b[<32;{at};{row}M").as_bytes());
        }
        let last = through.last().copied().unwrap_or(col);
        self.type_in(format!("\x1b[<0;{last};{row}m").as_bytes());
    }

    /// Make a terminal the way a person does: ask for one, then answer the naming box.
    ///
    /// An empty answer is a real one, and it means the default name.
    fn create_terminal(&mut self, name: &str) {
        let mark = self.painted.len();
        self.command(b'c');
        self.answer_prompt(mark, name);
    }

    /// Make a space the same way.
    fn create_space(&mut self, name: &str) {
        let mark = self.painted.len();
        self.command(b'w');
        self.answer_prompt(mark, name);
    }

    /// Type a name into the box that opened after `mark`, and confirm it.
    ///
    /// After `mark`, not anywhere in the stream. The box is painted into the stream like
    /// everything else, so a box from an earlier press is still in it: waiting on that
    /// answered "was a box ever open" and typed the name into a shell.
    ///
    /// Or on the screen as it stands. A box opened in the same read that confirmed the one
    /// before it takes that box's rows byte for byte, and a row that did not change is
    /// never written again, so nothing after `mark` shows it. Keys reach the interface in
    /// order, so the box on screen, or the one this press opens after it, is where the
    /// name lands.
    fn answer_prompt(&mut self, mark: usize, name: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let open = |ui: &Self| {
            ui.painted[mark..].contains("name the new")
                || ui.screen.to_string().contains("name the new")
        };
        while Instant::now() < deadline && !open(self) {
            let _ = self.wait_for("this needle never appears", Duration::from_millis(80));
        }
        assert!(
            open(self),
            "the naming box never opened:\n{}",
            &self.painted[mark..]
        );
        if !name.is_empty() {
            self.type_in(name.as_bytes());
        }
        std::thread::sleep(Duration::from_millis(50));
        self.type_in(b"\r");
    }

    /// The prefix key, then a command key. Two writes, because that is how a keyboard
    /// sends them and the decoder is resumable for exactly this reason.
    fn command(&mut self, key: u8) {
        self.type_in(&[0x01]);
        std::thread::sleep(Duration::from_millis(50));
        self.type_in(&[key]);
    }
}

/// G12.1: the interface opens, and the spaces, the agents, the tabs, the terminal, the
/// notice and the keys are all on screen.
#[test]
fn g12_1_the_interface_paints_spaces_agents_tabs_a_terminal_and_a_status_line() {
    let mut ui = Interface::start();
    assert!(
        ui.wait_for("detach", Duration::from_secs(20)).is_some(),
        "the key list never appeared:\n{}",
        ui.painted
    );
    // Two rows at the bottom, not one. The keys are a reference and never change; the row
    // above them is the news. One row carrying both meant the keys vanished exactly when
    // something went wrong, which is when a person most needs to be told which key to
    // press.
    let keys = ui
        .row(ROWS as usize)
        .unwrap_or_else(|| panic!("no key row:\n{}", ui.painted));
    assert!(keys.contains("ctrl-a:"), "{keys:?}");
    let notice = ui
        .row(ROWS as usize - 1)
        .unwrap_or_else(|| panic!("no notice row:\n{}", ui.painted));
    assert!(
        !notice.trim().is_empty() && !notice.contains("ctrl-a:"),
        "the notice row is empty or is the key list again: {notice:?}"
    );
    // Both panels of the left column, each with its own title, and the daemon's own space
    // named under the first of them. Waited for rather than checked: the first frame is
    // painted before the answers to `ListAgents` and `ListWorkspaces` arrive, and asserting
    // on whatever happened to be buffered is how a test reports a race as a failure.
    assert!(
        ui.wait_for_panel(
            FIRST_SPACE_ROW,
            |line| line.contains("default"),
            Duration::from_secs(15)
        )
        .is_some(),
        "the spaces panel does not name the daemon's own space: {:?}\n{}",
        ui.panel_line(FIRST_SPACE_ROW),
        ui.painted
    );
    let title = ui
        .panel_line(SPACES_TITLE_ROW)
        .map(|l| l.trim_end().to_string())
        .unwrap_or_default();
    // One title over the whole column: the order word, then the corner that makes a space.
    assert!(
        title.starts_with(" agents") && title.ends_with("tree +"),
        "the title lost its order word or its new-space corner: {title:?}"
    );
    // The shell is a tab and not an agent. The bar answers "what did I open" and the panel
    // answers "who needs me", and a shell is only ever the first of those, so the panel is
    // empty here and G12.7 is where it fills.
    assert!(
        ui.wait_for_tab(1, Duration::from_secs(15)),
        "the tab bar does not carry the open terminal: {:?}\n{}",
        ui.tab_bar(),
        ui.painted
    );
    let bar = ui.tab_bar().unwrap_or_default();
    assert!(bar.contains('+'), "the tab bar has no new tab: {bar:?}");
    // The program name lands after the daemon has named the foreground program, so the
    // name is waited for rather than read off the first frame that carries a tab:
    // asserting on whatever happened to be buffered is how a test reports a race
    // as a failure, and a loaded machine lands that frame with an empty name.
    let named = |bar: &str| {
        let label: String = bar[bar.find(" 1 ").map(|at| at + 3).unwrap_or_default()..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || "-_. ".contains(*c))
            .collect();
        // The label is the program, so the name is the first word of it.
        let program = label.split_whitespace().next().unwrap_or("");
        !program.is_empty()
            && program
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    assert!(
        ui.wait_for_tabs(named, Duration::from_secs(15)),
        "the tab names no program: {:?}",
        ui.tab_bar()
    );
    // The panel lists agents only: a plain shell is a tab, not an agent, so it never
    // gets a row there, and the space's rule opens with a blank and the name straight
    // after, holding nothing to fold.
    assert!(
        ui.wait_for_agents(|row| row.starts_with(" default"), Duration::from_secs(10)),
        "the empty space is not an open rule first: {:?}",
        agents_panel(&ui.screen)
    );
    let list = agents_panel(&ui.screen);
    assert!(
        // No row drew for the shell, or for anything else: the space holds nothing an
        // agents-only panel lists, so no row under the rule is an agent's (every agent
        // draws a `└` row under its name).
        !list.iter().any(|row| row.contains('\u{2514}')),
        "a plain shell is listed as an agent: {list:?}"
    );
    // And the terminal itself is being drawn: the shell's own output reaches the pane.
    ui.type_in(b"echo INTERFACE_$((6*7))\n");
    assert!(
        ui.wait_for("INTERFACE_42", Duration::from_secs(10))
            .is_some(),
        "what the shell printed never reached the pane:\n{}",
        ui.painted
    );
}

/// G12.2: create, switch and close, each from the keyboard and each through the protocol.
///
/// The protocol half is asserted by `action_coverage.rs`, which fails to compile if a
/// method has no action behind it. What is asserted here is that the keys do the thing.
#[test]
fn g12_2_a_terminal_can_be_created_switched_to_and_closed_from_the_keyboard() {
    let mut ui = Interface::start();
    assert!(ui.wait_for_tab(1, Duration::from_secs(20)));
    ui.type_in(b"echo FIRST_TERMINAL\n");
    assert!(
        ui.wait_for("FIRST_TERMINAL", Duration::from_secs(10))
            .is_some()
    );

    ui.create_terminal("");
    assert!(
        ui.wait_for_tab(2, Duration::from_secs(15)),
        "creating a terminal did not put a focused tab on the bar ({:?}), daemon said: {}",
        ui.tab_bar(),
        ui.daemon_log()
    );
    // The new one is focused, and what is typed reaches it rather than the one it came
    // from. This is the bug the first version had: the index moved and the keyboard did
    // not.
    ui.type_in(b"echo SECOND_TERMINAL\n");
    assert!(
        ui.wait_for("SECOND_TERMINAL", Duration::from_secs(10))
            .is_some(),
        "input went somewhere other than the new terminal:\n{}",
        ui.painted
    );

    // Back to the first, which still has its own history.
    ui.painted.clear();
    ui.command(b'n');
    assert!(
        ui.wait_for("FIRST_TERMINAL", Duration::from_secs(15))
            .is_some(),
        "switching did not bring back the first terminal's screen: {}",
        ui.daemon_log()
    );

    // And closing takes a row out of the index. Waited for rather than slept for: on a
    // machine running the rest of this suite, two seconds was sometimes not enough and the
    // gate reported a feature that works as broken.
    ui.command(b'x');
    assert!(
        ui.wait_for_tabs(|bar| !bar.contains(" 2 "), Duration::from_secs(15)),
        "the tab bar still carries a second tab after one terminal was closed: {:?}",
        ui.tab_bar()
    );
    // And what is left is usable. Asserting only that a tab went away is also satisfied by
    // a bar with nothing on it, and by a client that moved its focus without subscribing to
    // where it moved: measured before this, closing left the pane blank and the interface
    // looked hung.
    assert!(
        ui.wait_for_tab(1, Duration::from_secs(15)),
        "closing left nothing focused: {:?}",
        ui.tab_bar()
    );
    ui.painted.clear();
    ui.type_in(b"echo AFTER_THE_CLOSE\n");
    assert!(
        ui.wait_for("AFTER_THE_CLOSE", Duration::from_secs(15))
            .is_some(),
        "the terminal left after a close does not answer:\n{}",
        ui.painted
    );
}

/// G12.3: the child is sized to the pane, not to the window.
///
/// Asked of the child itself. The interface's own arithmetic agreeing with the interface
/// is not evidence, and a shell that believes it is wider than its pane wraps its lines in
/// the wrong place, which is the visible symptom.
#[test]
fn g12_3_the_child_is_sized_to_the_pane_and_not_to_the_window() {
    let mut ui = Interface::start();
    // A focused tab, not the key row: the key row paints while the client is still
    // connecting, and keys typed then reach no terminal.
    assert!(ui.wait_for_focused(Duration::from_secs(20)));
    ui.type_in(b"stty size\n");
    let (want_cols, want_rows) = qoro_tui::screen::pane_size(COLS, ROWS);
    let expected = format!("{want_rows} {want_cols}");
    assert!(
        ui.wait_for(&expected, Duration::from_secs(10)).is_some(),
        "the child reports a size that is not the pane's ({expected} expected):\n{}",
        ui.painted
    );
}

/// G12.4: the interface follows the process in the terminal, and does it without polling.
///
/// The command chosen prints nothing at all, which is the case that matters: a change the
/// interface could only learn by asking. The daemon pushes it.
#[test]
fn g12_4_the_interface_follows_the_foreground_process() {
    let mut ui = Interface::start();
    assert!(ui.wait_for_tab(1, Duration::from_secs(20)));

    ui.painted.clear();
    ui.type_in(b"sleep 12\n");
    let started = Instant::now();
    assert!(
        ui.wait_for_tabs(|bar| bar.contains("sleep"), Duration::from_secs(5)),
        "the tab never showed the running command:\n{}",
        ui.painted
    );
    let took = started.elapsed();
    // ADR-009's settle schedule is 5, 25 and 70 milliseconds, and the gate allows that plus
    // 100. Two seconds is the honest ceiling for a value measured through a pty on a
    // machine that is also running the rest of this suite.
    assert!(
        took < Duration::from_secs(2),
        "the interface took {took:?} to notice the process"
    );

    ui.type_in(&[0x03]);
    assert!(
        ui.wait_for_tabs(|bar| !bar.contains("sleep"), Duration::from_secs(12)),
        "the tab still shows the interrupted command: {:?}, pane row 1 is {:?}",
        ui.tab_bar(),
        ui.pane_line(1)
    );
}

/// G12.6: detaching restores the terminal and leaves the shell running.
#[test]
fn g12_6_detaching_restores_the_terminal_and_leaves_the_shell_running() {
    let mut ui = Interface::start();
    // A focused tab, not the key row: the key row paints while the client is still
    // connecting, and keys typed then reach no terminal.
    assert!(ui.wait_for_focused(Duration::from_secs(20)));
    ui.type_in(b"echo BEFORE_DETACH\n");
    assert!(
        ui.wait_for("BEFORE_DETACH", Duration::from_secs(10))
            .is_some()
    );

    ui.painted.clear();
    ui.command(b'd');
    let ended = Instant::now();
    // The same patience as every other wait in this file. Ten seconds is enough on an idle
    // machine and was measured failing on a loaded one, which is a flaky gate rather than
    // a finding: what is being asserted is that it exits, not how fast.
    while ended.elapsed() < Duration::from_secs(20) {
        if ui.child.try_wait().ok().flatten().is_some() {
            break;
        }
        let _ = ui.wait_for("never", Duration::from_millis(100));
    }
    let status = ui
        .child
        .try_wait()
        .expect("wait")
        .expect("the interface did not exit on detach");
    assert!(status.success(), "detaching exited with {status}");
    // The alternate screen is left, which is what gives the user their scrollback back.
    assert!(
        ui.saw("\x1b[?1049l"),
        "the interface did not leave the alternate screen:\n{:?}",
        ui.painted
    );
}

/// G12.7: an agent that needs a person is visible without looking at its terminal.
///
/// The whole product in one assertion. A stand in agent, because a real one cannot be
/// installed by a test, and it is a shell script on purpose: that is the shape that broke
/// the identity chain, where the executable is the interpreter and the agent's own name is
/// only in the argv.
#[test]
fn g12_7_an_agent_waiting_for_an_answer_is_visible_in_the_panel() {
    let mut ui = Interface::start();
    assert!(ui.wait_for_tab(1, Duration::from_secs(20)));

    let dir = ui.runtime_dir().join("bin");
    std::fs::create_dir_all(&dir).expect("dir");
    let agent = dir.join("claude");
    std::fs::write(
        &agent,
        "#!/bin/sh\nprintf 'Do you want to proceed with this edit?\\n'\n\
         printf '  1. Yes\\n  2. No\\n'\nsleep 30\n",
    )
    .expect("write");
    std::fs::set_permissions(
        &agent,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .expect("chmod");

    ui.type_in(format!("{}\n", agent.display()).as_bytes());

    // The name a person would say, not the interpreter that is running it. The
    // detail line sits one row lower than it used to: the open group carries its
    // own header now, so the title, the header, the head and then this line.
    assert!(
        ui.wait_for_agents(|line| line.contains("claude"), Duration::from_secs(15)),
        "the agents panel does not name the agent: {:?}",
        agents_panel(&ui.screen)
    );
    // What it is doing, read off the row that names it: the state dot at
    // the row's start wears the blocked colour. The stripped text says no
    // state, so this reads the raw bytes, in both painted forms: the row
    // may also have the keyboard.
    let blocked_inks = [
        qoro_tui::theme::fg(qoro_tui::theme::Role::Blocked),
        qoro_tui::theme::selected_in(qoro_tui::theme::Role::Blocked),
    ];
    assert!(
        ui.wait_for_agents_raw(
            |line| line.contains("claude")
                && blocked_inks.iter().any(|ink| line.contains(ink.as_str())),
            Duration::from_secs(15)
        ),
        "the agents panel does not show the agent as blocked: {:?}",
        agents_panel(&ui.screen)
    );
    // And the tab is named after it too, so a person can find it without reading the panel.
    assert!(
        ui.tab_bar().is_some_and(|bar| bar.contains("claude")),
        "the tab bar does not name the agent: {:?}",
        ui.tab_bar()
    );
    // And said out loud, so a person who is looking at another terminal still finds out.
    assert!(
        ui.saw("is blocked"),
        "the interface never said anything about it"
    );
    // Coloured, because a column of identical grey rows is a column nobody scans. This
    // row's own bytes: the pane beside it carries whatever the child printed, yellow
    // included, so a search of the whole screen answers a different question. Taken from
    // the theme rather than spelled out, so this gate cannot drift from it, and both forms
    // are allowed because the row may also have the keyboard.
    let blocked = [
        qoro_tui::theme::fg(qoro_tui::theme::Role::Blocked),
        qoro_tui::theme::selected_in(qoro_tui::theme::Role::Blocked),
    ];
    let panel = agents_panel(&ui.screen);
    let first = first_agent_row_of(&ui.screen);
    let detail = (0..panel.len())
        .find(|at| {
            panel[*at].contains("claude")
                && ui
                    .raw_panel_line(first + *at)
                    .is_some_and(|line| blocked.iter().any(|ink| line.contains(ink.as_str())))
        })
        .unwrap_or_else(|| panic!("no blocked row: {panel:?}"))
        + first;
    assert!(
        ui.raw_panel_line(detail)
            .is_some_and(|line| blocked.iter().any(|ink| line.contains(ink.as_str()))),
        "the blocked row is not painted as blocked: {:?}",
        ui.raw_panel_line(detail)
    );
}

/// G12.8: the tab bar is clickable, and a panel title is not a target.
///
/// Sent as the terminal sends it, which is the extended report `CSI < button ; col ; row M`
/// for a press and the same with `m` for the release. Acting on both would switch twice for
/// one click, so only the press is a command, and that is asserted here by sending both.
#[test]
fn g12_8_a_click_on_a_tab_moves_to_it_and_a_click_on_a_title_moves_to_nothing() {
    let mut ui = Interface::start();
    assert!(ui.wait_for_tab(1, Duration::from_secs(20)));
    ui.create_terminal("");
    assert!(
        ui.wait_for_tab(2, Duration::from_secs(15)),
        "the second terminal never appeared: {:?}",
        ui.tab_bar()
    );

    // The first terminal's own tab.
    let first = ui
        .tab_column(1)
        .unwrap_or_else(|| panic!("terminal 1 has no tab: {:?}", ui.tab_bar()));
    ui.click(first, qoro_tui::screen::TABS as u16);
    assert!(
        ui.wait_for_tab(1, Duration::from_secs(10)),
        "a click on the first tab did not move to it: {:?}",
        ui.tab_bar()
    );

    // A title names no terminal, so a click on it moves to none. Asserted from terminal 2,
    // which is not the one a one row slip in the panel's arithmetic would land on: with the
    // focus already on terminal 1, reading the spaces title as the first space row picks
    // the space that is already on screen, `focus` finds nothing to do, and the gate passes
    // through the bug.
    let second = ui
        .tab_column(2)
        .unwrap_or_else(|| panic!("terminal 2 has no tab: {:?}", ui.tab_bar()));
    ui.click(second, qoro_tui::screen::TABS as u16);
    assert!(ui.wait_for_tab(2, Duration::from_secs(10)));
    ui.click(5, SPACES_TITLE_ROW as u16);
    let _ = ui.wait_for("never appears", Duration::from_secs(2));
    assert_eq!(
        ui.focused_tab(),
        Some(2),
        "a click on a title moved the focus: {:?}",
        ui.tab_bar()
    );

    // And the tab bar itself, split across two reads, which is what a terminal does when
    // the report straddles a read boundary. Half of one passed through would type
    // `[<0;11;1M` at a shell.
    let col = ui
        .tab_column(2)
        .unwrap_or_else(|| panic!("terminal 2 has no tab: {:?}", ui.tab_bar()));
    ui.type_in(format!("\x1b[<0;{col}").as_bytes());
    std::thread::sleep(Duration::from_millis(100));
    ui.type_in(format!(";1M\x1b[<0;{col};1m").as_bytes());
    assert!(
        ui.wait_for_tab(2, Duration::from_secs(10)),
        "a split mouse report on the tab bar did not arrive whole: {:?}",
        ui.tab_bar()
    );
    assert!(
        !ui.saw(&format!("[<0;{col}")),
        "part of a mouse report reached the terminal"
    );

    // The `+` at the end of the bar is a way to make a terminal or an agent without
    // knowing a key binding, not decoration: a click on it offers both for this space.
    let bar = ui
        .tab_bar()
        .unwrap_or_else(|| panic!("no tab bar: {:?}", ui.tab_bar()));
    let plus = bar
        .find('+')
        .unwrap_or_else(|| panic!("no new tab button: {bar:?}"));
    // Read off the bar's own left edge, clicked in screen columns.
    let mark = ui.painted.len();
    ui.click(
        plus as u16 + 1 + qoro_tui::screen::left(COLS) as u16,
        qoro_tui::screen::TABS as u16,
    );
    // It asks which: `agent` first, `terminal` under it.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && !ui.painted[mark..].contains("new in") {
        let _ = ui.wait_for("this needle never appears", Duration::from_millis(80));
    }
    assert!(
        ui.painted[mark..].contains("new in"),
        "the + menu never opened:\n{}",
        &ui.painted[mark..]
    );
    ui.type_in(b"j");
    std::thread::sleep(Duration::from_millis(50));
    ui.type_in(b"\r");
    assert!(
        ui.wait_for_tab(3, Duration::from_secs(15)),
        "a click on + did not start a terminal: {:?}",
        ui.tab_bar()
    );
}

/// G12.9: a space is a place, and both the keyboard and the pointer get to it.
///
/// The gate the whole left column exists for. Without it a daemon could hold one workspace
/// forever and every screen would still look right, because a person with one space cannot
/// tell a real grouping from a number that never changes.
#[test]
fn g12_9_a_new_space_is_reachable_and_carries_its_own_tabs() {
    let mut ui = Interface::start();
    assert!(ui.wait_for_tab(1, Duration::from_secs(20)));
    ui.type_in(b"echo IN_THE_FIRST_SPACE\n");
    assert!(
        ui.wait_for("IN_THE_FIRST_SPACE", Duration::from_secs(10))
            .is_some()
    );

    // A new space, which starts with a terminal in it: a space with nothing in it shows an
    // empty pane and a bar with only a `+` on it, and nobody wants to land there.
    ui.create_space("");
    // Anywhere in the column: the first space's tab sits between the two rules.
    assert!(
        ui.wait_for_agents(|line| line.contains("space 2"), Duration::from_secs(15)),
        "the new space is not in the spaces panel: {:?}, daemon said: {}",
        agents_panel(&ui.screen),
        ui.daemon_log()
    );

    // Where the second space's rule is drawn: under the first space and its tab.
    let second = FIRST_SPACE_ROW
        + agents_panel(&ui.screen)
            .iter()
            .position(|line| line.contains("space 2"))
            .expect("the rule was just seen");

    // Its own tab bar: one tab, which is this space's, and not the other space's as well.
    assert!(
        ui.wait_for_tabs(
            |bar| bar.contains(" 1 ") && !bar.contains(" 2 "),
            Duration::from_secs(15)
        ),
        "the new space shows the other space's tabs: {:?}",
        ui.tab_bar()
    );

    // And back, by clicking the first space, which brings back that space's terminal with
    // the history it had.
    ui.painted.clear();
    ui.click(5, FIRST_SPACE_ROW as u16);
    assert!(
        ui.wait_for("IN_THE_FIRST_SPACE", Duration::from_secs(15))
            .is_some(),
        "clicking the first space did not bring its terminal back:\n{}",
        ui.painted
    );
    assert!(
        ui.focused_tab() == Some(1),
        "the tab bar did not follow the space: {:?}",
        ui.tab_bar()
    );

    // The keyboard reaches the other space too, so the pointer is not the only way in.
    ui.command(b's');
    assert!(
        ui.wait_for_space_at(second, Duration::from_secs(15))
            .is_some(),
        "ctrl-a s did not move to the next space: {:?}",
        ui.panel_line(second)
    );
    // And backwards, which wraps. From the second of two spaces that is the first one, and
    // its terminal still has the history it had.
    ui.painted.clear();
    ui.command(b'S');
    assert!(
        ui.wait_for("IN_THE_FIRST_SPACE", Duration::from_secs(15))
            .is_some(),
        "ctrl-a S did not go back to the first space:\n{}",
        ui.painted
    );
    // Backwards again, which wraps the other way, off the front of the list.
    ui.command(b'S');
    assert!(
        ui.wait_for_space_at(second, Duration::from_secs(15))
            .is_some(),
        "ctrl-a S from the first space did not wrap to the last: {:?}",
        ui.panel_line(second)
    );
}

/// G12.10: a terminal this daemon cannot read into is still on the panel, named after the
/// thing standing in the way.
///
/// The case the whole panel exists for, and the one that was invisible. An agent started
/// with `docker run` has no name this daemon can read, so the row can never say what is in
/// there. A panel that listed only what it could name left that terminal off the screen
/// entirely, which is the one terminal nobody can check by looking.
///
/// A stand in named `docker`, because the boundary is decided by the program's name and a
/// real container runtime cannot be a dependency of a test.
#[test]
fn g12_10_a_terminal_behind_a_namespace_is_on_the_panel_as_the_wrapper() {
    let mut ui = Interface::start();
    assert!(ui.wait_for_tab(1, Duration::from_secs(20)));

    let dir = ui.runtime_dir().join("bin");
    std::fs::create_dir_all(&dir).expect("dir");
    // A real executable and not a shell script, and compiled rather than copied. The
    // boundary is decided from the executable the process is actually running: a script
    // named `docker` is executed by `/bin/sh`, so the daemon sees through it to the
    // script's own name instead, which is the right answer for a wrapper script and not the
    // case under test. A copy of a system binary is not an option either, since the copy
    // loses its signature and this machine kills it on sight (measured: exit 137).
    let source = dir.join("docker.rs");
    std::fs::write(
        &source,
        "fn main() { std::thread::sleep(std::time::Duration::from_secs(30)); }\n",
    )
    .expect("write");
    let wrapper = dir.join("docker");
    let built = Command::new("rustc")
        .arg(&source)
        .arg("-o")
        .arg(&wrapper)
        .output()
        .expect("run rustc");
    assert!(
        built.status.success(),
        "the stand in did not build: {}",
        String::from_utf8_lossy(&built.stderr)
    );

    ui.type_in(format!("{}\n", wrapper.display()).as_bytes());
    // Unobserved agents draw bare, with no group header: this one is unknown, so
    // its detail sits directly under the title as before.
    assert!(
        ui.wait_for_agents(
            |line| line.contains("behind docker"),
            Duration::from_secs(15)
        ),
        "the panel does not carry the terminal behind the wrapper: {:?}\n{}",
        agents_panel(&ui.screen),
        ui.painted
    );
    // The wrapper's own name is never printed where a program's name goes: a person shown
    // `docker` there would conclude their agent is called docker.
    assert!(
        !agents_panel(&ui.screen)
            .iter()
            .any(|line| line.trim_start().starts_with("docker")),
        "the wrapper is named as the program: {:?}",
        agents_panel(&ui.screen)
    );
}

/// G12.11: a space with nothing in it takes the keyboard out of the terminal it left.
///
/// Entering a space is a claim about where the keyboard is. Without this, moving to an
/// empty space left every keystroke going to a terminal in the space that was just left,
/// left that terminal's output painting under this space's tab bar, and left `ctrl-a x`
/// closing it. Nothing on screen said any of that was happening.
#[test]
fn g12_11_an_empty_space_does_not_leave_the_keyboard_in_the_space_it_left() {
    let mut ui = Interface::start();
    assert!(ui.wait_for_tab(1, Duration::from_secs(20)));
    ui.type_in(b"echo BEFORE_THE_MOVE\n");
    assert!(
        ui.wait_for("BEFORE_THE_MOVE", Duration::from_secs(10))
            .is_some()
    );

    // A second space with a terminal in it, then close that terminal, which is the only way
    // to reach a space that is empty and stays empty.
    ui.create_space("");
    // On the second space, and not merely on a bar whose first tab is focused: every space
    // has a tab one, so waiting for that alone is satisfied before the space even exists.
    assert!(
        ui.wait_for_space(2, Duration::from_secs(15)),
        "the second space never came up: {:?}, daemon said: {}",
        ui.panel_line(FIRST_SPACE_ROW + 1),
        ui.daemon_log()
    );
    assert!(
        ui.wait_for_tabs(
            |bar| bar.contains(" 1 ") && !bar.contains(" 2 "),
            Duration::from_secs(15)
        ),
        "the new space came up with no terminal of its own: {:?}",
        ui.tab_bar()
    );
    ui.command(b'x');
    assert!(
        ui.wait_for("this space is empty", Duration::from_secs(15))
            .is_some(),
        "an empty space does not say so:\n{}",
        ui.painted
    );

    // Now type. Not one byte of it may reach the terminal in the other space.
    ui.painted.clear();
    ui.type_in(b"echo LEAKED_INTO_THE_OTHER_SPACE\n");
    let _ = ui.wait_for("never appears", Duration::from_secs(2));
    assert!(
        !ui.saw("LEAKED_INTO_THE_OTHER_SPACE"),
        "a keystroke reached a terminal in a space that was left:\n{}",
        ui.painted
    );

    // Leave the empty space and come back to it, which is the other way in: entering a
    // space that has nothing in it has to take the keyboard out of the terminal it left,
    // and not merely refuse to move it.
    ui.command(b's');
    assert!(
        ui.wait_for_space(1, Duration::from_secs(15)),
        "ctrl-a s did not go back to the first space: {:?}",
        ui.panel_line(FIRST_SPACE_ROW)
    );
    ui.command(b's');
    assert!(
        ui.wait_for("this space is empty", Duration::from_secs(15))
            .is_some(),
        "ctrl-a s did not return to the empty space:\n{}",
        ui.painted
    );
    ui.painted.clear();
    ui.type_in(b"echo LEAKED_ON_THE_WAY_BACK\n");
    let _ = ui.wait_for("never appears", Duration::from_secs(2));
    assert!(
        !ui.saw("LEAKED_ON_THE_WAY_BACK"),
        "entering an empty space left the keyboard in the space it came from:\n{}",
        ui.painted
    );

    // And the way out is still there: a terminal started here belongs to this space.
    ui.create_terminal("");
    assert!(
        ui.wait_for_focused(Duration::from_secs(15)),
        "ctrl-a c in an empty space did not start a terminal: {:?}",
        ui.tab_bar()
    );
    ui.type_in(b"echo BACK_IN_BUSINESS\n");
    assert!(
        ui.wait_for("BACK_IN_BUSINESS", Duration::from_secs(10))
            .is_some(),
        "the new terminal does not take the keyboard:\n{}",
        ui.painted
    );
}

/// G12.12: two spaces made in the same breath get two names and two terminals.
///
/// The name is counted off the list of spaces the client has, and neither request has been
/// answered yet when the second one is sent, so both counted the same list and both came out
/// as `space 2`: two rows in the panel with one name on them, and no way to tell which is
/// which.
///
/// What this gate does not reach is the other half of the same moment, both answers landing
/// in one read. They are folded one at a time and acted on once, after the read is drained,
/// so a single slot would keep the second and drop the first. That is held by construction
/// instead, with a queue: it could not be provoked from the outside here, the two answers
/// arriving in separate reads every time, and a gate that cannot go red is not a gate.
#[test]
fn g12_12_two_spaces_made_at_once_get_two_names_and_two_terminals() {
    let mut ui = Interface::start();
    assert!(ui.wait_for_tab(1, Duration::from_secs(20)));

    // Two, asked for back to back with no wait between them and both left unnamed, so both
    // fall back to the counted default before either answer has arrived. That is the case
    // the counter exists for: without it both came out as `space 2`.
    ui.create_space("");
    ui.create_space("");

    assert!(
        ui.wait_for_agents(|line| line.contains("space 3"), Duration::from_secs(15)),
        "the second space never appeared: {:?}, boxes: {}, confirms: {}, daemon said: {}",
        agents_panel(&ui.screen),
        ui.painted.matches("name the new space").count(),
        ui.painted.matches("starting a space").count(),
        ui.daemon_log()
    );

    // Every space, walked from where the interface left off, has to have a tab in it.
    for step in 0..3 {
        assert!(
            ui.wait_for_focused(Duration::from_secs(15)),
            "a space came up with no terminal in it after {step} moves: {:?}\n{}",
            ui.tab_bar(),
            ui.painted
        );
        ui.command(b's');
    }
}

/// G12.13: clicking an agent in another space brings that space with it.
///
/// The reason the agents panel crosses spaces at all. Without the space following the
/// terminal, the click moves the keyboard into a space the tab bar is not showing: the
/// focused terminal has no tab on screen, and the bar offers only the terminals of the space
/// the person just left. Nothing gated that, so the whole block could be deleted and every
/// other gate would stay green.
#[test]
fn g12_13_clicking_an_agent_in_another_space_brings_its_space_with_it() {
    let mut ui = Interface::start();
    assert!(ui.wait_for_tab(1, Duration::from_secs(20)));

    // A stand in agent in a second space, so there is a row in the panel to click. A shell
    // is not one: the panel lists agents.
    let dir = ui.runtime_dir().join("bin");
    std::fs::create_dir_all(&dir).expect("dir");
    let agent = dir.join("claude");
    std::fs::write(
        &agent,
        "#!/bin/sh\nprintf 'Do you want to proceed?\\n'\nprintf '  1. Yes\\n  2. No\\n'\nsleep 30\n",
    )
    .expect("write");
    std::fs::set_permissions(
        &agent,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .expect("chmod");

    ui.create_space("");
    assert!(
        ui.wait_for_space(2, Duration::from_secs(15)),
        "the second space never came up: {}",
        ui.daemon_log()
    );
    assert!(ui.wait_for_tabs(|bar| bar.contains(" 1 "), Duration::from_secs(15)));
    ui.type_in(format!("{}\n", agent.display()).as_bytes());
    assert!(
        ui.wait_for_agents(|line| line.contains("claude"), Duration::from_secs(20)),
        "the agent is not on the panel: {:?}\n{}",
        agents_panel(&ui.screen),
        ui.painted
    );
    // Wait for detection to settle: a fresh agent reports unknown, then idle
    // while its generation is new, and only then the blocked its screen shows.
    // The state is the dot's ink, so this reads the raw bytes like G12.7:
    // both painted forms, since the row may have the keyboard.
    let blocked_inks = [
        qoro_tui::theme::fg(qoro_tui::theme::Role::Blocked),
        qoro_tui::theme::selected_in(qoro_tui::theme::Role::Blocked),
    ];
    assert!(
        ui.wait_for_agents_raw(
            |line| line.contains("claude")
                && blocked_inks.iter().any(|ink| line.contains(ink.as_str())),
            Duration::from_secs(20)
        ),
        "the agent never showed as blocked: {:?}\n{}",
        agents_panel(&ui.screen),
        ui.painted
    );

    // Back to the first space, where the agent's own tab is not on the bar and its row in
    // the panel still is.
    ui.command(b's');
    assert!(
        ui.wait_for_space(1, Duration::from_secs(15)),
        "ctrl-a s did not go back: {:?}",
        ui.panel_line(FIRST_SPACE_ROW)
    );
    // The agent's own first row, which is the one that names the session
    // behind its state dot; the tab and the space live on the row under it.
    let panel = agents_panel(&ui.screen);
    let at = panel
        .iter()
        .position(|line| line.contains("claude"))
        .unwrap_or_else(|| panic!("the agent has no row: {}", ui.painted));
    let row = first_agent_row_of(&ui.screen) + at;

    ui.click(5, row as u16);
    assert!(
        ui.wait_for_tabs(|bar| bar.contains("claude"), Duration::from_secs(15)),
        "the click did not move the keyboard to the agent: {:?}",
        ui.tab_bar()
    );
    // The bar is the other space's now, and the panel says which space that is.
    assert!(
        ui.tab_bar().is_some_and(|bar| !bar.contains(" 2 ")),
        "the tab bar stayed in the space that was left: {:?}",
        ui.tab_bar()
    );
    assert!(
        ui.wait_for_space(2, Duration::from_secs(10)),
        "the column still marks the space that was left: {:?}",
        agents_panel(&ui.screen)
    );
}

/// G12.14: a digit picks the tab at that position in this space, and not in the daemon.
///
/// `ctrl-a 2` counts along the bar a person is reading, which is one space's worth of
/// terminals. Counting the daemon's whole list instead jumps into another space, and off by
/// one picks the neighbour: both are one character in the client and neither was gated.
#[test]
fn g12_14_a_digit_picks_the_tab_at_that_position_in_this_space() {
    let mut ui = Interface::start();
    assert!(ui.wait_for_tab(1, Duration::from_secs(20)));

    // A space of its own with three terminals in it, so that a client counting the daemon's
    // whole list lands somewhere visibly different from one counting this bar.
    ui.create_space("");
    assert!(
        ui.wait_for_space(2, Duration::from_secs(15)),
        "the second space never came up: {}",
        ui.daemon_log()
    );
    assert!(ui.wait_for_tabs(|bar| bar.contains(" 1 "), Duration::from_secs(15)));
    ui.create_terminal("");
    assert!(ui.wait_for_tab(2, Duration::from_secs(15)));
    ui.create_terminal("");
    assert!(ui.wait_for_tab(3, Duration::from_secs(15)));
    // Three tabs here, and a fourth terminal on the daemon that belongs to another space.
    ui.type_in(b"echo THE_THIRD_TAB\n");
    assert!(
        ui.wait_for("THE_THIRD_TAB", Duration::from_secs(10))
            .is_some()
    );

    ui.command(b'1');
    assert!(
        ui.wait_for_tab(1, Duration::from_secs(10)),
        "ctrl-a 1 did not pick this space's first tab: {:?}",
        ui.tab_bar()
    );
    // And it is this space's first tab, not the daemon's: the terminal in the other space
    // still has its own screen, and none of it is here.
    let _ = ui.wait_for("never appears", Duration::from_secs(1));
    assert!(
        ui.tab_bar().is_some_and(|bar| !bar.contains(" 4 ")),
        "ctrl-a 1 counted the daemon's terminals rather than this bar: {:?}",
        ui.tab_bar()
    );
    ui.painted.clear();
    ui.command(b'3');
    assert!(
        ui.wait_for("THE_THIRD_TAB", Duration::from_secs(10))
            .is_some(),
        "ctrl-a 3 did not pick this space's third tab: {:?}\n{}",
        ui.tab_bar(),
        ui.painted
    );
    // A digit past the end does nothing rather than something arbitrary.
    ui.command(b'9');
    let _ = ui.wait_for("never appears", Duration::from_secs(2));
    assert_eq!(
        ui.focused_tab(),
        Some(3),
        "a digit past the last tab moved the focus: {:?}",
        ui.tab_bar()
    );
}

/// G12.15: a shell that exits on its own takes its tab with it, and the keyboard goes to
/// what is left.
///
/// `exit` is how a person closes a terminal, and it never goes through this client: the
/// daemon notices the child is gone and the only sign of it is that the index has one fewer
/// row. Measured before this gate: the tab came off the bar and the focus stayed on a
/// terminal that no longer existed, with its dead screen still painted and every keystroke
/// after it going nowhere. Nothing on screen said so and no key got out of it.
///
/// And the last one ends the interface. With no terminals left it is a way in to nothing,
/// and a person who does not know `ctrl-q` is stuck in it.
#[test]
fn g12_15_a_shell_that_exits_takes_its_tab_and_hands_the_keyboard_on() {
    let mut ui = Interface::start();
    assert!(ui.wait_for_tab(1, Duration::from_secs(20)));
    ui.create_terminal("");
    assert!(
        ui.wait_for_tab(2, Duration::from_secs(15)),
        "the second terminal never appeared: {:?}",
        ui.tab_bar()
    );

    ui.type_in(b"exit\n");
    assert!(
        ui.wait_for_tabs(|bar| !bar.contains(" 2 "), Duration::from_secs(20)),
        "the tab of the shell that exited is still on the bar: {:?}",
        ui.tab_bar()
    );
    assert!(
        ui.wait_for_tab(1, Duration::from_secs(15)),
        "nothing has the keyboard after a shell exited: {:?}",
        ui.tab_bar()
    );
    // And it is really the keyboard, not just a bracket: the terminal that is left answers.
    ui.painted.clear();
    ui.type_in(b"echo AFTER_THE_EXIT\n");
    assert!(
        ui.wait_for("AFTER_THE_EXIT", Duration::from_secs(15))
            .is_some(),
        "the terminal left after an exit does not answer:\n{}",
        ui.painted
    );

    // The last one ends the interface, and it ends it the way detaching does: the outer
    // terminal gets its screen back.
    ui.type_in(b"exit\n");
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && ui.child.try_wait().ok().flatten().is_none() {
        let _ = ui.wait_for("never appears", Duration::from_millis(200));
    }
    let status = ui
        .child
        .try_wait()
        .expect("wait")
        .unwrap_or_else(|| panic!("the interface is still running with no terminals in it"));
    assert!(status.success(), "the interface exited with {status}");
    assert!(
        ui.saw("\x1b[?1049l"),
        "the interface did not leave the alternate screen"
    );
}

/// G12.16: ctrl-c interrupts a foreground process even when the daemon was
/// started backgrounded.
///
/// A daemon launched with `&` (and by launchd and systemd) carries SIGINT and
/// SIGQUIT as ignored, `exec` preserves an ignored disposition, and an
/// interactive shell does not heal those two. Without the daemon resetting
/// them for its children, every terminal ignores ctrl-c while ctrl-z keeps
/// working, which reads as a broken key rather than an inherited disposition.
/// The harness builds that condition on purpose: a daemon spawned with
/// default dispositions would pass this gate with the fix deleted.
#[test]
fn g12_16_ctrl_c_interrupts_a_foreground_process_for_a_backgrounded_daemon() {
    let mut ui = Interface::start_backgrounded();
    assert!(ui.wait_for_tab(1, Duration::from_secs(20)));
    // The marker is computed by the shell, so the typed echo cannot match it: waiting
    // for the echoed command line would pass without anything being interrupted.
    ui.type_in(b"sleep 30; echo CTRL_C_WOKE_$((40 + 2))\n");
    assert!(
        ui.wait_for_tabs(|bar| bar.contains("sleep"), Duration::from_secs(15)),
        "sleep never came to the foreground: {:?}",
        ui.tab_bar()
    );
    ui.type_in(&[0x03]);
    assert!(
        ui.wait_for("CTRL_C_WOKE_42", Duration::from_secs(15))
            .is_some(),
        "ctrl-c did not interrupt the foreground sleep:\n{}",
        ui.painted
    );
}

/// G12.16: an agent started through a wrapper is still an agent.
///
/// Measured on a real machine: opencode launched through a shell script that tunnels it
/// read as `opencode-vpn`, which is not a name in the agent table, so the tab said so and
/// the agents panel left the terminal out of the list entirely. The wrapper leads the
/// foreground process group and the agent runs under it, both in that one group, because
/// starting one job puts the whole job in one group.
///
/// The stand in is a script that runs a compiled `codex` and waits for it, so the leader is
/// the script and the agent is a member of its group: the exact shape that was invisible.
#[test]
fn g12_16_an_agent_behind_a_wrapper_script_is_on_the_panel() {
    let mut ui = Interface::start();
    assert!(ui.wait_for_tab(1, Duration::from_secs(20)));

    let dir = ui.runtime_dir().join("bin");
    std::fs::create_dir_all(&dir).expect("dir");
    let source = dir.join("codex.rs");
    std::fs::write(
        &source,
        "fn main() { std::thread::sleep(std::time::Duration::from_secs(30)); }\n",
    )
    .expect("write");
    let built = Command::new("rustc")
        .arg(&source)
        .arg("-o")
        .arg(dir.join("codex"))
        .output()
        .expect("run rustc");
    assert!(
        built.status.success(),
        "the stand in did not build: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    let wrapper = dir.join("codex-vpn");
    std::fs::write(
        &wrapper,
        format!("#!/bin/sh\n{} &\nwait\n", dir.join("codex").display()),
    )
    .expect("write");
    std::fs::set_permissions(
        &wrapper,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .expect("chmod");

    ui.type_in(format!("{}\n", wrapper.display()).as_bytes());
    // An agent row, not the tab's own faint row: until the agent behind the wrapper is
    // found, the tab reads as the wrapper, as it does in the tab bar.
    assert!(
        ui.wait_for_agents(
            |line| line.contains("codex") && !is_rule_line(line),
            Duration::from_secs(20)
        ),
        "the agent behind the wrapper is not on the panel: {:?}\n{}",
        agents_panel(&ui.screen),
        ui.painted
    );
    // And it is the agent's name, not the wrapper's: a person shown `codex-vpn` would go
    // looking for a program by that name.
    assert!(
        !agents_panel(&ui.screen)
            .iter()
            .any(|line| line.contains("codex-vpn")),
        "the wrapper is named as the agent: {:?}",
        agents_panel(&ui.screen)
    );
}

/// G12.20: a fresh attach sizes the adopted terminal to its own pane.
///
/// A restored session's children were sized for whoever had them before (a
/// restore spawns at the default): without a resize on adopt, the first
/// paint shows a small screen until anything moves the focus. A second
/// screen on the same session adopts what is already open and must size it
/// on arrival.
#[test]
fn g12_20_a_fresh_attach_sizes_the_adopted_terminal_to_its_pane() {
    let mut ui = Interface::start();
    assert!(ui.wait_for_tab(1, Duration::from_secs(20)));
    // The old screen goes away first: it owns the size, and a second watcher
    // never takes it. What is tested is the fresh attach after a disconnect.
    ui.disconnect_client();
    // Let the daemon reap the dead client and release its size ownership:
    // an attach racing that cleanup gets its resize refused.
    std::thread::sleep(Duration::from_secs(3));
    // Another screen with its own window size on the same session: no new
    // daemon, just a fresh adopt. A same-sized screen would prove nothing,
    // since the child is already that size. Dropped before `ui`, so the
    // directory outlives it.
    let mut ui2 = Interface::attach_sized(&ui.runtime_dir(), 120, 34);
    assert!(ui2.wait_for_tab(1, Duration::from_secs(20)));
    ui2.type_in(b"stty size\n");
    let (want_cols, want_rows) = qoro_tui::screen::pane_size(120, 34);
    let expected = format!("{want_rows} {want_cols}");
    assert!(
        ui2.wait_for(&expected, Duration::from_secs(10)).is_some(),
        "the adopted child reports a size that is not the pane's ({expected} expected):\n{}",
        ui2.painted
    );
}

#[test]
fn g12_21_clicking_the_order_word_flips_tree_and_priority() {
    let mut ui = Interface::start();
    let title = ui
        .wait_for_panel(
            FIRST_SPACE_ROW,
            |line| line.contains("default"),
            Duration::from_secs(15),
        )
        .and_then(|_| {
            agents_title_row(&ui.screen).and_then(|row| {
                ui.wait_for_panel(
                    row,
                    |line| line.trim_end().ends_with("tree +"),
                    Duration::from_secs(15),
                )
                .map(|_| row)
            })
        })
        .unwrap_or_else(|| panic!("the title never said tree:\n{}", ui.painted));
    // The order word flips the view, like `ctrl-a g`: the same agents, urgency first. The
    // word ends two columns short of the edge, before the space and the `+`, and both
    // words cover the column clicked here.
    let word = index_width() as u16 - 3;
    ui.click(word, title as u16);
    assert!(
        ui.wait_for_panel(
            title,
            |line| line.trim_end().ends_with("priority +"),
            Duration::from_secs(10)
        )
        .is_some(),
        "a click on tree did not flip to priority:\n{}",
        ui.painted
    );
    // And back again with a second click.
    ui.click(word, title as u16);
    assert!(
        ui.wait_for_panel(
            title,
            |line| line.trim_end().ends_with("tree +"),
            Duration::from_secs(10)
        )
        .is_some(),
        "a click on priority did not flip back to tree:\n{}",
        ui.painted
    );
    // The `spaces` word itself is nothing: a click there leaves the view alone.
    ui.click(5, title as u16);
    let _ = ui.wait_for("never appears", Duration::from_secs(2));
    assert!(
        ui.panel_line(title)
            .is_some_and(|line| line.trim_end().ends_with("tree +")),
        "a click on the spaces word moved the view:\n{}",
        ui.painted
    );
}

/// Complete frames sliced out of the accumulated stream after `mark`.
/// The stream only grows, so the last slice may be mid-frame and is dropped.
/// Empty when fewer than two separators arrived: a count, never a panic, so a
/// quiet daemon fails the assertion instead of the helper.
fn complete_frames(painted: &str, mark: usize) -> Vec<&str> {
    let Some(tail) = painted.get(mark..) else {
        return Vec::new();
    };
    let mut frames: Vec<&str> = tail.split("\x1b[?25l").collect();
    if frames.len() < 3 {
        return Vec::new();
    }
    frames.remove(0);
    frames.pop();
    // The first frame may predate the change: it was painted from a render
    // that arrived before the shell processed the escape.
    frames.remove(0);
    frames
}

#[test]
fn g12_22_a_hidden_child_cursor_stays_hidden_between_repaints() {
    let mut ui = Interface::start();
    assert!(ui.wait_for_tab(1, Duration::from_secs(20)));
    // Stream while hidden: every line is a render, every render a frame, and
    // none of those frames may show a cursor the child hid.
    ui.type_in(b"printf '\\e[?25l'; for i in 1 2 3 4 5 6; do echo line$i; sleep 0.4; done\n");
    assert!(
        ui.wait_for("line6", Duration::from_secs(15)).is_some(),
        "the hidden stream never printed:\n{}",
        ui.painted
    );
    let mark = ui.painted.len();
    ui.type_in(b"for i in 7 8 9; do echo more$i; sleep 0.4; done\n");
    assert!(
        ui.wait_for("more9", Duration::from_secs(15)).is_some(),
        "the second stream never printed:\n{}",
        ui.painted
    );
    let frames = complete_frames(&ui.painted, mark);
    assert!(
        !frames.is_empty(),
        "no frames arrived while streaming hidden: {}",
        ui.painted.len()
    );
    for frame in &frames {
        assert!(
            !frame.contains("?25h"),
            "a hidden cursor was shown mid-stream: {frame:?}"
        );
    }
    // And showing it again brings it back on the following frames.
    ui.type_in(b"printf '\\e[?25h'; echo back\n");
    assert!(
        ui.wait_for("back", Duration::from_secs(15)).is_some(),
        "the restore never printed:\n{}",
        ui.painted
    );
    let mark = ui.painted.len();
    ui.type_in(b"for i in 1 2 3; do echo showing$i; sleep 0.4; done\n");
    assert!(
        ui.wait_for("showing3", Duration::from_secs(15)).is_some(),
        "the visible check never printed:\n{}",
        ui.painted
    );
    let frames = complete_frames(&ui.painted, mark);
    assert!(
        frames.iter().any(|f| f.contains("?25h")),
        "a shown cursor never came back: {}",
        ui.painted.len()
    );
}

/// Radar G10: the tree view keeps creation order on live rows.
///
/// Two stand-in agents, because real ones cannot be installed by a test. The
/// stale dim cannot be driven live (two hours is not a test), so it stays
/// pinned at the unit level by G1; what is live here is a tree-order
/// snapshot with both agents settled.
#[test]
fn radar_tree_order_holds_creation_order() {
    let mut ui = Interface::start();
    assert!(ui.wait_for_tab(1, Duration::from_secs(20)));

    let dir = ui.runtime_dir().join("bin");
    std::fs::create_dir_all(&dir).expect("dir");
    for (name, body) in [
        ("claude", "printf 'working\\n'; sleep 30\n"),
        ("codex", "printf 'working\\n'; sleep 30\n"),
    ] {
        std::fs::write(dir.join(name), format!("#!/bin/sh\n{body}")).expect("write");
        std::fs::set_permissions(
            dir.join(name),
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .expect("chmod");
    }

    // A state row opens with a blank, then its two-cell braille mark straight into
    // the program name, with no space between them; a place row (`└ claude · ...`)
    // never does, so this pins the match to the state row alone.
    fn is_state_row(line: &str, program: &str) -> bool {
        line.strip_prefix(' ').is_some_and(|rest| {
            rest.chars()
                .next()
                .is_some_and(|c| ('\u{2800}'..='\u{28ff}').contains(&c))
                && rest
                    .trim_start_matches(|c: char| ('\u{2800}'..='\u{28ff}').contains(&c))
                    .starts_with(program)
        })
    }

    ui.type_in(format!("{}\n", dir.join("claude").display()).as_bytes());
    assert!(
        ui.wait_for_agents(|line| is_state_row(line, "claude"), Duration::from_secs(15)),
        "the agents panel does not name the agent: {:?}",
        agents_panel(&ui.screen)
    );

    ui.create_terminal("");
    assert!(
        ui.wait_for_tab(2, Duration::from_secs(15)),
        "the second terminal never appeared: {:?}",
        ui.tab_bar()
    );
    ui.type_in(format!("{}\n", dir.join("codex").display()).as_bytes());
    assert!(
        ui.wait_for_agents(|line| is_state_row(line, "codex"), Duration::from_secs(15)),
        "the agents panel does not name the agent: {:?}",
        agents_panel(&ui.screen)
    );

    // Grouped is daemon order: the first created agent draws first, whatever
    // the two have done since. The mark pins the match to first rows, so a
    // place row naming the program cannot satisfy it.
    let panel = agents_panel(&ui.screen);
    let claude = panel
        .iter()
        .position(|line| is_state_row(line, "claude"))
        .unwrap_or_else(|| panic!("claude left the panel: {panel:?}"));
    let codex = panel
        .iter()
        .position(|line| is_state_row(line, "codex"))
        .unwrap_or_else(|| panic!("codex left the panel: {panel:?}"));
    assert!(claude < codex, "tree order moved a row: {panel:?}");
}

/// Radar G12: dragging the divider resizes the panel, clamps at both ends,
/// and a motionless press changes nothing.
///
/// The rule column is read off the live screen: it sits one past the panel
/// width, so its index is the width. A press and release with no motion in
/// between must leave it exactly where it was.
#[test]
fn radar_dragging_the_divider_resizes_the_panel() {
    let mut ui = Interface::start();
    assert!(ui.wait_for_tab(1, Duration::from_secs(20)));

    // The rule column of a sidebar row, as a zero based index into the row.
    // Only the sidebar columns count: the pane beside it may print rules
    // of its own, and those live further right.
    fn rule_at(ui: &Interface) -> Option<usize> {
        for row in 1..=22usize {
            if let Some(at) = ui
                .screen
                .text(row)
                .and_then(|line| line.chars().position(|c| c == '│' || c == '┤'))
                .filter(|at| *at <= COLS as usize / 2)
            {
                return Some(at);
            }
        }
        None
    }
    fn wait_for_rule(ui: &mut Interface, want: usize) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let _ = ui.wait_for("this needle never appears", Duration::from_millis(100));
            if rule_at(ui) == Some(want) {
                return true;
            }
        }
        rule_at(ui) == Some(want)
    }

    // The default width, and the one based column of the divider after it.
    let w = qoro_tui::sidebar::WIDTH;
    let d = w as u16 + 1;
    assert_eq!(rule_at(&ui), Some(w), "the panel does not start at {w}");

    // A motionless press and release: no-op, not even a refetch flicker.
    ui.drag(d, 5, &[]);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(rule_at(&ui), Some(w), "a motionless press moved the panel");

    // Pull right: the panel widens to the dragged column minus one. The
    // first drag ends at width w + 4 (motion column d + 4), so the divider
    // sits at d + 4 for the next press.
    ui.drag(d, 5, &[d + 2, d + 4]);
    assert!(
        wait_for_rule(&mut ui, w + 4),
        "the panel did not widen to {}: {:?}",
        w + 4,
        rule_at(&ui)
    );

    // Pull past the end: clamps at half the window, never further.
    let half = COLS as usize / 2;
    ui.drag(d + 4, 5, &[60, 90]);
    assert!(
        wait_for_rule(&mut ui, half),
        "the panel did not clamp at {half}: {:?}",
        rule_at(&ui)
    );

    // Pull left past the start: clamps at 18.
    ui.drag(half as u16 + 1, 5, &[20, 10]);
    assert!(
        wait_for_rule(&mut ui, 18),
        "the panel did not clamp at 18: {:?}",
        rule_at(&ui)
    );
}
