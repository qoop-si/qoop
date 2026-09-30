//! Painting a frame, as a pure function of what the daemon said.
//!
//! No terminal library. What has to be produced is a byte string of positioned writes, and
//! the alternative was a dependency whose own idea of a frame would then have to be
//! reconciled with the one the daemon already sends. Everything here is text in, text out,
//! so a test can read a frame instead of looking at one.

use qoro_runtime::control::{AgentRow, FocusEvent, WorkspaceRow};
use qoro_runtime::daemon::{ColourWire, SnapshotWire, SpanWire, attrs};

use crate::palette::{Filter, Palette};
use crate::{layout, sidebar, tabs, theme, width};

/// One visible terminal and where it sits in the pane, in pane-local cells.
///
/// Empty in every test that draws a single pane: an empty list means the legacy
/// path, exactly one terminal filling the pane.
pub struct Tile<'a> {
    pub terminal: u32,
    pub rect: layout::Rect,
    pub snapshot: Option<&'a SnapshotWire>,
    pub focused: bool,
}

/// What the screen shows: the tab bar across the top, the spaces and agents down the left,
/// one terminal filling the rest, and a line saying what the keys do.
pub struct Frame<'a> {
    /// Every terminal the daemon reports, across every space. The panel on the left is a
    /// list across spaces on purpose, so it takes the whole thing.
    pub rows: &'a [AgentRow],
    pub spaces: &'a [WorkspaceRow],
    /// The space whose tabs are on the bar. `None` before the daemon has answered.
    pub space: Option<i64>,
    /// Split panes hidden from the top tab bar. Sub-terminals inside their
    /// tab, still listed in the sidebar when an agent runs in them.
    pub panes: std::collections::BTreeSet<u32>,
    pub focused: u32,
    /// The focused terminal's last snapshot. `None` before the first one arrives, which is
    /// a real state: a terminal that has produced nothing yet has nothing to draw.
    pub snapshot: Option<&'a SnapshotWire>,
    pub cols: u16,
    pub lines: u16,
    /// The space whose rule is asking whether it should close, or `None` while no rule is.
    pub armed: Option<i64>,
    /// Shown instead of the key list, when there is something to say.
    pub message: Option<&'a str>,
    /// Something worth saying while nothing else is happening, said until the person does
    /// anything. Under everything urgent, and never in place of it.
    pub hint: Option<&'a str>,
    /// How the column orders its agents: the tree of spaces, or flat by urgency.
    pub view: sidebar::View,
    /// The done rows this client has already looked at.
    pub seen: &'a sidebar::Seen,
    /// Rows that finished while the keyboard was away.
    pub unseen: &'a sidebar::Unseen,
    /// The animation step the working spinner is on.
    ///
    /// Advanced by the run loop while an agent is working, held while none is: a still
    /// panel is a panel with nothing to say, and a ticking one is a repainting one.
    pub tick: u64,
    /// How wide the open left column paints this frame.
    ///
    /// Read once per frame like the clock: every width below derives from
    /// this, so one paint never disagrees with itself.
    pub sidebar_width: usize,
    /// Paint-time unix seconds, read once per frame by the run loop.
    ///
    /// What idle tiers and recency order are computed from: one paint never
    /// disagrees with itself, and hit testing re-reads the clock for the
    /// same reason.
    pub now_unix: i64,
    /// The open search palette, drawn over the terminal when present.
    pub palette: Option<&'a Palette>,
    /// The open naming box, drawn over everything when a person is being asked what to
    /// call the thing they just asked for.
    pub prompt: Option<&'a crate::run::Prompt>,
    /// The open menu: which of its rows is selected, and which list it is showing.
    pub menu: Option<(usize, &'a crate::run::MenuKind)>,
    /// Whether the left column is folded away to a strip.
    pub collapsed: bool,
    /// A read only overlay opened from the menu.
    pub reading: Option<crate::run::Reading>,
    /// What the mouse has selected in the pane, so it can be shown as selected.
    pub selection: Option<crate::selection::Selection>,
    /// How many of the pane's rows come from behind the screen.
    pub scroll: usize,
    /// The row the top of the pane is pinned to, when it is not at the bottom.
    pub anchor: u64,
    /// How far back from the newest row that pin is.
    ///
    /// Passed in rather than worked out here, because it is a distance to the newest row
    /// the terminal has written and the painter knows only what this client has fetched:
    /// computing it from that stopped counting the moment the terminal printed again.
    pub rows_back: u64,
    /// The rows behind the screen this client has been given.
    pub behind: &'a crate::run::Behind,
    /// A picture the focused terminal emitted that the outer terminal cannot draw,
    /// as one dim line. `None` when there is no picture or the picture is on screen
    /// as itself. Painted as an overlay, never as grid, so the snapshot contract
    /// (plain rows) holds for every gate and the whole corpus.
    pub image_notice: Option<&'a str>,
    /// The visible terminals and their rectangles, in pane-local cells. Empty
    /// means one terminal filling the pane, drawn from `snapshot` exactly as
    /// before, so a single pane paints pixel for pixel what it always has.
    pub tiles: Vec<Tile<'a>>,
    /// What this client alone decided about how things look, beside the daemon's rows.
    pub chrome: Chrome<'a>,
}

/// Client-local display state, borrowed into each frame like the daemon's rows.
///
/// Kept together so a frame gains one field when the client learns one more thing to
/// remember, rather than one more field at every place a frame is built.
#[derive(Clone, Copy, Debug)]
pub struct Chrome<'a> {
    /// The spaces folded away under their rule.
    pub shut: &'a sidebar::Shut,
    /// Where the keyboard's attention is, for the key row.
    pub keys: KeyContext,
    /// What each local agent costs, sampled while `QORO_USAGE=1`.
    pub usage: &'a sidebar::Usage,
    /// An agent's last answer is open over the pane, and the key row says how to read it.
    pub answer: bool,
    /// The sleeping tab the keyboard rests on while the space on screen is asleep, zero
    /// otherwise. The column raises it the way it raises the focused terminal.
    pub dim: u32,
    /// What is typed into the keys overlay's search, while one runs.
    pub keys_query: Option<&'a str>,
    /// The busy terminal a close is waiting on its capital `Y` for, asked in a box.
    pub closing: Option<u32>,
    /// A mode on the notice row: its name and its keys, empty when the mode has something
    /// else to say (copy mode's search), which `message` then carries.
    pub mode: Option<(&'static str, &'static [(&'static str, &'static str)])>,
}

/// Nothing folded and nothing sampled, for a frame built without a client behind it.
static NO_SHUT: sidebar::Shut = sidebar::Shut::new();
static NO_USAGE: sidebar::Usage = sidebar::Usage::new();

impl Default for Chrome<'_> {
    fn default() -> Self {
        Chrome {
            shut: &NO_SHUT,
            keys: KeyContext::default(),
            usage: &NO_USAGE,
            answer: false,
            dim: 0,
            keys_query: None,
            closing: None,
            mode: None,
        }
    }
}

impl Frame<'_> {
    /// A close waiting on its answer, when its box fits. Otherwise the question stays on
    /// the notice row, or the keys would be taken with nothing on screen saying why.
    fn confirm(&self) -> Option<u32> {
        self.chrome.closing.filter(|t| confirm_fits(self, *t))
    }

    /// Whether a box is open over the screen, which fades everything behind it.
    fn spotlight(&self) -> bool {
        self.prompt.is_some()
            || self.palette.is_some()
            || self.menu.is_some()
            || self.reading.is_some()
            || self.confirm().is_some()
    }

    /// The pieces the left column is drawn from, at this frame's clock and tick.
    pub fn column(&self) -> sidebar::Column<'_> {
        sidebar::Column {
            spaces: self.spaces,
            agents: self.rows,
            focused_space: self.space.unwrap_or_default(),
            focused_terminal: if self.focused != 0 {
                self.focused
            } else {
                self.chrome.dim
            },
            armed: self.armed,
            tick: self.tick,
            view: self.view,
            seen: self.seen,
            unseen: self.unseen,
            now_unix: self.now_unix,
            width: self.sidebar_width,
            shut: self.chrome.shut,
            usage: Some(self.chrome.usage),
        }
    }
}

/// Where the keyboard's attention is, for the key row to name what works there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyContext {
    /// Typing goes to the terminal; the prefix commands are what work.
    #[default]
    Pane,
    /// The last press landed in the left column: what the column answers to.
    Sidebar,
    /// `ctrl-a` is down and the next key is a command: every command there is.
    Prefix,
}

/// One entry of the key row: the key, what it does, and how soon it goes when the row
/// is short, where zero never goes. An entry with no action is a label, the prefix
/// named once before the letters it applies to.
type Hint = (&'static str, Option<&'static str>, u8);

/// What the key row offers in each place, most useful first within each rank.
fn all_hints(context: KeyContext) -> &'static [Hint] {
    match context {
        KeyContext::Pane => &[
            ("ctrl-a:", None, 0),
            ("space", Some("find"), 1),
            ("c", Some("new tab"), 2),
            ("n", Some("next"), 3),
            ("m", Some("menu"), 4),
            ("v", Some("split"), 5),
            ("f", Some("fold"), 6),
            ("F", Some("focus"), 6),
            ("[", Some("copy"), 5),
            ("?", Some("keys"), 2),
            ("d", Some("detach"), 0),
        ],
        KeyContext::Sidebar => &[
            ("click", Some("open"), 1),
            ("drag edge", Some("resize"), 7),
            ("ctrl-a:", None, 0),
            ("g", Some("tree/priority"), 3),
            ("w", Some("new space"), 2),
            ("R", Some("rename space"), 4),
            ("b", Some("hide column"), 5),
            ("d", Some("detach"), 0),
        ],
        KeyContext::Prefix => &[
            ("n", Some("next"), 1),
            ("p", Some("prev"), 3),
            ("o", Some("next pane"), 3),
            ("c", Some("new"), 1),
            ("x", Some("close"), 2),
            ("r", Some("rename"), 4),
            ("v", Some("split \u{2502}"), 4),
            ("-", Some("split \u{2500}"), 4),
            ("z", Some("zoom"), 5),
            ("[", Some("copy"), 5),
            ("e", Some("resize"), 6),
            ("=", Some("equalize"), 7),
            ("s", Some("space"), 3),
            ("w", Some("new space"), 5),
            ("R", Some("rename space"), 6),
            ("W", Some("close space"), 7),
            ("Z", Some("sleep"), 7),
            ("space", Some("find"), 1),
            ("f", Some("fold"), 5),
            ("F", Some("focus"), 5),
            ("g", Some("view"), 6),
            ("m", Some("menu"), 4),
            ("M", Some("last answer"), 6),
            ("?", Some("keys"), 2),
            ("U", Some("update"), 7),
            ("d", Some("detach"), 0),
        ],
    }
}

/// What the column can do right now, which decides which of its keys the row offers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Place {
    /// A space can fold: the tree is showing (open or folded to the strip).
    pub fold: bool,
    /// The column is folded away to its strip.
    pub collapsed: bool,
}

/// The entries that are true right now: no fold where nothing folds, no divider to drag on
/// the strip, and the strip's `b` shows the column rather than hiding it.
fn hints(context: KeyContext, place: Place) -> Vec<Hint> {
    all_hints(context)
        .iter()
        .filter(|h| place.fold || h.1 != Some("fold"))
        .filter(|h| !place.collapsed || h.0 != "drag edge")
        .map(|h| {
            if place.collapsed && h.1 == Some("hide column") {
                (h.0, Some("show column"), h.2)
            } else {
                *h
            }
        })
        .collect()
}
/// The columns one entry takes: `  key action`, or `  label` for the prefix. The first on
/// the row takes one space less, at the edge.
fn hint_width(hint: &Hint) -> usize {
    match hint.1 {
        Some(action) => 3 + width::width(hint.0) + width::width(action),
        None => 2 + width::width(hint.0),
    }
}

/// The entries that fit in `cols`: the highest rank goes first, the latest of a rank
/// before the earliest, and rank zero never goes, so the way out is always on the row.
fn fitted(context: KeyContext, place: Place, cols: usize) -> Vec<Hint> {
    let mut kept: Vec<Hint> = hints(context, place);
    while kept.iter().map(hint_width).sum::<usize>() > cols {
        let Some(worst) = kept
            .iter()
            .enumerate()
            .filter(|(_, h)| h.2 > 0)
            .max_by_key(|(at, h)| (h.2, *at))
            .map(|(at, _)| at)
        else {
            break;
        };
        kept.remove(worst);
    }
    // Still too wide with only what never goes: the prefix label gives way to the way out.
    if kept.iter().map(hint_width).sum::<usize>() > cols {
        kept.retain(|h| h.1.is_some());
    }
    kept
}

/// The key row, `key action` two spaces apart, in the status colours, exactly `cols` wide.
///
/// The key bright and the action quiet, as every box's footer says them.
fn key_row(context: KeyContext, place: Place, cols: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for hint in fitted(context, place, cols) {
        let lead = if used == 0 { " " } else { "  " };
        let wide = hint_width(&hint) + lead.len() - 2;
        if used + wide > cols {
            break;
        }
        used += wide;
        match hint.1 {
            None => {
                out.push_str(&theme::status_in(theme::Role::Faint));
                out.push_str(lead);
                out.push_str(hint.0);
            }
            Some(action) => {
                out.push_str(&theme::status());
                out.push_str(lead);
                out.push_str(&theme::status_key());
                out.push_str(hint.0);
                out.push_str(&theme::status());
                out.push(' ');
                out.push_str(action);
            }
        }
    }
    out.push_str(&theme::status());
    if used == 0 {
        // Narrower than one entry: the way out, plainly, cut to what fits.
        let plain = width::fit(" d detach", cols);
        used = width::width(&plain);
        out.push_str(&plain);
    }
    out.push_str(&" ".repeat(cols.saturating_sub(used)));
    out
}

/// Rows the tab bar takes, at the top of the pane.
pub const TABS: usize = 1;

/// The screen row the left column starts on.
///
/// One, the very top. The tab bar sits over the pane and not over the whole width: the
/// tabs belong to the space, the space is what the column on the left selects, and a bar
/// spanning both puts the thing being selected under the thing selecting it. It also cost
/// the column a row for no reason, since the bar has nothing to say about it.
pub const SIDEBAR_TOP: usize = 1;

/// Rows the status lines take, at the bottom: the notice, then the keys.
pub const STATUS: usize = 2;

/// The columns and rows left for the terminal once the tab bar, the left column and the
/// status line have taken theirs.
///
/// Public because it is also what the client tells the daemon to size the child to, and a
/// second copy of that arithmetic in the caller is how a pane and its child end up
/// disagreeing about how wide they are.
pub fn pane_size(cols: u16, lines: u16) -> (u16, u16) {
    pane_size_for(cols, lines, false, 0, sidebar::WIDTH)
}

/// The same, for a column that may be folded away to a strip and pulled to
/// a width.
pub fn pane_size_for(
    cols: u16,
    lines: u16,
    collapsed: bool,
    right: usize,
    sidebar_width: usize,
) -> (u16, u16) {
    (
        cols.saturating_sub(left_for(cols, collapsed, sidebar_width) as u16)
            .saturating_sub(right as u16)
            .max(1),
        lines.saturating_sub((TABS + STATUS) as u16).max(1),
    )
}

/// The columns spent before the pane starts: the left column and the rule beside it, or
/// nothing at all when the window is too narrow to hold both.
///
/// Dropped rather than squeezed. The column is as wide as the person left
/// it plus the rule beside it, so a window of twenty three columns or fewer
/// had every row painted wider than the terminal: each line wrapped, the
/// screen slid up, and every position this module writes addressed the wrong
/// row, which also puts every click on the wrong thing. Measured at 10,
/// 20 and 23 columns, all of which painted 24. A narrow window is a real one:
/// two panes side by side in a tmux split is how it happens.
pub fn left(cols: u16) -> usize {
    left_for(cols, false, sidebar::WIDTH)
}

/// The same, for a column that may be folded away to a strip, at a pulled
/// width.
///
/// Folded, it keeps the one thing a glance needs: a state mark and a number per row. The
/// names come back with the column, and the terminal gets twenty six columns back in the
/// meantime.
pub fn left_for(cols: u16, collapsed: bool, sidebar_width: usize) -> usize {
    let width = if collapsed {
        sidebar::COLLAPSED
    } else {
        sidebar_width
    };
    // One column for the pane, or there is nothing to show and the left column is the whole
    // point of nothing.
    if (cols as usize) > width + 2 {
        width + 1
    } else {
        0
    }
}

/// The terminals of one space, in the order the daemon reports them.
///
/// The tab bar is per space and the panel on the left is not, so this filter lives here
/// rather than in either of them: both a click and a repaint have to agree on which
/// terminals the bar is carrying, and two copies of the filter is how they stop agreeing.
///
/// A `None` space is every terminal, which is the honest answer before the daemon has said
/// what spaces exist: showing nothing at that moment would read as a daemon with no
/// terminals in it.
pub fn in_space(rows: &[AgentRow], space: Option<i64>) -> Vec<AgentRow> {
    rows.iter()
        .filter(|r| space.is_none_or(|s| r.workspace == s))
        .cloned()
        .collect()
}

/// The escape sequence that puts the cursor at a one-based row and column.
pub(crate) fn at(row: usize, col: usize) -> String {
    format!("\x1b[{row};{col}H")
}

/// The picked pet stamped at the right of the column's foot, as raw output.
///
/// Pure so tests stamp the same bytes the paint does: same fits gate, same
/// rows, same escapes. Empty whenever the pet does not fit, the column is
/// folded, or colour is off.
pub(crate) fn avatar_overlay_paint(
    column_rows: usize,
    sidebar_width: usize,
    collapsed: bool,
    wide_enough: bool,
    avatar: Option<&qoro_runtime::avatar::Avatar>,
    colour: bool,
) -> String {
    let Some(pet) = avatar else {
        return String::new();
    };
    if !wide_enough || collapsed {
        return String::new();
    }
    if !sidebar::avatar_fits(column_rows, sidebar_width, true) {
        return String::new();
    }
    let (cols, art) = sidebar::avatar_overlay(pet, colour);
    let rows = sidebar::avatar_overlay_rows(SIDEBAR_TOP, column_rows);
    // At the right of the foot, one cell of margin before the column's edge.
    let col = sidebar_width.saturating_sub(cols);
    let mut out = String::new();
    for (i, row) in art.iter().enumerate() {
        out.push_str(&at(rows[i], col));
        out.push_str(row);
    }
    out
}

/// Where a real picture's top-left cell goes: the focused pane's first row, one based.
/// Pictures draw downward, so anchored anywhere lower they cover the status rows.
pub(crate) fn picture_cell(frame: &Frame) -> Option<(usize, usize)> {
    // A window too short to hold a pane has nowhere to put one: placed anyway, it lands
    // on the status rows.
    if (frame.lines as usize) <= TABS + STATUS {
        return None;
    }
    let left = left_for(frame.cols, frame.collapsed, frame.sidebar_width);
    if frame.tiles.len() < 2 {
        return Some((TABS + 1, left + 1));
    }
    let t = frame.tiles.iter().find(|t| t.focused)?;
    Some((TABS + 1 + t.rect.y, left + 1 + t.rect.x))
}

/// Where the host cursor goes for the focused snapshot, one-based, or `None`
/// when no tile carries it.
///
/// The cursor lives on the focused tile alone: unfocused tiles never carry
/// one, so a glance always knows where typing goes. With fewer than two tiles
/// the legacy arithmetic is byte for byte what it always was. One place, so
/// the cursor-only fast path cannot drift from the full repaint.
pub(crate) fn pane_cursor(frame: &Frame, snapshot: &SnapshotWire) -> Option<(usize, usize)> {
    let (pane_cols, pane_rows) = pane_size_for(
        frame.cols,
        frame.lines,
        frame.collapsed,
        0,
        frame.sidebar_width,
    );
    if frame.tiles.len() < 2 {
        let (row, cell) = snapshot.cursor;
        let col = cursor_column(
            snapshot.lines.get(row).map(String::as_str).unwrap_or(""),
            cell,
        );
        let row = row.min(pane_rows as usize - 1) + 1 + TABS;
        let col = col.min(pane_cols as usize - 1)
            + left_for(frame.cols, frame.collapsed, frame.sidebar_width)
            + 1;
        Some((row, col))
    } else {
        let tile = frame.tiles.iter().find(|t| t.focused)?;
        let (row, cell) = snapshot.cursor;
        let col = cursor_column(
            snapshot.lines.get(row).map(String::as_str).unwrap_or(""),
            cell,
        );
        let pane_left = left_for(frame.cols, frame.collapsed, frame.sidebar_width);
        let row = tile.rect.y + row.min(tile.rect.rows.saturating_sub(1)) + 1 + TABS;
        let col = pane_left + tile.rect.x + col.min(tile.rect.cols.saturating_sub(1)) + 1;
        Some((row, col))
    }
}

/// Paint the whole screen.
///
/// Whole, not incremental, so a frame is a pure function a test can read. What actually
/// reaches the terminal is only the rows that changed: see [`crate::present`].
pub fn paint(frame: &Frame) -> String {
    let left = left_for(frame.cols, frame.collapsed, frame.sidebar_width);
    let right = 0;
    let (pane_cols, pane_rows) = pane_size_for(
        frame.cols,
        frame.lines,
        frame.collapsed,
        right,
        frame.sidebar_width,
    );
    let mut out = String::with_capacity(frame.cols as usize * frame.lines as usize + 256);

    // Hidden while painting: a cursor walking the screen during a repaint is the flicker
    // people notice most.
    out.push_str("\x1b[?25l");

    let wide_enough = left > 0;

    // Any box open fades what is behind it to one grey, so the box is what gets read.
    let spotlight = frame.spotlight();
    let faded = theme::fg(theme::Role::Unobserved);

    // The tab bar, over the pane and only over the pane. It starts where the pane starts,
    // past the column and its rule. Split panes are sub-terminals, never top tabs.
    out.push_str(&at(1, left + 1));
    let tabs: Vec<AgentRow> = in_space(frame.rows, frame.space)
        .into_iter()
        .filter(|r| !frame.panes.contains(&r.terminal))
        .collect();
    for (piece, style) in tabs::painted(&tabs, frame.focused, pane_cols as usize, frame.unseen) {
        out.push_str(&match style {
            _ if spotlight => faded.clone(),
            tabs::Ink::Focused => theme::tab_on(),
            // The bar shows one space's terminals, so one space colour lands on
            // all of them: identity, not confetti. Focus keeps the pink pill.
            tabs::Ink::Other => theme::tab_in(theme::space_of(frame.space.unwrap_or_default())),
            tabs::Ink::Quiet => theme::fg(theme::Role::Faint),
        });
        out.push_str(&piece);
    }
    out.push_str(theme::RESET);

    // The column runs the whole height, from the top row to the status line. It is taller
    // than the pane by the tab bar's row, which is the row the bar no longer takes from it.
    let column_rows = (frame.lines as usize).saturating_sub(STATUS);
    // The pet stands on the foot of the column. Read once per frame so the
    // reservation below and the overlay further down paint the same pet.
    let avatar = sidebar::current_avatar();
    let column = frame.column();
    let panel: Vec<(sidebar::Line, Option<sidebar::Hit>)> = if wide_enough && frame.collapsed {
        sidebar::rows_collapsed(&column, column_rows)
    } else if wide_enough {
        sidebar::rows_folded(&column, column_rows, avatar.as_ref())
    } else {
        Vec::new()
    };
    // The lines around split panes, worked out once for the whole frame.
    let edges = if frame.tiles.len() < 2 {
        Vec::new()
    } else {
        pane_edges(
            &frame.tiles,
            frame.rows,
            pane_cols as usize,
            pane_rows as usize,
        )
    };
    // The space of the focused tab when that tab runs no agent, and so draws no row.
    let shell_space = frame
        .rows
        .iter()
        .find(|r| r.terminal == column.focused_terminal && !r.agent)
        .map(|r| r.workspace);
    for row in 0..column_rows {
        out.push_str(&at(row + SIDEBAR_TOP, 1));
        // The state's colour, on the whole row. A person scanning this column is looking
        // for the one row that needs them, and a coloured word inside an otherwise grey
        // line is a word they have to find first.
        //
        // The keyboard's own terminal rows carry the raised background, which is
        // what the designs draw as a selected background. No marker character:
        // the rows are text and the background alone says where the
        // keyboard is. A space rule never takes it: the focused space already
        // reads as the tab bar's colour and the raised rows sitting inside its
        // box, so raising the rule too paints one selection straddling the box
        // edge.
        if let Some((line, _)) = panel.get(row).filter(|_| spotlight) {
            out.push_str(&faded);
            out.push_str(&line.text);
            out.push_str(theme::RESET);
        } else if let Some((line, hit)) = panel.get(row) {
            let selected = match hit {
                // A tab with no agent has no row in the column, so the keyboard on one is
                // shown on its space's rule instead: the column always says where it is.
                Some(sidebar::Hit::Space(space)) => shell_space == Some(*space),
                Some(sidebar::Hit::Terminal(terminal)) => *terminal == column.focused_terminal,
                Some(sidebar::Hit::Title | sidebar::Hit::Foot | sidebar::Hit::Next(_)) | None => {
                    false
                }
            };
            // The next row sits on the status bar's band: the one container on the column,
            // so it reads as the place to look, not as another row of the list.
            let band = matches!(hit, Some(sidebar::Hit::Next(_)));
            let base = match (selected, band, line.colour) {
                (true, _, Some(role)) => theme::selected_in(role),
                (true, _, None) => theme::selected(),
                (false, true, Some(role)) => theme::status_in(role),
                (false, true, None) => theme::status(),
                (false, false, Some(role)) => theme::fg(role),
                (false, false, None) => theme::fg(theme::Role::Text),
            };
            out.push_str(&base);
            // The coloured runs inside the row: the space's name, the opening sign, and
            // whatever else the row carries (rule dashes, a count, a quiet worktree).
            // Painted by cutting the row at their edges rather than by drawing over it, so
            // the row is still written once and left to right, and a wide character in a
            // name cannot be split. Runs never overlap by construction; ordered by column,
            // the paint below reads as one pass over the text.
            let mut runs: Vec<(usize, usize, theme::Role)> = line.runs.clone();
            if let Some(run) = line.accent {
                runs.push(run);
            }
            if let Some(run) = line.indicator {
                runs.push(run);
            }
            runs.retain(|(at, len, _)| *len > 0 && at + len <= width::width(&line.text));
            runs.sort_by_key(|(at, _, _)| *at);
            if runs.is_empty() {
                out.push_str(&line.text);
            } else {
                let cut = |from: usize, to: usize| {
                    line.text
                        .chars()
                        .scan(0usize, |col, c| {
                            let start = *col;
                            *col += width::columns(c);
                            Some((start, c))
                        })
                        .filter(|(start, _)| *start >= from && *start < to)
                        .map(|(_, c)| c)
                        .collect::<String>()
                };
                let escape = |role: theme::Role| {
                    if selected {
                        theme::selected_in(role)
                    } else if band {
                        theme::status_in(role)
                    } else {
                        theme::fg(role)
                    }
                };
                let mut pos = 0;
                for (at, len, role) in runs {
                    // A run that starts inside the previous one is a caller bug, and the
                    // honest answer is to skip it rather than paint backwards.
                    if at < pos {
                        continue;
                    }
                    out.push_str(&cut(pos, at));
                    out.push_str(&escape(role));
                    out.push_str(&cut(at, at + len));
                    out.push_str(&base);
                    pos = at + len;
                }
                out.push_str(&cut(pos, width::width(&line.text)));
            }
            out.push_str(theme::RESET);
        }
        // The rule between the column and the terminal, drawn every row so that a terminal
        // line long enough to reach it cannot swallow it.
        if wide_enough {
            out.push_str(&theme::fg(theme::Role::Rule));
            // The panel separator runs into this column, so the two meet in a join rather
            // than a horizontal line stopping one column short of a vertical one.
            let joins = panel
                .get(row)
                .is_some_and(|(line, _)| line.colour == Some(theme::Role::Rule));
            out.push(if joins { '\u{2524}' } else { '\u{2502}' });
            out.push_str(theme::RESET);
        }
        // The pane starts under the tab bar, so the column's first row has the bar beside
        // it and every row after that has a row of the terminal.
        let Some(pane_row) = row.checked_sub(TABS) else {
            continue;
        };
        if pane_row >= pane_rows as usize {
            continue;
        }
        // Scrolled back, the window is the tail of what has gone past followed by the top
        // of the live screen, so a person reading upwards sees one continuous terminal
        // rather than two. The rows behind it are text alone: what was written to disk is
        // rows, and colour there would be a second format nobody reads back.
        // Tiled rows only when there is more than one tile to show: a single
        // tile paints the legacy path, so one terminal paints pixel for pixel
        // what it always has.
        if frame.tiles.len() < 2 {
            let (line, spans) = window_row(
                frame.snapshot,
                frame.behind,
                frame.scroll,
                frame.anchor,
                pane_row,
            );
            let selected = frame
                .selection
                .and_then(|s| s.columns_on(pane_row, pane_cols as usize));
            out.push_str(&pane_row_paint(
                frame,
                line,
                spans,
                links_of(frame.snapshot),
                pane_cols as usize,
                selected,
            ));
        } else {
            paint_tiled_row(frame, &edges, pane_row, pane_cols as usize, left, &mut out);
        }
    }

    // Two lines, not one. The keys never change and the notice always does, and one row
    // carrying both meant the key list vanished exactly when something went wrong: a person
    // who has just been told a terminal is blocked is the same person who then cannot see
    // which key gets them there. They also read differently, one being a reference and the
    // other an event, and a row that is sometimes one and sometimes the other is a row
    // nobody learns the shape of.
    //
    // Both rows always exist, whether or not there is anything to say, because the pane is
    // a real pty: a row that came and went would resize the child and reflow its output
    // every time an agent changed state.
    out.push_str(&at(frame.lines as usize - 1, 1));
    // A close waiting on its answer asks in a box, and the notice row goes back to its
    // news.
    let confirm = frame.confirm();
    let message = if confirm.is_some() {
        None
    } else {
        frame.message
    };
    // A transient message wins, then the single next thing to do, then where the keyboard
    // is. Never empty: a blank row is a row that has to be explained.
    let (notice, notice_role) = match (
        message,
        triage(
            frame.rows,
            frame.spaces,
            frame.focused,
            frame.seen,
            frame.unseen,
        ),
    ) {
        (Some(message), _) => (
            message.to_string(),
            waiting(frame.rows, frame.focused, frame.seen, frame.unseen),
        ),
        // Being somewhere other than the bottom wins over what needs a person, because it
        // is the one thing on this row that explains what the pane is showing: a screen
        // that is not moving while an agent works looks broken until it says why.
        _ if frame.scroll > 0 => {
            // How far back the top of the pane is, which is not how much of the pane is
            // history: scrolled far enough, the whole pane is history and the distance
            // keeps growing behind it.
            let back = frame.rows_back.max(1);
            (
                format!(
                    "{back} {} back, type to come down",
                    if back == 1 { "row" } else { "rows" }
                ),
                None,
            )
        }
        (None, Some(next)) => (
            next,
            waiting(frame.rows, frame.focused, frame.seen, frame.unseen),
        ),
        (None, None) => match frame.hint {
            Some(hint) => (hint.to_string(), None),
            None => (here(frame), None),
        },
    };
    // `mode` is never set while a close is asked, so the two never meet.
    match frame.chrome.mode {
        Some((word, keys)) => {
            out.push_str(&mode_row(word, keys, &notice, frame.cols as usize));
        }
        None => {
            out.push_str(&match notice_role {
                Some(role) => theme::status_in(role),
                None => theme::status(),
            });
            out.push_str(&width::fit(&notice, frame.cols as usize));
        }
    }
    out.push_str(theme::RESET);

    out.push_str(&at(frame.lines as usize, 1));
    // Raised, not reverse. A full width bar of reverse video is the brightest thing on the
    // screen, and the key list is the least urgent thing on it.
    out.push_str(&theme::status());
    // A box says its own keys in its footer, so under one the row is quiet. Otherwise a
    // mode names its own keys, because in a mode the others do not work.
    let mode = if spotlight {
        Some("")
    } else if frame.chrome.answer {
        Some("j/k scroll  space page  g/G top/bottom  q close")
    } else {
        None
    };
    // Otherwise the keys of the place the keyboard is: the pane, the column after a click
    // in it, or every command while `ctrl-a` waits for its letter. A short window drops
    // the least used first and never the way out.
    match mode {
        // Written with ctrl-a and named on the way out, like the keys box.
        Some(keys) => out.push_str(&width::fit(&crate::keys::named(keys), frame.cols as usize)),
        None => out.push_str(&crate::keys::named(&key_row(
            frame.chrome.keys,
            Place {
                fold: frame.view == sidebar::View::Tree,
                collapsed: frame.collapsed,
            },
            frame.cols as usize,
        ))),
    }
    out.push_str(theme::RESET);

    // A picture the outer terminal cannot draw, named on the focused pane's last
    // row. Before the floating overlays, so an open palette covers it the way it
    // covers terminal text.
    if let Some(notice) = frame.image_notice {
        let (tile_x, tile_y, tile_cols, tile_rows) = if frame.tiles.len() < 2 {
            (0, 0, pane_cols as usize, pane_rows as usize)
        } else if let Some(tile) = frame.tiles.iter().find(|t| t.focused) {
            (tile.rect.x, tile.rect.y, tile.rect.cols, tile.rect.rows)
        } else {
            (0, 0, 0, 0)
        };
        if tile_cols > 0 && tile_rows > 0 {
            out.push_str(&at(TABS + tile_y + tile_rows, left + 1 + tile_x));
            out.push_str(&theme::fg(theme::Role::Faint));
            out.push_str(&width::fit(notice, tile_cols));
            out.push_str(theme::RESET);
        }
    }

    // The picked pet, as itself, at the right of the foot's first rows. An
    // overlay like the picture notice: never grid, so the snapshot contract
    // (plain rows) holds for every gate and the whole corpus. Before the
    // floating overlays, so an open palette covers it the way it covers
    // terminal text.
    // Only where the column gave the pet its rows: the list comes first.
    if wide_enough && !frame.collapsed && sidebar::pet_shown(&column, column_rows, avatar.is_some())
    {
        out.push_str(&avatar_overlay_paint(
            column_rows,
            frame.sidebar_width,
            frame.collapsed,
            wide_enough,
            avatar.as_ref(),
            crate::colorlevel::level() != crate::colorlevel::Level::Mono && !spotlight,
        ));
    }

    // The palette floats over the terminal, so it is painted last: every row it takes
    // overwrites what the pane and the column drew under it.
    if let Some(palette) = frame.palette {
        paint_palette(frame, palette, &mut out);
    }
    if let Some(prompt) = frame.prompt {
        paint_prompt(frame, prompt, &mut out);
    }
    if let Some((selected, kind)) = frame.menu {
        paint_menu(frame, selected, kind, &mut out);
    }
    let reading_cursor = frame
        .reading
        .and_then(|reading| paint_reading(frame, reading, &mut out));
    if let Some(terminal) = confirm {
        paint_confirm(frame, terminal, &mut out);
    }

    // The cursor goes where the child put it, offset into the pane, so that what the user
    // types appears where they are looking. With the palette open it sits at the end of
    // the query instead, because that is where typing goes.
    if let Some(prompt) = frame.prompt {
        // At the end of what has been typed, as much of it as the field shows, or at the
        // start of an empty field before the faint name it keeps.
        if let Some(b) = PromptBox::at(frame.cols, frame.lines) {
            let shown = width::width(&field_tail(&prompt.text, b.span() - 1));
            out.push_str(&at(b.text_row() + 1, b.field_col() + shown + 1));
        }
    } else if let Some(palette) = frame.palette {
        // At the end of the query, inside the box, wherever the box ended up.
        let w = palette_width(frame.cols);
        let left = (frame.cols as usize).saturating_sub(w) / 2;
        let rows = palette
            .items(
                frame.rows,
                frame.spaces,
                frame.focused,
                frame.seen,
                frame.unseen,
            )
            .len()
            .max(1);
        let row = palette_query_row(frame.lines, rows + count_headers(frame, palette));
        // Past the border and the prompt marker, at the end of what has been typed, and
        // never on the filters beside it.
        let inner = w.saturating_sub(2);
        let chips = chips_width(&palette_chips(palette.filter, &palette.query, inner));
        let col = (left + 4 + width::width(&palette.query))
            .min(left + 3 + inner.saturating_sub(3 + chips).max(1));
        out.push_str(&at(row + 1, col + 1));
    } else if let Some((row, col)) = reading_cursor {
        // At the end of the keys search, where typing goes.
        out.push_str(&at(row + 1, col + 1));
    } else if confirm.is_none()
        && let Some(snapshot) = frame.snapshot
    {
        // Shown only when the program wants it seen. A child redraws with its
        // cursor hidden, and a shown-anyway cursor is one the program hid:
        // flicker. The frame starts hidden, so a hidden snapshot cursor stays
        // hidden with nothing more said.
        if snapshot.modes.cursor_visible
            && let Some((row, col)) = pane_cursor(frame, snapshot)
        {
            out.push_str(&at(row, col));
        }
    }
    // Back on for our own placements (prompt, palette) and for a shown
    // snapshot cursor. A hidden snapshot cursor stays off, and a terminal that
    // has produced nothing yet keeps the visible default it always had.
    let show = frame.prompt.is_some()
        || frame.palette.is_some()
        || reading_cursor.is_some()
        || frame.snapshot.is_none_or(|s| s.modes.cursor_visible);
    if show {
        out.push_str("\x1b[?25h");
    }
    out
}

/// The single next thing to do, when something is waiting: the first of the next row's
/// queue, so the status line, the next row and `ctrl-a tab` name the same agent.
///
/// Blocked first, then failed, then finished-while-away: the first of each in
/// daemon order, never the focused one. `None` when nothing waits, and then the
/// status line carries the key list instead, which is how a person finds out they
/// are done.
fn triage(
    rows: &[AgentRow],
    spaces: &[WorkspaceRow],
    focused: u32,
    seen: &sidebar::Seen,
    unseen: &sidebar::Unseen,
) -> Option<String> {
    let row = *sidebar::queue(rows, focused, seen, unseen).first()?;
    let space = spaces
        .iter()
        .find(|s| s.id == row.workspace)
        .map(|s| s.name.clone())
        .unwrap_or_else(|| row.workspace.to_string());
    // A derived idle-after-work is news, not calm: name it finished work
    // rather than repeating the daemon's idle word.
    let word = if sidebar::is_new(row, focused, seen, unseen) {
        "done"
    } else {
        row.state.as_str()
    };
    Some(crate::keys::named(&format!(
        "next: {space}/{} is {word}  ctrl-a tab",
        palette_label(row),
    )))
}

/// Where the keyboard is, for the notice row when nothing is waiting.
///
/// Said rather than left blank. The row exists whether or not there is news, so the calm
/// case gets the one thing worth restating: which space, which tab, and what it is doing.
fn here(frame: &Frame) -> String {
    let space = frame
        .spaces
        .iter()
        .find(|s| Some(s.id) == frame.space)
        .map(|s| s.name.clone())
        .unwrap_or_default();
    match frame.rows.iter().find(|r| r.terminal == frame.focused) {
        Some(row) => {
            let state = if row.state == "unknown" {
                String::new()
            } else if row.state == "sleeping" {
                // A slept tab has no screen and takes no keys: name the way
                // out, or the blank pane reads as a hang.
                crate::keys::named(", sleeping  ctrl-a x dismiss")
            } else {
                format!(", {}", row.state)
            };
            format!("{space} / {}{state}", palette_label(row))
        }
        None => format!("{space} / nothing open"),
    }
}

/// The state of the thing the status line is about to name, when something waits.
///
/// The same queue [`triage`] reads, so the colour and the words are about one terminal.
fn waiting(
    rows: &[AgentRow],
    focused: u32,
    seen: &sidebar::Seen,
    unseen: &sidebar::Unseen,
) -> Option<theme::Role> {
    let queue = sidebar::queue(rows, focused, seen, unseen);
    sidebar::urgency_ink(queue, focused, seen, unseen)
}

/// What a terminal is called outside the tab bar: its program's last segment, the
/// wrapper standing in the way, or its number when there is nothing else.
///
/// The tab bar, the sidebar and the palette each spell this in their own module;
/// this one is the status line's, kept beside its only caller.
fn palette_label(row: &AgentRow) -> String {
    match (&row.program, &row.behind) {
        (Some(program), _) => program
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or(program)
            .to_string(),
        (None, Some(wrapper)) => format!("behind {wrapper}"),
        (None, None) => format!("term {}", row.terminal),
    }
}

/// How wide the palette box is: most of the window on a narrow one, a centred
/// card on a wide one.
fn palette_width(cols: u16) -> usize {
    let cols = cols as usize;
    cols.min(60).max(cols.min(20))
}

/// The open palette, painted over the pane and the column.
///
/// One query line, then section headers and rows, scrolled to keep the selection
/// on screen. Every line is padded to the box width so nothing underneath shows
/// through. The selected row is reverse video rather than bracketed, like the
/// status line.
fn paint_palette(frame: &Frame, palette: &Palette, out: &mut String) {
    let cols = frame.cols as usize;
    let lines = frame.lines as usize;
    if cols < 10 || lines < TABS + STATUS + 2 {
        return;
    }
    let entries = palette.items(
        frame.rows,
        frame.spaces,
        frame.focused,
        frame.seen,
        frame.unseen,
    );
    let w = palette_width(frame.cols);
    let left = cols.saturating_sub(w) / 2;
    // What each row is, so the painter can put the same colours on it that the column puts
    // on the same things: a header, a row and which entry it came from.
    enum Kind<'a> {
        Header(&'a str),
        Row(&'a crate::palette::Entry),
        Empty,
    }
    let mut display: Vec<(Kind<'_>, bool)> = Vec::new();
    let mut section = "";
    for (at, entry) in entries.iter().enumerate() {
        if entry.section != section {
            section = entry.section;
            display.push((Kind::Header(section), false));
        }
        display.push((Kind::Row(entry), at == palette.selected));
    }
    if display.is_empty() {
        display.push((Kind::Empty, false));
    }
    // The edges, the query line and the footer, and the entries take what is left.
    let chrome = PALETTE_CHROME;
    let budget = (lines.saturating_sub(TABS + STATUS + chrome))
        .max(1)
        .min(display.len());
    let first = if display.len() <= budget {
        0
    } else {
        let selected_at = display
            .iter()
            .position(|(_, selected)| *selected)
            .unwrap_or(0);
        selected_at
            .saturating_sub(budget.saturating_sub(1))
            .min(display.len() - budget)
    };
    // Centred down the screen as well as across it. Pinned near the top it sat in the
    // corner of a window three times its height, which reads as something that failed to
    // open rather than something waiting to be typed into.
    let height = budget + chrome;
    let top = (lines.saturating_sub(height)) / 2;
    let inner = w.saturating_sub(2);

    // The query, with the prompt in the colour the pane's own prompts are not, so the box
    // says where typing goes. The filters sit at its right, the one in force bright.
    let chips = palette_chips(palette.filter, &palette.query, inner);
    let mut query = format!(
        "{} > {}{}",
        theme::fg(theme::Role::Brand),
        theme::fg(theme::Role::Focused),
        width::fit(
            &palette.query,
            inner.saturating_sub(3 + chips_width(&chips))
        )
    );
    for (i, chip) in chips.iter().enumerate() {
        query.push_str(&theme::fg(if *chip == palette.filter {
            theme::Role::Focused
        } else {
            theme::Role::Faint
        }));
        query.push_str(chip.name());
        query.push_str(if i + 1 == chips.len() { " " } else { "  " });
    }
    let mut body = vec![query];
    for (kind, selected) in display.iter().skip(first).take(budget) {
        let mut line = String::new();
        match kind {
            Kind::Header(name) => {
                line.push_str(&theme::fg(theme::Role::Title));
                line.push_str(&width::fit(&format!(" {name}"), inner));
            }
            Kind::Empty => {
                line.push_str(&theme::fg(theme::Role::Faint));
                line.push_str(&width::fit(" no match", inner));
            }
            Kind::Row(entry) => {
                // The same three colours the column uses, for the same three things: the
                // space in its own, the state in its own, and everything else plain. A row
                // that arrives here grey is a row a person has to read to place.
                let base = if *selected {
                    theme::selected()
                } else {
                    theme::fg(theme::Role::Text)
                };
                let state_ink = entry.state.as_deref().map(theme::of);
                let space_ink = entry.space.map(theme::space_of);
                // Cut to the box: on the narrowest windows a state word alone is wider
                // than the room beside its two spaces, and the row ran off the screen.
                let sub = width::text(
                    &entry.sub,
                    width::width(&entry.sub).min(inner.saturating_sub(2)),
                );
                let head = entry.text.clone();
                let room = inner.saturating_sub(width::width(&sub) + 2);
                // Composed text, so every space in it is meant: the grid fitter would drop
                // the one after a wide character.
                let head = width::text(&head, room);
                line.push_str(&base);
                line.push(' ');
                match (space_ink, entry.space_len) {
                    (Some(ink), len) if len > 0 && len <= width::width(&head) => {
                        line.push_str(&if *selected {
                            theme::selected_in(ink)
                        } else {
                            theme::fg(ink)
                        });
                        // Split where the columns reach `len`, not after `len` characters:
                        // counted in characters, a wide space name lost text and pulled
                        // the right border two columns in.
                        let mut cols = 0;
                        let cut = head
                            .char_indices()
                            .find_map(|(i, c)| {
                                let here = (cols >= len).then_some(i);
                                cols += width::columns(c);
                                here
                            })
                            .unwrap_or(head.len());
                        let (lead, tail) = head.split_at(cut);
                        line.push_str(lead);
                        line.push_str(&base);
                        line.push_str(tail);
                    }
                    _ => line.push_str(&head),
                }
                line.push(' ');
                line.push_str(&match (state_ink, *selected) {
                    (Some(ink), true) => theme::selected_in(ink),
                    (Some(ink), false) => theme::fg(ink),
                    (None, true) => theme::selected(),
                    (None, false) => theme::fg(theme::Role::Faint),
                });
                line.push_str(&sub);
                line.push_str(&base);
            }
        }
        body.push(line);
    }
    body.push(hint_line(PALETTE_HINT, inner));
    // The frame, so the box has edges: a floating list with no border is a list that has
    // fallen into whatever is behind it.
    let (rule, title) = spot_inks();
    paint_boxed(out, lines, (left, top, w), "find", (&rule, &title), &body);
}

/// The palette's rows that are not entries: both edges, the query line and the footer.
const PALETTE_CHROME: usize = 4;

/// The palette's keys. Tab is the one nobody would guess, so it is said here.
const PALETTE_HINT: &[(&str, &str)] = &[("enter", "go"), ("tab", "filter"), ("esc", "close")];

/// Which filters the query row can name beside a query of this width.
///
/// All of them when they fit. Otherwise the query wins, but the filter in force stays
/// named, or the list would shrink with nothing on screen saying why; under `all` there
/// is nothing to explain, so nothing is shown.
fn palette_chips(filter: Filter, query: &str, inner: usize) -> Vec<Filter> {
    let all = Filter::ORDER.to_vec();
    if 3 + width::width(query) + 1 + chips_width(&all) <= inner {
        all
    } else if filter != Filter::All && 3 + 1 + chips_width(&[filter]) <= inner {
        vec![filter]
    } else {
        Vec::new()
    }
}

/// The columns the named filters take: two spaces between them and one after the last.
fn chips_width(chips: &[Filter]) -> usize {
    if chips.is_empty() {
        return 0;
    }
    chips.iter().map(|c| c.name().len()).sum::<usize>() + 2 * (chips.len() - 1) + 1
}

/// How many section headers the palette will draw, which take rows like anything else.
fn count_headers(frame: &Frame, palette: &Palette) -> usize {
    let entries = palette.items(
        frame.rows,
        frame.spaces,
        frame.focused,
        frame.seen,
        frame.unseen,
    );
    let mut sections = 0;
    let mut section = "";
    for entry in &entries {
        if entry.section != section {
            section = entry.section;
            sections += 1;
        }
    }
    sections
}

/// Where the palette's query line is drawn, so the cursor can be put at the end of it.
fn palette_query_row(lines: u16, rows: usize) -> usize {
    let lines = lines as usize;
    let budget = (lines.saturating_sub(TABS + STATUS + PALETTE_CHROME))
        .max(1)
        .min(rows);
    (lines.saturating_sub(budget + PALETTE_CHROME)) / 2 + 1
}

/// The screen column the cursor sits at, counted from zero, given the row it is on and the
/// grid cell it is in.
///
/// Two conversions, and the old arithmetic got both of them backwards.
///
/// A snapshot row arrives right trimmed, so a cursor to the right of the last printed
/// character is in a cell the string does not contain. Taking that many characters returned
/// fewer, and the cursor landed left of where the person was typing: measured against a
/// `sh` prompt, which prints `"$ "` and arrives as `"$"`, the cursor sat on top of the `$`
/// on every line anybody typed. The cells past the end are spaces the engine dropped, and
/// each of them is one column.
///
/// The other way, the grid's spacer after a wide character is a cell and not a column, so
/// counting it moved the cursor one column right for every wide character on the row.
fn cursor_column(line: &str, cell: usize) -> usize {
    let mut cells = 0;
    let mut cols = 0;
    let mut spacer = false;
    for c in line.chars() {
        if cells == cell {
            break;
        }
        cells += 1;
        if spacer && c == ' ' {
            spacer = false;
            continue;
        }
        spacer = width::columns(c) == 2;
        cols += width::columns(c);
    }
    cols + (cell - cells)
}

/// Where the naming box sits and how its rows fall. Shared by the painter, the cursor and
/// the mouse, so a click and a repaint agree on where each button is.
pub(crate) struct PromptBox {
    /// Zero based screen column of the left edge and row of the top edge, and the width.
    left: usize,
    top: usize,
    w: usize,
    /// With its blank rows and the label over the field, or squeezed into a short window
    /// without them.
    roomy: bool,
}

/// Both edges and eight rows: blank, label, field, underline, note, blank, buttons, blank.
const PROMPT_ROOMY: usize = 10;
/// Both edges and the three rows a short window keeps: field, note, buttons.
const PROMPT_TIGHT: usize = 5;

/// One button of the naming box: zero based row and column, its label, and the key it
/// stands for, which is what a click on it presses.
pub(crate) struct PromptButton {
    row: usize,
    col: usize,
    label: String,
    key: u8,
    primary: bool,
}

impl PromptButton {
    /// Its columns on screen: the label and a cell of padding either side.
    fn width(&self) -> usize {
        width::width(&self.label) + 2
    }
}

impl PromptBox {
    /// In the upper third of the pane, where the eye already is, or `None` on a window
    /// too small for even the tight box.
    pub(crate) fn at(cols: u16, lines: u16) -> Option<Self> {
        let (cols, lines) = (cols as usize, lines as usize);
        let room = lines.saturating_sub(TABS + STATUS);
        let (roomy, h) = match room {
            _ if cols < 20 => return None,
            r if r >= PROMPT_ROOMY => (true, PROMPT_ROOMY),
            r if r >= PROMPT_TIGHT => (false, PROMPT_TIGHT),
            _ => return None,
        };
        let w = cols.min(66);
        Some(Self {
            left: (cols - w) / 2,
            top: TABS + (room - h) / 3,
            w,
            roomy,
        })
    }

    /// The blank columns inside each side rule.
    fn pad(&self) -> usize {
        if self.roomy { 2 } else { 1 }
    }

    /// The columns between the paddings: the field, its underline and the buttons row.
    fn span(&self) -> usize {
        self.w - 2 - 2 * self.pad()
    }

    /// Zero based screen column where the field starts.
    fn field_col(&self) -> usize {
        self.left + 1 + self.pad()
    }

    /// Zero based screen row of the typed text.
    fn text_row(&self) -> usize {
        self.top + BOX_FIRST_ROW + if self.roomy { 2 } else { 0 }
    }

    /// The buttons: clear and reset on the left, cancel and the answer on the right, the
    /// answer last where the eye ends. A narrow box drops the left ones first, then
    /// cancel, and never the answer.
    fn buttons(&self, what: crate::run::Naming) -> Vec<PromptButton> {
        let chip = |label: &str, key: u8, primary: bool| (label.to_string(), key, primary);
        let verb = format!("\u{21b5} {}", what.verb());
        let mut right = vec![chip("esc cancel", 0x1b, false), chip(&verb, b'\r', true)];
        let mut left = vec![chip("^u clear", 0x15, false)];
        if what.resets() {
            left.push(chip("^r reset", 0x12, false));
        }
        // A prefix chosen as ctrl-u or ctrl-r is the prefix in here too, so its button
        // would say a key that does something else.
        left.retain(|(_, key, _)| *key != crate::keys::shown_prefix());
        let size = |set: &[(String, u8, bool)]| {
            set.iter().map(|(l, ..)| width::width(l) + 2).sum::<usize>()
                + 2 * set.len().saturating_sub(1)
        };
        let room = self.span();
        if size(&right) > room {
            right.remove(0);
        }
        if size(&left) + 2 + size(&right) > room {
            left.clear();
        }
        let row = self.top + BOX_FIRST_ROW + if self.roomy { 6 } else { 2 };
        let mut out = Vec::new();
        for (set, mut col) in [
            (left, self.field_col()),
            (right.clone(), self.field_col() + room - size(&right)),
        ] {
            for (label, key, primary) in set {
                let button = PromptButton {
                    row,
                    col,
                    label,
                    key,
                    primary,
                };
                col += button.width() + 2;
                out.push(button);
            }
        }
        out
    }
}

/// The key a click at the zero based `row` and `col` presses in the naming box, when it
/// lands on one of the box's buttons.
pub(crate) fn prompt_click(
    cols: u16,
    lines: u16,
    what: crate::run::Naming,
    row: usize,
    col: usize,
) -> Option<u8> {
    PromptBox::at(cols, lines)?
        .buttons(what)
        .into_iter()
        .find(|b| b.row == row && (b.col..b.col + b.width()).contains(&col))
        .map(|b| b.key)
}

/// The zero based row and column of the naming box's button for `key`, for a test to
/// click it.
#[cfg(test)]
pub(crate) fn prompt_button(
    cols: u16,
    lines: u16,
    what: crate::run::Naming,
    key: u8,
) -> Option<(usize, usize)> {
    PromptBox::at(cols, lines)?
        .buttons(what)
        .into_iter()
        .find(|b| b.key == key)
        .map(|b| (b.row, b.col))
}

/// What fits of `text` in `room` columns: all of it, or its end behind a `…`, so the
/// letter being typed is always the one on screen and the cursor never lands on it.
fn field_tail(text: &str, room: usize) -> String {
    if width::width(text) <= room {
        return text.to_string();
    }
    let mut kept = Vec::new();
    let mut used = 1;
    for c in text.chars().rev() {
        let w = width::columns(c);
        if used + w > room {
            break;
        }
        used += w;
        kept.push(c);
    }
    std::iter::once('\u{2026}')
        .chain(kept.into_iter().rev())
        .collect()
}

/// The naming box, painted over the upper middle of the screen.
///
/// A form: what is asked on the edge, a label over the field, the answer on an underline,
/// a note under it saying what an empty answer does, and buttons for the keys. A rename
/// opens empty over the name it has, shown faint, so a new name is typed straight in.
/// In the middle of the pane rather than in the column, because it is a question about
/// something that is about to exist and has no row of its own to sit in yet.
fn paint_prompt(frame: &Frame, prompt: &crate::run::Prompt, out: &mut String) {
    let Some(b) = PromptBox::at(frame.cols, frame.lines) else {
        return;
    };
    let inner = b.w - 2;
    let span = b.span();
    let pad = " ".repeat(b.pad());
    let tail = " ".repeat(b.pad());
    // One row: the padding, `text` (at most `span` columns) in its ink, then blanks. Text,
    // not a grid row, so a space after a wide character is a space.
    let row = |text: &str, ink: &str| {
        format!(
            "{}{pad}{ink}{}{}{tail}",
            theme::RESET,
            width::text(text, span),
            theme::RESET
        )
    };
    let limit = prompt.what.limit();
    let typed = width::width(&prompt.text);
    let field = if prompt.text.is_empty() {
        // The cursor's cell, then the name it has, faint.
        row(&format!(" {}", prompt.was), &theme::fg(theme::Role::Faint))
    } else {
        row(
            &field_tail(&prompt.text, span - 1),
            &theme::fg(theme::Role::Focused),
        )
    };
    let underline = row(
        &"\u{2594}".repeat(span),
        &theme::fg(if prompt.text.is_empty() {
            theme::Role::Rule
        } else {
            theme::Role::Brand
        }),
    );
    // What empty does, or at the limit that typing has stopped, and the count on the right
    // once anything is typed.
    let full = typed >= limit;
    let count = if typed > 0 {
        format!("{typed}/{limit}")
    } else {
        String::new()
    };
    let (said, said_ink) = if full {
        ("at the limit".to_string(), theme::Role::Brand)
    } else {
        (prompt.what.note(&prompt.was), theme::Role::Faint)
    };
    let note = format!(
        "{}{pad}{}{}{}{count}{}{tail}",
        theme::RESET,
        theme::fg(said_ink),
        width::text(&said, span - count.len()),
        theme::fg(if full {
            theme::Role::Brand
        } else {
            theme::Role::Faint
        }),
        theme::RESET
    );
    let mut buttons = theme::RESET.to_string();
    let mut used = 0;
    for button in b.buttons(prompt.what) {
        let at = button.col - (b.left + 1);
        buttons.push_str(&" ".repeat(at - used));
        buttons.push_str(&theme::button(button.primary));
        buttons.push_str(&format!(" {} ", button.label));
        buttons.push_str(theme::RESET);
        used = at + button.width();
    }
    buttons.push_str(&" ".repeat(inner - used));
    let blank = " ".repeat(inner);
    let body: Vec<String> = if b.roomy {
        vec![
            blank.clone(),
            row(prompt.what.label(), &theme::fg(theme::Role::Detail)),
            field,
            underline,
            note,
            blank.clone(),
            buttons,
            blank,
        ]
    } else {
        vec![field, note, buttons]
    };
    let (rule, title) = spot_inks();
    paint_boxed(
        out,
        frame.lines as usize,
        (b.left, b.top, b.w),
        prompt.what.asks(),
        (&rule, &title),
        &body,
    );
}

/// Where the centred menu box sits: left, top, width and height in cells.
///
/// Zero based, like the painter counts. The height covers both borders (the title sits
/// in the top one), one row per menu entry and one hint row. Shared with hit testing,
/// so a click and a repaint agree on which row is which and a second copy of
/// this arithmetic cannot drift.
pub(crate) fn menu_rect(
    cols: u16,
    lines: u16,
    rows: &[(String, String)],
    title: &str,
) -> Option<(usize, usize, usize, usize)> {
    let cols = cols as usize;
    let lines = lines as usize;
    let (max_name, max_key) = rows.iter().fold((0, 0), |(n, k), (name, right)| {
        let keypart = if right.is_empty() {
            0
        } else {
            width::width(right)
        };
        (n.max(width::width(name)), k.max(keypart))
    });
    let inner = (1 + max_name + 1 + max_key)
        .max(width::width(title.trim()) + 4)
        .max(footer_width(MENU_HINT));
    let w = inner + 2;
    let height = rows.len() + 3;
    if cols < w + 2 || lines < height + 2 {
        return None;
    }
    let left = cols.saturating_sub(w) / 2;
    let top = lines.saturating_sub(height) / 2;
    Some((left, top, w, height))
}

/// Which menu row a one based screen position lands on, if it lands on one.
pub(crate) fn menu_row_at(
    cols: u16,
    lines: u16,
    rows: &[(String, String)],
    title: &str,
    row: u16,
    col: u16,
) -> Option<usize> {
    let (left, top, w, height) = menu_rect(cols, lines, rows, title)?;
    let r = (row as usize).checked_sub(1)?;
    let c = (col as usize).checked_sub(1)?;
    if c < left || c >= left + w || r < top || r >= top + height {
        return None;
    }
    let item = r.checked_sub(top + BOX_FIRST_ROW)?;
    (item < rows.len()).then_some(item)
}

/// The menu's keys, said once for the box and for the width it needs.
const MENU_HINT: &[(&str, &str)] = &[("j k", "move"), ("enter", "choose"), ("esc", "close")];

/// The menu, centred over the pane like the palette.
///
/// It used to sit low over the left column, above the row it was opened from,
/// where it covered the spaces list and read as a glitch of the column rather
/// than a choice. A short list opened from a word at the foot of the column is
/// still a modal choice, so it gets the modal treatment: centred, with a title
/// and a hint row, painted last over everything except the read only overlay.
fn paint_menu(frame: &Frame, selected: usize, kind: &crate::run::MenuKind, out: &mut String) {
    let rows = crate::run::menu_rows(kind);
    let title = crate::run::menu_title(kind);
    let Some((left, top, w, _)) = menu_rect(frame.cols, frame.lines, &rows, &title) else {
        return;
    };
    let inner = w.saturating_sub(2);
    let mut body: Vec<String> = rows
        .iter()
        .enumerate()
        .map(|(i, (name, keypart))| {
            let ink = if i == selected {
                theme::selected()
            } else {
                theme::fg(theme::Role::Text)
            };
            // The name on the left, its key or its state on the right: the main menu
            // teaches the shortcut each row replaces, so the next visit skips it.
            let gap = inner.saturating_sub(1 + width::width(name) + width::width(keypart));
            let cell = format!(" {name}{}{keypart}", " ".repeat(gap));
            format!("{ink}{}", width::fit(&cell, inner))
        })
        .collect();
    body.push(hint_line(MENU_HINT, inner));
    let (rule, ink) = spot_inks();
    // The title names what is open, the way the palette names its query.
    paint_boxed(
        out,
        frame.lines as usize,
        (left, top, w),
        &title,
        (&rule, &ink),
        &body,
    );
}

/// A read only overlay: the keys, or what this daemon is set to.
///
/// Every line of it is a real value read from the interface itself rather than a screen
/// somebody drew once: a settings page that says what the program wishes were true is worse
/// than no settings page. Answers where the cursor goes while the keys are searched.
fn paint_reading(
    frame: &Frame,
    reading: crate::run::Reading,
    out: &mut String,
) -> Option<(usize, usize)> {
    if reading == crate::run::Reading::Keybinds {
        return paint_keys(frame, frame.chrome.keys_query, out);
    }
    let lines = [
        format!(" window          {} by {}", frame.cols, frame.lines),
        format!(
            " left column     {} columns{}",
            if frame.collapsed {
                sidebar::COLLAPSED
            } else {
                frame.sidebar_width
            },
            if frame.collapsed { ", folded" } else { "" }
        ),
        format!(" spaces          {}", frame.spaces.len()),
        format!(" terminals       {}", frame.rows.len()),
        format!(
            " agents          {}",
            frame
                .rows
                .iter()
                .filter(|r| r.agent && r.state != "sleeping")
                .count()
        ),
        format!(
            " waiting         {}",
            frame
                .rows
                .iter()
                .filter(|r| matches!(r.state.as_str(), "blocked" | "failed"))
                .count()
        ),
    ];
    let file = crate::config::shown();
    let widest = lines
        .iter()
        .chain(&file)
        .map(|l| width::width(l))
        .max()
        .unwrap_or(0);
    let inner = box_inner(frame, widest + 1, "settings");
    let rows: Vec<String> = lines
        .iter()
        .map(|line| live_row(line, inner))
        .chain(file.iter().map(|line| {
            // The file's lines as written, a `[section]` as a heading.
            let ink = if line.trim_start().starts_with('[') {
                theme::Role::Title
            } else {
                theme::Role::Text
            };
            format!("{}{}", theme::fg(ink), width::fit(line, inner))
        }))
        .chain(std::iter::once(hint_line(CLOSE_HINT, inner)))
        .collect();
    paint_box(frame, "settings", inner, &rows, out);
    None
}

/// What closes a box that is only being read. Any key does; esc is the one to say.
const CLOSE_HINT: &[(&str, &str)] = &[("esc", "close")];

/// One of the live settings rows: its name faint and its value plain, somebody waiting
/// in the colour that says so. The value starts after the name's run of spaces.
fn live_row(text: &str, inner: usize) -> String {
    let fitted = width::fit(text, inner);
    let name = fitted.trim_start();
    let at = (fitted.len() - name.len())
        + name.find("  ").map_or(name.len(), |gap| {
            gap + (name[gap..].len() - name[gap..].trim_start().len())
        });
    let (head, tail) = fitted.split_at(at);
    let ink = if head.trim() == "waiting" && tail.trim() != "0" {
        theme::Role::Blocked
    } else {
        theme::Role::Text
    };
    format!(
        "{}{head}{}{tail}",
        theme::fg(theme::Role::Faint),
        theme::fg(ink)
    )
}

/// One group of the keys overlay: its name, which ink of the space wheel tells it apart,
/// and each key with what it does.
type KeyGroup = (&'static str, usize, &'static [(&'static str, &'static str)]);

/// Every key, grouped the way a person looks for one. The keys are what follows `ctrl-a`,
/// which the box's title says once; copy mode's and the mouse's say where they work.
/// Short on purpose: three columns of these fit an 80 by 24 window, which is where a person
/// who does not know the keys yet is most likely to be.
pub(crate) const KEY_GROUPS: &[KeyGroup] = &[
    (
        "tab",
        3,
        &[
            ("n/p", "next, prev"),
            ("1..9", "go to tab"),
            ("c", "new tab"),
            ("r", "rename tab"),
            ("x", "close tab"),
        ],
    ),
    (
        "space",
        4,
        &[
            ("s/S", "next, prev"),
            ("w", "new space"),
            ("W", "close empty"),
            ("R", "rename space"),
            ("Z", "sleep, wake"),
            ("f", "fold"),
            ("g", "tree or rank"),
        ],
    ),
    (
        "pane",
        1,
        &[
            ("v/-", "split beside, below"),
            ("H/J/K/L", "focus side"),
            ("o", "next pane"),
            ("z", "zoom"),
            ("X", "back to a tab"),
            ("e", "resize"),
            ("=", "equalize"),
        ],
    ),
    (
        "copy mode",
        0,
        &[
            ("[", "start, then:"),
            ("hjkl", "move"),
            ("w b e", "by word"),
            ("g G", "top, bottom"),
            ("^u ^d", "half page"),
            ("v V y", "select, copy"),
            ("/ ? n N", "search"),
            ("q esc", "leave"),
        ],
    ),
    (
        "general",
        5,
        &[
            ("space", "find"),
            ("tab", "needs you"),
            ("m", "menu"),
            ("M", "last answer"),
            ("F", "focus view"),
            ("b", "fold column"),
            ("d", "detach, ctrl-q"),
            ("U", "update"),
            ("l", "repaint"),
            ("?", "this list"),
        ],
    ),
    // The mouse is not a key, and it is the part of this a person is least likely to
    // guess: an interface that takes the mouse has taken their terminal's own selection
    // with it, so it owes them these lines.
    (
        "mouse",
        2,
        &[
            ("click", "focus"),
            ("drag", "select"),
            ("drag rule", "resize"),
            ("wheel", "scroll"),
            ("shift-drag", "over apps"),
        ],
    ),
];

/// Which groups stand side by side, widest first: the first that fits the window is used.
/// Always every group, so a search empties rows and never moves the box.
const KEY_COLUMNS: [&[&[usize]]; 3] = [
    &[&[0, 1], &[2, 3], &[4, 5]],
    &[&[0, 1, 2], &[3, 4, 5]],
    &[&[0, 1, 2, 3, 4, 5]],
];

const KEYS_HINT: &[(&str, &str)] = &[("/", "search"), ("esc", "close")];

/// The keys overlay: the groups in columns, filtered by the search when one is typed.
/// Answers where the cursor goes while searching.
fn paint_keys(frame: &Frame, query: Option<&str>, out: &mut String) -> Option<(usize, usize)> {
    let title = crate::keys::named("keys, after ctrl-a");
    let key_w = |ids: &[usize]| {
        ids.iter()
            .flat_map(|i| KEY_GROUPS[*i].2.iter())
            .map(|(key, _)| width::width(key))
            .max()
            .unwrap_or(0)
    };
    let cell_w = |ids: &[usize]| {
        let words = ids
            .iter()
            .flat_map(|i| KEY_GROUPS[*i].2.iter())
            .map(|(_, word)| width::width(word))
            .max()
            .unwrap_or(0);
        let names = ids
            .iter()
            .map(|i| width::width(KEY_GROUPS[*i].0))
            .max()
            .unwrap_or(0);
        (1 + key_w(ids) + 1 + words).max(1 + names)
    };
    let room = (frame.cols as usize).saturating_sub(2);
    let wanted = |columns: &[&[usize]]| {
        columns.iter().map(|ids| cell_w(ids)).sum::<usize>() + 2 * (columns.len() - 1) + 1
    };
    // The widest layout that fits, or one column cut to the window.
    let columns = KEY_COLUMNS
        .iter()
        .find(|columns| wanted(columns) <= room)
        .unwrap_or(&KEY_COLUMNS[2]);
    let inner = box_inner(frame, wanted(columns), &title);
    if inner < 4 {
        return None;
    }
    let cells: Vec<usize> = if columns.len() == 1 {
        vec![inner - 1]
    } else {
        columns.iter().map(|ids| cell_w(ids)).collect()
    };

    let needle = query.unwrap_or("").to_lowercase();
    let full = |ids: &[usize]| {
        ids.iter()
            .map(|i| 1 + KEY_GROUPS[*i].2.len())
            .sum::<usize>()
            + ids.len()
            - 1
    };
    let height = columns.iter().map(|ids| full(ids)).max().unwrap_or(0);
    let painted: Vec<Vec<String>> = columns
        .iter()
        .zip(&cells)
        .map(|(ids, &cell)| {
            let mut rows = Vec::new();
            for &i in *ids {
                let (name, ink, keys) = KEY_GROUPS[i];
                let whole = name.contains(&needle);
                let shown: Vec<_> = keys
                    .iter()
                    .filter(|(key, word)| {
                        whole
                            || key.to_lowercase().contains(&needle)
                            || word.to_lowercase().contains(&needle)
                    })
                    .collect();
                if shown.is_empty() {
                    continue;
                }
                if !rows.is_empty() {
                    rows.push(" ".repeat(cell));
                }
                rows.push(format!(
                    "{}{}",
                    theme::fg(theme::Role::Space(ink)),
                    width::fit(&format!(" {name}"), cell)
                ));
                let kw = key_w(ids).min(cell.saturating_sub(2));
                for (key, word) in shown {
                    rows.push(key_cell(key, word, kw, cell, &needle));
                }
            }
            rows.resize(height, " ".repeat(cell));
            rows
        })
        .collect();
    let mut body: Vec<String> = (0..height)
        .map(|row| {
            let mut line = String::new();
            for (i, column) in painted.iter().enumerate() {
                if i > 0 {
                    line.push_str("  ");
                }
                line.push_str(&column[row]);
            }
            line.push(' ');
            line
        })
        .collect();
    // Taller than the window: what fits, then a line that says the rest can be searched.
    let lines = frame.lines as usize;
    let fit = box_rows(lines).saturating_sub(1);
    if body.len() > fit {
        body.truncate(fit.saturating_sub(1));
        if fit > 0 {
            body.push(format!(
                "{}{}",
                theme::fg(theme::Role::Faint),
                width::fit(" more: / search", inner)
            ));
        }
    }
    let (footer, typed_at) = match query {
        Some(query) => {
            // ` / query  esc clear`, the query cut to what is left of the row.
            let room = inner.saturating_sub(3 + "  esc clear".len() + 1);
            // Built here and not by `hint_line`: the query is not a key and a word.
            let shown = width::text(query, width::width(query).min(room));
            let plain = 3 + width::width(&shown) + "  esc clear".len();
            if plain > inner {
                // Too narrow for the words: the query alone, cut to the box.
                let row = format!(
                    "{}{}",
                    theme::fg(theme::Role::Text),
                    width::fit(&format!(" / {shown}"), inner)
                );
                (row, Some((3 + width::width(&shown)).min(inner - 1)))
            } else {
                (
                    format!(
                        "{} {}/ {}{shown}{}  {}esc{} clear{}",
                        theme::fg(theme::Role::Faint),
                        theme::fg(theme::Role::Focused),
                        theme::fg(theme::Role::Text),
                        theme::fg(theme::Role::Faint),
                        theme::fg(theme::Role::Focused),
                        theme::fg(theme::Role::Faint),
                        " ".repeat(inner.saturating_sub(plain))
                    ),
                    Some(3 + width::width(&shown)),
                )
            }
        }
        None => (hint_line(KEYS_HINT, inner), None),
    };
    body.push(footer);
    let (left, top) = paint_box(frame, &title, inner, &body, out)?;
    typed_at.map(|col| (top + BOX_FIRST_ROW + body.len() - 1, left + 1 + col))
}

/// One key of the keys overlay, `cell` columns: the key bright, what it does faint, and the
/// part of that the search matched underlined.
fn key_cell(key: &str, word: &str, kw: usize, cell: usize, needle: &str) -> String {
    let key = width::text(key, kw);
    let room = cell.saturating_sub(kw + 2);
    let word = width::text(word, width::width(word).min(room));
    let word = word.trim_end();
    let pad = " ".repeat(room.saturating_sub(width::width(word)));
    let faint = theme::fg(theme::Role::Faint);
    let marked = match word.to_lowercase().find(needle) {
        Some(at) if !needle.is_empty() && word.is_ascii() => format!(
            "{}\x1b[4m{}\x1b[24m{}",
            &word[..at],
            &word[at..at + needle.len()],
            &word[at + needle.len()..]
        ),
        _ => word.to_string(),
    };
    format!(
        "{faint} {}{key}{faint} {marked}{pad}",
        theme::fg(theme::Role::Focused)
    )
}

/// The inner width of a box whose rows need `content` columns, with this title on its
/// edge, on this window.
fn box_inner(frame: &Frame, content: usize, title: &str) -> usize {
    content
        .max(width::width(title.trim()) + 4)
        .min((frame.cols as usize).saturating_sub(2))
}

/// The rows a box can hold between the tab bar and the foot, edges not counted.
fn box_rows(lines: usize) -> usize {
    lines.saturating_sub(TABS + STATUS + 2)
}

/// A bordered box in the middle of the screen: `rows` already styled to `inner` columns,
/// clipped to the window. Answers where its top left corner went.
fn paint_box(
    frame: &Frame,
    title: &str,
    inner: usize,
    rows: &[String],
    out: &mut String,
) -> Option<(usize, usize)> {
    let cols = frame.cols as usize;
    let lines = frame.lines as usize;
    let w = inner + 2;
    // Clipped to the window rather than skipped: skipping painted nothing while the
    // overlay still took the next key, which read as a frozen screen. Between the tab bar
    // and the two rows at the foot, which keep saying what is going on.
    let rows = &rows[..rows.len().min(box_rows(lines))];
    if rows.is_empty() || w < 6 {
        return None;
    }
    let left = cols.saturating_sub(w) / 2;
    let top = (lines.saturating_sub(rows.len() + 2) / 2)
        .max(TABS)
        .min(lines.saturating_sub(STATUS + rows.len() + 2));
    let (rule, ink) = spot_inks();
    paint_boxed(out, lines, (left, top, w), title, (&rule, &ink), rows);
    Some((left, top))
}

/// Resize mode's keys, for the notice row.
pub(crate) const RESIZE_HINT: &[(&str, &str)] = &[
    ("hjkl", "move 5%"),
    ("HJKL", "20%"),
    ("=", "equalize"),
    ("esc", "done"),
];

/// Copy mode's keys, for the notice row while it has nothing else to say.
pub(crate) const COPY_HINT: &[(&str, &str)] = &[
    ("hjkl", "move"),
    ("w b e", "words"),
    ("/ ?", "search"),
    ("n N", "again"),
    ("v V", "select"),
    ("y", "copy"),
    ("q", "quit"),
];

/// A mode on the notice row: its name in the brand colour, the one mode colour, then its
/// keys the way the key row says them, or what it has to say instead when `keys` is empty.
fn mode_row(word: &str, keys: &[(&str, &str)], said: &str, cols: usize) -> String {
    let mut out = format!("{} {word}", theme::status_in(theme::Role::Brand));
    let mut used = 1 + width::width(word);
    if keys.is_empty() {
        let text = width::fit(&format!("  {said}"), cols.saturating_sub(used));
        used += width::width(&text);
        out.push_str(&theme::status());
        out.push_str(&text);
    }
    for (key, what) in keys {
        let wide = 3 + width::width(key) + width::width(what);
        if used + wide > cols {
            break;
        }
        used += wide;
        out.push_str(&theme::status());
        out.push_str("  ");
        out.push_str(&theme::status_key());
        out.push_str(key);
        out.push_str(&theme::status());
        out.push(' ');
        out.push_str(what);
    }
    out.push_str(&theme::status());
    out.push_str(&" ".repeat(cols.saturating_sub(used)));
    out
}

/// Whether the box asking about closing `terminal` fits this window, so the notice row
/// can leave the question to it.
fn confirm_fits(frame: &Frame, terminal: u32) -> bool {
    frame.cols >= 24
        && frame.lines as usize >= TABS + STATUS + 5
        && frame.rows.iter().any(|r| r.terminal == terminal)
}

/// Asking before a busy agent is closed: the only box in the colour that says work can be
/// lost, what it is and what it is doing, and the one key that closes it.
fn paint_confirm(frame: &Frame, terminal: u32, out: &mut String) {
    let Some(row) = frame.rows.iter().find(|r| r.terminal == terminal) else {
        return;
    };
    let program = row
        .program
        .as_deref()
        .map(|p| p.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or(p))
        .unwrap_or("this agent");
    let space = frame
        .spaces
        .iter()
        .find(|s| s.id == row.workspace)
        .map(|s| s.name.as_str())
        .unwrap_or("");
    // Cleaned like the title: a name can come from another machine, and a control
    // character in it would be written to the terminal as one.
    let clean = |text: &str| width::text(text, width::width(text));
    let (space, program, state) = (clean(space), clean(program), clean(&row.state));
    let cols = frame.cols as usize;
    let lines = frame.lines as usize;
    let w = palette_width(frame.cols);
    let inner = w - 2;
    let plain = format!(" {space} / {program} is {state}");
    let body = if width::width(&plain) <= inner {
        format!(
            "{} {}{space}{} / {}{program} is {}{}{}",
            theme::fg(theme::Role::Text),
            theme::fg(theme::space_of(row.workspace)),
            theme::fg(theme::Role::Faint),
            theme::fg(theme::Role::Text),
            theme::fg(theme::of(&row.state)),
            state,
            " ".repeat(inner - width::width(&plain))
        )
    } else {
        format!(
            "{}{}",
            theme::fg(theme::Role::Text),
            width::fit(&plain, inner)
        )
    };
    let footer = hint_line(&[("Y", "close it"), ("any other key", "keep it")], inner);
    let ink = theme::fg(theme::Role::Blocked);
    // In one colour the warning ink is plain, so the title says it in bold instead.
    let title_ink = if crate::colorlevel::level() == crate::colorlevel::Level::Mono {
        theme::fg(theme::Role::Focused)
    } else {
        ink.clone()
    };
    let left = cols.saturating_sub(w) / 2;
    let top = lines.saturating_sub(4) / 2;
    paint_boxed(
        out,
        lines,
        (left, top, w),
        &format!("close {program}?"),
        (&ink, &title_ink),
        &[body, footer],
    );
}

/// The row under a box's top edge, where its first row goes. Hit testing reads it too.
pub(crate) const BOX_FIRST_ROW: usize = 1;

/// A box's rule and title inks: a rule bright enough to stand over the faded screen
/// behind it, and a bold title. The close question keeps its warning ink instead.
fn spot_inks() -> (String, String) {
    (
        theme::fg(theme::Role::Detail),
        format!("{}\x1b[1m", theme::fg(theme::Role::Focused)),
    )
}

/// One floating box: the rounded edge in `inks.0`, `title` cut into the top edge in
/// `inks.1`, and `rows` between the side rules, each already styled and exactly `w - 2`
/// columns wide.
///
/// Every overlay goes through here, so a box looks the same whoever opens it. A row past
/// the bottom of the window is dropped rather than written below the screen.
fn paint_boxed(
    out: &mut String,
    lines: usize,
    (left, top, w): (usize, usize, usize),
    title: &str,
    (ink, title_ink): (&str, &str),
    rows: &[String],
) {
    let inner = w.saturating_sub(2);
    let put = |row: usize, text: &str, out: &mut String| {
        if row < lines {
            out.push_str(&at(row + 1, left + 1));
            out.push_str(text);
            out.push_str(theme::RESET);
        }
    };
    let title = title.trim();
    // `─ title ` and at least one more rule after it, or no title at all. A wide character
    // cut at the limit leaves a pad, which is not part of the name.
    let shown = width::text(title, width::width(title).min(inner.saturating_sub(4)));
    let shown = shown.trim_end();
    let used = width::width(shown);
    // The brand's one dash beside the corner, on every box.
    let brand = theme::fg(theme::Role::Brand);
    let top_edge = if used == 0 {
        format!(
            "{ink}\u{256d}{brand}{}{ink}{}\u{256e}",
            "\u{2500}".repeat(inner.min(1)),
            "\u{2500}".repeat(inner.saturating_sub(1))
        )
    } else {
        format!(
            "{ink}\u{256d}{brand}\u{2500}{ink} {title_ink}{shown}{ink} {}\u{256e}",
            "\u{2500}".repeat(inner - used - 3)
        )
    };
    put(top, &top_edge, out);
    for (i, row) in rows.iter().enumerate() {
        put(
            top + BOX_FIRST_ROW + i,
            &format!("{ink}\u{2502}{row}{ink}\u{2502}"),
            out,
        );
    }
    let bottom = format!("{ink}\u{2570}{}\u{256f}", "\u{2500}".repeat(inner));
    put(top + BOX_FIRST_ROW + rows.len(), &bottom, out);
}

/// The columns a footer of `hints` needs: a leading space, each `key word`, two spaces
/// between them, and one to spare.
fn footer_width(hints: &[(&str, &str)]) -> usize {
    hints
        .iter()
        .map(|(key, word)| width::width(key) + 1 + width::width(word))
        .sum::<usize>()
        + 2 * hints.len()
}

/// A box's footer: keys bright, what they do faint, two spaces apart, padded to `inner`.
///
/// Cut whole from the end when the box is narrow, never the first entry: it is the one
/// that says how to answer.
fn hint_line(hints: &[(&str, &str)], inner: usize) -> String {
    let mut shown = hints.len();
    while shown > 1 && footer_width(&hints[..shown]) > inner {
        shown -= 1;
    }
    let mut line = String::new();
    let mut plain = String::new();
    for (i, (key, word)) in hints[..shown].iter().enumerate() {
        let gap = if i == 0 { " " } else { "  " };
        line.push_str(&theme::fg(theme::Role::Faint));
        line.push_str(gap);
        line.push_str(&theme::fg(theme::Role::Focused));
        line.push_str(key);
        line.push_str(&theme::fg(theme::Role::Faint));
        line.push(' ');
        line.push_str(word);
        plain.push_str(&format!("{gap}{key} {word}"));
    }
    let used = width::width(&plain);
    if used > inner {
        // Narrower than even the first entry: its words, cut, in one ink.
        return format!(
            "{}{}",
            theme::fg(theme::Role::Faint),
            width::fit(&plain, inner)
        );
    }
    line.push_str(&" ".repeat(inner - used));
    line
}

/// One row of the terminal, with its colour.
///
/// The runs are in the grid's columns and the text is painted in the screen's, and those
/// two are the same number only until a wide character appears on the row. So the row is
/// walked once, in cells, and each cell is emitted with the style the run says it has: a
/// style change writes one escape sequence and nothing else does.
/// One row of the pane, wherever it comes from.
///
/// The window is one buffer: every row ever written, then the rows on the screen now.
/// Scrolled back by `s`, the first `s` rows of the pane come from behind the screen and the
/// rest are the screen itself, shifted down by `s`, so reading upwards crosses no seam.
///
/// One function, because the painter and the copier both have to answer this the same way:
/// a selection made over rows that went past has to copy those rows and not whatever is at
/// the same position on the live screen.
pub fn window_row<'a>(
    snapshot: Option<&'a SnapshotWire>,
    behind: &'a crate::run::Behind,
    scroll: usize,
    anchor: u64,
    pane_row: usize,
) -> (&'a str, &'a [SpanWire]) {
    if pane_row < scroll {
        let at = anchor + pane_row as u64;
        // Rows behind the screen are text alone: what was written to disk is rows, and
        // colour there would be a second format nobody reads back.
        return (behind.row(at).unwrap_or(""), &[]);
    }
    let row = pane_row - scroll;
    (
        snapshot
            .and_then(|s| s.lines.get(row))
            .map(String::as_str)
            .unwrap_or(""),
        snapshot
            .and_then(|s| s.styles.get(row))
            .map(Vec::as_slice)
            .unwrap_or(&[]),
    )
}

/// The link targets a screen's runs point into.
fn links_of(snapshot: Option<&SnapshotWire>) -> &[String] {
    snapshot.map(|s| s.links.as_slice()).unwrap_or(&[])
}

/// Whether a tile covers a pane-local cell.
fn covers(frame: &Frame, x: usize, pane_row: usize) -> bool {
    frame.tiles.iter().any(|t| {
        pane_row >= t.rect.y
            && pane_row < t.rect.y + t.rect.rows
            && x >= t.rect.x
            && x < t.rect.x + t.rect.cols
    })
}

/// How a cell of the lines around split panes is painted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EdgeInk {
    /// A line no focused pane touches: the rule grey.
    Rule,
    /// A line of the focused pane's box, in its space's colour.
    Lit,
    /// A pane's name on its top edge.
    Title,
    /// The focused pane's name.
    TitleLit,
}

/// The lines around split panes, as a grid over the pane area.
///
/// Each tile gets a box one cell out from it, and boxes that meet share the cells between
/// them, so two panes side by side have one line between them and a frame round both.
/// Where lines meet the cell joins them (`┬ ┴ ├ ┤ ┼`), the outer corners round the way every
/// other box on this screen does, and the focused pane's box is drawn in its space's colour
/// with its name on the top edge: the keyboard's pane is findable without reading.
fn pane_edges(
    tiles: &[Tile],
    agents: &[AgentRow],
    cols: usize,
    rows: usize,
) -> Vec<Vec<Option<(char, EdgeInk)>>> {
    // Which ways a line leaves each cell: up, down, left, right.
    let mut ways = vec![vec![[false; 4]; cols]; rows];
    let mut lit = vec![vec![false; cols]; rows];
    for tile in tiles {
        let r = tile.rect;
        // The box around the tile. A tile against the area's edge (an area too small for
        // the frame) has no line on that side.
        let (Some(left), Some(top)) = (r.x.checked_sub(1), r.y.checked_sub(1)) else {
            continue;
        };
        let (right, bottom) = (r.x + r.cols, r.y + r.rows);
        if right >= cols || bottom >= rows {
            continue;
        }
        for x in left..=right {
            for y in [top, bottom] {
                ways[y][x][2] |= x > left;
                ways[y][x][3] |= x < right;
                lit[y][x] |= tile.focused;
            }
        }
        for y in top..=bottom {
            for x in [left, right] {
                ways[y][x][0] |= y > top;
                ways[y][x][1] |= y < bottom;
                lit[y][x] |= tile.focused;
            }
        }
    }
    let mut grid: Vec<Vec<Option<(char, EdgeInk)>>> = ways
        .iter()
        .zip(&lit)
        .map(|(row, lit)| {
            row.iter()
                .zip(lit)
                .map(|(way, lit)| {
                    edge_char(*way).map(|c| (c, if *lit { EdgeInk::Lit } else { EdgeInk::Rule }))
                })
                .collect()
        })
        .collect();
    // Each pane's name on its top edge, two cells in from its corner.
    for tile in tiles {
        let r = tile.rect;
        let Some(top) = r.y.checked_sub(1) else {
            continue;
        };
        let Some(row) = agents.iter().find(|a| a.terminal == tile.terminal) else {
            continue;
        };
        let room = r.cols.saturating_sub(2);
        if room < 3 {
            continue;
        }
        let name = crate::sidebar::label(row);
        let name = width::fit(&name, width::width(&name).min(room - 2));
        let ink = if tile.focused {
            EdgeInk::TitleLit
        } else {
            EdgeInk::Title
        };
        let mut x = r.x + 1;
        for c in format!(" {} ", name.trim_end()).chars() {
            let wide = width::columns(c) == 2;
            if let Some(cell) = grid[top].get_mut(x) {
                *cell = Some((c, ink));
            }
            // The cell a wide character spills into paints nothing of its own.
            if wide && let Some(cell) = grid[top].get_mut(x + 1) {
                *cell = Some(('\0', ink));
            }
            x += if wide { 2 } else { 1 };
        }
    }
    grid
}

/// The line-drawing character for a cell the lines leave by `[up, down, left, right]`.
fn edge_char(way: [bool; 4]) -> Option<char> {
    Some(match way {
        [true, true, true, true] => '\u{253c}',
        [true, true, true, false] => '\u{2524}',
        [true, true, false, true] => '\u{251c}',
        [true, false, true, true] => '\u{2534}',
        [false, true, true, true] => '\u{252c}',
        [false, true, false, true] => '\u{256d}',
        [false, true, true, false] => '\u{256e}',
        [true, false, false, true] => '\u{2570}',
        [true, false, true, false] => '\u{256f}',
        [true, _, false, false] | [_, true, false, false] => '\u{2502}',
        [false, false, true, _] | [false, false, _, true] => '\u{2500}',
        [false, false, false, false] => return None,
    })
}

/// The escape for one kind of edge cell. The focused pane's name is bold in the space's
/// colour; its box is the space's colour, and bold only without colour, which is how every
/// other focus on this screen reads in monochrome.
fn edge_style(ink: EdgeInk, space: theme::Role) -> String {
    let mono = crate::colorlevel::level() == crate::colorlevel::Level::Mono;
    match ink {
        EdgeInk::Rule => theme::fg(theme::Role::Rule),
        EdgeInk::Title => theme::fg(theme::Role::Detail),
        EdgeInk::Lit if !mono => theme::fg(space),
        EdgeInk::Lit | EdgeInk::TitleLit => format!("{}\x1b[1m", theme::fg(space)),
    }
}

/// One pane row with several tiles in it, and the lines around them.
///
/// Each tile shows the leftmost columns of its own snapshot. Every tile is
/// sized to its rectangle, so nothing is hidden, unless another client owns
/// that terminal's size and it is cropped. Scrollback, selection and the
/// cursor belong to the focused tile alone. Cells no tile covers are the
/// frame and the lines between the tiles, from [`pane_edges`].
fn paint_tiled_row(
    frame: &Frame,
    edges: &[Vec<Option<(char, EdgeInk)>>],
    pane_row: usize,
    pane_cols: usize,
    pane_left: usize,
    out: &mut String,
) {
    // One based, like every other position on the screen.
    let screen_row = pane_row + TABS + 1;
    for tile in &frame.tiles {
        if pane_row < tile.rect.y || pane_row >= tile.rect.y + tile.rect.rows {
            continue;
        }
        let local = pane_row - tile.rect.y;
        let (line, spans) = if tile.focused {
            window_row(
                frame.snapshot,
                frame.behind,
                frame.scroll,
                frame.anchor,
                local,
            )
        } else {
            (
                tile.snapshot
                    .and_then(|s| s.lines.get(local))
                    .map(String::as_str)
                    .unwrap_or(""),
                tile.snapshot
                    .and_then(|s| s.styles.get(local))
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            )
        };
        let selected = if tile.focused {
            frame
                .selection
                .and_then(|s| s.columns_on(local, tile.rect.cols))
        } else {
            None
        };
        out.push_str(&at(screen_row, pane_left + 1 + tile.rect.x));
        let links = links_of(if tile.focused {
            frame.snapshot
        } else {
            tile.snapshot
        });
        out.push_str(&pane_row_paint(
            frame,
            line,
            spans,
            links,
            tile.rect.cols,
            selected,
        ));
    }
    let space = theme::space_of(frame.space.unwrap_or_default());
    let row = edges.get(pane_row);
    let mut x = 0;
    while x < pane_cols {
        if covers(frame, x, pane_row) {
            x += 1;
            continue;
        }
        // A run of uncovered cells in one ink, written with one escape.
        let cell = |x: usize| row.and_then(|r| r.get(x).copied().flatten());
        let ink = cell(x).map(|(_, ink)| ink);
        out.push_str(&at(screen_row, pane_left + 1 + x));
        out.push_str(&ink.map_or_else(
            || theme::fg(theme::Role::Rule),
            |ink| edge_style(ink, space),
        ));
        while x < pane_cols && !covers(frame, x, pane_row) && cell(x).map(|(_, ink)| ink) == ink {
            match cell(x).map_or(' ', |(c, _)| c) {
                '\0' => {}
                c => out.push(c),
            }
            x += 1;
        }
        out.push_str(theme::RESET);
    }
}

/// One chrome row, left column or right: its text in its own colour, with the
/// keyboard's own row in reverse.
///
/// The state's colour goes on the whole row. A person scanning the column is looking for
/// the one row that needs them, and a coloured word inside an otherwise grey line is a
/// word they have to find first.
///
/// One run inside the row wears a colour of its own, which is the name of the space the
pub(crate) fn paint_row(
    line: &str,
    spans: &[SpanWire],
    links: &[String],
    cols: usize,
    selected: Option<std::ops::Range<usize>>,
) -> String {
    if spans.is_empty() && selected.is_none() {
        return width::fit(line, cols);
    }
    let mut out = String::with_capacity(cols * 2);
    let mut used = 0;
    let mut current: Option<&SpanWire> = None;
    // The grid puts a spacer after a wide character; it is a cell for the run arithmetic
    // and not a column for the screen, so it is counted and not painted.
    let mut skip_spacer = false;
    // Whether the cell just painted was part of the selection, so the escape is written
    // once at each edge of it rather than before every character.
    let mut reversed = false;
    // Whether a link is open on the terminal above. Closed before any change of run and
    // at the end of what this paints, so a link never reaches a cell it was not on: not
    // the next tile, not the chrome, not the next frame's rows.
    let mut linked = false;
    for (cell, c) in line.chars().enumerate() {
        // A grid cell never holds a control character when this machine's engine filled
        // it, but a screen relayed from another machine is only as honest as that
        // machine: an escape in one would reach this terminal as a command. Still counted
        // as a cell, so the styles after it stay aligned.
        if c.is_control() {
            continue;
        }
        if skip_spacer && c == ' ' {
            skip_spacer = false;
            continue;
        }
        skip_spacer = width::columns(c) == 2;
        let w = width::columns(c);
        if used + w > cols {
            break;
        }
        let span = spans
            .iter()
            .find(|s| cell >= s.at && cell < s.at + s.len)
            .filter(|s| !is_plain(s));
        // Reverse video for the selection, and reverse deliberately: a background colour
        // would have to win against whatever the program painted underneath, and on a dark
        // answer in a light theme it loses. Reverse is what every terminal marks a
        // selection with, so it is also what a person already recognises.
        let mark = selected.as_ref().is_some_and(|r| r.contains(&used));
        if span.map(|s| s as *const _) != current.map(|s| s as *const _) || mark != reversed {
            if std::mem::take(&mut linked) {
                out.push_str(crate::hyperlink::CLOSE);
            }
            out.push_str("\x1b[0m");
            if let Some(span) = span {
                out.push_str(&sgr(span));
            }
            if mark {
                out.push_str("\x1b[7m");
            }
            if let Some(uri) = span
                .and_then(|s| s.link)
                .and_then(|at| links.get(at as usize))
                .and_then(|uri| crate::hyperlink::clean_uri(uri))
            {
                out.push_str(&crate::hyperlink::open(uri));
                linked = true;
            }
            current = span;
            reversed = mark;
        }
        out.push(c);
        used += w;
    }
    if linked {
        out.push_str(crate::hyperlink::CLOSE);
    }
    if current.is_some() || reversed {
        out.push_str("\x1b[0m");
    }
    for _ in used..cols {
        out.push(' ');
    }
    out
}

/// One row of a terminal in the pane, faded to a single grey while a box is open, so the box is the only thing on the screen a person reads. The text stays,
/// only its colour, selection and links go, and they come back with the next frame.
fn pane_row_paint(
    frame: &Frame,
    line: &str,
    spans: &[SpanWire],
    links: &[String],
    cols: usize,
    selected: Option<std::ops::Range<usize>>,
) -> String {
    if !frame.spotlight() {
        return paint_row(line, spans, links, cols, selected);
    }
    format!(
        "{}{}{}",
        theme::fg(theme::Role::Unobserved),
        paint_row(line, &[], &[], cols, None),
        theme::RESET
    )
}

fn is_plain(s: &SpanWire) -> bool {
    s.fg.is_none() && s.bg.is_none() && s.attrs == 0 && s.link.is_none()
}

/// The focus list, as rows of text with the role each wears.
///
/// Oldest first in, bottom-pinned out: `offset` counts rows hidden below the
/// window, so zero shows the tail and the list follows new events by itself.
/// A short list shows what it has; an empty one says why, with the way out
/// and the command that fills it. Pure, so a test reads rows, not a screen.
pub fn paint_focus_list(
    events: &[FocusEvent],
    width: usize,
    offset: usize,
    height: usize,
) -> Vec<(String, theme::Role)> {
    if events.is_empty() {
        return vec![(
            "no hook reports yet: F closes, qoro hooks install".to_string(),
            theme::Role::Faint,
        )];
    }
    // Clamped before slicing, not after: an offset past the head shows the
    // head, never an empty window.
    let height = height.max(1);
    let offset = offset.min(events.len().saturating_sub(height));
    let end = events.len() - offset;
    let start = end.saturating_sub(height);
    events[start..end]
        .iter()
        .map(|event| {
            let (mark, role) = match event.kind.as_str() {
                "user" => ("›", theme::Role::Text),
                "blocked" => ("!", theme::Role::Blocked),
                "done" => ("·", theme::Role::Faint),
                // `tool`, and anything a newer daemon stored that this build
                // does not name: one collapsed row, never a failure.
                _ => ("▸", theme::Role::Faint),
            };
            // Fitted here, so every row paints exactly `width` columns and
            // nothing of the terminal shows through the list.
            (width::fit(&format!("{mark} {}", event.label), width), role)
        })
        .collect()
}

/// The focus list over the pane, painted after the frame like the palette.
///
/// Full-width rows, so nothing of the terminal shows through. The cursor is
/// left hidden behind it: the list takes no input, and a caret on a
/// read-only view points at nothing. The next repaint shows it again.
pub fn paint_focus(frame: &Frame, events: &[FocusEvent], offset: usize, out: &mut String) {
    let left = left_for(frame.cols, frame.collapsed, frame.sidebar_width);
    let right = 0;
    let (pane_cols, pane_rows) = pane_size_for(
        frame.cols,
        frame.lines,
        frame.collapsed,
        right,
        frame.sidebar_width,
    );
    let rows = paint_focus_list(events, pane_cols as usize, offset, pane_rows as usize);
    for (i, (text, role)) in rows.iter().enumerate().take(pane_rows as usize) {
        out.push_str(&at(TABS + 1 + i, left + 1));
        out.push_str(&theme::fg(*role));
        out.push_str(text);
        out.push_str(theme::RESET);
    }
    out.push_str("\x1b[?25l");
}

/// An agent's last answer as a page over the pane: its title on the first row, then the
/// answer laid out to the pane's width from where the reader has scrolled to, every row of
/// the pane covered so nothing of the terminal shows through.
pub fn paint_answer(frame: &Frame, answer: &crate::answer::Answer, out: &mut String) {
    let left = left_for(frame.cols, frame.collapsed, frame.sidebar_width);
    let (pane_cols, pane_rows) = pane_size_for(
        frame.cols,
        frame.lines,
        frame.collapsed,
        0,
        frame.sidebar_width,
    );
    let (cols, rows) = (pane_cols as usize, pane_rows as usize);
    if cols == 0 || rows == 0 {
        return;
    }
    let body = answer.rows_for(cols);
    let height = rows - 1;
    let first = answer.offset.min(body.len().saturating_sub(height));
    out.push_str(&at(TABS + 1, left + 1));
    out.push_str(&theme::fg(theme::Role::Title));
    out.push_str(&width::text(
        &format!(
            " last answer: {}   {} of {} rows",
            answer.title,
            first + height.min(body.len()),
            body.len()
        ),
        cols,
    ));
    out.push_str(theme::RESET);
    for i in 0..height {
        out.push_str(&at(TABS + 2 + i, left + 1));
        match body.get(first + i) {
            Some(row) => out.push_str(row),
            None => {
                out.push_str(&theme::fg(theme::Role::Text));
                out.push_str(&" ".repeat(cols));
                out.push_str(theme::RESET);
            }
        }
    }
    out.push_str("\x1b[?25l");
}

/// The escape sequence for one run's style, at the session's colour level.
fn sgr(span: &SpanWire) -> String {
    sgr_in(span, crate::colorlevel::level())
}

/// The escape sequence for one run's style at an explicit colour level. Pure,
/// so the downgrade is tested without touching process globals. Attributes
/// (bold, inverse and the rest) always survive: they are not colour, and under
/// `NO_COLOR` they are what is left to tell runs apart.
fn sgr_in(span: &SpanWire, level: crate::colorlevel::Level) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (bit, code) in [
        (attrs::BOLD, "1"),
        (attrs::DIM, "2"),
        (attrs::ITALIC, "3"),
        (attrs::UNDERLINE, "4"),
        (attrs::INVERSE, "7"),
        (attrs::STRIKE, "9"),
    ] {
        if span.attrs & bit != 0 {
            parts.push(code.to_string());
        }
    }
    parts.extend(span.fg.and_then(|fg| colour_in(fg, true, level)));
    parts.extend(span.bg.and_then(|bg| colour_in(bg, false, level)));
    format!("\x1b[{}m", parts.join(";"))
}

/// A colour, written the way every terminal since xterm reads it.
///
/// The first sixteen palette entries get their own short codes rather than `38;5;n`,
/// because those are the ones a person has themed and some terminals treat the two spellings
/// differently.
/// A colour at an explicit colour level, or nothing when the level leaves no
/// colour to spend. Pure, see [`sgr_in`]. Indexed entries pass through
/// untouched at every coloured level: 0 to 15 belong to the pane program's
/// own theme. RGB values pass through on truecolour and map to the nearest
/// palette entry in 16 to 255 on the palette level, never into the themed
/// range (see `colorlevel::nearest_256`).
fn colour_in(c: ColourWire, foreground: bool, level: crate::colorlevel::Level) -> Option<String> {
    use crate::colorlevel::Level as ColourLevel;
    match level {
        ColourLevel::Mono => None,
        ColourLevel::True => Some(match c {
            ColourWire::Indexed { i } if i < 8 => {
                format!("{}", if foreground { 30 + i } else { 40 + i })
            }
            ColourWire::Indexed { i } if i < 16 => {
                format!("{}", if foreground { 82 + i } else { 92 + i })
            }
            ColourWire::Indexed { i } => format!("{};5;{i}", if foreground { 38 } else { 48 }),
            ColourWire::Rgb { r, g, b } => {
                format!("{};2;{r};{g};{b}", if foreground { 38 } else { 48 })
            }
        }),
        ColourLevel::Palette => Some(match c {
            ColourWire::Indexed { i } if i < 8 => {
                format!("{}", if foreground { 30 + i } else { 40 + i })
            }
            ColourWire::Indexed { i } if i < 16 => {
                format!("{}", if foreground { 82 + i } else { 92 + i })
            }
            ColourWire::Indexed { i } => format!("{};5;{i}", if foreground { 38 } else { 48 }),
            ColourWire::Rgb { r, g, b } => {
                let near = crate::colorlevel::nearest_256(r, g, b);
                format!("{};5;{near}", if foreground { 38 } else { 48 })
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Paint-time clock for frame tests: every row here is unstamped, so
    /// tiers never apply, but the clock is named rather than zero so a
    /// stamped row added later reads a real tier instead of a silent Fresh.
    const TEST_NOW: i64 = 1_750_000_000;

    fn spaces() -> Vec<WorkspaceRow> {
        vec![WorkspaceRow {
            id: 1,
            name: "app".to_string(),
            terminals: 1,
            asleep: false,
            sleeping: Vec::new(),
            closable: true,
        }]
    }

    fn row(terminal: u32, program: &str, state: &str) -> AgentRow {
        AgentRow {
            news: false,
            terminal,
            name: None,
            workspace: 1,
            agent: true,
            program: Some(program.to_string()),
            behind: None,
            state: state.to_string(),
            generation: Some(1),
            readiness: "ready".to_string(),
            source: None,
            rule: None,
            manifest: None,
            agent_session: None,
            agent_title: None,
            last_active_at_unix: None,
            cwd: None,
            slept_at_unix: None,
            activity: None,
            pgid: None,
            discards: None,
        }
    }

    fn snapshot(lines: &[&str]) -> SnapshotWire {
        SnapshotWire {
            modes: Default::default(),
            behind: 0,
            cols: 40,
            rows: lines.len(),
            cursor: (0, 0),
            lines: lines.iter().map(|l| l.to_string()).collect(),
            styles: Vec::new(),
            links: Vec::new(),
        }
    }

    #[test]
    fn the_pane_is_what_is_left_after_the_index_and_the_status_line() {
        // Two rows at the bottom now: the notice and the keys.
        assert_eq!(pane_size(80, 24), (80 - sidebar::WIDTH as u16 - 1, 21));
    }

    #[test]
    fn a_window_too_small_for_the_index_still_leaves_a_pane() {
        // Never zero and never a panic: a person dragging a window narrow is not an error.
        let (cols, rows) = pane_size(4, 1);
        assert!(cols >= 1 && rows >= 1);
    }

    fn sky_pet() -> qoro_runtime::avatar::Avatar {
        qoro_runtime::avatar::Avatar {
            body: qoro_runtime::avatar::BODY.into(),
            hair: "mohawk".into(),
            eyes: "squint".into(),
            hair_ink: 39,
            body_ink: 33,
        }
    }

    /// The overlay stamps the pet itself: cursor moves plus art rows, in the
    /// pet's own inks, at the right of the foot's first five rows.
    #[test]
    fn the_overlay_stamps_the_pet_above_the_foot() {
        let pet = sky_pet();
        let (cols, art) = sidebar::avatar_overlay(&pet, true);
        let rows = sidebar::avatar_overlay_rows(SIDEBAR_TOP, 38);
        let col = sidebar::WIDTH.saturating_sub(cols);
        let out = avatar_overlay_paint(38, sidebar::WIDTH, false, true, Some(&pet), true);
        assert!(out.starts_with(&at(rows[0], col)), "{out:?}");
        assert!(out.contains(&at(rows[1], col)), "{out:?}");
        assert!(out.contains("\x1b[38;5;39m"), "hair ink: {out:?}");
        assert!(out.contains("\x1b[48;5;33m"), "body ink: {out:?}");
        // Five rows of half blocks, the pet's own width, stamped byte for byte.
        for row in &art {
            assert!(out.contains(row.as_str()), "{out:?}");
        }
        assert_eq!(rows.len(), qoro_runtime::avatar::ART_ROWS, "{rows:?}");
    }

    /// Folded, narrow, short, colourless, or petless: no overlay bytes at all.
    #[test]
    fn the_overlay_stays_quiet_when_the_pet_does_not_fit() {
        let pet = sky_pet();
        assert!(avatar_overlay_paint(38, sidebar::WIDTH, true, true, Some(&pet), true).is_empty());
        assert!(avatar_overlay_paint(38, 10, false, true, Some(&pet), true).is_empty());
        assert!(avatar_overlay_paint(8, sidebar::WIDTH, false, true, Some(&pet), true).is_empty());
        assert!(
            avatar_overlay_paint(38, sidebar::WIDTH, false, false, Some(&pet), true).is_empty()
        );
        assert!(avatar_overlay_paint(38, sidebar::WIDTH, false, true, None, true).is_empty());
        assert!(
            avatar_overlay_paint(38, sidebar::WIDTH, false, true, Some(&pet), false).is_empty()
        );
    }

    /// A screen relayed from another machine can hold an escape in a cell. The styled path
    /// draws cells one by one, and it must drop it as the plain path does.
    #[test]
    fn a_relayed_screen_cannot_smuggle_an_escape() {
        let out = paint_row("a\x1b]52;c;aGk=\x07b", &[], &[], 20, Some(0..3));
        assert!(!out.contains('\x07'), "{out:?}");
        assert!(!out.contains("\x1b]"), "{out:?}");
        assert!(out.contains('b'), "{out:?}");
    }

    /// An agent TUI survives the paint byte for byte: truecolour runs, box
    /// drawing and an inverse sweep all arrive on screen, cropped to the tile
    /// and never spilling past it.
    #[test]
    fn an_agent_tui_with_truecolour_and_inverse_paints_faithfully() {
        let line = "┃  Ask anything… \"Fix it\"";
        let spans = vec![
            SpanWire {
                at: 0,
                len: 1,
                fg: Some(ColourWire::Rgb {
                    r: 92,
                    g: 156,
                    b: 245,
                }),
                bg: None,
                attrs: 0,
                link: None,
            },
            SpanWire {
                at: 3,
                len: 12,
                fg: None,
                bg: None,
                attrs: attrs::INVERSE,
                link: None,
            },
        ];
        let painted = paint_row(line, &spans, &[], 40, None);
        // Truecolour arrives as truecolour, inverse as inverse, and a reset
        // closes every run so no style bleeds into the next cell.
        assert!(painted.contains("38;2;92;156;245"), "{painted:?}");
        assert!(painted.contains("\x1b[7m"), "{painted:?}");
        assert!(painted.contains("Ask anything"), "{painted:?}");
        assert!(
            painted.ends_with("\x1b[0m") || !painted.contains("\x1b[7m \x1b"),
            "a style leaked past its run: {painted:?}"
        );
        let mut plain = String::new();
        let mut chars = painted.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
                continue;
            }
            plain.push(c);
        }
        assert_eq!(crate::width::width(&plain), 40, "{plain:?}");
    }

    /// Under `NO_COLOR` a pane run keeps its attributes and loses its colours:
    /// bold survives, truecolour and indexed runs do not.
    #[test]
    fn a_monochrome_run_keeps_attributes_and_drops_colour() {
        use crate::colorlevel::Level;
        let span = SpanWire {
            at: 0,
            len: 3,
            fg: Some(ColourWire::Rgb {
                r: 92,
                g: 156,
                b: 245,
            }),
            bg: Some(ColourWire::Indexed { i: 200 }),
            attrs: attrs::BOLD,
            link: None,
        };
        let mono = sgr_in(&span, Level::Mono);
        assert!(mono.contains('1'), "bold lost: {mono:?}");
        assert!(
            !mono.contains("38;") && !mono.contains("48;"),
            "colour leaked into a monochrome run: {mono:?}"
        );
    }

    /// On the palette level an RGB run arrives as the nearest 256 entry and
    /// never as truecolour; indexed runs pass through untouched.
    #[test]
    fn a_palette_run_downgrades_rgb_and_passes_indexed_through() {
        use crate::colorlevel::Level;
        let rgb = SpanWire {
            at: 0,
            len: 1,
            fg: Some(ColourWire::Rgb {
                r: 92,
                g: 156,
                b: 245,
            }),
            bg: None,
            attrs: 0,
            link: None,
        };
        let downgraded = sgr_in(&rgb, Level::Palette);
        assert!(downgraded.contains("38;5;75"), "{downgraded:?}");
        assert!(!downgraded.contains("38;2"), "{downgraded:?}");
        let indexed = SpanWire {
            at: 0,
            len: 1,
            fg: Some(ColourWire::Indexed { i: 3 }),
            bg: Some(ColourWire::Indexed { i: 200 }),
            attrs: 0,
            link: None,
        };
        let kept = sgr_in(&indexed, Level::Palette);
        assert!(kept.contains("33"), "{kept:?}");
        assert!(kept.contains("48;5;200"), "{kept:?}");
        let full = sgr_in(&rgb, Level::True);
        assert!(full.contains("38;2;92;156;245"), "{full:?}");
    }

    fn linked(at: usize, len: usize, link: u32) -> SpanWire {
        SpanWire {
            at,
            len,
            fg: None,
            bg: None,
            attrs: 0,
            link: Some(link),
        }
    }

    /// A linked run opens after its colour and closes before the next run starts, so
    /// the link covers exactly its own text and two neighbouring links never merge.
    #[test]
    fn a_link_opens_after_its_colour_and_closes_before_the_next_run() {
        let spans = vec![
            SpanWire {
                at: 0,
                len: 2,
                fg: Some(ColourWire::Indexed { i: 1 }),
                bg: None,
                attrs: 0,
                link: Some(0),
            },
            linked(2, 2, 1),
        ];
        let links = [
            "https://a.example".to_string(),
            "https://b.example".to_string(),
        ];
        let painted = paint_row("abcd tail", &spans, &links, 20, None);
        let open_a = "\x1b]8;;https://a.example\x1b\\";
        let open_b = "\x1b]8;;https://b.example\x1b\\";
        let expected = format!(
            "\x1b[0m\x1b[31m{open_a}ab{close}\x1b[0m\x1b[m{open_b}cd{close}\x1b[0m tail",
            close = crate::hyperlink::CLOSE
        );
        assert!(painted.starts_with(&expected), "{painted:?}");
        assert_eq!(painted.matches("\x1b]8;;h").count(), 2);
        assert_eq!(painted.matches(crate::hyperlink::CLOSE).count(), 2);
    }

    /// A linked run cut by the edge of its tile is still closed, inside the tile, so the
    /// divider and the next tile are not part of the link.
    #[test]
    fn a_link_cut_by_the_tile_edge_is_closed_at_the_edge() {
        let spans = vec![linked(0, 30, 0)];
        let links = ["https://example.com".to_string()];
        let painted = paint_row(&"x".repeat(30), &spans, &links, 10, None);
        assert!(
            painted.ends_with(&format!("{}\x1b[0m", crate::hyperlink::CLOSE)),
            "{painted:?}"
        );
        assert_eq!(painted.matches(crate::hyperlink::CLOSE).count(), 1);
    }

    /// A target that could end the escape early, or an index past the table, paints its
    /// text unlinked.
    #[test]
    fn a_hostile_link_target_paints_its_text_unlinked() {
        let spans = vec![linked(0, 4, 0), linked(4, 4, 7)];
        let links = ["https://x\x07\x1b]2;owned".to_string()];
        let painted = paint_row("linklink", &spans, &links, 10, None);
        assert!(!painted.contains("\x1b]"), "{painted:?}");
        assert!(painted.contains("link"), "{painted:?}");
    }

    /// The whole screen with the naming box open over one idle shell.
    fn painted_prompt(prompt: &crate::run::Prompt, cols: u16, lines: u16) -> String {
        let rows = [row(1, "zsh", "idle")];
        paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces(),
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: None,
            cols,
            lines,
            message: None,
            hint: None,
            palette: None,
            prompt: Some(prompt),
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &crate::run::Behind {
                terminal: 0,
                from: 0,
                lines: Vec::new(),
                total: 0,
            },
            image_notice: None,
            chrome: Default::default(),
        })
    }

    fn prompt(what: crate::run::Naming, text: &str, was: &str) -> crate::run::Prompt {
        crate::run::Prompt {
            what,
            text: text.to_string(),
            was: was.to_string(),
        }
    }

    /// The escape that puts the caret `shown` columns into the naming box's field.
    fn caret_in_field(cols: u16, lines: u16, shown: usize) -> String {
        let b = PromptBox::at(cols, lines).expect("the box fits");
        at(b.text_row() + 1, b.field_col() + shown + 1)
    }

    /// The caret sits right after what has been typed.
    #[test]
    fn the_naming_box_cursor_sits_at_the_end_of_the_typed_text() {
        let painted = painted_prompt(&prompt(crate::run::Naming::Space, "ab", ""), 100, 24);
        assert!(
            painted.contains(&caret_in_field(100, 24, 2)),
            "no caret at the end of the typed text: {painted:?}"
        );
    }

    /// A name at the limit says so with a count, and the caret still sits after its last
    /// letter rather than on it. Behind the box, the tab's pill fades with the rest.
    #[test]
    fn a_name_at_the_limit_shows_the_count_and_a_caret_past_it() {
        let what = crate::run::Naming::RenameSession(1);
        let painted = painted_prompt(&prompt(what, &"x".repeat(what.limit()), ""), 100, 24);
        assert!(painted.contains("54/54"), "no count: {painted:?}");
        assert!(painted.contains("at the limit"), "{painted:?}");
        assert!(
            painted.contains(&caret_in_field(100, 24, 54)),
            "caret on the last letter: {painted:?}"
        );
        assert!(!painted.contains(&theme::tab_on()), "{painted:?}");
    }

    /// A name longer than the field, one given before today's limit, shows its end behind
    /// a `…`, and the caret sits after its last letter inside the box.
    #[test]
    fn a_name_wider_than_the_field_shows_its_end() {
        let long = format!("{}END", "x".repeat(80));
        let painted = painted_prompt(&prompt(crate::run::Naming::Remote, &long, ""), 70, 24);
        let b = PromptBox::at(70, 24).expect("fits");
        assert!(painted.contains("\u{2026}"), "{painted:?}");
        assert!(painted.contains("xEND"), "{painted:?}");
        assert!(
            painted.contains(&caret_in_field(70, 24, b.span() - 1)),
            "{painted:?}"
        );
    }

    /// A rename opens empty over the name it has, shown in the field and in the note.
    #[test]
    fn a_rename_opens_empty_over_the_name_it_has() {
        let what = crate::run::Naming::RenameTerminal(1);
        let painted = painted_prompt(&prompt(what, "", "build logs"), 100, 24);
        assert!(painted.contains(" build logs"), "{painted:?}");
        assert!(
            painted.contains("empty keeps \"build logs\""),
            "{painted:?}"
        );
        assert!(painted.contains(&caret_in_field(100, 24, 0)), "{painted:?}");
        assert!(painted.contains("^r reset"), "{painted:?}");
        // A space has no default to go back to.
        let space = painted_prompt(
            &prompt(crate::run::Naming::RenameSpace(1), "", "x"),
            100,
            24,
        );
        assert!(!space.contains("^r reset"), "{space:?}");
    }

    /// Each button is painted where a click on it lands, the answer last on the right.
    #[test]
    fn the_buttons_are_where_a_click_lands() {
        let what = crate::run::Naming::RenameTerminal(1);
        let b = PromptBox::at(100, 24).expect("fits");
        let buttons = b.buttons(what);
        let keys: Vec<u8> = buttons.iter().map(|b| b.key).collect();
        assert_eq!(keys, [0x15, 0x12, 0x1b, b'\r']);
        let painted = painted_prompt(&prompt(what, "", ""), 100, 24);
        for button in &buttons {
            assert!(
                painted.contains(&format!(" {} ", button.label)),
                "{painted:?}"
            );
            assert_eq!(
                prompt_click(100, 24, what, button.row, button.col + 1),
                Some(button.key)
            );
        }
        assert_eq!(prompt_click(100, 24, what, buttons[0].row, 0), None);
        // A window too narrow for all of them keeps the answer.
        let narrow = PromptBox::at(24, 24).expect("fits").buttons(what);
        assert!(narrow.iter().any(|b| b.key == b'\r'), "{:?}", narrow.len());
    }

    /// A short window keeps the field, the note and the buttons, without the blank rows.
    #[test]
    fn a_short_window_gets_the_tight_box() {
        let lines = (TABS + STATUS + PROMPT_TIGHT) as u16;
        let b = PromptBox::at(80, lines).expect("the tight box fits");
        assert!(!b.roomy);
        assert!(PromptBox::at(80, lines - 1).is_none());
        let painted = painted_prompt(&prompt(crate::run::Naming::Space, "ab", ""), 80, lines);
        assert!(
            painted.contains(&caret_in_field(80, lines, 2)),
            "{painted:?}"
        );
    }

    /// The keybind list is taller than an 80x24 window. It is clipped to fit, not skipped:
    /// skipped, the overlay painted nothing and still took the next key.
    #[test]
    fn the_keybinds_overlay_paints_on_an_80_by_24_window() {
        let rows = [row(1, "zsh", "idle")];
        let painted = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces(),
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: None,
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: Some(crate::run::Reading::Keybinds),
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &crate::run::Behind {
                terminal: 0,
                from: 0,
                lines: Vec::new(),
                total: 0,
            },
            image_notice: None,
            chrome: Default::default(),
        });
        assert!(painted.contains("keys, after"), "no keys box on 80x24");
        assert!(
            !painted.contains("\x1b[25;"),
            "the box ran past the last row"
        );
    }

    #[test]
    fn the_terminals_output_the_tabs_and_both_panels_are_in_the_frame() {
        let rows = [row(1, "zsh", "idle"), row(2, "claude", "working")];
        let snapshot = snapshot(&["hello from the shell"]);
        let painted = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces(),
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 2,
            snapshot: Some(&snapshot),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &crate::run::Behind {
                terminal: 0,
                from: 0,
                lines: Vec::new(),
                total: 0,
            },
            image_notice: None,
            chrome: Default::default(),
        });
        assert!(painted.contains("hello from the shell"));
        // Once on the tab and once in the column: the bar says what is open in this
        // space and the panel says what it is doing.
        assert!(painted.matches("claude").count() >= 2, "{painted:?}");
        assert!(strip(&painted).contains("d detach"));
        assert!(painted.contains("agents"), "no agents panel: {painted:?}");
        assert!(
            painted.contains("app"),
            "the space is not named: {painted:?}"
        );
    }

    #[test]
    fn a_split_pane_is_not_a_top_tab_but_stays_in_the_sidebar() {
        let rows = [row(1, "zsh", "idle"), row(2, "claude", "working")];
        let panes: std::collections::BTreeSet<u32> = std::collections::BTreeSet::from([2]);
        let snapshot = snapshot(&["hello from the shell"]);
        let painted = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces(),
            panes,
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &crate::run::Behind {
                terminal: 0,
                from: 0,
                lines: Vec::new(),
                total: 0,
            },
            image_notice: None,
            chrome: Default::default(),
        });
        // The tab bar is the first row: between the cursor address and the reset.
        let bar_start = painted.find("\x1b[1;").expect("no tab bar row");
        let bar_end = painted[bar_start..].find("\x1b[0m").expect("no bar reset");
        let bar = &painted[bar_start..bar_start + bar_end];
        assert!(bar.contains("zsh"), "tab missing from bar: {bar:?}");
        assert!(!bar.contains("claude"), "split pane leaked as tab: {bar:?}");
        assert!(
            painted.contains("claude"),
            "pane agent left sidebar: {painted:?}"
        );
    }

    #[test]
    fn unfocused_tabs_wear_the_focused_space_while_focus_keeps_the_pill() {
        // The bar shows one space's terminals: every unfocused tab wears that
        // space's ink, and only the focused tab wears the pink pill.
        let rows = [row(1, "zsh", "idle"), row(3, "sh", "idle")];
        let snapshot = snapshot(&["hello from the shell"]);
        let painted = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces(),
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &crate::run::Behind {
                terminal: 0,
                from: 0,
                lines: Vec::new(),
                total: 0,
            },
            image_notice: None,
            chrome: Default::default(),
        });
        let bar_start = painted.find("\x1b[1;").expect("no tab bar row");
        let bar_end = painted[bar_start..].find("\x1b[0m").expect("no bar reset");
        let bar = &painted[bar_start..bar_start + bar_end];
        assert!(
            bar.contains(&crate::theme::tab_in(crate::theme::space_of(1))),
            "unfocused tab lost its space colour: {bar:?}"
        );
        assert!(
            bar.contains(&crate::theme::tab_on()),
            "focused tab lost its pill: {bar:?}"
        );
    }

    #[test]
    fn a_focused_pane_leaves_the_bar_without_a_bracket() {
        // Panes are sub-terminals, never tabs: with the keyboard in one, no
        // tab wears the bracket, and the tile border carries the focus alone.
        let rows = [row(1, "zsh", "idle"), row(2, "claude", "working")];
        let panes: std::collections::BTreeSet<u32> = std::collections::BTreeSet::from([2]);
        let snapshot = snapshot(&["hello from the shell"]);
        let painted = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces(),
            panes: panes.clone(),
            space: Some(1),
            focused: 2,
            snapshot: Some(&snapshot),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &crate::run::Behind {
                terminal: 0,
                from: 0,
                lines: Vec::new(),
                total: 0,
            },
            image_notice: None,
            chrome: Default::default(),
        });
        let bar_start = painted.find("\x1b[1;").expect("no tab bar row");
        let bar_end = painted[bar_start..].find("\x1b[0m").expect("no bar reset");
        let addr_end = painted[bar_start..].find('H').expect("no bar address") + 1;
        let bar = &painted[bar_start + addr_end..bar_start + bar_end];
        assert!(bar.contains("zsh"), "tab missing from bar: {bar:?}");
        assert!(!bar.contains("[1 "), "no tab owns the keyboard: {bar:?}");
    }

    #[test]
    fn the_menu_teaches_the_key_each_row_replaces() {
        let rows = [row(1, "zsh", "idle")];
        let painted = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces(),
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: None,
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: Some((0, &crate::run::MenuKind::Main)),
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &crate::run::Behind {
                terminal: 0,
                from: 0,
                lines: Vec::new(),
                total: 0,
            },
            image_notice: None,
            chrome: Default::default(),
        });
        assert!(painted.contains("new tab"), "menu missing row: {painted:?}");
        assert!(
            painted.contains("ctrl-a c"),
            "menu hides its key: {painted:?}"
        );
        assert!(painted.contains("split side by side"), "{painted:?}");
        assert!(painted.contains("rename space"), "{painted:?}");
        assert!(painted.contains("close tab"), "{painted:?}");
        assert!(
            painted.contains("ctrl-a x"),
            "close tab hides its key: {painted:?}"
        );
        assert!(painted.contains(" menu"), "menu missing title: {painted:?}");
        assert!(
            strip(&painted).contains("enter choose"),
            "menu missing hint: {painted:?}"
        );
    }

    #[test]
    fn the_menu_is_centred_not_hugging_the_column() {
        let rows = crate::run::menu_rows(&crate::run::MenuKind::Main);
        let Some((left, top, w, height)) = menu_rect(80, 24, &rows, " menu") else {
            panic!("no menu rect on 80x24");
        };
        assert_eq!(height, crate::run::MENU.len() + 3);
        assert_eq!(left, (80 - w) / 2, "menu not centred across");
        assert_eq!(top, (24 - height) / 2, "menu not centred down");
        assert!(left > 0, "menu still hugs the left edge");
    }

    #[test]
    fn the_palette_names_its_filters_and_keeps_the_one_in_force_when_narrow() {
        assert_eq!(palette_chips(Filter::All, "", 58), Filter::ORDER.to_vec());
        let long = "x".repeat(40);
        assert_eq!(
            palette_chips(Filter::Spaces, &long, 58),
            vec![Filter::Spaces],
            "a long query hid the filter in force"
        );
        assert!(palette_chips(Filter::All, &long, 58).is_empty());
        // The row the chips share is still exactly the box: query room plus chips.
        let chips = palette_chips(Filter::All, "", 58);
        assert_eq!(chips_width(&chips), 31);
    }

    #[test]
    fn a_menu_click_address_matches_the_painted_rows() {
        // Row 0 sits right under the top edge, which carries the title.
        let rows = crate::run::menu_rows(&crate::run::MenuKind::Main);
        let Some((left, top, _, _)) = menu_rect(80, 24, &rows, " menu") else {
            panic!("no menu rect on 80x24");
        };
        let row = (top + BOX_FIRST_ROW + 4 + 1) as u16;
        let col = (left + 3 + 1) as u16;
        assert_eq!(
            menu_row_at(80, 24, &rows, " menu", row, col),
            Some(4),
            "close tab row"
        );
        assert_eq!(
            menu_row_at(80, 24, &rows, " menu", 1, 1),
            None,
            "outside reads as a row"
        );
    }

    #[test]
    fn a_terminal_line_cannot_overwrite_the_rule_or_run_past_the_window() {
        let rows = [row(1, "zsh", "idle")];
        let long = "x".repeat(500);
        let snapshot = snapshot(&[&long]);
        let painted = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces(),
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 40,
            lines: 5,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &crate::run::Behind {
                terminal: 0,
                from: 0,
                lines: Vec::new(),
                total: 0,
            },
            image_notice: None,
            chrome: Default::default(),
        });
        let (pane_cols, _) = pane_size(40, 5);
        // Exactly the pane's width of it, and not one column more.
        assert!(painted.contains(&"x".repeat(pane_cols as usize)));
        assert!(!painted.contains(&"x".repeat(pane_cols as usize + 1)));
    }

    #[test]
    fn a_frame_with_no_snapshot_yet_still_paints() {
        let rows = [row(1, "zsh", "starting")];
        let painted = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces(),
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: None,
            cols: 40,
            lines: 5,
            message: Some("starting a terminal"),
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &crate::run::Behind {
                terminal: 0,
                from: 0,
                lines: Vec::new(),
                total: 0,
            },
            image_notice: None,
            chrome: Default::default(),
        });
        assert!(painted.contains("starting a terminal"));
    }

    #[test]
    fn a_coloured_run_reaches_the_screen_and_is_closed_after_it() {
        let mut snapshot = snapshot(&["red then plain"]);
        snapshot.styles = vec![vec![SpanWire {
            at: 0,
            len: 3,
            fg: Some(ColourWire::Indexed { i: 1 }),
            bg: None,
            attrs: attrs::BOLD,
            link: None,
        }]];
        let rows = [row(1, "sh", "idle")];
        let painted = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces(),
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            // Wide enough that the whole run fits beside the column, since what is being
            // read is where the colour stops and not where the pane does, and tall enough
            // for a pane under the tab bar and over the two status rows.
            cols: 60,
            lines: 5,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &crate::run::Behind {
                terminal: 0,
                from: 0,
                lines: Vec::new(),
                total: 0,
            },
            image_notice: None,
            chrome: Default::default(),
        });
        // Bold and red on, the text, and then off again: a run that is not closed leaks its
        // colour into the rest of the screen, which is the whole pane and then the status
        // line.
        assert!(painted.contains("\x1b[1;31mred"), "{painted:?}");
        assert!(painted.contains("\x1b[0m"), "{painted:?}");
        assert!(painted.contains("then plain"));
    }

    #[test]
    fn an_unstyled_row_carries_no_escape_of_its_own() {
        // The common case, and the one that must not pay for the feature: a plain row is
        // the same bytes it was before colour existed.
        let snapshot = snapshot(&["plain"]);
        let rows = [row(1, "sh", "idle")];
        let painted = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces(),
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 50,
            lines: 5,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &crate::run::Behind {
                terminal: 0,
                from: 0,
                lines: Vec::new(),
                total: 0,
            },
            image_notice: None,
            chrome: Default::default(),
        });
        // The row the shell printed, found by what it printed and then read as bytes. The
        // chrome around it resets after every piece it paints, so the pane's own run
        // starts at one of those resets; which rule glyph precedes it depends on whether
        // the panel drew a separator on that row, and that is not what is being tested.
        let (pane_cols, _) = pane_size(50, 5);
        let pane = painted
            .split(theme::RESET)
            .map(|after| after.split('\x1b').next().unwrap_or_default())
            .find(|piece| piece.starts_with("plain"))
            .unwrap_or_else(|| panic!("the pane never drew its row: {painted:?}"));
        // The whole row and nothing else in it, and not one escape of its own.
        assert_eq!(width::width(pane), pane_cols as usize, "{pane:?}");
        assert!(!pane.contains('\x1b'), "{pane:?}");
    }

    #[test]
    fn a_colour_run_lines_up_with_a_wide_character() {
        // The run is in cells and the paint is in columns. A style that started on the
        // spacer of a wide character would colour the wrong glyph.
        let mut snapshot = snapshot(&["中 X"]);
        snapshot.styles = vec![vec![SpanWire {
            at: 2,
            len: 1,
            fg: Some(ColourWire::Indexed { i: 2 }),
            bg: None,
            attrs: 0,
            link: None,
        }]];
        let rows = [row(1, "sh", "idle")];
        let painted = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces(),
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 30,
            lines: 5,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &crate::run::Behind {
                terminal: 0,
                from: 0,
                lines: Vec::new(),
                total: 0,
            },
            image_notice: None,
            chrome: Default::default(),
        });
        // `X` is at cell 2, so it is the green one, and the character before it is not.
        assert!(painted.contains("\x1b[32mX"), "{painted:?}");
    }

    /// Where the cursor goes is where the person is looking, and both conversions it needs
    /// were wrong in opposite directions.
    #[test]
    fn the_cursor_column_counts_columns_and_not_cells() {
        // A right trimmed row: the shell printed `"$ "` and the snapshot carries `"$"`, so
        // the cursor is in a cell the string does not have. It is one column past the `$`.
        assert_eq!(cursor_column("$", 2), 2);
        assert_eq!(cursor_column("", 0), 0);
        assert_eq!(cursor_column("", 7), 7);
        // Plain text, where cells and columns are the same number.
        assert_eq!(cursor_column("abc", 3), 3);
        assert_eq!(cursor_column("abc", 1), 1);
        // A wide character is two columns in one cell, and the grid's spacer after it is a
        // cell that is no columns at all.
        assert_eq!(cursor_column("\u{4e2d} X", 3), 3);
        assert_eq!(cursor_column("\u{4e2d} X", 1), 2);
        assert_eq!(cursor_column("\u{4e2d} \u{4e2d} ", 4), 4);
    }

    #[test]
    fn the_cursor_lands_on_the_column_the_shell_is_typing_at() {
        let mut snapshot = snapshot(&["$"]);
        snapshot.cursor = (0, 2);
        let rows = [row(1, "sh", "idle")];
        let painted = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces(),
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &crate::run::Behind {
                terminal: 0,
                from: 0,
                lines: Vec::new(),
                total: 0,
            },
            image_notice: None,
            chrome: Default::default(),
        });
        // One row down for the tab bar, and the pane starts after the column and its rule.
        let want = format!("\x1b[{};{}H", 1 + TABS, 2 + sidebar::WIDTH + 2);
        assert!(
            painted.ends_with(&format!("{want}\x1b[?25h")),
            "{painted:?}"
        );
    }

    /// A cursor the program hid stays hidden: no placement, no show. A shown
    /// cursor painted over a child that hid its own is the flicker.
    #[test]
    fn a_hidden_snapshot_cursor_is_neither_placed_nor_shown() {
        let mut snapshot = snapshot(&["$"]);
        snapshot.cursor = (0, 2);
        snapshot.modes.cursor_visible = false;
        let rows = [row(1, "sh", "idle")];
        let painted = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces(),
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &crate::run::Behind {
                terminal: 0,
                from: 0,
                lines: Vec::new(),
                total: 0,
            },
            image_notice: None,
            chrome: Default::default(),
        });
        let placed = format!("\x1b[{};{}H", 1 + TABS, 2 + sidebar::WIDTH + 2);
        assert!(!painted.contains(&placed), "hid cursor placed: {painted:?}");
        assert!(!painted.contains("?25h"), "hid cursor shown: {painted:?}");
    }

    /// A window too narrow for the left column is a real one: two panes side by side in a
    /// tmux split. Painting the column anyway made every row wider than the terminal, so
    /// every row wrapped, the screen slid, and every position written after that addressed
    /// the wrong row.
    #[test]
    fn a_window_too_narrow_for_the_left_column_paints_no_wider_than_it_is() {
        let rows = [row(1, "sh", "idle")];
        let snapshot = snapshot(&["x".repeat(200).as_str()]);
        for cols in [1u16, 5, 10, 20, 23, 24, 25, 40] {
            let painted = paint(&Frame {
                armed: None,
                rows: &rows,
                spaces: &spaces(),
                panes: std::collections::BTreeSet::new(),
                space: Some(1),
                focused: 1,
                snapshot: Some(&snapshot),
                cols,
                lines: 6,
                message: None,
                hint: None,
                palette: None,
                prompt: None,
                menu: None,
                collapsed: false,
                reading: None,
                selection: None,
                scroll: 0,
                tick: 0,
                now_unix: TEST_NOW,
                sidebar_width: sidebar::WIDTH,
                view: sidebar::View::Tree,
                seen: &sidebar::Seen::new(),
                unseen: &sidebar::Unseen::new(),
                anchor: 0,
                rows_back: 0,
                tiles: Vec::new(),
                behind: &crate::run::Behind::default(),
                image_notice: None,
                chrome: Default::default(),
            });
            // Every positioned run, measured as a terminal measures it.
            for run in painted.split("\x1b[").skip(1) {
                let Some(text) = run.strip_suffix("").and_then(|r| r.split_once('H')) else {
                    continue;
                };
                let drawn = text.1.split('\x1b').next().unwrap_or_default();
                assert!(
                    width::width(drawn) <= cols as usize,
                    "{cols} columns, painted {} of them: {drawn:?}",
                    width::width(drawn)
                );
            }
        }
    }

    /// A terminal that has produced nothing yet is every terminal for its first moments,
    /// and it still has to have a cursor: the frame turns the cursor off before painting.
    #[test]
    fn the_cursor_comes_back_on_even_before_the_first_snapshot() {
        let rows = [row(1, "sh", "starting")];
        let painted = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces(),
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: None,
            cols: 40,
            lines: 5,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &crate::run::Behind {
                terminal: 0,
                from: 0,
                lines: Vec::new(),
                total: 0,
            },
            image_notice: None,
            chrome: Default::default(),
        });
        assert!(painted.contains("\x1b[?25l"));
        assert!(painted.ends_with("\x1b[?25h"), "{painted:?}");
    }

    /// The last answer covers every row of the pane: its title on the first, the page
    /// under it, blank rows past its end, and the key row says how to read it.
    #[test]
    fn an_open_answer_covers_the_pane_and_the_key_row_says_how_to_read_it() {
        let rows = [row(1, "claude", "idle")];
        let (seen, unseen) = (sidebar::Seen::new(), sidebar::Unseen::new());
        let behind = crate::run::Behind::default();
        let panes = std::collections::BTreeSet::new();
        let spaces = spaces();
        let frame = Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces,
            panes,
            space: Some(1),
            focused: 1,
            snapshot: None,
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &seen,
            unseen: &unseen,
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &behind,
            image_notice: None,
            chrome: Chrome {
                answer: true,
                ..Default::default()
            },
        };
        let answer = crate::answer::Answer::new(
            "claude".into(),
            "# Done\n\nThe key row follows the keyboard.".into(),
        );
        let mut out = String::new();
        paint_answer(&frame, &answer, &mut out);
        let (_, pane_rows) = pane_size_for(80, 24, false, 0, sidebar::WIDTH);
        let moves = (0..pane_rows as usize)
            .filter(|i| out.contains(&at(TABS + 1 + i, sidebar::WIDTH + 2)))
            .count();
        assert_eq!(
            moves, pane_rows as usize,
            "a pane row was left showing through"
        );
        let shown = strip(&out);
        assert!(shown.contains("last answer: claude"), "{shown:?}");
        assert!(
            shown.contains("Done") && shown.contains("follows the keyboard"),
            "{shown:?}"
        );
        let painted = paint(&frame);
        assert!(
            strip(&painted).contains("j/k scroll"),
            "the key row did not say how to read"
        );
    }
    /// The key row follows the keyboard: the prefix commands in the pane, what the column
    /// answers to after a click in it, every command while `ctrl-a` waits. Each drops its
    /// least used entries on a short row and keeps the way out on every one.
    #[test]
    fn the_key_row_follows_the_keyboard_and_keeps_the_way_out() {
        let open = Place {
            fold: true,
            collapsed: false,
        };
        let plain = |context, cols| strip(&key_row(context, open, cols));
        let pane = plain(KeyContext::Pane, 200);
        assert!(pane.starts_with(" ctrl-a:  space find"), "{pane:?}");
        let column = plain(KeyContext::Sidebar, 200);
        assert!(column.contains("click open"), "{column:?}");
        assert!(column.contains("g tree/priority"), "{column:?}");
        let prefix = plain(KeyContext::Prefix, 200);
        assert!(
            !prefix.contains("ctrl-a:"),
            "the prefix is already down: {prefix:?}"
        );
        assert!(prefix.contains("z zoom"), "{prefix:?}");
        for context in [KeyContext::Pane, KeyContext::Sidebar, KeyContext::Prefix] {
            for cols in [5usize, 10, 11, 18, 20, 40, 80, 120, 200] {
                let row = plain(context, cols);
                assert_eq!(width::width(&row), cols, "{context:?} at {cols}: {row:?}");
                // Under one entry the way out is said plainly, cut to fit.
                assert!(
                    row.contains("d detach") || (cols < 10 && row.starts_with(" d")),
                    "{context:?} at {cols}: {row:?}"
                );
            }
        }
        // The cut takes the highest rank first, the latest of a rank first: at eighty
        // columns fold, focus and split go and the rest stay in order.
        assert_eq!(
            plain(KeyContext::Pane, 80).trim_end(),
            " ctrl-a:  space find  c new tab  n next  m menu  v split  ? keys  d detach"
        );
        // Where nothing folds the row offers no fold, and the strip has no edge to drag.
        let strip_place = Place {
            fold: false,
            collapsed: true,
        };
        let strip_row = strip(&key_row(KeyContext::Sidebar, strip_place, 200));
        assert!(!strip_row.contains("fold"), "{strip_row:?}");
        assert!(!strip_row.contains("drag edge"), "{strip_row:?}");
        assert!(strip_row.contains("b show column"), "{strip_row:?}");
    }
    /// The way out is the one key a person stuck in here has to be able to read, so it is
    /// never the one the cut takes.
    #[test]
    fn the_key_list_always_keeps_the_way_out() {
        let rows = [row(1, "sh", "idle")];
        for cols in [40u16, 60, 80, 100, 200] {
            let painted = paint(&Frame {
                armed: None,
                rows: &rows,
                spaces: &spaces(),
                panes: std::collections::BTreeSet::new(),
                space: Some(1),
                focused: 1,
                snapshot: None,
                cols,
                lines: 24,
                message: None,
                hint: None,
                palette: None,
                prompt: None,
                menu: None,
                collapsed: false,
                reading: None,
                selection: None,
                scroll: 0,
                tick: 0,
                now_unix: TEST_NOW,
                sidebar_width: sidebar::WIDTH,
                view: sidebar::View::Tree,
                seen: &sidebar::Seen::new(),
                unseen: &sidebar::Unseen::new(),
                anchor: 0,
                rows_back: 0,
                tiles: Vec::new(),
                behind: &crate::run::Behind::default(),
                image_notice: None,
                chrome: Default::default(),
            });
            let keys = strip(painted.rsplit(&at(24, 1)).next().unwrap_or_default());
            assert!(
                keys.contains("d detach"),
                "{cols} columns: no way out on the key row: {keys:?}"
            );
            assert!(
                width::width(&keys) <= cols as usize,
                "{cols} columns: the key row is wider than the window: {keys:?}"
            );
        }
    }

    /// How many commands there are, the one number the list below and the match share.
    const COMMANDS: usize = 38;

    /// A command's place in the list, checked against [`COMMANDS`] when this compiles.
    const fn place(at: usize) -> usize {
        assert!(at < COMMANDS, "a command numbered past COMMANDS");
        at
    }

    /// Where the keys overlay teaches a command: the start of its line, and the command's
    /// place in the list below.
    ///
    /// A new command fails to compile three times over until it is taught: the match here
    /// wants an arm, its number has to be under [`COMMANDS`] (checked at compile time), and
    /// raising [`COMMANDS`] makes the list below, an array of exactly that many, one short.
    /// A key nobody can find from inside is a key most people never press.
    fn taught_as(action: &crate::keys::Action) -> (usize, Option<&'static str>) {
        use crate::keys::Action;
        match action {
            // Typing is not a command: it goes to the terminal.
            Action::Input(_) => (const { place(0) }, None),
            Action::Next => (const { place(1) }, Some("ctrl-a n/p")),
            Action::Previous => (const { place(2) }, Some("ctrl-a n/p")),
            Action::Create => (const { place(3) }, Some("ctrl-a c")),
            Action::Close => (const { place(4) }, Some("ctrl-a x")),
            Action::Detach => (const { place(5) }, Some("ctrl-a d")),
            Action::Click { .. } => (const { place(6) }, Some("click")),
            Action::Drag { .. } => (const { place(7) }, Some("drag")),
            Action::Release { .. } => (const { place(8) }, Some("drag")),
            Action::Wheel { .. } => (const { place(9) }, Some("wheel")),
            Action::Redraw => (const { place(10) }, Some("ctrl-a l")),
            Action::NextSpace => (const { place(11) }, Some("ctrl-a s/S")),
            Action::PreviousSpace => (const { place(12) }, Some("ctrl-a s/S")),
            Action::CreateSpace => (const { place(13) }, Some("ctrl-a w")),
            Action::CloseSpace => (const { place(14) }, Some("ctrl-a W")),
            Action::Rename => (const { place(15) }, Some("ctrl-a r")),
            Action::RenameSpace => (const { place(16) }, Some("ctrl-a R")),
            Action::Collapse => (const { place(17) }, Some("ctrl-a b")),
            Action::Update => (const { place(18) }, Some("ctrl-a U")),
            Action::Menu => (const { place(19) }, Some("ctrl-a m")),
            Action::Keybinds => (const { place(20) }, Some("ctrl-a ?")),
            Action::Tab(_) => (const { place(21) }, Some("ctrl-a 1..9")),
            Action::Palette => (const { place(22) }, Some("ctrl-a space")),
            Action::ToggleView => (const { place(23) }, Some("ctrl-a g")),
            Action::Focus => (const { place(24) }, Some("ctrl-a F")),
            Action::SplitV => (const { place(25) }, Some("ctrl-a v/-")),
            Action::SplitH => (const { place(26) }, Some("ctrl-a v/-")),
            Action::PaneMove(_) => (const { place(27) }, Some("ctrl-a H/J/K/L")),
            Action::PaneNext => (const { place(28) }, Some("ctrl-a o")),
            Action::Zoom => (const { place(29) }, Some("ctrl-a z")),
            Action::DetachPane => (const { place(30) }, Some("ctrl-a X")),
            Action::Resize => (const { place(31) }, Some("ctrl-a e")),
            Action::Equalize => (const { place(32) }, Some("ctrl-a =")),
            Action::CopyMode => (const { place(33) }, Some("ctrl-a [")),
            Action::FoldSpace => (const { place(34) }, Some("ctrl-a f")),
            Action::Answer => (const { place(35) }, Some("ctrl-a M")),
            Action::Sleep => (const { place(36) }, Some("ctrl-a Z")),
            Action::UpNext => (const { place(37) }, Some("ctrl-a tab")),
        }
    }

    #[test]
    fn every_command_has_a_line_in_the_keys_overlay() {
        use crate::keys::Action;
        let every: [Action; COMMANDS] = [
            Action::Input(Vec::new()),
            Action::Next,
            Action::Previous,
            Action::Create,
            Action::Close,
            Action::Detach,
            Action::Click {
                row: 1,
                col: 1,
                shift: false,
            },
            Action::Drag {
                row: 1,
                col: 1,
                shift: false,
            },
            Action::Release {
                row: 1,
                col: 1,
                shift: false,
            },
            Action::Wheel {
                row: 1,
                col: 1,
                up: true,
                shift: false,
            },
            Action::Redraw,
            Action::NextSpace,
            Action::PreviousSpace,
            Action::CreateSpace,
            Action::CloseSpace,
            Action::Rename,
            Action::RenameSpace,
            Action::Collapse,
            Action::Update,
            Action::Menu,
            Action::Keybinds,
            Action::Tab(1),
            Action::Palette,
            Action::ToggleView,
            Action::Focus,
            Action::SplitV,
            Action::SplitH,
            Action::PaneMove(crate::layout::Move::Left),
            Action::PaneNext,
            Action::Zoom,
            Action::DetachPane,
            Action::Resize,
            Action::Equalize,
            Action::CopyMode,
            Action::FoldSpace,
            Action::Answer,
            Action::Sleep,
            Action::UpNext,
        ];
        // Every place has exactly one sample: no command listed twice, none left out.
        let mut numbers: Vec<usize> = every.iter().map(|a| taught_as(a).0).collect();
        numbers.sort();
        numbers.dedup();
        assert_eq!(numbers, (0..COMMANDS).collect::<Vec<_>>());
        for action in &every {
            let Some(taught) = taught_as(action).1 else {
                continue;
            };
            // The overlay writes a command without its prefix, which its title says.
            let key = taught.strip_prefix("ctrl-a ").unwrap_or(taught);
            assert!(
                KEY_GROUPS
                    .iter()
                    .flat_map(|(_, _, keys)| keys.iter())
                    .any(|(k, _)| *k == key),
                "{action:?} has no line in the keys overlay under {taught:?}"
            );
        }
    }

    /// Found in review: at 80 by 24 the keys drew nothing, and at 120 by 40 the second
    /// column ran off the box. Asserted on what is painted, not on what is listed.
    #[test]
    fn every_key_is_on_screen_at_80_by_24_and_at_120_by_40() {
        let rows = [row(1, "sh", "idle")];
        for (cols, lines) in [(80u16, 24u16), (120, 40), (160, 50)] {
            let painted = paint(&Frame {
                armed: None,
                rows: &rows,
                spaces: &spaces(),
                panes: std::collections::BTreeSet::new(),
                space: Some(1),
                focused: 1,
                snapshot: None,
                cols,
                lines,
                message: None,
                hint: None,
                palette: None,
                prompt: None,
                menu: None,
                collapsed: false,
                reading: Some(crate::run::Reading::Keybinds),
                selection: None,
                scroll: 0,
                tick: 0,
                now_unix: TEST_NOW,
                sidebar_width: sidebar::WIDTH,
                view: sidebar::View::Tree,
                seen: &sidebar::Seen::new(),
                unseen: &sidebar::Unseen::new(),
                anchor: 0,
                rows_back: 0,
                tiles: Vec::new(),
                behind: &crate::run::Behind::default(),
                image_notice: None,
                chrome: Default::default(),
            });
            let shown = strip(&painted);
            assert!(
                !shown.contains("more: / search"),
                "{cols}x{lines}: the keys were cut"
            );
            for (name, _, keys) in KEY_GROUPS {
                assert!(shown.contains(name), "{cols}x{lines}: no {name} group");
                for (key, word) in *keys {
                    // The key, its padding and what it does, as one run of the screen.
                    let run = shown
                        .lines()
                        .any(|l| l.contains(&format!(" {key} ")) && l.contains(word));
                    assert!(run, "{cols}x{lines}: {key} {word} is not on screen");
                }
            }
        }
    }

    #[test]
    fn a_pane_row_is_never_painted_over_the_status_line() {
        let rows = [row(1, "sh", "idle")];
        let snapshot = snapshot(&["PANE_CONTENT"]);
        for lines in [3u16, 4, 5] {
            let painted = paint(&Frame {
                armed: None,
                rows: &rows,
                spaces: &spaces(),
                panes: std::collections::BTreeSet::new(),
                space: Some(1),
                focused: 1,
                snapshot: Some(&snapshot),
                cols: 60,
                lines,
                message: Some("STATUS_LINE"),
                hint: None,
                palette: None,
                prompt: None,
                menu: None,
                collapsed: false,
                reading: None,
                selection: None,
                scroll: 0,
                tick: 0,
                now_unix: TEST_NOW,
                sidebar_width: sidebar::WIDTH,
                view: sidebar::View::Tree,
                seen: &sidebar::Seen::new(),
                unseen: &sidebar::Unseen::new(),
                anchor: 0,
                rows_back: 0,
                tiles: Vec::new(),
                behind: &crate::run::Behind::default(),
                image_notice: None,
                chrome: Default::default(),
            });
            // The two status rows own the bottom of the screen, so nothing else may
            // address either of them: the notice is the row above the keys.
            let notice_row = at(lines as usize - 1, 1);
            let keys_row = at(lines as usize, 1);
            let after = painted
                .split(&notice_row)
                .nth(1)
                .unwrap_or_default()
                .split(&keys_row)
                .next()
                .unwrap_or_default();
            assert!(after.contains("STATUS_LINE"), "{lines} lines: {painted:?}");
            assert!(
                painted
                    .rsplit(&keys_row)
                    .next()
                    .unwrap_or_default()
                    .contains("ctrl-a:"),
                "{lines} lines: the keys are not on the last row: {painted:?}"
            );
            for row in [&notice_row, &keys_row] {
                assert_eq!(
                    painted.matches(row).count(),
                    1,
                    "{lines} lines: something else drew on a status row: {painted:?}"
                );
            }
        }
    }

    /// A space wears its own colour on its rule, and the agent under it keeps the colour of
    /// its state: a person reads where from the rule and what from the dot.
    #[test]
    fn a_space_wears_its_colour_on_its_rule_and_the_agent_its_state() {
        let rows = [row(1, "claude", "working")];
        let painted = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces(),
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: None,
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &crate::run::Behind {
                terminal: 0,
                from: 0,
                lines: Vec::new(),
                total: 0,
            },
            image_notice: None,
            chrome: Default::default(),
        });
        let ink = theme::fg(theme::space_of(1));
        let raised = theme::selected_in(theme::space_of(1));
        // On the rule of the space. Either form, because the row may also have the keyboard.
        let worn = painted.matches(ink.as_str()).count() + painted.matches(raised.as_str()).count();
        assert!(
            worn >= 1,
            "the space colour appears {worn} times: {painted:?}"
        );
        // And the state the agent is in is still its own colour, not the space's.
        assert!(
            painted.contains(&theme::fg(theme::Role::Working))
                || painted.contains(&theme::selected_in(theme::Role::Working)),
            "the state lost its colour to the space: {painted:?}"
        );
    }

    /// Frame zero is still; past it the working row turns the spinner, and only that
    /// row changes. The status sentence keeps the word either way.
    #[test]
    fn the_working_row_turns_the_spinner_past_frame_zero() {
        let rows = [row(1, "claude", "working")];
        let paint_at = |tick: u64| {
            paint(&Frame {
                armed: None,
                rows: &rows,
                spaces: &spaces(),
                panes: std::collections::BTreeSet::new(),
                space: Some(1),
                focused: 1,
                snapshot: None,
                cols: 80,
                lines: 24,
                message: None,
                hint: None,
                palette: None,
                prompt: None,
                menu: None,
                collapsed: false,
                reading: None,
                selection: None,
                scroll: 0,
                tick,
                now_unix: TEST_NOW,
                sidebar_width: sidebar::WIDTH,
                view: sidebar::View::Tree,
                seen: &sidebar::Seen::new(),
                unseen: &sidebar::Unseen::new(),
                anchor: 0,
                rows_back: 0,
                tiles: Vec::new(),
                behind: &crate::run::Behind {
                    terminal: 0,
                    from: 0,
                    lines: Vec::new(),
                    total: 0,
                },
                image_notice: None,
                chrome: Default::default(),
            })
        };
        // The still, two-cell mark a working row wears at frame zero, and the frame it
        // turns to at tick one: `cell_mark(Role::Working, true, tick)` in sidebar.rs is
        // the source of truth these are copied from.
        let still = "\u{2818}\u{2802}";
        let frame_one = "\u{2818}\u{2800}";
        let first = paint_at(0);
        assert!(
            strip(&first).contains(still),
            "frame zero is not the still mark: {first:?}"
        );
        assert!(
            !strip(&first).contains(frame_one),
            "frame zero moved: {first:?}"
        );
        let second = paint_at(1);
        assert!(
            strip(&second).contains(frame_one),
            "the working row did not turn the spinner: {second:?}"
        );
        // The status sentence keeps the word on purpose: it says what the focused
        // terminal is doing in prose, which is a different surface from the panel.
        // Only the panel trades the word for the spinner.
        assert!(
            strip(&first).contains("app / claude, working"),
            "the status line lost its sentence: {first:?}"
        );
    }

    /// The run is measured in columns, so a name with a wide character in it must not be
    /// cut in half or leave the rest of the row shifted.
    #[test]
    fn an_accented_row_is_still_exactly_the_column_wide() {
        let wide = vec![WorkspaceRow {
            id: 1,
            name: "\u{4e2d}\u{6587}".to_string(),
            terminals: 1,
            asleep: false,
            sleeping: Vec::new(),
            closable: true,
        }];
        let rows = [row(1, "claude", "working")];
        let painted = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &wide,
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: None,
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &crate::run::Behind {
                terminal: 0,
                from: 0,
                lines: Vec::new(),
                total: 0,
            },
            image_notice: None,
            chrome: Default::default(),
        });
        assert!(!painted.contains('\u{fffd}'), "a name was cut in half");
        // Every panel row still ends where the rule begins.
        for row in 1..22 {
            let at = at(row, 1);
            let Some(start) = painted.find(&at) else {
                continue;
            };
            // Cut in columns, which is what the column is measured in: taking that many
            // characters overshoots the moment one of them is two columns wide, which is
            // the case under test.
            let painted_row = strip(&painted[start + at.len()..]);
            let line = width::fit(&painted_row, sidebar::WIDTH);
            assert_eq!(width::width(&line), sidebar::WIDTH, "row {row}: {line:?}");
            // The rule sits in the very next column, so the row ended exactly there.
            assert!(
                painted_row
                    .chars()
                    .scan(0usize, |col, c| {
                        let start = *col;
                        *col += width::columns(c);
                        Some((start, c))
                    })
                    .any(|(at, c)| at == sidebar::WIDTH && (c == '\u{2502}' || c == '\u{2524}')),
                "row {row} does not end at the rule: {painted_row:?}"
            );
        }
    }

    /// Escapes out, so a test can measure what a terminal would show.
    fn strip(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c != '\x1b' {
                out.push(c);
                continue;
            }
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        }
        out
    }

    #[test]
    fn a_line_is_cut_on_a_character_boundary() {
        // The pane is 19 columns here, and the string is Turkish, so a byte cut would put
        // a replacement character on the screen.
        let snapshot = snapshot(&["şğüöçİĞÜÖÇşğüöçİĞÜÖÇşğüöçİĞÜÖÇ"]);
        let rows = [row(1, "zsh", "idle")];
        let painted = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces(),
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 40,
            lines: 3,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &crate::run::Behind {
                terminal: 0,
                from: 0,
                lines: Vec::new(),
                total: 0,
            },
            image_notice: None,
            chrome: Default::default(),
        });
        assert!(!painted.contains('\u{fffd}'));
    }

    fn blocked_row(terminal: u32, program: &str) -> AgentRow {
        AgentRow {
            news: false,
            terminal,
            name: None,
            workspace: 1,
            agent: true,
            program: Some(program.to_string()),
            behind: None,
            state: "blocked".to_string(),
            generation: Some(1),
            readiness: "ready".to_string(),
            source: None,
            rule: None,
            manifest: None,
            agent_session: None,
            agent_title: None,
            last_active_at_unix: None,
            cwd: None,
            slept_at_unix: None,
            activity: None,
            pgid: None,
            discards: None,
        }
    }

    /// The status line's words and colour read one queue, so they name one agent: with
    /// the keyboard on a blocked agent and another failed, both say failed.
    #[test]
    fn the_status_line_words_and_colour_name_the_same_agent() {
        let mut failed = blocked_row(2, "claude");
        failed.state = "failed".to_string();
        let rows = [blocked_row(1, "codex"), failed];
        let (seen, unseen) = (sidebar::Seen::new(), sidebar::Unseen::new());
        let words = triage(&rows, &spaces(), 1, &seen, &unseen).expect("one waits");
        assert!(words.contains("claude is failed"), "{words}");
        assert_eq!(waiting(&rows, 1, &seen, &unseen), Some(theme::Role::Failed));
        // Nothing but the focused agent waits: nothing is next.
        assert_eq!(triage(&rows[..1], &spaces(), 1, &seen, &unseen), None);
        assert_eq!(waiting(&rows[..1], 1, &seen, &unseen), None);
    }

    /// Triage: one line saying what to do next, while something waits and the keyboard
    /// is elsewhere: the agent it is on is already in front of the person.
    #[test]
    fn a_blocked_agent_puts_the_next_action_on_the_status_line() {
        let rows = [blocked_row(1, "codex")];
        let snapshot = snapshot(&["waiting"]);
        let painted = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces(),
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 2,
            snapshot: Some(&snapshot),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &crate::run::Behind {
                terminal: 0,
                from: 0,
                lines: Vec::new(),
                total: 0,
            },
            image_notice: None,
            chrome: Default::default(),
        });
        assert!(
            painted.contains("next: app/codex is blocked"),
            "{painted:?}"
        );
        assert!(painted.contains("ctrl-a tab"), "{painted:?}");
    }

    #[test]
    fn with_nothing_waiting_the_status_line_carries_the_keys() {
        let rows = [row(1, "zsh", "idle")];
        let snapshot = snapshot(&["quiet"]);
        let painted = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces(),
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &crate::run::Behind {
                terminal: 0,
                from: 0,
                lines: Vec::new(),
                total: 0,
            },
            image_notice: None,
            chrome: Default::default(),
        });
        assert!(strip(&painted).contains("d detach"), "{painted:?}");
        assert!(!painted.contains("next:"), "{painted:?}");
    }

    #[test]
    fn a_transient_message_wins_over_the_next_action() {
        let rows = [blocked_row(1, "codex")];
        let snapshot = snapshot(&["waiting"]);
        let painted = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces(),
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 80,
            lines: 24,
            message: Some("starting a terminal"),
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &crate::run::Behind {
                terminal: 0,
                from: 0,
                lines: Vec::new(),
                total: 0,
            },
            image_notice: None,
            chrome: Default::default(),
        });
        assert!(painted.contains("starting a terminal"), "{painted:?}");
    }

    /// A space named in wide characters keeps its whole name and the row keeps its border:
    /// the colour split used to count characters where the name was measured in columns.
    #[test]
    fn a_wide_space_name_in_the_palette_loses_nothing() {
        let rows = [blocked_row(1, "codex")];
        let wide = vec![WorkspaceRow {
            id: 1,
            name: "中文ab".to_string(),
            terminals: 1,
            asleep: false,
            sleeping: Vec::new(),
            closable: true,
        }];
        let palette = Palette::open();
        let painted = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &wide,
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 0,
            snapshot: None,
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            palette: Some(&palette),
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &crate::run::Behind {
                terminal: 0,
                from: 0,
                lines: Vec::new(),
                total: 0,
            },
            image_notice: None,
            chrome: Default::default(),
        });
        let shown = strip(&painted);
        assert!(shown.contains("中文ab codex"), "{shown:?}");
    }

    /// The palette floats over the terminal with what needs a person first.
    #[test]
    fn an_open_palette_lists_what_needs_you_over_the_pane() {
        let rows = [blocked_row(1, "codex"), row(2, "claude", "working")];
        let snapshot = snapshot(&["terminal output"]);
        let palette = Palette::open();
        let painted = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces(),
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 2,
            snapshot: Some(&snapshot),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            palette: Some(&palette),
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &crate::run::Behind {
                terminal: 0,
                from: 0,
                lines: Vec::new(),
                total: 0,
            },
            image_notice: None,
            chrome: Default::default(),
        });
        // Read as a terminal shows it: the row is painted in pieces now, since the space
        // wears its own colour and the state wears its own.
        let shown = strip(&painted);
        assert!(shown.contains("> "), "{shown:?}");
        assert!(shown.contains("needs you"), "{shown:?}");
        assert!(shown.contains("app codex"), "{shown:?}");
        assert!(shown.contains("enter go"), "{shown:?}");
        // A box with edges, drawn in the middle of the window rather than in a corner of
        // it, which is what an unopened thing looks like.
        assert!(
            shown.contains('\u{256d}') && shown.contains('\u{256f}'),
            "{shown:?}"
        );
        let w = palette_width(80);
        let left = (80usize.saturating_sub(w)) / 2;
        let rows = palette
            .items(
                &rows,
                &spaces(),
                0,
                &sidebar::Seen::new(),
                &sidebar::Unseen::new(),
            )
            .len();
        let top = palette_query_row(24, rows + 4);
        assert!(top > 1, "the box is pinned to the top: {shown:?}");
        // The cursor sits at the end of the query, inside the box, where typing goes.
        assert!(painted.contains(&at(top + 1, left + 4 + 1)), "{painted:?}");
    }

    /// The palette is painted over a pane whose program left the terminal in some
    /// colour and some attribute. Every line of the box has to say what it is
    /// rather than inherit that, and cover the full width of the box, or it is a
    /// box only in the sense that its text lands in the right cells.
    #[test]
    fn every_palette_line_resets_the_style_and_covers_the_box() {
        let rows = [blocked_row(1, "codex"), row(2, "claude", "working")];
        let mut snapshot = snapshot(&["output in a colour the program chose"]);
        snapshot.styles = vec![vec![SpanWire {
            at: 0,
            len: 36,
            fg: Some(ColourWire::Indexed { i: 5 }),
            bg: None,
            attrs: attrs::BOLD,
            link: None,
        }]];
        let palette = Palette::open();
        let painted = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces(),
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 2,
            snapshot: Some(&snapshot),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            palette: Some(&palette),
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &crate::run::Behind {
                terminal: 0,
                from: 0,
                lines: Vec::new(),
                total: 0,
            },
            image_notice: None,
            chrome: Default::default(),
        });
        let w = palette_width(80);
        let left = (80usize.saturating_sub(w)) / 2;
        // Every row the box drew, found by the position it was drawn at.
        let mut found = 0;
        for row in TABS + 1..24 {
            let Some(cut) = painted.find(&at(row, left + 1)) else {
                continue;
            };
            found += 1;
            let rest = &painted[cut + at(row, left + 1).len()..];
            assert!(
                rest.starts_with("\x1b[0;") || rest.starts_with("\x1b[0m"),
                "row {row} of the palette inherits the pane's style: {:?}",
                &rest[..rest.len().min(40)]
            );
            // Every row covers the whole box. Measured across the row's pieces, because a
            // row is painted in several now: the border, the space in its own colour, the
            // state in its own, and the rest.
            let painted_row = strip(rest);
            let text = width::fit(&painted_row, w);
            assert_eq!(
                width::width(&text),
                w,
                "row {row} of the palette does not cover the box: {text:?}"
            );
            assert!(
                text.starts_with('\u{2502}')
                    || text.starts_with('\u{256d}')
                    || text.starts_with('\u{2570}'),
                "row {row} of the palette has no edge: {text:?}"
            );
        }
        assert!(found >= 3, "the palette drew {found} rows");
    }

    /// On a window a few columns wider than the smallest palette, a long state word
    /// is cut to the box. Uncut, the row ran past the right border and off the screen.
    #[test]
    fn a_palette_row_never_runs_past_its_box_on_a_narrow_window() {
        let rows = [blocked_row(1, "codex"), row(2, "claude", "sleeping")];
        let palette = Palette::open();
        let behind = crate::run::Behind::default();
        for cols in 10..=13u16 {
            let painted = paint(&Frame {
                armed: None,
                rows: &rows,
                spaces: &spaces(),
                panes: std::collections::BTreeSet::new(),
                space: Some(1),
                focused: 2,
                snapshot: None,
                cols,
                lines: 24,
                message: None,
                hint: None,
                palette: Some(&palette),
                prompt: None,
                menu: None,
                collapsed: false,
                reading: None,
                selection: None,
                scroll: 0,
                tick: 0,
                now_unix: TEST_NOW,
                sidebar_width: sidebar::WIDTH,
                view: sidebar::View::Tree,
                seen: &sidebar::Seen::new(),
                unseen: &sidebar::Unseen::new(),
                anchor: 0,
                rows_back: 0,
                tiles: Vec::new(),
                behind: &behind,
                image_notice: None,
                chrome: Default::default(),
            });
            let w = palette_width(cols);
            let left = (cols as usize - w) / 2;
            let mut found = 0;
            for row in TABS + 1..24 {
                // The last move to the box's edge: the overlay paints after the pane,
                // which on a window this narrow starts at the same column.
                let Some(cut) = painted.rfind(&at(row, left + 1)) else {
                    continue;
                };
                found += 1;
                let rest = &painted[cut + at(row, left + 1).len()..];
                // This row alone: up to the next cursor move.
                let end = rest
                    .match_indices("\x1b[")
                    .find(|(i, _)| {
                        rest[i + 2..].chars().find(|c| c.is_ascii_alphabetic()) == Some('H')
                    })
                    .map_or(rest.len(), |(i, _)| i);
                let text = strip(&rest[..end]);
                assert_eq!(
                    width::width(&text),
                    w,
                    "{cols} columns, row {row} is not the box width: {text:?}"
                );
            }
            assert!(found >= 4, "{cols} columns: the palette drew {found} rows");
        }
    }

    /// A real picture draws downward from its anchor, so the anchor is the
    /// focused pane's first row. Anchored on the last row, it covered the
    /// status rows and ran past the bottom of the screen.
    #[test]
    fn a_picture_is_anchored_at_the_focused_pane_first_row() {
        let rows = [row(1, "zsh", "idle"), row(2, "zsh", "idle")];
        let spaces = spaces();
        let behind = crate::run::Behind::default();
        let seen = sidebar::Seen::new();
        let unseen = sidebar::Unseen::new();
        let rect = |x, y| crate::layout::Rect {
            x,
            y,
            cols: 20,
            rows: 8,
        };
        let frame_of = |tiles: Vec<Tile<'static>>| Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces,
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 2,
            snapshot: None,
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            anchor: 0,
            rows_back: 0,
            tiles,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &seen,
            unseen: &unseen,
            behind: &behind,
            image_notice: None,
            chrome: Default::default(),
        };
        let left = left_for(80, false, sidebar::WIDTH);
        assert_eq!(
            picture_cell(&frame_of(Vec::new())),
            Some((TABS + 1, left + 1))
        );
        let tiles = vec![
            Tile {
                terminal: 1,
                rect: rect(0, 0),
                snapshot: None,
                focused: false,
            },
            Tile {
                terminal: 2,
                rect: rect(21, 9),
                snapshot: None,
                focused: true,
            },
        ];
        assert_eq!(
            picture_cell(&frame_of(tiles)),
            Some((TABS + 1 + 9, left + 1 + 21))
        );
    }

    #[test]
    fn a_palette_query_filters_what_is_on_screen() {
        let rows = [blocked_row(1, "codex"), row(2, "claude", "working")];
        let snapshot = snapshot(&["terminal output"]);
        let mut palette = Palette::open();
        for c in "working".chars() {
            palette.push(c);
        }
        let painted = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces(),
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 2,
            snapshot: Some(&snapshot),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            palette: Some(&palette),
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &crate::run::Behind {
                terminal: 0,
                from: 0,
                lines: Vec::new(),
                total: 0,
            },
            image_notice: None,
            chrome: Default::default(),
        });
        let shown = strip(&painted);
        assert!(shown.contains("> working"), "{shown:?}");
        assert!(!shown.contains("app codex"), "{shown:?}");
    }

    #[test]
    fn a_tree_fits_nine_agents_without_scrolling() {
        let spaces = vec![WorkspaceRow {
            id: 1,
            name: "app".to_string(),
            terminals: 9,
            asleep: false,
            sleeping: Vec::new(),
            closable: true,
        }];
        let states = [
            "blocked", "failed", "working", "working", "done", "idle", "idle", "idle", "idle",
        ];
        let rows: Vec<AgentRow> = states
            .iter()
            .enumerate()
            .map(|(i, state)| AgentRow {
                news: false,
                terminal: i as u32 + 1,
                name: None,
                workspace: 1,
                agent: true,
                program: Some("claude".to_string()),
                behind: None,
                state: state.to_string(),
                generation: Some(1),
                readiness: "ready".to_string(),
                source: None,
                rule: None,
                manifest: None,
                agent_session: None,
                agent_title: None,
                last_active_at_unix: None,
                cwd: None,
                slept_at_unix: None,
                activity: None,
                pgid: None,
                discards: None,
            })
            .collect();
        // Nothing scrolls: all nine agents draw their row under the space's rule, and
        // the title names the order.
        let shut = sidebar::Shut::new();
        let (seen, unseen) = (sidebar::Seen::new(), sidebar::Unseen::new());
        let lines = sidebar::render_folded(
            &sidebar::Column {
                armed: None,
                spaces: &spaces,
                agents: &rows,
                focused_space: 1,
                focused_terminal: 1,
                tick: 0,
                view: sidebar::View::Tree,
                seen: &seen,
                unseen: &unseen,
                now_unix: 0,
                width: sidebar::WIDTH,
                shut: &shut,
                usage: None,
            },
            44,
        );
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .collect();
        // Under the title: the next row above it names one of the same agents. Each
        // agent's row opens with a blank, then its two-cell state mark
        // (braille U+2800..=U+28FF), whatever state it is in, straight into the name
        // with no space.
        assert_eq!(
            texts
                .iter()
                .skip_while(|t| !t.starts_with(" agents"))
                .filter(|t| {
                    t.strip_prefix(' ').is_some_and(|rest| {
                        rest.chars()
                            .next()
                            .is_some_and(|c| ('\u{2800}'..='\u{28ff}').contains(&c))
                            && rest
                                .trim_start_matches(|c: char| {
                                    ('\u{2800}'..='\u{28ff}').contains(&c)
                                })
                                .starts_with("claude")
                    })
                })
                .count(),
            9,
            "agents lost rows: {texts:?}"
        );
        assert!(!texts.iter().any(|t| t.contains('\u{25b8}')), "{texts:?}");
        assert!(
            texts
                .iter()
                .any(|t| t.starts_with(" agents") && t.ends_with("tree +")),
            "{texts:?}"
        );
    }

    /// A selection is drawn as selected, and only where it is.
    #[test]
    fn the_selected_part_of_a_row_is_marked_and_the_rest_is_not() {
        let snapshot = snapshot(&["the quick brown fox", "another line"]);
        let rows = [row(1, "sh", "idle")];
        let mut selection = crate::selection::Selection::start(0, 4);
        selection.extend(0, 8);
        let painted = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces(),
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 60,
            lines: 6,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: Some(selection),
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &crate::run::Behind::default(),
            image_notice: None,
            chrome: Default::default(),
        });
        // The five columns under the drag are reversed, and the escape is written once at
        // each edge of them rather than around every character.
        assert!(
            painted.contains("\x1b[7mquick"),
            "the selected word is not marked: {painted:?}"
        );
        assert_eq!(
            painted.matches("\x1b[7m").count(),
            1,
            "the reverse was written more than once: {painted:?}"
        );
        let after = painted
            .split("\x1b[7mquick")
            .nth(1)
            .expect("the rest of the screen");
        assert!(
            after.starts_with("\x1b[0m"),
            "the reverse was never turned off, so the rest of the screen wears it: \
             {after:?}"
        );
        assert!(
            !after.contains(" brown fox\x1b[7m"),
            "the row after the selection is marked too: {after:?}"
        );
    }

    /// Without a selection nothing changes about how a row is painted.
    #[test]
    fn a_click_that_selected_nothing_leaves_the_screen_exactly_as_it_was() {
        let snapshot = snapshot(&["plain text"]);
        let rows = [row(1, "sh", "idle")];
        let build = |selection| {
            paint(&Frame {
                armed: None,
                rows: &rows,
                spaces: &spaces(),
                panes: std::collections::BTreeSet::new(),
                space: Some(1),
                focused: 1,
                snapshot: Some(&snapshot),
                cols: 60,
                lines: 6,
                message: None,
                hint: None,
                palette: None,
                prompt: None,
                menu: None,
                collapsed: false,
                reading: None,
                selection,
                scroll: 0,
                tick: 0,
                now_unix: TEST_NOW,
                sidebar_width: sidebar::WIDTH,
                view: sidebar::View::Tree,
                seen: &sidebar::Seen::new(),
                unseen: &sidebar::Unseen::new(),
                anchor: 0,
                rows_back: 0,
                tiles: Vec::new(),
                behind: &crate::run::Behind::default(),
                image_notice: None,
                chrome: Default::default(),
            })
        };
        assert_eq!(
            build(None),
            build(Some(crate::selection::Selection::start(0, 3))),
            "a click that selected nothing changed the screen"
        );
    }

    /// Scrolled back, the window is one continuous terminal: what went past above, what is
    /// on the screen below, with no seam and nothing repeated.
    #[test]
    fn a_scrolled_window_joins_what_went_past_to_what_is_on_the_screen() {
        let snapshot = snapshot(&["live 0", "live 1", "live 2", "live 3"]);
        let rows = [row(1, "sh", "idle")];
        // A hundred rows written, of which this client holds the newest ten.
        let behind = crate::run::Behind {
            terminal: 1,
            from: 90,
            lines: (90..100).map(|i| format!("gone {i}")).collect(),
            total: 100,
        };
        let painted = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces(),
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 60,
            lines: 7,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 2,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            // Pinned to the second to last row written, so the pane shows the last two
            // rows that went past and then the screen from its top.
            anchor: 98,
            rows_back: 2,
            behind: &behind,
            image_notice: None,
            chrome: Default::default(),
            tiles: Vec::new(),
        });
        // Two rows back: the last two rows written, then the screen from its top.
        assert!(painted.contains("gone 98"), "{painted:?}");
        assert!(painted.contains("gone 99"), "{painted:?}");
        assert!(painted.contains("live 0"), "{painted:?}");
        assert!(painted.contains("live 1"), "{painted:?}");
        // And the bottom of the screen has gone off the window, which is what being
        // scrolled back means.
        assert!(
            !painted.contains("live 3"),
            "the window kept the newest row while claiming to be above it: {painted:?}"
        );
        // The row above what it holds is blank rather than wrong, and never another row.
        assert!(!painted.contains("gone 89"), "{painted:?}");
        // A person is told where they are, on the row that says what just happened.
        assert!(painted.contains("2 rows back"), "{painted:?}");
    }

    /// At the bottom, nothing about the history is read at all.
    #[test]
    fn a_window_at_the_bottom_is_the_live_screen_and_says_nothing_about_scrolling() {
        let snapshot = snapshot(&["live 0", "live 1"]);
        let rows = [row(1, "sh", "idle")];
        let behind = crate::run::Behind {
            terminal: 1,
            from: 0,
            lines: vec!["gone".to_string()],
            total: 1,
        };
        let painted = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces(),
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 60,
            lines: 6,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &behind,
            image_notice: None,
            chrome: Default::default(),
        });
        assert!(painted.contains("live 0") && painted.contains("live 1"));
        assert!(!painted.contains("gone"), "{painted:?}");
        assert!(!painted.contains("rows back"), "{painted:?}");
    }

    /// The row a copy reads is the row that was drawn, scrolled back or not.
    ///
    /// The two used to be separate: the painter drew the window and the copier read the
    /// live screen, so a selection made over rows that had gone past put whatever happened
    /// to be at the same position on the screen onto the clipboard.
    #[test]
    fn the_row_a_copy_reads_is_the_row_that_was_drawn() {
        let snapshot = snapshot(&["live 0", "live 1", "live 2"]);
        let behind = crate::run::Behind {
            terminal: 1,
            from: 50,
            lines: (50..60).map(|i| format!("gone {i}")).collect(),
            total: 60,
        };
        // At the bottom, every row is the live screen.
        for row in 0..3 {
            assert_eq!(
                window_row(Some(&snapshot), &behind, 0, 0, row).0,
                format!("live {row}")
            );
        }
        // Pinned to row 58 with two of the pane's rows behind the screen: the top two are
        // rows 58 and 59, and the rest is the screen from its top.
        assert_eq!(window_row(Some(&snapshot), &behind, 2, 58, 0).0, "gone 58");
        assert_eq!(window_row(Some(&snapshot), &behind, 2, 58, 1).0, "gone 59");
        assert_eq!(window_row(Some(&snapshot), &behind, 2, 58, 2).0, "live 0");
        // A row older than what was fetched is blank rather than another row.
        assert_eq!(window_row(Some(&snapshot), &behind, 2, 10, 0).0, "");
        // And rows behind the screen carry no colour, because none was written down.
        assert!(window_row(Some(&snapshot), &behind, 2, 58, 0).1.is_empty());
    }

    fn focus_event(kind: &str, label: &str) -> FocusEvent {
        FocusEvent {
            kind: kind.to_string(),
            label: label.to_string(),
            at_unix: 0,
        }
    }

    #[test]
    fn an_empty_focus_log_says_why_and_how_to_fill_it() {
        let rows = paint_focus_list(&[], 40, 0, 20);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, theme::Role::Faint);
        assert!(rows[0].0.contains("no hook reports yet"), "{rows:?}");
        assert!(rows[0].0.contains("qoro hooks install"), "{rows:?}");
    }

    #[test]
    fn every_focus_kind_has_its_mark_and_its_colour() {
        let events = vec![
            focus_event("user", "why is this slow"),
            focus_event("tool", "Read foo.rs"),
            focus_event("blocked", "Allow command?"),
            focus_event("done", ""),
        ];
        let rows = paint_focus_list(&events, 40, 0, 20);
        assert_eq!(rows.len(), 4);
        assert!(rows[0].0.starts_with("› "), "{rows:?}");
        assert_eq!(rows[0].1, theme::Role::Text);
        assert!(rows[1].0.starts_with("▸ "), "{rows:?}");
        assert_eq!(rows[1].1, theme::Role::Faint);
        assert!(rows[2].0.starts_with("! "), "{rows:?}");
        assert_eq!(rows[2].1, theme::Role::Blocked);
        assert!(rows[3].0.starts_with("· "), "{rows:?}");
        assert_eq!(rows[3].1, theme::Role::Faint);
        // A kind a newer daemon stored that this build does not name is one
        // collapsed row, never a failure.
        let rows = paint_focus_list(&[focus_event("note", "hi")], 40, 0, 20);
        assert!(rows[0].0.starts_with("▸ "), "{rows:?}");
    }

    #[test]
    fn the_focus_window_is_bottom_pinned_and_moves_with_the_offset() {
        let events: Vec<FocusEvent> = (0..5)
            .map(|i| focus_event("tool", &format!("tool {i}")))
            .collect();
        // Offset zero shows the tail: the list follows new events by itself.
        let rows = paint_focus_list(&events, 40, 0, 3);
        assert_eq!(rows.len(), 3);
        assert!(rows[0].0.contains("tool 2"), "{rows:?}");
        assert!(rows[2].0.contains("tool 4"), "{rows:?}");
        // Scrolled up by two: the two newest rows hide below the window.
        let rows = paint_focus_list(&events, 40, 2, 3);
        assert_eq!(rows.len(), 3);
        assert!(rows[0].0.contains("tool 0"), "{rows:?}");
        assert!(rows[2].0.contains("tool 2"), "{rows:?}");
        // Past the oldest row clamps to the oldest row.
        let rows = paint_focus_list(&events, 40, 100, 3);
        assert!(rows[0].0.contains("tool 0"), "{rows:?}");
    }

    fn paint_two_panes(
        rows: &[AgentRow],
        spaces: &[WorkspaceRow],
        focused_snap: Option<&SnapshotWire>,
        other_snap: Option<&SnapshotWire>,
        focused: u32,
        rects: Vec<(u32, crate::layout::Rect)>,
    ) -> String {
        let behind = crate::run::Behind::default();
        let tiles = rects
            .into_iter()
            .map(|(terminal, rect)| Tile {
                terminal,
                rect,
                snapshot: if terminal == focused {
                    focused_snap
                } else {
                    other_snap
                },
                focused: terminal == focused,
            })
            .collect();
        paint(&Frame {
            armed: None,
            rows,
            spaces,
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused,
            snapshot: focused_snap,
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            anchor: 0,
            rows_back: 0,
            tiles,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            behind: &behind,
            image_notice: None,
            chrome: Default::default(),
        })
    }

    #[test]
    fn a_single_tile_paints_exactly_what_a_single_pane_always_has() {
        let rows = [row(1, "zsh", "idle")];
        let spaces = spaces();
        let snap = snapshot(&["hello from the shell"]);
        let behind = crate::run::Behind::default();
        let (pane_cols, pane_rows) = pane_size(80, 24);
        let legacy = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces,
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snap),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            behind: &behind,
            image_notice: None,
            chrome: Default::default(),
        });
        let tiled = paint(&Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces,
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snap),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            anchor: 0,
            rows_back: 0,
            tiles: vec![Tile {
                terminal: 1,
                rect: crate::layout::Rect {
                    x: 0,
                    y: 0,
                    cols: pane_cols as usize,
                    rows: pane_rows as usize,
                },
                snapshot: Some(&snap),
                focused: true,
            }],
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            behind: &behind,
            image_notice: None,
            chrome: Default::default(),
        });
        assert_eq!(tiled, legacy);
    }

    /// A picture the outer terminal cannot draw is named on the pane's last row,
    /// dimmed, and nowhere else. Without a notice the same frame paints no such
    /// line: the overlay is the notice, not a standing row.
    #[test]
    fn an_undrawable_picture_is_named_on_the_pane_last_row() {
        let rows = [row(1, "zsh", "idle")];
        let spaces = spaces();
        let snap = snapshot(&["hello from the shell"]);
        let behind = crate::run::Behind::default();
        let panes = std::collections::BTreeSet::new();
        let seen = sidebar::Seen::new();
        let unseen = sidebar::Unseen::new();
        let frame_of = |notice: Option<&'static str>| Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces,
            panes: panes.clone(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snap),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &seen,
            unseen: &unseen,
            behind: &behind,
            image_notice: notice,
            chrome: Default::default(),
        };
        let plain = paint(&frame_of(None));
        assert!(!plain.contains("[image"), "{plain:?}");
        let named = paint(&frame_of(Some("[image 800x600, 123KB]")));
        assert!(named.contains("[image 800x600, 123KB]"), "{named:?}");
        // On the pane's last row, not floating somewhere above the status lines.
        let (_, pane_rows) = pane_size(80, 24);
        let left = left_for(80, false, sidebar::WIDTH);
        let last_cell = format!("\x1b[{};{}H", TABS + pane_rows as usize, left + 1);
        let at = named.find("[image 800x600, 123KB]").expect("notice drawn");
        let placed = named.rfind(&last_cell).expect("a placement opens the row");
        assert!(
            placed < at,
            "notice is not on the last pane row: {:?}",
            &named[placed..at + 30]
        );
    }

    /// A split tab is framed the way the column's boxes are: rounded outer corners, one
    /// shared line between the panes joined into the frame, and the focused pane's box and
    /// name lit while the other's stay grey.
    #[test]
    fn split_panes_are_framed_and_the_focused_one_is_lit() {
        let rows = [row(1, "zsh", "idle"), row(2, "claude", "working")];
        let area = crate::layout::Rect {
            x: 0,
            y: 0,
            cols: 40,
            rows: 10,
        };
        let mut layout = crate::layout::Layout::single(1);
        assert!(layout.split_placeholder(1, crate::layout::Dir::V, area));
        assert!(layout.bind_placeholder(2));
        let tiles: Vec<Tile> = layout
            .tile_rects(area)
            .into_iter()
            .map(|(terminal, rect)| Tile {
                terminal,
                rect,
                snapshot: None,
                focused: terminal == 2,
            })
            .collect();
        let grid = pane_edges(&tiles, &rows, 40, 10);
        let text =
            |y: usize| -> String { grid[y].iter().map(|c| c.map_or(' ', |(c, _)| c)).collect() };
        let left = tiles[0].rect;
        let divider = left.x + left.cols;
        assert!(text(0).starts_with('\u{256d}'), "{:?}", text(0));
        assert!(text(0).ends_with('\u{256e}'), "{:?}", text(0));
        assert_eq!(grid[0][divider].map(|(c, _)| c), Some('\u{252c}'));
        assert_eq!(grid[9][divider].map(|(c, _)| c), Some('\u{2534}'));
        assert!(text(9).starts_with('\u{2570}') && text(9).ends_with('\u{256f}'));
        assert_eq!(grid[4][divider].map(|(c, _)| c), Some('\u{2502}'));
        // Each pane's name on its top edge, the focused one lit.
        assert!(
            text(0).contains(" zsh ") && text(0).contains(" claude "),
            "{:?}",
            text(0)
        );
        let top = text(0);
        let at = top[..top.find(" claude ").expect("its name")]
            .chars()
            .count();
        assert_eq!(grid[0][at + 1].map(|(_, ink)| ink), Some(EdgeInk::TitleLit));
        let top = text(0);
        let at = top[..top.find(" zsh ").expect("its name")].chars().count();
        assert_eq!(grid[0][at + 1].map(|(_, ink)| ink), Some(EdgeInk::Title));
        // The shared line is the focused pane's edge too; the far side is not.
        assert_eq!(grid[4][divider].map(|(_, ink)| ink), Some(EdgeInk::Lit));
        assert_eq!(grid[4][39].map(|(_, ink)| ink), Some(EdgeInk::Lit));
        assert_eq!(grid[4][0].map(|(_, ink)| ink), Some(EdgeInk::Rule));
        // Nothing drawn inside a pane.
        assert!(grid[4][left.x + 2].is_none());
    }

    #[test]
    fn two_side_by_side_panes_both_paint_and_share_the_width() {
        let rows = [row(1, "zsh", "idle"), row(2, "claude", "working")];
        let spaces = spaces();
        let left = snapshot(&["left side"]);
        let right = snapshot(&["right side"]);
        let (pane_cols, pane_rows) = pane_size(80, 24);
        let half = (pane_cols as usize - 1) / 2;
        let painted = paint_two_panes(
            &rows,
            &spaces,
            Some(&left),
            Some(&right),
            1,
            vec![
                (
                    1,
                    crate::layout::Rect {
                        x: 0,
                        y: 0,
                        cols: half,
                        rows: pane_rows as usize,
                    },
                ),
                (
                    2,
                    crate::layout::Rect {
                        x: half + 1,
                        y: 0,
                        cols: pane_cols as usize - half - 1,
                        rows: pane_rows as usize,
                    },
                ),
            ],
        );
        assert!(painted.contains("left side"), "{painted:?}");
        assert!(painted.contains("right side"), "{painted:?}");
        assert!(painted.contains('\u{2502}'), "no divider: {painted:?}");
    }

    #[test]
    fn a_pane_without_a_snapshot_yet_paints_blank_but_keeps_its_place() {
        let rows = [row(1, "zsh", "idle"), row(2, "zsh", "starting")];
        let spaces = spaces();
        let left = snapshot(&["left side"]);
        let (pane_cols, pane_rows) = pane_size(80, 24);
        let half = (pane_cols as usize - 1) / 2;
        let painted = paint_two_panes(
            &rows,
            &spaces,
            Some(&left),
            None,
            1,
            vec![
                (
                    1,
                    crate::layout::Rect {
                        x: 0,
                        y: 0,
                        cols: half,
                        rows: pane_rows as usize,
                    },
                ),
                (
                    2,
                    crate::layout::Rect {
                        x: half + 1,
                        y: 0,
                        cols: pane_cols as usize - half - 1,
                        rows: pane_rows as usize,
                    },
                ),
            ],
        );
        assert!(painted.contains("left side"), "{painted:?}");
        assert!(painted.contains('\u{2502}'), "no divider: {painted:?}");
    }

    #[test]
    fn stacked_panes_are_separated_by_a_horizontal_divider() {
        let rows = [row(1, "zsh", "idle"), row(2, "zsh", "idle")];
        let spaces = spaces();
        let top = snapshot(&["top row"]);
        let bottom = snapshot(&["bottom row"]);
        let (pane_cols, pane_rows) = pane_size(80, 24);
        let area = crate::layout::Rect {
            x: 0,
            y: 0,
            cols: pane_cols as usize,
            rows: pane_rows as usize,
        };
        // Real tiles, split the way the tree splits them: the frame around each and
        // the shared line between them is `Layout::inner`'s and `pane_edges`' job, not
        // something a test builds by hand.
        let mut layout = crate::layout::Layout::single(1);
        assert!(layout.split_placeholder(1, crate::layout::Dir::H, area));
        assert!(layout.bind_placeholder(2));
        let painted = paint_two_panes(
            &rows,
            &spaces,
            Some(&top),
            Some(&bottom),
            1,
            layout.tile_rects(area),
        );
        assert!(painted.contains("top row"), "{painted:?}");
        assert!(painted.contains("bottom row"), "{painted:?}");
        // The join between the two frames, or a plain rule inside the pane area: either
        // is the horizontal line the shared border draws.
        assert!(
            painted.contains('\u{251c}')
                || painted.contains('\u{2524}')
                || painted.contains('\u{2500}'),
            "no divider: {painted:?}"
        );
    }
    #[test]
    fn the_pane_shrinks_from_the_right_exactly_as_it_shrinks_from_the_left() {
        assert_eq!(
            pane_size_for(120, 24, false, 39, sidebar::WIDTH),
            (120 - sidebar::WIDTH as u16 - 1 - 39, 21)
        );
        assert_eq!(
            pane_size_for(120, 24, false, 0, sidebar::WIDTH),
            pane_size(120, 24)
        );
        // Pulled widths move both the rule and the pane with it.
        assert_eq!(left_for(120, false, 18), 19);
        assert_eq!(left_for(120, false, 36), 37);
        assert_eq!(pane_size_for(120, 24, false, 0, 18), (120 - 18 - 1, 21));
    }

    /// The settings census counts live agents: a sleeping tomb is listed,
    /// not counted.
    #[test]
    fn the_settings_census_counts_live_agents_not_sleepers() {
        let rows = [row(1, "claude", "working"), row(2, "claude", "sleeping")];
        let mut out = String::new();
        paint_reading(
            &Frame {
                armed: None,
                rows: &rows,
                spaces: &spaces(),
                panes: std::collections::BTreeSet::new(),
                space: Some(1),
                focused: 1,
                snapshot: None,
                cols: 100,
                lines: 24,
                message: None,
                hint: None,
                palette: None,
                prompt: None,
                menu: None,
                collapsed: false,
                reading: Some(crate::run::Reading::Settings),
                selection: None,
                scroll: 0,
                tick: 0,
                now_unix: TEST_NOW,
                sidebar_width: sidebar::WIDTH,
                view: sidebar::View::Tree,
                seen: &sidebar::Seen::new(),
                unseen: &sidebar::Unseen::new(),
                anchor: 0,
                rows_back: 0,
                tiles: Vec::new(),
                behind: &crate::run::Behind {
                    terminal: 0,
                    from: 0,
                    lines: Vec::new(),
                    total: 0,
                },
                image_notice: None,
                chrome: Default::default(),
            },
            crate::run::Reading::Settings,
            &mut out,
        );
        let shown = strip(&out);
        assert!(shown.contains(" agents          1"), "{out:?}");
        assert!(shown.contains(" terminals       2"), "{out:?}");
    }

    /// The close box and the keys box on windows that cannot hold them, and a name that
    /// carries a control sequence. Found in the last review: the keys box underflowed at
    /// two columns, and the close box wrote a program's name to the terminal raw.
    #[test]
    fn the_close_box_cleans_what_it_names_and_tiny_windows_paint_nothing_wrong() {
        let rows = [row(1, "claude\x1b]52;c;aGk=\x07", "working")];
        let (seen, unseen) = (sidebar::Seen::new(), sidebar::Unseen::new());
        let behind = crate::run::Behind::default();
        let frame = |cols, lines, closing, reading| Frame {
            armed: None,
            rows: &rows,
            spaces: &[],
            panes: std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: None,
            cols,
            lines,
            message: Some("close claude (working)? Y closes it, any other key keeps it"),
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &seen,
            unseen: &unseen,
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &behind,
            image_notice: None,
            chrome: Chrome {
                closing,
                ..Default::default()
            },
        };
        let painted = paint(&frame(100, 30, Some(1), None));
        assert!(strip(&painted).contains("close claude"), "{painted:?}");
        assert!(
            !painted.contains("\x1b]52"),
            "a name wrote a sequence: {painted:?}"
        );
        // Too small for the box: the question stays on the notice row.
        let small = strip(&paint(&frame(100, 6, Some(1), None)));
        assert!(small.contains("Y closes it"), "{small:?}");
        // The keys box at one and two columns paints nothing and does not panic.
        for cols in [1u16, 2, 3] {
            let _ = paint(&frame(cols, 24, None, Some(crate::run::Reading::Keybinds)));
        }
    }

    /// A focused slept tab has a blank pane that takes no keys: the notice
    /// row names the way out instead of leaving it to read as a hang.
    #[test]
    fn a_focused_sleeping_tab_names_how_to_dismiss_it() {
        let rows = [row(1, "claude", "working"), row(2, "claude", "sleeping")];
        let (spaces, panes) = (spaces(), std::collections::BTreeSet::new());
        let (seen, unseen) = (sidebar::Seen::new(), sidebar::Unseen::new());
        let behind = crate::run::Behind {
            terminal: 0,
            from: 0,
            lines: Vec::new(),
            total: 0,
        };
        let frame = |focused| Frame {
            armed: None,
            rows: &rows,
            spaces: &spaces,
            panes: panes.clone(),
            space: Some(1),
            focused,
            snapshot: None,
            cols: 100,
            lines: 24,
            message: None,
            hint: None,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            now_unix: TEST_NOW,
            sidebar_width: sidebar::WIDTH,
            view: sidebar::View::Tree,
            seen: &seen,
            unseen: &unseen,
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &behind,
            image_notice: None,
            chrome: Default::default(),
        };
        let slept = here(&frame(2));
        assert!(slept.ends_with(", sleeping  ctrl-a x dismiss"), "{slept:?}");
        let live = here(&frame(1));
        assert!(live.ends_with(", working"), "{live:?}");
    }
}
