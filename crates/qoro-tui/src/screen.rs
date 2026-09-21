//! Painting a frame, as a pure function of what the daemon said.
//!
//! No terminal library. What has to be produced is a byte string of positioned writes, and
//! the alternative was a dependency whose own idea of a frame would then have to be
//! reconciled with the one the daemon already sends. Everything here is text in, text out,
//! so a test can read a frame instead of looking at one.

use qoro_runtime::control::{AgentRow, FocusEvent, WorkspaceRow};
use qoro_runtime::daemon::{ColourWire, SnapshotWire, SpanWire, attrs};

use crate::palette::Palette;
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
    pub panes: &'a std::collections::BTreeSet<u32>,
    pub focused: u32,
    /// The focused terminal's last snapshot. `None` before the first one arrives, which is
    /// a real state: a terminal that has produced nothing yet has nothing to draw.
    pub snapshot: Option<&'a SnapshotWire>,
    pub cols: u16,
    pub lines: u16,
    /// Shown instead of the key list, when there is something to say.
    pub message: Option<&'a str>,
    /// Something worth saying while nothing else is happening, said until the person does
    /// anything. Under everything urgent, and never in place of it.
    pub hint: Option<&'a str>,
    /// Which idle groups of the agents panel are expanded.
    pub fold: sidebar::Fold,
    /// The agents panel as labelled groups, or as a flat urgency list.
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
    /// The open search palette, drawn over the terminal when present.
    pub palette: Option<&'a Palette>,
    /// The open naming box, drawn over everything when a person is being asked what to
    /// call the thing they just asked for.
    pub prompt: Option<&'a crate::run::Prompt>,
    /// The open menu, and which of its rows is selected.
    pub menu: Option<usize>,
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
    /// The visible terminals and their rectangles, in pane-local cells. Empty
    /// means one terminal filling the pane, drawn from `snapshot` exactly as
    /// before, so a single pane paints pixel for pixel what it always has.
    pub tiles: Vec<Tile<'a>>,
}

/// Every command the prefix takes, for a window wide enough to read them all.
const EVERY_KEY: &str = "ctrl-a: n tab  c new  r rename  R space name  w new space  W close space  x close  s space  space find  g view  F focus  m menu  ? keys  d detach  U update";

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
    pane_size_for(cols, lines, false, 0)
}

/// The same, for a column that may be folded away to a strip.
pub fn pane_size_for(cols: u16, lines: u16, collapsed: bool, right: usize) -> (u16, u16) {
    (
        cols.saturating_sub(left_for(cols, collapsed) as u16)
            .saturating_sub(right as u16)
            .max(1),
        lines.saturating_sub((TABS + STATUS) as u16).max(1),
    )
}

/// The columns spent before the pane starts: the left column and the rule beside it, or
/// nothing at all when the window is too narrow to hold both.
///
/// Dropped rather than squeezed. The left column is a fixed width and the rule is one more,
/// so a window of twenty three columns or fewer had every row painted wider than the
/// terminal: each line wrapped, the screen slid up, and every position this module writes
/// addressed the wrong row, which also puts every click on the wrong thing. Measured at 10,
/// 20 and 23 columns, all of which painted 24. A narrow window is a real one: two panes
/// side by side in a tmux split is how it happens.
pub fn left(cols: u16) -> usize {
    left_for(cols, false)
}

/// The same, for a column that may be folded away to a strip.
///
/// Folded, it keeps the one thing a glance needs: a state mark and a number per row. The
/// names come back with the column, and the terminal gets twenty six columns back in the
/// meantime.
pub fn left_for(cols: u16, collapsed: bool) -> usize {
    let width = if collapsed {
        sidebar::COLLAPSED
    } else {
        sidebar::WIDTH
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
fn at(row: usize, col: usize) -> String {
    format!("\x1b[{row};{col}H")
}

/// Paint the whole screen.
///
/// Whole, not incremental. A terminal snapshot already arrives as complete rows, so the
/// saving from a diff is one write of a few kilobytes against another, and the cost of
/// getting a diff wrong is a screen that is quietly out of date. When a frame is expensive
/// enough to matter, the daemon has already coalesced it.
pub fn paint(frame: &Frame) -> String {
    let left = left_for(frame.cols, frame.collapsed);
    let right = 0;
    let (pane_cols, pane_rows) = pane_size_for(frame.cols, frame.lines, frame.collapsed, right);
    let mut out = String::with_capacity(frame.cols as usize * frame.lines as usize + 256);

    // Hidden while painting: a cursor walking the screen during a repaint is the flicker
    // people notice most.
    out.push_str("\x1b[?25l");

    let wide_enough = left > 0;

    // The tab bar, over the pane and only over the pane. It starts where the pane starts,
    // past the column and its rule. Split panes are sub-terminals, never top tabs.
    out.push_str(&at(1, left + 1));
    let tabs: Vec<AgentRow> = in_space(frame.rows, frame.space)
        .into_iter()
        .filter(|r| !frame.panes.contains(&r.terminal))
        .collect();
    for (piece, style) in tabs::painted(&tabs, frame.focused, pane_cols as usize, frame.unseen) {
        out.push_str(&match style {
            tabs::Ink::Focused => theme::tab_on(),
            tabs::Ink::Other => theme::tab_off(),
            tabs::Ink::Quiet => theme::fg(theme::Role::Faint),
        });
        out.push_str(&piece);
    }
    out.push_str(theme::RESET);

    // The column runs the whole height, from the top row to the status line. It is taller
    // than the pane by the tab bar's row, which is the row the bar no longer takes from it.
    let column_rows = (frame.lines as usize).saturating_sub(STATUS);
    let panel: Vec<(sidebar::Line, Option<sidebar::Hit>)> = if wide_enough && frame.collapsed {
        sidebar::rows_collapsed(
            frame.spaces,
            frame.rows,
            frame.space.unwrap_or_default(),
            frame.focused,
            column_rows,
        )
    } else if wide_enough {
        sidebar::rows_folded(
            frame.spaces,
            frame.rows,
            frame.space.unwrap_or_default(),
            frame.focused,
            column_rows,
            &frame.fold,
            frame.tick,
            frame.view,
            frame.seen,
            frame.unseen,
        )
    } else {
        Vec::new()
    };
    for row in 0..column_rows {
        out.push_str(&at(row + SIDEBAR_TOP, 1));
        // The state's colour, on the whole row. A person scanning this column is looking
        // for the one row that needs them, and a coloured word inside an otherwise grey
        // line is a word they have to find first.
        //
        // The keyboard's own rows also carry the raised background, which is
        // what the designs draw as a selected background. No marker character:
        // the rows are static text and the background alone says where the
        // keyboard is.
        if let Some((line, hit)) = panel.get(row) {
            let selected = match hit {
                Some(sidebar::Hit::Space(id)) => *id == frame.space.unwrap_or_default(),
                Some(sidebar::Hit::Terminal(terminal)) => *terminal == frame.focused,
                Some(sidebar::Hit::Fold(_) | sidebar::Hit::New | sidebar::Hit::Menu)
                | Some(sidebar::Hit::Collapse)
                | None => false,
            };
            let base = match (selected, line.colour) {
                (true, Some(role)) => theme::selected_in(role),
                (true, None) => theme::selected(),
                (false, Some(role)) => theme::fg(role),
                (false, None) => theme::fg(theme::Role::Text),
            };
            out.push_str(&base);
            // The coloured runs inside the row: the space's name on the number, the
            // opening sign at column zero. Painted by cutting the row at their edges
            // rather than by drawing over it, so the row is still written once and left to
            // right, and a wide character in a name cannot be split. The two runs never
            // overlap by construction (one opens the row, one closes it); ordered by
            // column, the paint below reads as one pass over the text.
            let mut runs: Vec<(usize, usize, theme::Role)> = Vec::new();
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
            out.push_str(&paint_row(line, spans, pane_cols as usize, selected));
        } else {
            paint_tiled_row(frame, pane_row, pane_cols as usize, left, &mut out);
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
    // A transient message wins, then the single next thing to do, then where the keyboard
    // is. Never empty: a blank row is a row that has to be explained.
    let (notice, notice_role) = match (
        frame.message,
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
    out.push_str(&match notice_role {
        Some(role) => theme::status_in(role),
        None => theme::status(),
    });
    out.push_str(&width::fit(&notice, frame.cols as usize));
    out.push_str(theme::RESET);

    out.push_str(&at(frame.lines as usize, 1));
    // Raised, not reverse. A full width bar of reverse video is the brightest thing on the
    // screen, and the key list is the least urgent thing on it.
    out.push_str(&theme::status());
    // The prefix once, then the letters. Spelled out for every key it took 96 columns, and
    // a person on an eighty column window was told about the first three commands and not
    // about the one that gets them out.
    //
    // A mode names its own keys, because in a mode the others do not work.
    let keys = if frame.prompt.is_some() {
        "enter to make it  esc to stop"
    } else if frame.palette.is_some() {
        "ctrl-a space find  enter go  esc back"
    } else if frame.menu.is_some() {
        "up/down or j/k move  enter choose  esc close"
    } else if frame.reading.is_some() {
        "any key to close"
    } else if (frame.cols as usize) >= width::width(EVERY_KEY) {
        // Every key when there is room for every key. On a narrower window the list is cut
        // from the right, so the way out is not last: the cut takes the update key
        // first, and a person stuck in here can still read the one that gets them out.
        EVERY_KEY
    } else {
        "ctrl-a: n tab  c new  r rename  F focus  d detach  space find  m menu  ? keys"
    };
    out.push_str(&width::fit(keys, frame.cols as usize));
    out.push_str(theme::RESET);

    // The palette floats over the terminal, so it is painted last: every row it takes
    // overwrites what the pane and the column drew under it.
    if let Some(palette) = frame.palette {
        paint_palette(frame, palette, &mut out);
    }
    if let Some(prompt) = frame.prompt {
        paint_prompt(frame, prompt, &mut out);
    }
    if let Some(selected) = frame.menu {
        paint_menu(frame, selected, &mut out);
    }
    if let Some(reading) = frame.reading {
        paint_reading(frame, reading, &mut out);
    }

    // The cursor goes where the child put it, offset into the pane, so that what the user
    // types appears where they are looking. With the palette open it sits at the end of
    // the query instead, because that is where typing goes.
    if let Some(prompt) = frame.prompt {
        let w = palette_width(frame.cols);
        let left = (frame.cols as usize).saturating_sub(w) / 2;
        let top = ((frame.lines as usize) / 3).max(TABS + 1);
        // Past the ` > ` marker, at the end of what has been typed. The row
        // carries no border cell, unlike the palette's query row below, so
        // this is one less than the palette's otherwise identical sum.
        let col = (left + 3 + width::width(&prompt.text)).min(left + w.saturating_sub(1));
        out.push_str(&at(top + 1, col + 1));
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
        // Past the border and the prompt marker, at the end of what has been typed.
        let col = (left + 4 + width::width(&palette.query)).min(left + w.saturating_sub(2));
        out.push_str(&at(row + 1, col + 1));
    } else if let Some(snapshot) = frame.snapshot {
        // The cursor lives on the focused tile alone. Unfocused tiles never
        // carry one, so a glance always knows where typing goes. With fewer
        // than two tiles the legacy arithmetic below is byte for byte what it
        // always was.
        if frame.tiles.len() < 2 {
            let (row, cell) = snapshot.cursor;
            let col = cursor_column(
                snapshot.lines.get(row).map(String::as_str).unwrap_or(""),
                cell,
            );
            let row = row.min(pane_rows as usize - 1) + 1 + TABS;
            let col = col.min(pane_cols as usize - 1) + left_for(frame.cols, frame.collapsed) + 1;
            out.push_str(&at(row, col));
        } else if let Some(tile) = frame.tiles.iter().find(|t| t.focused) {
            let (row, cell) = snapshot.cursor;
            let col = cursor_column(
                snapshot.lines.get(row).map(String::as_str).unwrap_or(""),
                cell,
            );
            let pane_left = left_for(frame.cols, frame.collapsed);
            let row = tile.rect.y + row.min(tile.rect.rows.saturating_sub(1)) + 1 + TABS;
            let col = pane_left + tile.rect.x + col.min(tile.rect.cols.saturating_sub(1)) + 1;
            out.push_str(&at(row, col));
        }
    }
    // Always back on, even with no snapshot yet. It is turned off at the top of every frame
    // so the repaint does not walk a visible cursor across the screen, and leaving it off
    // when there is nothing to place it against meant a terminal that had produced no output
    // yet, which is every terminal for its first moments, had no cursor at all.
    out.push_str("\x1b[?25h");
    out
}

/// The single next thing to do, when something is waiting.
///
/// Blocked first, then failed, then finished-while-away: the first of each in
/// daemon order. `None` when nothing waits, and then the status line carries
/// the key list instead, which is how a person finds out they are done.
fn triage(
    rows: &[AgentRow],
    spaces: &[WorkspaceRow],
    focused: u32,
    seen: &sidebar::Seen,
    unseen: &sidebar::Unseen,
) -> Option<String> {
    let row = rows
        .iter()
        .find(|r| r.state == "blocked")
        .or_else(|| rows.iter().find(|r| r.state == "failed"))
        .or_else(|| {
            rows.iter()
                .find(|r| sidebar::is_new(r, focused, seen, unseen))
        })?;
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
    Some(format!(
        "next: {space}/{} is {word}  ^a space",
        palette_label(row),
    ))
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
/// The same order [`triage`] picks in, so the colour and the words are about one terminal.
fn waiting(
    rows: &[AgentRow],
    focused: u32,
    seen: &sidebar::Seen,
    unseen: &sidebar::Unseen,
) -> Option<theme::Role> {
    rows.iter()
        .find(|r| r.state == "blocked")
        .or_else(|| rows.iter().find(|r| r.state == "failed"))
        .or_else(|| {
            rows.iter()
                .find(|r| sidebar::is_new(r, focused, seen, unseen))
        })
        .map(|r| {
            if sidebar::is_new(r, focused, seen, unseen) {
                theme::of("done")
            } else {
                theme::of(&r.state)
            }
        })
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
    // The query line and a border above and below it, and the entries take what is left.
    let chrome = 3;
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

    // The frame, so the box has edges: a floating list with no border is a list that has
    // fallen into whatever is behind it.
    let border = theme::fg(theme::Role::Rule);
    let line_at = |row: usize, out: &mut String| -> bool {
        if row >= lines {
            return false;
        }
        out.push_str(&at(row + 1, left + 1));
        true
    };
    let mut row = top;
    if line_at(row, out) {
        out.push_str(&border);
        out.push('\u{256d}');
        for _ in 0..inner {
            out.push('\u{2500}');
        }
        out.push('\u{256e}');
        out.push_str(theme::RESET);
    }
    row += 1;
    // The query, with the prompt in the colour the pane's own prompts are not, so the box
    // says where typing goes.
    if line_at(row, out) {
        out.push_str(&border);
        out.push('\u{2502}');
        out.push_str(&theme::fg(theme::Role::Faint));
        out.push_str(" > ");
        out.push_str(&theme::fg(theme::Role::Focused));
        out.push_str(&width::fit(&palette.query, inner.saturating_sub(3)));
        out.push_str(&border);
        out.push('\u{2502}');
        out.push_str(theme::RESET);
    }
    row += 1;
    for (kind, selected) in display.iter().skip(first).take(budget) {
        if !line_at(row, out) {
            break;
        }
        row += 1;
        out.push_str(&border);
        out.push('\u{2502}');
        match kind {
            Kind::Header(name) => {
                out.push_str(&theme::fg(theme::Role::Title));
                out.push_str(&width::fit(&format!(" {name}"), inner));
            }
            Kind::Empty => {
                out.push_str(&theme::fg(theme::Role::Faint));
                out.push_str(&width::fit(" no match", inner));
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
                let sub = entry.sub.clone();
                let head = entry.text.clone();
                let room = inner.saturating_sub(width::width(&sub) + 2);
                let head = width::fit(&head, room);
                out.push_str(&base);
                out.push(' ');
                match (space_ink, entry.space_len) {
                    (Some(ink), len) if len > 0 && len <= width::width(&head) => {
                        out.push_str(&if *selected {
                            theme::selected_in(ink)
                        } else {
                            theme::fg(ink)
                        });
                        out.push_str(&width::fit(&head, len));
                        out.push_str(&base);
                        out.push_str(&head.chars().skip(len).collect::<String>());
                    }
                    _ => out.push_str(&head),
                }
                out.push(' ');
                out.push_str(&match (state_ink, *selected) {
                    (Some(ink), true) => theme::selected_in(ink),
                    (Some(ink), false) => theme::fg(ink),
                    (None, true) => theme::selected(),
                    (None, false) => theme::fg(theme::Role::Faint),
                });
                out.push_str(&sub);
                out.push_str(&base);
            }
        }
        out.push_str(&border);
        out.push('\u{2502}');
        out.push_str(theme::RESET);
    }
    if line_at(row, out) {
        out.push_str(&border);
        out.push('\u{2570}');
        for _ in 0..inner {
            out.push('\u{2500}');
        }
        out.push('\u{256f}');
        out.push_str(theme::RESET);
    }
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
    let budget = (lines.saturating_sub(TABS + STATUS + 3)).max(1).min(rows);
    (lines.saturating_sub(budget + 3)) / 2 + 1
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

/// The naming box, painted over the middle of the screen.
///
/// Three rows: what is being asked, what has been typed, and how to get out. In the middle
/// of the pane rather than in the column, because it is a question about something that is
/// about to exist and has no row of its own to sit in yet.
fn paint_prompt(frame: &Frame, prompt: &crate::run::Prompt, out: &mut String) {
    let cols = frame.cols as usize;
    let lines = frame.lines as usize;
    if cols < 20 || lines < TABS + STATUS + 3 {
        return;
    }
    let w = palette_width(frame.cols);
    let left = cols.saturating_sub(w) / 2;
    let top = (lines / 3).max(TABS + 1);
    let rows = [
        (
            theme::fg(theme::Role::Title),
            format!(" {}", prompt.what.asks()),
        ),
        (theme::selected(), format!(" > {}", prompt.text)),
        (
            theme::fg(theme::Role::Faint),
            format!(" {}", prompt.what.hint()),
        ),
    ];
    for (i, (ink, text)) in rows.iter().enumerate() {
        if top + i >= lines {
            break;
        }
        out.push_str(&at(top + i, left + 1));
        out.push_str(ink);
        out.push_str(&width::fit(text, w));
        out.push_str(theme::RESET);
    }
}

/// Where the centred menu box sits: left, top, width and height in cells.
///
/// Zero based, like the painter counts. The height covers both borders plus the
/// title row, one row per menu entry and one hint row. Shared with hit testing,
/// so a click and a repaint agree on which row is which and a second copy of
/// this arithmetic cannot drift.
pub(crate) fn menu_rect(cols: u16, lines: u16) -> Option<(usize, usize, usize, usize)> {
    let cols = cols as usize;
    let lines = lines as usize;
    let (max_name, max_key) = crate::run::MENU
        .iter()
        .fold((0, 0), |(n, k), (name, key, _)| {
            let keypart = if key.is_empty() {
                0
            } else {
                3 + width::width(key)
            };
            (n.max(width::width(name)), k.max(keypart))
        });
    let hint = "arrows/jk move  enter choose  esc close";
    let inner = (1 + max_name + 1 + max_key)
        .max(width::width(" menu") + 1)
        .max(width::width(hint) + 1);
    let w = inner + 2;
    let height = crate::run::MENU.len() + 4;
    if cols < w + 2 || lines < height + 2 {
        return None;
    }
    let left = cols.saturating_sub(w) / 2;
    let top = lines.saturating_sub(height) / 2;
    Some((left, top, w, height))
}

/// Which menu row a one based screen position lands on, if it lands on one.
pub(crate) fn menu_row_at(cols: u16, lines: u16, row: u16, col: u16) -> Option<usize> {
    let (left, top, w, height) = menu_rect(cols, lines)?;
    let r = (row as usize).checked_sub(1)?;
    let c = (col as usize).checked_sub(1)?;
    if c < left || c >= left + w || r < top || r >= top + height {
        return None;
    }
    let item = r.checked_sub(top + 2)?;
    (item < crate::run::MENU.len()).then_some(item)
}

/// The menu, centred over the pane like the palette.
///
/// It used to sit low over the left column, above the row it was opened from,
/// where it covered the spaces list and read as a glitch of the column rather
/// than a choice. A short list opened from a word at the foot of the column is
/// still a modal choice, so it gets the modal treatment: centred, with a title
/// and a hint row, painted last over everything except the read only overlay.
fn paint_menu(frame: &Frame, selected: usize, out: &mut String) {
    let Some((left, top, w, _)) = menu_rect(frame.cols, frame.lines) else {
        return;
    };
    let inner = w.saturating_sub(2);
    let border = theme::fg(theme::Role::Rule);
    let edge = |out: &mut String, row: usize, open: char, fill: char, close: char| {
        out.push_str(&at(row + 1, left + 1));
        out.push_str(&border);
        out.push(open);
        for _ in 0..w.saturating_sub(2) {
            out.push(fill);
        }
        out.push(close);
        out.push_str(theme::RESET);
    };
    edge(out, top, '\u{256d}', '\u{2500}', '\u{256e}');
    // The title names what is open, the way the palette names its query.
    out.push_str(&at(top + 2, left + 1));
    out.push_str(&border);
    out.push('\u{2502}');
    out.push_str(&theme::fg(theme::Role::Title));
    out.push_str(&width::fit(" menu", inner));
    out.push_str(&border);
    out.push('\u{2502}');
    out.push_str(theme::RESET);
    for (i, (name, key, _)) in crate::run::MENU.iter().enumerate() {
        let row = top + 2 + i;
        out.push_str(&at(row + 1, left + 1));
        out.push_str(&border);
        out.push('\u{2502}');
        out.push_str(&if i == selected {
            theme::selected()
        } else {
            theme::fg(theme::Role::Text)
        });
        // The name on the left, its key on the right: the menu teaches the
        // shortcut it replaces, so the next visit skips the menu entirely.
        let keypart = if key.is_empty() {
            String::new()
        } else {
            format!("^a {key}")
        };
        let gap = inner.saturating_sub(1 + width::width(name) + width::width(&keypart));
        let cell = format!(" {name}{}{keypart}", " ".repeat(gap));
        out.push_str(&width::fit(&cell, inner));
        out.push_str(&border);
        out.push('\u{2502}');
        out.push_str(theme::RESET);
    }
    let hint_row = top + 2 + crate::run::MENU.len();
    out.push_str(&at(hint_row + 1, left + 1));
    out.push_str(&border);
    out.push('\u{2502}');
    out.push_str(&theme::fg(theme::Role::Faint));
    out.push_str(&width::fit(
        " arrows/jk move  enter choose  esc close",
        inner,
    ));
    out.push_str(&border);
    out.push('\u{2502}');
    out.push_str(theme::RESET);
    edge(
        out,
        top + 3 + crate::run::MENU.len(),
        '\u{2570}',
        '\u{2500}',
        '\u{256f}',
    );
}

/// A read only overlay: the keys, or what this daemon is set to.
///
/// Every line of it is a real value read from the interface itself rather than a screen
/// somebody drew once: a settings page that says what the program wishes were true is worse
/// than no settings page.
fn paint_reading(frame: &Frame, reading: crate::run::Reading, out: &mut String) {
    let lines: Vec<String> = match reading {
        crate::run::Reading::Keybinds => vec![
            " tabs".to_string(),
            " ctrl-a n      next tab".to_string(),
            " ctrl-a p      previous tab".to_string(),
            " ctrl-a 1..9   the nth tab of this space".to_string(),
            " ctrl-a c      new tab, name it first".to_string(),
            " ctrl-a r      rename the focused tab".to_string(),
            " ctrl-a x      close the focused tab".to_string(),
            " spaces".to_string(),
            " ctrl-a s      next space".to_string(),
            " ctrl-a S      previous space".to_string(),
            " ctrl-a w      new space, name it first".to_string(),
            " ctrl-a W      close this space, when it holds nothing".to_string(),
            " ctrl-a R      rename this space".to_string(),
            " find and view".to_string(),
            " ctrl-a space  find tabs, spaces, agents".to_string(),
            " ctrl-a f      fold the idle groups".to_string(),
            " ctrl-a g      agents as groups or by urgency".to_string(),
            " ctrl-a F      focus view: last prompt, last turns, one row a tool".to_string(),
            " ctrl-a b      fold the left column away".to_string(),
            " ctrl-a m      this menu".to_string(),
            " ctrl-a ?      every key, without the menu".to_string(),
            " panes".to_string(),
            " ctrl-a v/-    split side by side / stacked".to_string(),
            " ctrl-a H/J/K/L focus the pane left/down/up/right".to_string(),
            " ctrl-a o      next pane".to_string(),
            " ctrl-a z      zoom the focused pane".to_string(),
            " ctrl-a X      drop the pane, keeping its terminal".to_string(),
            " session".to_string(),
            " ctrl-a d      detach, keep everything running".to_string(),
            " ctrl-q        detach".to_string(),
            " ctrl-a U      say if an update waits, and how".to_string(),
            " ctrl-a l      repaint".to_string(),
            " mouse".to_string(),
            // The mouse is not a key, and it is the part of this a person is least
            // likely to guess: an interface that takes the mouse has taken their
            // terminal's own selection with it, so it owes them this line.
            " drag          select, and copy when you let go".to_string(),
            " shift-drag    the same, over an agent that took the mouse".to_string(),
            " wheel         scroll: the program's own, or back through what went past".to_string(),
        ],
        crate::run::Reading::Settings => vec![
            format!(" window          {} by {}", frame.cols, frame.lines),
            format!(
                " left column     {} columns{}",
                if frame.collapsed {
                    sidebar::COLLAPSED
                } else {
                    sidebar::WIDTH
                },
                if frame.collapsed { ", folded" } else { "" }
            ),
            format!(" spaces          {}", frame.spaces.len()),
            format!(" terminals       {}", frame.rows.len()),
            format!(
                " agents          {}",
                frame.rows.iter().filter(|r| r.agent).count()
            ),
            format!(
                " waiting         {}",
                frame
                    .rows
                    .iter()
                    .filter(|r| matches!(r.state.as_str(), "blocked" | "failed"))
                    .count()
            ),
        ],
    };
    let title = match reading {
        crate::run::Reading::Keybinds => " keybinds",
        crate::run::Reading::Settings => " settings",
    };
    paint_box(frame, title, &lines, out);
}

/// A bordered box in the middle of the screen, with a title and some rows.
fn paint_box(frame: &Frame, title: &str, rows: &[String], out: &mut String) {
    let cols = frame.cols as usize;
    let lines = frame.lines as usize;
    let widest = rows
        .iter()
        .chain(std::iter::once(&title.to_string()))
        .map(|r| width::width(r))
        .max()
        .unwrap_or(0);
    let w = (widest + 3).min(cols);
    if lines < rows.len() + 4 || w < 6 {
        return;
    }
    let left = cols.saturating_sub(w) / 2;
    let top = lines.saturating_sub(rows.len() + 3) / 2;
    let border = theme::fg(theme::Role::Rule);
    let inner = w - 2;
    let mut row = top;
    let edge = |out: &mut String, row: usize, open: char, close: char| {
        out.push_str(&at(row + 1, left + 1));
        out.push_str(&border);
        out.push(open);
        for _ in 0..inner {
            out.push('\u{2500}');
        }
        out.push(close);
        out.push_str(theme::RESET);
    };
    edge(out, row, '\u{256d}', '\u{256e}');
    row += 1;
    for (i, text) in std::iter::once(title.to_string())
        .chain(rows.iter().cloned())
        .enumerate()
    {
        out.push_str(&at(row + 1, left + 1));
        out.push_str(&border);
        out.push('\u{2502}');
        out.push_str(&theme::fg(if i == 0 {
            theme::Role::Title
        } else {
            theme::Role::Text
        }));
        out.push_str(&width::fit(&text, inner));
        out.push_str(&border);
        out.push('\u{2502}');
        out.push_str(theme::RESET);
        row += 1;
    }
    edge(out, row, '\u{2570}', '\u{256f}');
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

/// Whether a tile covers a pane-local cell.
fn covers(frame: &Frame, x: usize, pane_row: usize) -> bool {
    frame.tiles.iter().any(|t| {
        pane_row >= t.rect.y
            && pane_row < t.rect.y + t.rect.rows
            && x >= t.rect.x
            && x < t.rect.x + t.rect.cols
    })
}

/// One pane row with several tiles in it, plus the dividers between them.
///
/// Each tile shows the leftmost columns of its own snapshot: the focused tile
/// was resized to its rectangle so nothing is hidden there, the others are
/// viewport-cropped. Scrollback, selection and the cursor belong to the
/// focused tile alone. Cells no tile covers are dividers.
fn paint_tiled_row(
    frame: &Frame,
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
        out.push_str(&paint_row(line, spans, tile.rect.cols, selected));
    }
    let mut x = 0;
    while x < pane_cols {
        if covers(frame, x, pane_row) {
            x += 1;
            continue;
        }
        let start = x;
        while x < pane_cols && !covers(frame, x, pane_row) {
            x += 1;
        }
        // A gap with tiles on both sides on this row is a vertical divider,
        // anything else is a horizontal one.
        let vertical = start > 0
            && x < pane_cols
            && covers(frame, start - 1, pane_row)
            && covers(frame, x, pane_row);
        out.push_str(&theme::fg(theme::Role::Rule));
        out.push_str(&at(screen_row, pane_left + 1 + start));
        for _ in start..x {
            out.push(if vertical { '\u{2502}' } else { '\u{2500}' });
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
    for (cell, c) in line.chars().enumerate() {
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
            out.push_str("\x1b[0m");
            if let Some(span) = span {
                out.push_str(&sgr(span));
            }
            if mark {
                out.push_str("\x1b[7m");
            }
            current = span;
            reversed = mark;
        }
        out.push(c);
        used += w;
    }
    if current.is_some() || reversed {
        out.push_str("\x1b[0m");
    }
    for _ in used..cols {
        out.push(' ');
    }
    out
}

fn is_plain(s: &SpanWire) -> bool {
    s.fg.is_none() && s.bg.is_none() && s.attrs == 0
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
    let left = left_for(frame.cols, frame.collapsed);
    let right = 0;
    let (pane_cols, pane_rows) = pane_size_for(frame.cols, frame.lines, frame.collapsed, right);
    let rows = paint_focus_list(events, pane_cols as usize, offset, pane_rows as usize);
    for (i, (text, role)) in rows.iter().enumerate().take(pane_rows as usize) {
        out.push_str(&at(TABS + 1 + i, left + 1));
        out.push_str(&theme::fg(*role));
        out.push_str(text);
        out.push_str(theme::RESET);
    }
    out.push_str("\x1b[?25l");
}

/// The escape sequence for one run's style.
fn sgr(span: &SpanWire) -> String {
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
    if let Some(fg) = span.fg {
        parts.push(colour(fg, true));
    }
    if let Some(bg) = span.bg {
        parts.push(colour(bg, false));
    }
    format!("\x1b[{}m", parts.join(";"))
}

/// A colour, written the way every terminal since xterm reads it.
///
/// The first sixteen palette entries get their own short codes rather than `38;5;n`,
/// because those are the ones a person has themed and some terminals treat the two spellings
/// differently.
fn colour(c: ColourWire, foreground: bool) -> String {
    match c {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spaces() -> Vec<WorkspaceRow> {
        vec![WorkspaceRow {
            id: 1,
            name: "app".to_string(),
            terminals: 1,
        }]
    }

    fn row(terminal: u32, program: &str, state: &str) -> AgentRow {
        AgentRow {
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

    /// The naming box cursor sits at the end of what has been typed, past the
    /// ` > ` marker: one cell left of where it used to sit, when the sum was
    /// copied from the bordered palette row.
    #[test]
    fn the_naming_box_cursor_sits_at_the_end_of_the_typed_text() {
        let rows = [row(1, "zsh", "idle")];
        let prompt = crate::run::Prompt {
            what: crate::run::Naming::Space,
            text: "ab".to_string(),
        };
        let cols = 100u16;
        let lines = 24u16;
        let painted = paint(&Frame {
            rows: &rows,
            spaces: &spaces(),
            panes: &std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: None,
            cols,
            lines,
            message: None,
            hint: None,
            fold: sidebar::Fold::all_open(),
            palette: None,
            prompt: Some(&prompt),
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            view: sidebar::View::Grouped,
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
        });
        // The box is centred: ` > ab` starts three cells past its left edge,
        // so the caret lands on the cell right after the `b`.
        let w = cols.min(60).max(cols.min(20)) as usize;
        let left = (cols as usize - w) / 2;
        let top = ((lines as usize) / 3).max(TABS + 1);
        let want = format!("\x1b[{};{}H", top + 1, left + 4 + 2);
        assert!(
            painted.contains(&want),
            "no caret at the end of the typed text: {painted:?}"
        );
    }

    #[test]
    fn the_terminals_output_the_tabs_and_both_panels_are_in_the_frame() {
        let rows = [row(1, "zsh", "idle"), row(2, "claude", "working")];
        let snapshot = snapshot(&["hello from the shell"]);
        let painted = paint(&Frame {
            rows: &rows,
            spaces: &spaces(),
            panes: &std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 2,
            snapshot: Some(&snapshot),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            fold: sidebar::Fold::all_open(),
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            view: sidebar::View::Grouped,
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
        });
        assert!(painted.contains("hello from the shell"));
        // Once on the tab and once in the agents panel: the bar says what is open in this
        // space and the panel says what it is doing.
        assert!(painted.matches("claude").count() >= 2, "{painted:?}");
        assert!(painted.contains("d detach"));
        assert!(painted.contains("spaces"), "no spaces panel: {painted:?}");
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
            rows: &rows,
            spaces: &spaces(),
            panes: &panes,
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            fold: sidebar::Fold::all_open(),
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            view: sidebar::View::Grouped,
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
    fn a_focused_pane_leaves_the_bar_without_a_bracket() {
        // Panes are sub-terminals, never tabs: with the keyboard in one, no
        // tab wears the bracket, and the tile border carries the focus alone.
        let rows = [row(1, "zsh", "idle"), row(2, "claude", "working")];
        let panes: std::collections::BTreeSet<u32> = std::collections::BTreeSet::from([2]);
        let snapshot = snapshot(&["hello from the shell"]);
        let painted = paint(&Frame {
            rows: &rows,
            spaces: &spaces(),
            panes: &panes,
            space: Some(1),
            focused: 2,
            snapshot: Some(&snapshot),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            fold: sidebar::Fold::all_open(),
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            view: sidebar::View::Grouped,
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
            rows: &rows,
            spaces: &spaces(),
            panes: &std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: None,
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            fold: sidebar::Fold::all_open(),
            palette: None,
            prompt: None,
            menu: Some(0),
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            view: sidebar::View::Grouped,
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
        });
        assert!(painted.contains("new tab"), "menu missing row: {painted:?}");
        assert!(painted.contains("^a c"), "menu hides its key: {painted:?}");
        assert!(painted.contains("split side by side"), "{painted:?}");
        assert!(painted.contains("rename space"), "{painted:?}");
        assert!(painted.contains("close tab"), "{painted:?}");
        assert!(
            painted.contains("^a x"),
            "close tab hides its key: {painted:?}"
        );
        assert!(painted.contains(" menu"), "menu missing title: {painted:?}");
        assert!(
            painted.contains("enter choose"),
            "menu missing hint: {painted:?}"
        );
    }

    #[test]
    fn the_menu_is_centred_not_hugging_the_column() {
        let Some((left, top, w, height)) = menu_rect(80, 24) else {
            panic!("no menu rect on 80x24");
        };
        assert_eq!(height, crate::run::MENU.len() + 4);
        assert_eq!(left, (80 - w) / 2, "menu not centred across");
        assert_eq!(top, (24 - height) / 2, "menu not centred down");
        assert!(left > 0, "menu still hugs the left edge");
    }

    #[test]
    fn a_menu_click_address_matches_the_painted_rows() {
        // Row 0 sits two rows under the box top: border plus title.
        let Some((left, top, _, _)) = menu_rect(80, 24) else {
            panic!("no menu rect on 80x24");
        };
        let row = (top + 2 + 4 + 1) as u16;
        let col = (left + 3 + 1) as u16;
        assert_eq!(menu_row_at(80, 24, row, col), Some(4), "close tab row");
        assert_eq!(menu_row_at(80, 24, 1, 1), None, "outside reads as a row");
    }

    #[test]
    fn a_terminal_line_cannot_overwrite_the_rule_or_run_past_the_window() {
        let rows = [row(1, "zsh", "idle")];
        let long = "x".repeat(500);
        let snapshot = snapshot(&[&long]);
        let painted = paint(&Frame {
            rows: &rows,
            spaces: &spaces(),
            panes: &std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 40,
            lines: 5,
            message: None,
            hint: None,
            fold: sidebar::Fold::all_open(),
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            view: sidebar::View::Grouped,
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
            rows: &rows,
            spaces: &spaces(),
            panes: &std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: None,
            cols: 40,
            lines: 5,
            message: Some("starting a terminal"),
            hint: None,
            fold: sidebar::Fold::all_open(),
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            view: sidebar::View::Grouped,
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
        }]];
        let rows = [row(1, "sh", "idle")];
        let painted = paint(&Frame {
            rows: &rows,
            spaces: &spaces(),
            panes: &std::collections::BTreeSet::new(),
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
            fold: sidebar::Fold::all_open(),
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            view: sidebar::View::Grouped,
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
            rows: &rows,
            spaces: &spaces(),
            panes: &std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 40,
            lines: 5,
            message: None,
            hint: None,
            fold: sidebar::Fold::all_open(),
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            view: sidebar::View::Grouped,
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
        });
        // The row the shell printed, found by what it printed and then read as bytes. The
        // chrome around it resets after every piece it paints, so the pane's own run
        // starts at one of those resets; which rule glyph precedes it depends on whether
        // the panel drew a separator on that row, and that is not what is being tested.
        let (pane_cols, _) = pane_size(40, 5);
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
        }]];
        let rows = [row(1, "sh", "idle")];
        let painted = paint(&Frame {
            rows: &rows,
            spaces: &spaces(),
            panes: &std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 30,
            lines: 5,
            message: None,
            hint: None,
            fold: sidebar::Fold::all_open(),
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            view: sidebar::View::Grouped,
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
            rows: &rows,
            spaces: &spaces(),
            panes: &std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            fold: sidebar::Fold::all_open(),
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            view: sidebar::View::Grouped,
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
        });
        // One row down for the tab bar, and the pane starts after the column and its rule.
        let want = format!("\x1b[{};{}H", 1 + TABS, 2 + sidebar::WIDTH + 2);
        assert!(
            painted.ends_with(&format!("{want}\x1b[?25h")),
            "{painted:?}"
        );
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
                rows: &rows,
                spaces: &spaces(),
                panes: &std::collections::BTreeSet::new(),
                space: Some(1),
                focused: 1,
                snapshot: Some(&snapshot),
                cols,
                lines: 6,
                message: None,
                hint: None,
                fold: sidebar::Fold::all_open(),
                palette: None,
                prompt: None,
                menu: None,
                collapsed: false,
                reading: None,
                selection: None,
                scroll: 0,
                tick: 0,
                view: sidebar::View::Grouped,
                seen: &sidebar::Seen::new(),
                unseen: &sidebar::Unseen::new(),
                anchor: 0,
                rows_back: 0,
                tiles: Vec::new(),
                behind: &crate::run::Behind::default(),
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
            rows: &rows,
            spaces: &spaces(),
            panes: &std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: None,
            cols: 40,
            lines: 5,
            message: None,
            hint: None,
            fold: sidebar::Fold::all_open(),
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            view: sidebar::View::Grouped,
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
        });
        assert!(painted.contains("\x1b[?25l"));
        assert!(painted.ends_with("\x1b[?25h"), "{painted:?}");
    }

    /// The way out is the one key a person stuck in here has to be able to read, so it is
    /// never the one the cut takes.
    #[test]
    fn the_key_list_always_keeps_the_way_out() {
        let rows = [row(1, "sh", "idle")];
        for cols in [40u16, 60, 80, 100, 200] {
            let painted = paint(&Frame {
                rows: &rows,
                spaces: &spaces(),
                panes: &std::collections::BTreeSet::new(),
                space: Some(1),
                focused: 1,
                snapshot: None,
                cols,
                lines: 24,
                message: None,
                hint: None,
                fold: sidebar::Fold::all_open(),
                palette: None,
                prompt: None,
                menu: None,
                collapsed: false,
                reading: None,
                selection: None,
                scroll: 0,
                tick: 0,
                view: sidebar::View::Grouped,
                seen: &sidebar::Seen::new(),
                unseen: &sidebar::Unseen::new(),
                anchor: 0,
                rows_back: 0,
                tiles: Vec::new(),
                behind: &crate::run::Behind::default(),
            });
            let keys = strip(painted.rsplit(&at(24, 1)).next().unwrap_or_default());
            assert!(
                keys.contains("d detach") || cols < 52,
                "{cols} columns: no way out on the key row: {keys:?}"
            );
            assert!(
                width::width(&keys) <= cols as usize,
                "{cols} columns: the key row is wider than the window: {keys:?}"
            );
        }
    }

    #[test]
    fn a_pane_row_is_never_painted_over_the_status_line() {
        let rows = [row(1, "sh", "idle")];
        let snapshot = snapshot(&["PANE_CONTENT"]);
        for lines in [3u16, 4, 5] {
            let painted = paint(&Frame {
                rows: &rows,
                spaces: &spaces(),
                panes: &std::collections::BTreeSet::new(),
                space: Some(1),
                focused: 1,
                snapshot: Some(&snapshot),
                cols: 60,
                lines,
                message: Some("STATUS_LINE"),
                hint: None,
                fold: sidebar::Fold::all_open(),
                palette: None,
                prompt: None,
                menu: None,
                collapsed: false,
                reading: None,
                selection: None,
                scroll: 0,
                tick: 0,
                view: sidebar::View::Grouped,
                seen: &sidebar::Seen::new(),
                unseen: &sidebar::Unseen::new(),
                anchor: 0,
                rows_back: 0,
                tiles: Vec::new(),
                behind: &crate::run::Behind::default(),
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

    /// A space's colour has to reach the agents panel, or it says nothing that the word
    /// beside it did not already say.
    #[test]
    fn a_space_wears_one_colour_in_both_panels() {
        let rows = [row(1, "claude", "working")];
        let painted = paint(&Frame {
            rows: &rows,
            spaces: &spaces(),
            panes: &std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: None,
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            fold: sidebar::Fold::all_open(),
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            view: sidebar::View::Grouped,
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
        });
        let ink = theme::fg(theme::space_of(1));
        let raised = theme::selected_in(theme::space_of(1));
        // Once in the spaces panel, once beside the agent that lives there. Either form,
        // because the row may also have the keyboard.
        let worn = painted.matches(ink.as_str()).count() + painted.matches(raised.as_str()).count();
        assert!(
            worn >= 2,
            "the space colour appears {worn} times: {painted:?}"
        );
        // And the state the agent is in is still its own colour, not the space's.
        assert!(
            painted.contains(&theme::fg(theme::Role::Working))
                || painted.contains(&theme::selected_in(theme::Role::Working)),
            "the state lost its colour to the space: {painted:?}"
        );
    }

    /// The frame is static: two ticks paint the same bytes, the working
    /// row says its word on the right, and no spinner glyph ever appears.
    #[test]
    fn the_working_row_is_static_across_ticks() {
        let rows = [row(1, "claude", "working")];
        let paint_at = |tick: u64| {
            paint(&Frame {
                rows: &rows,
                spaces: &spaces(),
                panes: &std::collections::BTreeSet::new(),
                space: Some(1),
                focused: 1,
                snapshot: None,
                cols: 80,
                lines: 24,
                message: None,
                hint: None,
                fold: sidebar::Fold::all_open(),
                palette: None,
                prompt: None,
                menu: None,
                collapsed: false,
                reading: None,
                selection: None,
                scroll: 0,
                tick,
                view: sidebar::View::Grouped,
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
            })
        };
        let first = paint_at(0);
        let second = paint_at(1);
        assert_eq!(first, second, "the tick repainted a static screen");
        // The working row says its word on the right instead of spinning.
        assert!(
            strip(&first).contains("working"),
            "the working word never reached the screen: {first:?}"
        );
        for glyph in ['\u{2809}', '\u{2838}', '\u{28c0}', '\u{2807}'] {
            assert!(
                !strip(&first).contains(glyph),
                "a spinner frame leaked onto the screen: {first:?}"
            );
        }
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
        }];
        let rows = [row(1, "claude", "working")];
        let painted = paint(&Frame {
            rows: &rows,
            spaces: &wide,
            panes: &std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: None,
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            fold: sidebar::Fold::all_open(),
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            view: sidebar::View::Grouped,
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
            rows: &rows,
            spaces: &spaces(),
            panes: &std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 40,
            lines: 3,
            message: None,
            hint: None,
            fold: sidebar::Fold::all_open(),
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            view: sidebar::View::Grouped,
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
        });
        assert!(!painted.contains('\u{fffd}'));
    }

    fn blocked_row(terminal: u32, program: &str) -> AgentRow {
        AgentRow {
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
        }
    }

    /// Triage: one line saying what to do next, while something waits.
    #[test]
    fn a_blocked_agent_puts_the_next_action_on_the_status_line() {
        let rows = [blocked_row(1, "codex")];
        let snapshot = snapshot(&["waiting"]);
        let painted = paint(&Frame {
            rows: &rows,
            spaces: &spaces(),
            panes: &std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            fold: sidebar::Fold::default_closed(),
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            view: sidebar::View::Grouped,
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
        });
        assert!(
            painted.contains("next: app/codex is blocked"),
            "{painted:?}"
        );
        assert!(painted.contains("^a space"), "{painted:?}");
    }

    #[test]
    fn with_nothing_waiting_the_status_line_carries_the_keys() {
        let rows = [row(1, "zsh", "idle")];
        let snapshot = snapshot(&["quiet"]);
        let painted = paint(&Frame {
            rows: &rows,
            spaces: &spaces(),
            panes: &std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            fold: sidebar::Fold::default_closed(),
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            view: sidebar::View::Grouped,
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
        });
        assert!(painted.contains("d detach"), "{painted:?}");
        assert!(!painted.contains("next:"), "{painted:?}");
    }

    #[test]
    fn a_transient_message_wins_over_the_next_action() {
        let rows = [blocked_row(1, "codex")];
        let snapshot = snapshot(&["waiting"]);
        let painted = paint(&Frame {
            rows: &rows,
            spaces: &spaces(),
            panes: &std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 80,
            lines: 24,
            message: Some("starting a terminal"),
            hint: None,
            fold: sidebar::Fold::default_closed(),
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            view: sidebar::View::Grouped,
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
        });
        assert!(painted.contains("starting a terminal"), "{painted:?}");
    }

    /// The palette floats over the terminal with what needs a person first.
    #[test]
    fn an_open_palette_lists_what_needs_you_over_the_pane() {
        let rows = [blocked_row(1, "codex"), row(2, "claude", "working")];
        let snapshot = snapshot(&["terminal output"]);
        let palette = Palette::open();
        let painted = paint(&Frame {
            rows: &rows,
            spaces: &spaces(),
            panes: &std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 2,
            snapshot: Some(&snapshot),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            fold: sidebar::Fold::default_closed(),
            palette: Some(&palette),
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            view: sidebar::View::Grouped,
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
        }]];
        let palette = Palette::open();
        let painted = paint(&Frame {
            rows: &rows,
            spaces: &spaces(),
            panes: &std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 2,
            snapshot: Some(&snapshot),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            fold: sidebar::Fold::default_closed(),
            palette: Some(&palette),
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            view: sidebar::View::Grouped,
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

    #[test]
    fn a_palette_query_filters_what_is_on_screen() {
        let rows = [blocked_row(1, "codex"), row(2, "claude", "working")];
        let snapshot = snapshot(&["terminal output"]);
        let mut palette = Palette::open();
        for c in "working".chars() {
            palette.push(c);
        }
        let painted = paint(&Frame {
            rows: &rows,
            spaces: &spaces(),
            panes: &std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 2,
            snapshot: Some(&snapshot),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            fold: sidebar::Fold::default_closed(),
            palette: Some(&palette),
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            view: sidebar::View::Grouped,
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
        });
        let shown = strip(&painted);
        assert!(shown.contains("> working"), "{shown:?}");
        assert!(!shown.contains("app codex"), "{shown:?}");
    }

    /// Folding reaches the screen: nine agents become three actionable rows.
    #[test]
    fn a_folded_panel_fits_nine_agents_without_scrolling() {
        let spaces = vec![WorkspaceRow {
            id: 1,
            name: "app".to_string(),
            terminals: 9,
        }];
        let states = [
            "blocked", "failed", "working", "working", "done", "idle", "idle", "idle", "idle",
        ];
        let rows: Vec<AgentRow> = states
            .iter()
            .enumerate()
            .map(|(i, state)| AgentRow {
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
            })
            .collect();
        let snapshot = snapshot(&["output"]);
        // Terminal 5 is done and already looked at, so it folds like before.
        let mut seen = sidebar::Seen::new();
        seen.insert((5, 1));
        let painted = paint(&Frame {
            rows: &rows,
            spaces: &spaces,
            panes: &std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 80,
            // Room for two-line pairs: every open agent takes two rows, so
            // the folded summary needs the height to fit without scrolling.
            lines: 30,
            message: None,
            hint: None,
            fold: sidebar::Fold::default_closed(),
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            view: sidebar::View::Grouped,
            seen: &seen,
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
        });
        assert!(painted.contains("1 finished"), "{painted:?}");
        assert!(painted.contains("4 idle"), "{painted:?}");
        assert!(painted.contains("agents 4 of 9"), "{painted:?}");
    }

    /// A selection is drawn as selected, and only where it is.
    #[test]
    fn the_selected_part_of_a_row_is_marked_and_the_rest_is_not() {
        let snapshot = snapshot(&["the quick brown fox", "another line"]);
        let rows = [row(1, "sh", "idle")];
        let mut selection = crate::selection::Selection::start(0, 4);
        selection.extend(0, 8);
        let painted = paint(&Frame {
            rows: &rows,
            spaces: &spaces(),
            panes: &std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 60,
            lines: 6,
            message: None,
            hint: None,
            fold: sidebar::Fold::all_open(),
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: Some(selection),
            scroll: 0,
            tick: 0,
            view: sidebar::View::Grouped,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &crate::run::Behind::default(),
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
                rows: &rows,
                spaces: &spaces(),
                panes: &std::collections::BTreeSet::new(),
                space: Some(1),
                focused: 1,
                snapshot: Some(&snapshot),
                cols: 60,
                lines: 6,
                message: None,
                hint: None,
                fold: sidebar::Fold::all_open(),
                palette: None,
                prompt: None,
                menu: None,
                collapsed: false,
                reading: None,
                selection,
                scroll: 0,
                tick: 0,
                view: sidebar::View::Grouped,
                seen: &sidebar::Seen::new(),
                unseen: &sidebar::Unseen::new(),
                anchor: 0,
                rows_back: 0,
                tiles: Vec::new(),
                behind: &crate::run::Behind::default(),
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
            rows: &rows,
            spaces: &spaces(),
            panes: &std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 60,
            lines: 7,
            message: None,
            hint: None,
            fold: sidebar::Fold::all_open(),
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 2,
            tick: 0,
            view: sidebar::View::Grouped,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            // Pinned to the second to last row written, so the pane shows the last two
            // rows that went past and then the screen from its top.
            anchor: 98,
            rows_back: 2,
            behind: &behind,
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
            rows: &rows,
            spaces: &spaces(),
            panes: &std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 60,
            lines: 6,
            message: None,
            hint: None,
            fold: sidebar::Fold::all_open(),
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick: 0,
            view: sidebar::View::Grouped,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &behind,
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
            rows,
            spaces,
            panes: &std::collections::BTreeSet::new(),
            space: Some(1),
            focused,
            snapshot: focused_snap,
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            fold: sidebar::Fold::all_open(),
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
            view: sidebar::View::Grouped,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            behind: &behind,
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
            rows: &rows,
            spaces: &spaces,
            panes: &std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snap),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            fold: sidebar::Fold::all_open(),
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
            view: sidebar::View::Grouped,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            behind: &behind,
        });
        let tiled = paint(&Frame {
            rows: &rows,
            spaces: &spaces,
            panes: &std::collections::BTreeSet::new(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snap),
            cols: 80,
            lines: 24,
            message: None,
            hint: None,
            fold: sidebar::Fold::all_open(),
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
            view: sidebar::View::Grouped,
            seen: &sidebar::Seen::new(),
            unseen: &sidebar::Unseen::new(),
            behind: &behind,
        });
        assert_eq!(tiled, legacy);
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
        let half = (pane_rows as usize - 1) / 2;
        let painted = paint_two_panes(
            &rows,
            &spaces,
            Some(&top),
            Some(&bottom),
            1,
            vec![
                (
                    1,
                    crate::layout::Rect {
                        x: 0,
                        y: 0,
                        cols: pane_cols as usize,
                        rows: half,
                    },
                ),
                (
                    2,
                    crate::layout::Rect {
                        x: 0,
                        y: half + 1,
                        cols: pane_cols as usize,
                        rows: pane_rows as usize - half - 1,
                    },
                ),
            ],
        );
        assert!(painted.contains("top row"), "{painted:?}");
        assert!(painted.contains("bottom row"), "{painted:?}");
        assert!(painted.contains('\u{2500}'), "no divider: {painted:?}");
    }
    #[test]
    fn the_pane_shrinks_from_the_right_exactly_as_it_shrinks_from_the_left() {
        assert_eq!(
            pane_size_for(120, 24, false, 39),
            (120 - sidebar::WIDTH as u16 - 1 - 39, 21)
        );
        assert_eq!(pane_size_for(120, 24, false, 0), pane_size(120, 24));
    }
}
