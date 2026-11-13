//! An agent's own numbered menu, read off the bottom of its screen: "Do you want to proceed?"
//! over "1. Yes", "2. Yes, and do not ask again", "3. No". A person's client shows it on the
//! agent's row (`AgentRow::menu`) and answers it with `AnswerMenu`, which types the keys
//! `menu_keys` works out from the screen as it is then. The phone's `menu.ts` reads the same
//! way and keeps doing so for older daemons.

use std::hash::{DefaultHasher, Hash, Hasher};

use qoop_terminal::{Colour, Snapshot, Span};
use unicode_width::UnicodeWidthChar;

use crate::control::{AgentMenu, ControlResponse, MenuChoice};

/// One option. `detail`: the rows under its first one, as the agent drew them (Claude Code's
/// description under an answer, or the rest of a long one wrapped). `checked`: the box of a
/// multiple choice question, ticked or not; `None` on a plain answer. `input`: Claude Code's
/// field for an answer of one's own ("Type something" until it holds words).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuOption {
    pub key: String,
    pub label: String,
    pub selected: bool,
    pub detail: Option<String>,
    pub checked: Option<bool>,
    pub input: bool,
}

/// One tab of a Claude Code question dialog ("☒ Branch commit  ☐ AWS spend  ✔ Submit"): done
/// once its question is answered, `submit` for the last one that sends the answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    pub name: String,
    pub done: bool,
    pub submit: bool,
}

/// `at`: the row of its first option. `context`: the rows above it (the command it asks
/// about), so a menu over another command is another menu. `prompt`: every row of the prompt
/// above the options, from the box edge, rule or tool title that opens it (else the whole
/// screen above): what an answer approves, and what `seal` binds. `tabs`, `current`: the dialog's
/// tab row, empty and `None` without one. `top`: the first row of the question block, tab row
/// included (`at` when there is none). `submit`: a multiple choice question's Submit row sits
/// under the option with this number. `confirm`: a pick is the number then Enter (the preview
/// layout, where the number only moves the cursor).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Menu {
    pub question: String,
    pub options: Vec<MenuOption>,
    pub at: usize,
    pub context: String,
    pub prompt: String,
    pub tabs: Vec<Tab>,
    pub current: Option<usize>,
    pub top: usize,
    pub submit: Option<usize>,
    pub confirm: bool,
}

/// What to type for an answer, and whether it answers the menu (a box ticked does not).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuKeys {
    pub keys: String,
    pub answers: bool,
}

/// The key of a multiple choice question's Submit: no option is numbered so.
pub const SUBMIT: &str = "Submit";
/// `AnswerMenu`'s refusals, short codes a client words itself. The menu shown is no longer on
/// the agent's screen, or the screen has not been read since the last keys.
pub const ALREADY_ANSWERED: &str = "already_answered";
/// A key the menu has not.
pub const NO_SUCH_ANSWER: &str = "no_such_answer";
/// Keys are still queued for the terminal: ask again.
pub const BUSY: &str = "busy";

/// A connection no person sees through: programs never answer a menu.
pub const NOT_A_PERSON: &str = "not_a_person";
/// What may sit under a menu: a hint line, a rule, the status line.
const BELOW: usize = 4;
/// How far up a menu's question may be from its first option.
const ABOVE: usize = 3;
/// How far up from the options a tab row may be: a long question sits between.
const TABS_ABOVE: usize = 14;
/// How far up from the bottom row a menu's first option may be.
const BOTTOM: usize = 24;

/// The cursor an agent puts on the chosen row, before the number.
const CURSORS: [char; 3] = ['>', '❯', '›'];
/// A box's border at either end of a row, which is not the menu's text.
const BORDERS: [char; 3] = ['│', '┃', '|'];
/// The marks of a tab row's cells: a box, answered or not, or the tick of Submit.
const BOXES: [char; 5] = ['☐', '☒', '☑', '✔', '✓'];

/// A row with a box's border cut from either end, and its trailing blanks.
fn unbox(row: &str) -> String {
    let mut s = row;
    if let Some(rest) = s.trim_start().strip_prefix(BORDERS) {
        s = rest.strip_prefix(char::is_whitespace).unwrap_or(rest);
    }
    if let Some(rest) = s.trim_end().strip_suffix(BORDERS) {
        s = rest;
    }
    s.trim_end().to_string()
}

/// `> 1. Yes`: whether the cursor is on it, its number as written, and its text.
fn option_row(row: &str) -> Option<(bool, &str, &str)> {
    let s = row.trim_start();
    let (cursor, s) = match s.strip_prefix(CURSORS) {
        Some(rest) => (true, rest.trim_start()),
        None => (false, s),
    };
    let digits = s.len() - s.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if !(1..=2).contains(&digits) {
        return None;
    }
    let (number, s) = s.split_at(digits);
    let s = s.strip_prefix(['.', ')'])?;
    let text = s.trim_start();
    (text.len() < s.len() && !text.is_empty()).then_some((cursor, number, text))
}

/// A multiple choice question's Submit row, and whether the cursor is on it.
fn submit_row(row: &str) -> Option<bool> {
    let s = row.trim_start();
    let (cursor, s) = match s.strip_prefix(CURSORS) {
        Some(rest) => (true, rest.trim_start()),
        None => (false, s),
    };
    (s.trim_end() == SUBMIT).then_some(cursor)
}

/// At least `n` of `set` at the start of `t`.
fn starts_with_run(t: &str, set: &[char], n: usize) -> bool {
    t.chars().take_while(|c| set.contains(c)).count() >= n
}

/// The rest of an option's text, wrapped onto the next row: indented, and not a rule.
fn wrapped(row: &str) -> bool {
    let t = row.trim_start();
    row.chars().take_while(|c| c.is_whitespace()).count() >= 2
        && !t.is_empty()
        && !starts_with_run(t, &['─', '━', '-'], 4)
}

/// A rule between options: Claude Code draws one over its last answer ("Chat about this").
fn rule(row: &str) -> bool {
    starts_with_run(row.trim_start(), &['─', '━'], 4)
}

/// A row drawn only of rule and box characters, eight or more.
fn is_rule(line: &str) -> bool {
    const RULE: &str = "─━═╌┄-=│┃║╭╮╰╯┌┐└┘├┤┬┴┼╔╗╚╝";
    line.chars().all(|c| c.is_whitespace() || RULE.contains(c)) && line.trim().chars().count() >= 8
}

/// A row that says something: not blank, not a rule or a box edge, not a bare prompt.
fn quiet(row: &str) -> bool {
    let t = row.trim();
    !t.is_empty()
        && !is_rule(t)
        && !row
            .chars()
            .all(|c| c.is_whitespace() || "─━═-╭╮╰╯┌┐└┘│┃".contains(c))
        && !(t.chars().count() == 1 && ">❯›$%#".contains(t))
}

/// A tool's own title over the command it asks about (Claude Code's "Bash command").
fn tool(t: &str) -> bool {
    [
        "Bash command",
        "Edit file",
        "Create file",
        "Write file",
        "Read file",
        "Fetch",
        "Tool use",
    ]
    .iter()
    .any(|name| t.eq_ignore_ascii_case(name))
}

/// A box's top edge or a rule: either marks where the prompt around a command starts.
fn edge(row: &str) -> bool {
    let t = row.trim_start();
    if t.starts_with(['╭', '┌']) {
        return true;
    }
    let t = t.trim_end();
    t.chars().count() >= 4 && t.chars().all(|c| "─━═".contains(c))
}

/// The hint of Claude Code's preview layout, as a phrase of its own.
fn notes_hint(row: &str) -> bool {
    const HINT: &str = "n to add notes";
    let word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    row.match_indices(HINT).any(|(p, _)| {
        !word(row[..p].chars().next_back()) && !word(row[p + HINT.len()..].chars().next())
    })
}

/// A row with the preview layout's side pane cut off: two blanks, then the pane's border.
fn cut_pane(row: &str) -> String {
    let (mut start, mut run) = (0, 0);
    for (i, c) in row.char_indices() {
        if c.is_whitespace() {
            if run == 0 {
                start = i;
            }
            run += 1;
            continue;
        }
        if run >= 2 && "│┃|├┌└╭╰+".contains(c) {
            return row[..start].to_string();
        }
        run = 0;
    }
    row.to_string()
}

/// Every row of the prompt above the options at `at`, from the box edge, rule or tool title
/// that opens it, else the whole screen above. A long command is all of it, not its last rows.
fn prompt_rows(rows: &[String], at: usize) -> String {
    let from = (0..at)
        .rev()
        .find(|&i| edge(&rows[i]) || tool(rows[i].trim()))
        .unwrap_or(0);
    rows[from..at]
        .iter()
        .map(|r| r.trim())
        .filter(|r| !r.is_empty() && !is_rule(r))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A list row in a question: a bullet or a number, then a blank.
fn list_item(t: &str) -> bool {
    let rest = match t.strip_prefix(['-', '*', '•', '●', '→', '⎿', '#']) {
        Some(rest) => rest,
        None => {
            let digits = t.trim_start_matches(|c: char| c.is_ascii_digit());
            if digits.len() == t.len() {
                return false;
            }
            match digits.strip_prefix(['.', ')']) {
                Some(rest) => rest,
                None => return false,
            }
        }
    };
    rest.starts_with(char::is_whitespace)
}

/// A multiple choice answer's box before its label (`[ ]`, `[x]`, `[✔]`): its tick and the label.
fn checkbox(text: &str) -> Option<(char, &str)> {
    let mut rest = text.strip_prefix('[')?.chars();
    let tick = rest.next().filter(|c| " xX✓✔".contains(*c))?;
    let after = rest.as_str().strip_prefix(']')?;
    let label = after.trim_start();
    (label.len() < after.len()).then_some((tick, label))
}

/// One option as the agent drew it: a box before its label is its tick, not its words.
fn option_of(key: String, text: &str, selected: bool) -> MenuOption {
    let (label, checked) = match checkbox(text) {
        Some((tick, label)) => (label, Some(tick != ' ')),
        None => (text, None),
    };
    MenuOption {
        key,
        label: label.to_string(),
        selected,
        detail: None,
        checked,
        input: false,
    }
}

/// The cells of a tab row: each as written, its mark, and its name, which ends at two blanks,
/// at the next cell or arrow, or at the end.
fn cells(row: &str) -> Vec<(String, char, String)> {
    let c: Vec<char> = row.chars().collect();
    let blanks = |from: usize| c[from..].iter().take_while(|x| x.is_whitespace()).count();
    let ends = |k: usize| {
        let ws = blanks(k);
        ws >= 2 || k + ws == c.len() || BOXES.contains(&c[k + ws]) || c[k + ws] == '→'
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let ws = blanks(i + 1);
        let start = i + 1 + ws;
        if !BOXES.contains(&c[i]) || ws == 0 || start >= c.len() {
            i += 1;
            continue;
        }
        let end = (start + 1..=c.len()).find(|&k| ends(k)).unwrap_or(c.len());
        out.push((
            c[i..end].iter().collect(),
            c[i],
            c[start..end].iter().collect(),
        ));
        i = end;
    }
    out
}

/// The tab row of a question dialog above its first option at row `at`: the nearest row made
/// of tab cells and nothing else, with an arrow, or two cells, or one isolated cell (a single
/// question's header: indented at most one column, blank above and below). A todo list is
/// neither: its rows are one cell each and touch. The scan stops at a rule, a tool's title or
/// a box edge, and at the first row that starts like a tab row and is none.
/// ponytail: a tab row a narrow terminal wrapped onto two rows reads as no tab row.
fn tab_row(rows: &[String], at: usize) -> Option<(usize, Vec<Tab>)> {
    for i in (at.saturating_sub(TABS_ABOVE)..at).rev() {
        let r = &rows[i];
        if is_rule(r.trim()) || edge(r) || tool(r.trim()) {
            return None;
        }
        if !r.trim_start().starts_with(['←', '☐', '☒', '☑', '✔', '✓']) {
            continue;
        }
        let cells = cells(r);
        let mut rest = r.clone();
        for (whole, ..) in &cells {
            if let Some(p) = rest.find(whole.as_str()) {
                rest.replace_range(p..p + whole.len(), "");
            }
        }
        let rest_empty = rest
            .chars()
            .all(|c| c == '←' || c == '→' || c.is_whitespace());
        let arrows = r.contains(['←', '→']);
        let header = r
            .strip_prefix(' ')
            .unwrap_or(r)
            .starts_with(|c: char| !c.is_whitespace());
        let lone = cells.len() == 1
            && !arrows
            && header
            && (i == 0 || rows[i - 1].trim().is_empty() || is_rule(rows[i - 1].trim()))
            && rows.get(i + 1).is_some_and(|n| n.trim().is_empty());
        if cells.is_empty() || !rest_empty || !(arrows || cells.len() > 1 || lone) {
            return None;
        }
        let tabs = cells
            .into_iter()
            .map(|(_, mark, name)| Tab {
                name: name.trim().to_string(),
                done: matches!(mark, '☒' | '☑'),
                submit: matches!(mark, '✔' | '✓'),
            })
            .collect();
        return Some((i, tabs));
    }
    None
}

/// The texts of a row's filled runs (a background or inverse video), cut at the spans' edges
/// in the daemon's cells: a wide character is two, and the blank after it is its spacer.
fn filled_runs(line: &str, spans: &[Span]) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut cells: Vec<(String, usize)> = Vec::new();
    let mut total = 0;
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        let width = ch.width().unwrap_or(1);
        i += 1;
        if width == 0
            && let Some(last) = cells.last_mut()
        {
            last.0.push(ch);
            continue;
        }
        cells.push((ch.to_string(), total));
        total += width.max(1);
        if width == 2 && chars.get(i) == Some(&' ') {
            i += 1;
        }
    }
    let mut edges = vec![0, total];
    for s in spans {
        edges.push(s.at);
        edges.push((s.at + s.len).min(total));
    }
    edges.sort_unstable();
    edges.dedup();
    edges
        .windows(2)
        .filter_map(|w| {
            let (from, to) = (w[0], w[1]);
            let span = spans.iter().find(|s| from >= s.at && from < s.at + s.len)?;
            if span.style.bg == Colour::Default && !span.style.inverse {
                return None;
            }
            let text: String = cells
                .iter()
                .filter(|(_, at)| *at >= from && *at < to)
                .map(|(t, _)| t.as_str())
                .collect();
            (!text.is_empty()).then_some(text)
        })
        .collect()
}

/// The tab the dialog is on. Claude Code fills it, which is in the row's spans and not in its
/// text; a fill on one tab only says it. Otherwise the first tab not answered yet, else the
/// last (Submit).
fn current_tab(tabs: &[Tab], raw: &str, spans: Option<&[Span]>) -> usize {
    if let Some(spans) = spans.filter(|s| !s.is_empty()) {
        let hot = filled_runs(raw, spans);
        let on: Vec<usize> = (0..tabs.len())
            .filter(|&i| hot.iter().any(|run| run.contains(&tabs[i].name)))
            .collect();
        if let [one] = on[..] {
            return one;
        }
    }
    tabs.iter()
        .position(|t| !t.done && !t.submit)
        .unwrap_or(tabs.len().saturating_sub(1))
}

/// The question of a tabbed dialog: the rows between the tab row and the first option. Wrapped
/// rows join with a space, a blank row is a paragraph break, a list row keeps its own line.
fn asked_between(rows: &[String], from: usize, to: usize) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut gap = false;
    for r in &rows[from..to] {
        let t = r.trim();
        if t.is_empty() {
            gap = !out.is_empty();
        } else if !quiet(r) || is_rule(t) || t.chars().all(|c| c == '╌' || c == '┄') {
            continue;
        } else if out.is_empty() {
            out.push(t.to_string());
        } else if gap {
            out.extend([String::new(), t.to_string()]);
        } else if list_item(t) {
            out.push(t.to_string());
        } else if let Some(last) = out.last_mut() {
            last.push(' ');
            last.push_str(t);
        }
        if !t.is_empty() {
            gap = false;
        }
    }
    out.join("\n")
}

/// A run of options numbered from 1 at row `at`.
struct Run {
    at: usize,
    options: Vec<MenuOption>,
    end: usize,
    submit: Option<usize>,
    on_submit: bool,
}

/// The options numbered 1, 2, 3 and on from row `i`, wrapped rows, a Submit row and a rule
/// between them allowed; none unless there are two.
fn run_at(raw: &[String], rows: &[String], i: usize, last: usize) -> Option<Run> {
    let (cursor, number, text) = option_row(&rows[i])?;
    if number != "1" {
        return None;
    }
    let mut options = vec![option_of("1".to_string(), text.trim(), cursor)];
    let (mut want, mut end, mut submit, mut on_submit) = (2, i, None, false);
    for j in i + 1..=last {
        let row = &rows[j];
        let prev = options.len() - 1;
        if let Some((cursor, number, text)) = option_row(row) {
            if number.parse::<usize>() != Ok(want) {
                break;
            }
            options.push(option_of(want.to_string(), text.trim(), cursor));
            want += 1;
            end = j;
        } else if raw[j].trim().is_empty() {
            break;
        } else if row.trim().is_empty() {
            // A row of the side pane alone.
            end = j;
        } else if let Some(cursor) =
            submit_row(row).filter(|_| submit.is_none() && options[prev].checked.is_some())
        {
            submit = Some(options.len());
            on_submit = cursor;
            // The box over Submit is the field.
            options[prev].input = true;
            end = j;
        } else if rule(row) {
            // Claude Code's field is the answer right over its rule, known by its place, as
            // the words typed in it replace its "Type something".
            if end + 1 == j {
                options[prev].input = true;
            }
        } else if wrapped(row) {
            let t = row.trim();
            let o = &mut options[prev];
            o.detail = Some(match o.detail.take() {
                Some(d) => format!("{d} {t}"),
                None => t.to_string(),
            });
            end = j;
        } else {
            break;
        }
    }
    (options.len() >= 2).then_some(Run {
        at: i,
        options,
        end,
        submit,
        on_submit,
    })
}

/// The agent's menu at the bottom of `screen`, if it is waiting on one. `styles` are the rows'
/// spans, which say which tab of a dialog is the current one.
pub fn menu_of(screen: &[String], styles: Option<&[Vec<Span>]>) -> Option<Menu> {
    let raw: Vec<String> = screen.iter().map(|r| unbox(r)).collect();
    // Only the bottom of the screen: a numbered list in the middle of an answer is prose.
    let last = raw.iter().rposition(|r| !r.trim().is_empty())?;
    // The preview layout, known by its own key hint on the last row: a side pane right of the
    // options, which is no part of them.
    // ponytail: a hint a narrow terminal wraps onto two rows reads as no preview.
    let confirm = notes_hint(&raw[last]);
    let rows: Vec<String> = if confirm {
        raw.iter().map(|r| cut_pane(r)).collect()
    } else {
        raw.clone()
    };
    let found = (last.saturating_sub(BOTTOM)..=last)
        .rev()
        .find_map(|i| run_at(&raw, &rows, i, last))?;
    // Near the bottom: at most a few lines of hints under it.
    let under = rows[found.end + 1..=last]
        .iter()
        .filter(|r| !r.trim().is_empty())
        .count();
    if under > BELOW {
        return None;
    }
    // Exactly one row under the cursor (a Submit row is one): a numbered list in an answer has
    // none, a quoted list a marker on every row. A list of ballot boxes is another agent's,
    // whose keys are not known.
    let selected = found.options.iter().filter(|o| o.selected).count();
    if selected + usize::from(found.on_submit) != 1
        || found
            .options
            .iter()
            .any(|o| o.label.starts_with(['☐', '☑']))
    {
        return None;
    }
    let context = rows[found.at.saturating_sub(8)..found.at]
        .iter()
        .map(|r| r.trim())
        .filter(|r| !r.is_empty() && !is_rule(r))
        .collect::<Vec<_>>()
        .join("\n");
    let mut menu = Menu {
        question: String::new(),
        options: found.options,
        at: found.at,
        context,
        prompt: prompt_rows(&rows, found.at),
        tabs: Vec::new(),
        current: None,
        top: found.at,
        submit: found.submit,
        confirm,
    };
    // A tabbed dialog: its question is whatever stands between the tab row and the options.
    if let Some((row, tabs)) = tab_row(&rows, found.at) {
        let spans = styles.and_then(|s| s.get(row)).map(Vec::as_slice);
        menu.current = Some(current_tab(&tabs, &screen[row], spans));
        menu.question = asked_between(&rows, row + 1, found.at);
        menu.tabs = tabs;
        menu.top = row;
        return Some(menu);
    }
    // Any other menu: the nearest row above, when it ends in a question mark.
    for k in (found.at.saturating_sub(ABOVE)..found.at).rev() {
        let t = rows[k].trim();
        if t.is_empty() {
            continue;
        }
        if t.ends_with('?') {
            menu.question = t.to_string();
            menu.top = k;
        }
        break;
    }
    Some(menu)
}

/// What to type for `key` on `menu` as it is on the screen now. A number picks its option;
/// with the cursor in Claude Code's field, Up goes first, or the number would be typed into
/// it, and on Submit or "Chat about this", where a number does nothing, Up to the option row
/// over the field; in the preview layout Enter follows. An option numbered 10 or more has no
/// key: its first digit would answer alone. A box's number only ticks it, and the
/// field's number only puts the cursor in it: no answer yet. `SUBMIT` moves the cursor to the
/// Submit row and presses Enter. `None` for a key the menu has not. `app_cursor`: the program
/// asked for application cursor keys.
pub fn menu_keys(menu: &Menu, key: &str, app_cursor: bool) -> Option<MenuKeys> {
    let arrow = |up: bool, n: usize| {
        let mode = if app_cursor { 'O' } else { '[' };
        format!("\x1b{mode}{}", if up { 'A' } else { 'B' }).repeat(n)
    };
    let number = |i: usize| menu.options[i].key.parse::<usize>().ok();
    let cursor = menu.options.iter().position(|o| o.selected);
    if key == SUBMIT {
        let submit = menu.submit?;
        // Each option is one stop, its description is not. No option under the cursor: it is
        // on Submit.
        let moves = match cursor.and_then(number) {
            None => String::new(),
            Some(k) if k <= submit => arrow(false, submit - k + 1),
            Some(k) => arrow(true, k - submit),
        };
        return Some(MenuKeys {
            keys: format!("{moves}\r"),
            answers: true,
        });
    }
    // One key only: "10" typed would pick 1 first.
    let option = menu
        .options
        .iter()
        .find(|o| o.key == key && o.key.len() == 1)?;
    // The cursor's stops top down: each option, and Submit (`None`) under option `submit`.
    let stops: Vec<Option<usize>> = (0..menu.options.len())
        .flat_map(|i| {
            let submit_under = menu.submit.is_some() && number(i) == menu.submit;
            std::iter::once(Some(i)).chain(submit_under.then_some(None))
        })
        .collect();
    let at = stops.iter().position(|s| *s == cursor);
    let field = stops
        .iter()
        .position(|s| s.is_some_and(|i| menu.options[i].input));
    let up = match (at, field) {
        (Some(at), Some(field)) if at >= field => at - field + 1,
        _ => 0,
    };
    let enter = if menu.confirm { "\r" } else { "" };
    Some(MenuKeys {
        keys: format!("{}{key}{enter}", arrow(true, up)),
        answers: option.checked.is_none() && !option.input,
    })
}

/// What makes two readings the same menu: the command it asks about, its question, its tabs
/// and every key and label.
#[allow(clippy::type_complexity)]
fn identity(m: &Menu) -> (&str, &str, Vec<(&str, bool)>, Vec<(&str, &str)>) {
    (
        &m.context,
        &m.question,
        m.tabs.iter().map(|t| (t.name.as_str(), t.done)).collect(),
        m.options
            .iter()
            .map(|o| (o.key.as_str(), o.label.as_str()))
            .collect(),
    )
}

/// The same menu, option for option and over the same command.
pub fn same_menu(a: &Menu, b: &Menu) -> bool {
    identity(a) == identity(b)
}

/// What an answer approves, as a number a client carries back, below 2^53: the whole prompt,
/// its question and tabs, and every option with the rows under it (the scope of a "don't ask
/// again"). Process-local: `DefaultHasher` is not stable across Rust releases, so it is only
/// compared within one daemon (a new binary only refuses an old tap). Never recompute it
/// anywhere else, in a client or for an older daemon.
pub fn seal(menu: &Menu) -> u64 {
    let mut h = DefaultHasher::new();
    (&menu.prompt, &menu.question).hash(&mut h);
    for t in &menu.tabs {
        (&t.name, t.done).hash(&mut h);
    }
    for o in &menu.options {
        (&o.key, &o.label, &o.detail).hash(&mut h);
    }
    h.finish() & ((1 << 53) - 1)
}

impl Menu {
    /// The menu as a person's client shows it.
    pub fn view(&self) -> AgentMenu {
        AgentMenu {
            question: self.question.clone(),
            options: self
                .options
                .iter()
                .map(|o| MenuChoice {
                    key: o.key.clone(),
                    label: o.label.clone(),
                    detail: o.detail.clone(),
                    checked: o.checked,
                    input: o.input,
                })
                .collect(),
            multi: self.options.iter().any(|o| o.checked.is_some()),
            seal: seal(self),
        }
    }
}

/// The keys answering `key` on the screen now, only if its menu is the one `sealed`.
pub(crate) fn answer_on(
    screen: &Snapshot,
    sealed: u64,
    key: &str,
) -> Result<MenuKeys, &'static str> {
    let menu = menu_of(&screen.lines, Some(screen.styles.as_slice()))
        .filter(|m| seal(m) == sealed)
        .ok_or(ALREADY_ANSWERED)?;
    menu_keys(&menu, key, screen.modes.application_cursor).ok_or(NO_SUCH_ANSWER)
}

/// Answer an `AnswerMenu` from a person (`who`: the connection and its kind): only while that
/// agent, in that generation, is blocked. The terminal's own task then reads the menu and the
/// keys off its live screen and types them as that person's input.
pub(crate) async fn answer(
    id: String,
    (terminal, generation): (u32, Option<u64>),
    seal: u64,
    key: String,
    (from, kind): (qoop_domain::ClientId, &'static str),
    terminals: &crate::daemon::TerminalRegistry,
    agents: &crate::agents::AgentRegistry,
) -> ControlResponse {
    let refuse = |id: String, reason: &str| ControlResponse::Failed {
        id,
        reason: reason.to_string(),
    };
    let blocked = agents.agent(terminal).is_some_and(|a| {
        a.belief.state == qoop_domain::AgentRuntimeState::Blocked
            && a.identity.generation().map(|g| g.0) == generation
    });
    let Some(handle) = terminals.get(terminal).filter(|_| blocked) else {
        return refuse(id, ALREADY_ANSWERED);
    };
    let (reply, typed) = tokio::sync::oneshot::channel();
    let asked = crate::daemon::ClientCommand::AnswerMenu {
        from,
        kind,
        seal,
        key,
        reply,
    };
    if handle.commands.send(asked).await.is_err() {
        return refuse(id, ALREADY_ANSWERED);
    }
    match typed.await {
        Ok(Ok(answers)) => ControlResponse::MenuAnswered { id, answers },
        Ok(Err(reason)) => refuse(id, reason),
        Err(_) => refuse(id, ALREADY_ANSWERED),
    }
}

#[cfg(test)]
mod tests {
    use qoop_terminal::Style;

    use super::*;

    fn rows(screen: &[&str]) -> Vec<String> {
        screen.iter().map(|r| r.to_string()).collect()
    }

    fn read(screen: &[&str]) -> Option<Menu> {
        menu_of(&rows(screen), None)
    }

    fn menu(screen: &[&str]) -> Menu {
        read(screen).expect("a menu")
    }

    fn labels(m: &Menu) -> Vec<&str> {
        m.options.iter().map(|o| o.label.as_str()).collect()
    }

    fn keys(m: &Menu, key: &str, app_cursor: bool) -> Option<(String, bool)> {
        menu_keys(m, key, app_cursor).map(|k| (k.keys, k.answers))
    }

    fn sent(keys: &str, answers: bool) -> Option<(String, bool)> {
        Some((keys.to_string(), answers))
    }

    const PERMISSION: [&str; 11] = [
        "  Bash command",
        "",
        "    cargo test -p qoop-tui",
        "    Run the TUI tests",
        "",
        "  Do you want to proceed?",
        "  > 1. Yes",
        "    2. Yes, and don't ask again for cargo",
        "       test commands in /Users/me/coding/qoop",
        "    3. No, and tell the agent what to do",
        "       differently (esc)",
    ];

    #[test]
    fn a_non_claude_agents_menu_carries_each_options_words() {
        let m = menu(&[
            "Apply this patch?",
            "> 1. Apply",
            "  2. Skip",
            "  3. Abort the run",
        ]);
        let pairs: Vec<(&str, &str)> = m
            .options
            .iter()
            .map(|o| (o.key.as_str(), o.label.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [("1", "Apply"), ("2", "Skip"), ("3", "Abort the run")]
        );
    }

    #[test]
    fn reads_a_permission_prompt_with_wrapped_options() {
        let m = menu(&PERMISSION);
        assert_eq!(m.question, "Do you want to proceed?");
        let keys: Vec<&str> = m.options.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(keys, ["1", "2", "3"]);
        assert_eq!(
            m.options[0],
            MenuOption {
                key: "1".into(),
                label: "Yes".into(),
                selected: true,
                detail: None,
                checked: None,
                input: false,
            }
        );
        assert_eq!(m.options[1].label, "Yes, and don't ask again for cargo");
        assert!(!m.options[2].selected);
    }

    #[test]
    fn keeps_what_claude_code_writes_under_each_answer_and_the_answer_under_its_rule() {
        let m = menu(&[
            "← ☐ AWS credit  ☐ Storage  ✔ Submit  →",
            "",
            "Is there AWS credit for the move? Without it the move costs about $450 a month more.",
            "",
            "❯ 1. Credit there",
            "     Start the move now, the cost comes off the credit.",
            "  2. No credit, start anyway",
            "     The move happens now despite the extra cost.",
            "  3. No credit, wait",
            "     Free work first, the move once traffic comes.",
            "  4. Type something.",
            "────────────────────────────────────────────────",
            "  5. Chat about this",
            "",
            "Enter to select · Tab/Arrow keys to navigate · Esc to cancel",
        ]);
        assert_eq!(
            labels(&m),
            [
                "Credit there",
                "No credit, start anyway",
                "No credit, wait",
                "Type something.",
                "Chat about this"
            ]
        );
        let details: Vec<Option<&str>> = m.options.iter().map(|o| o.detail.as_deref()).collect();
        assert_eq!(
            details,
            [
                Some("Start the move now, the cost comes off the credit."),
                Some("The move happens now despite the extra cost."),
                Some("Free work first, the move once traffic comes."),
                None,
                None,
            ]
        );
        assert!(m.options[0].selected);
    }

    #[test]
    fn a_wrapped_answer_keeps_its_rest_as_its_detail() {
        assert_eq!(
            menu(&PERMISSION).options[1].detail.as_deref(),
            Some("test commands in /Users/me/coding/qoop")
        );
    }

    #[test]
    fn blank_rows_below_and_a_hint_line_do_not_hide_it() {
        let mut screen = PERMISSION.to_vec();
        screen.extend(["", "  Esc to cancel", "", ""]);
        assert_eq!(menu(&screen).options.len(), 3);
    }

    #[test]
    fn a_menu_drawn_in_a_box_reads_the_same() {
        let boxed = [
            "╭──────────────────────────────────────────╮",
            "│ Do you want to proceed?                  │",
            "│ ❯ 1. Yes                                 │",
            "│   2. No, and tell the agent what to do   │",
            "╰──────────────────────────────────────────╯",
        ];
        assert_eq!(
            labels(&menu(&boxed)),
            ["Yes", "No, and tell the agent what to do"]
        );
    }

    #[test]
    fn a_menu_with_the_heavy_arrow_cursor_and_no_question_is_still_a_menu() {
        let m = menu(&["❯ 1. Resume", "  2. Start fresh"]);
        assert_eq!(labels(&m), ["Resume", "Start fresh"]);
        assert_eq!(m.question, "");
    }

    #[test]
    fn a_numbered_list_inside_an_answer_is_prose_not_a_menu() {
        assert_eq!(
            read(&[
                "Here is the plan:",
                "1. Add the parser",
                "2. Add the tests",
                "3. Ship it",
                "",
                "Tell me which first.",
                "More text.",
                "More text.",
                "More text.",
            ]),
            None
        );
    }

    #[test]
    fn a_question_followed_by_a_numbered_list_with_no_cursor_is_prose() {
        assert_eq!(
            read(&[
                "I can do this two ways. Which do you prefer?",
                "1. Rewrite the parser",
                "2. Patch the caller",
                "",
                "> ",
            ]),
            None
        );
    }

    #[test]
    fn the_input_box_and_its_shortcuts_line_under_an_answer_are_not_a_menu() {
        assert_eq!(
            read(&[
                "Which one?",
                "1. Rewrite the parser",
                "2. Patch the caller",
                "",
                "╭────────╮",
                "│ >      │",
                "╰────────╯",
                "  ? for shortcuts",
            ]),
            None
        );
    }

    #[test]
    fn a_list_that_does_not_start_at_1_is_not_a_menu() {
        assert_eq!(
            read(&["  Do you want to go on?", "  > 2. Yes", "    3. No"]),
            None
        );
    }

    #[test]
    fn the_real_prose_screen_of_a_waiting_agent_is_not_a_menu() {
        assert_eq!(
            read(&[
                "  4. Space collapse -> 4.2. A tree.",
                "  5. CPU and RAM -> 5.1.",
                "  6. mdfried -> not now.",
                "",
                "  Send your picks (example: 1.2, 2.1)",
                "  and I write them into the plan file now.",
                "",
                "> ",
            ]),
            None
        );
    }

    #[test]
    fn an_empty_or_blank_screen_has_no_menu() {
        assert_eq!(read(&[]), None);
        assert_eq!(read(&["", "  "]), None);
    }

    #[test]
    fn only_the_bottom_most_menu_counts() {
        let m = menu(&[
            "Old question?",
            "> 1. Old yes",
            "  2. Old no",
            "",
            "work",
            "Do you want to proceed?",
            "> 1. Yes",
            "  2. No",
        ]);
        assert_eq!(labels(&m), ["Yes", "No"]);
    }

    #[test]
    fn the_same_labels_over_another_command_are_another_menu() {
        let a = menu(&[
            "  Bash command",
            "    ls /tmp",
            "  Do you want to proceed?",
            "  > 1. Yes",
            "    2. No",
        ]);
        let b = menu(&[
            "  Bash command",
            "    rm -rf ~/work",
            "  Do you want to proceed?",
            "  > 1. Yes",
            "    2. No",
        ]);
        assert!(!same_menu(&a, &b));
        assert_ne!(a.view().seal, b.view().seal);
    }

    #[test]
    fn a_quoted_list_is_no_menu_and_a_multiple_choice_list_is_one() {
        assert_eq!(read(&["> 1. Read the file", "> 2. Edited it"]), None);
        let m = menu(&["Pick some?", "❯ 1. [ ] Alpha", "  2. [ ] Beta"]);
        let read: Vec<(&str, Option<bool>)> = m
            .options
            .iter()
            .map(|o| (o.label.as_str(), o.checked))
            .collect();
        assert_eq!(read, [("Alpha", Some(false)), ("Beta", Some(false))]);
        assert!(m.view().multi);
    }

    #[test]
    fn two_readings_are_the_same_menu_only_when_every_key_and_label_match() {
        let a = menu(&["Do you want to proceed?", "> 1. Yes", "  2. No"]);
        let b = menu(&["Do you want to proceed?", "> 1. Yes", "  2. No"]);
        let c = menu(&[
            "Do you want to proceed?",
            "> 1. Yes",
            "  2. No, tell it what to do",
        ]);
        assert!(same_menu(&a, &b));
        assert_eq!(seal(&a), seal(&b));
        assert!(!same_menu(&a, &c));
    }

    #[test]
    fn a_menu_under_a_long_command_is_read() {
        let command: Vec<String> = (0..14)
            .map(|i| format!("    psql -c \"delete from rows where id = {i}\""))
            .collect();
        let mut long = vec!["  Bash command".to_string(), String::new()];
        long.extend(command.iter().cloned());
        long.push(String::new());
        long.extend(PERMISSION[5..].iter().map(|r| r.to_string()));
        let m = menu_of(&long, None).expect("a menu");
        assert_eq!(m.options.len(), 3);
        assert_eq!(m.question, "Do you want to proceed?");
    }

    #[test]
    fn the_plain_yes_and_no_are_options_and_boxes_named_so_are_ticks() {
        let m = menu(&[
            "Do you want to proceed?",
            "> 1. Yes",
            "  2. Yes, and don't ask again for cargo",
            "  3. No, and tell Claude what to do differently (esc)",
        ]);
        assert_eq!(keys(&m, "1", false), sent("1", true));
        let boxes = menu(&[
            "Which apply?",
            "❯ 1. [ ] Yes",
            "  2. [ ] No",
            "  3. [ ] Type something",
            "     Submit",
        ]);
        let checked: Vec<Option<bool>> = boxes.options.iter().map(|o| o.checked).collect();
        assert_eq!(checked, [Some(false); 3]);
        assert_eq!(keys(&boxes, "1", false), sent("1", false));
    }

    #[test]
    fn a_permission_prompts_keys_are_its_own() {
        let m = menu(&PERMISSION);
        for key in ["1", "2", "3"] {
            assert_eq!(keys(&m, key, false), sent(key, true));
        }
        assert_eq!(keys(&m, "4", false), None);
    }

    #[test]
    fn a_list_scrolled_past_its_first_rows_is_no_menu() {
        assert_eq!(
            read(&[
                "Pick a model",
                "  ↑ 9 more",
                "  10. model number 10",
                "› 11. model number 11",
                "  12. model number 12",
                "Press enter to confirm",
                "",
                "> ",
            ]),
            None
        );
        assert_eq!(
            read(&[
                "Pick a model",
                "  ↑ 3 more",
                "  4. model number 4",
                "❯ 5. model number 5, a long name that",
                "     wraps onto a second row",
                "  6. model number 6",
                "  7. model number 7",
                "  ↓ 5 more",
            ]),
            None
        );
        // Another agent's multiple choice list is read, its boxes ticks.
        let m = menu(&[
            "Which ones?",
            "❯ 1. [ ] Parser",
            "  2. [x] Tests",
            "",
            "Enter to submit",
        ]);
        let checked: Vec<Option<bool>> = m.options.iter().map(|o| o.checked).collect();
        assert_eq!(checked, [Some(false), Some(true)]);
        for screen in [
            &["Allow running cargo build? [y/n]", "> "][..],
            &["> 4. quoted", "> 5. quoted", "> 6. quoted"][..],
            &["  1. a", "› 5. b"][..],
            &["  4. a", "› 5. b", "one", "two", "three", "four", "five"][..],
        ] {
            assert_eq!(read(screen), None, "{screen:?}");
        }
    }

    // The Claude Code question dialog from the owner's phone (2026-11-02): a tab row, one long
    // question that ends in a full stop, five answers with their descriptions, a rule over the
    // last one.
    const TAB_ROW: &str = "← ☒ Branch commit  ☐ AWS spend  ☐ 7-day rule  ✔ Submit  →";
    const QUESTION: &str = "When should AWS spending start? The Phase 1 foundation (network, ALB, Valkey, empty services, everything off) costs about $49/month from credits; production is untouched.";
    const ANSWERS: [&str; 11] = [
        "❯ 1. Right after Phase 0 (Recommended)",
        "     Infrastructure goes up once the code is ready, then the rehearsal. Credits are spent only for what is needed.",
        "  2. Start now, in parallel with Phase 0",
        "     About 1-2 days faster, but the foundation sits idle until the code is ready.",
        "  3. I'll say later",
        "     Only Phase 0 runs; I come back to ask before touching AWS.",
        "  4. Type something.",
        "────────────────────────────────────────────────",
        "  5. Chat about this",
        "",
        "Enter to select · Tab/Arrow keys to navigate · Esc to cancel",
    ];
    const RULE: &str = "──────────────────────────────────────────────────────────────";

    fn aws() -> Vec<String> {
        let mut screen = rows(&[
            "Başlamadan önce 3 karar lazım:",
            RULE,
            TAB_ROW,
            "",
            QUESTION,
            "",
        ]);
        screen.extend(rows(&ANSWERS));
        screen
    }

    /// Rows with the dialog's answers under them.
    fn over_answers(above: &[&str]) -> Vec<String> {
        let mut screen = rows(above);
        screen.extend(rows(&ANSWERS));
        screen
    }

    /// A span that fills `text` on `row`, in the daemon's cells.
    fn fill(row: &str, text: &str) -> Span {
        let width = |s: &str| s.chars().map(|c| c.width().unwrap_or(1)).sum::<usize>();
        Span {
            at: width(&row[..row.find(text).expect("the text is on the row")]),
            len: width(text),
            style: Style {
                bg: Colour::Rgb(90, 90, 200),
                ..Style::default()
            },
        }
    }

    /// Spans for the rows of `screen`: `on` for the row `pick` chooses, none for the rest.
    fn styled(screen: &[String], pick: impl Fn(&str) -> bool, on: Vec<Span>) -> Vec<Vec<Span>> {
        screen
            .iter()
            .map(|r| {
                if pick(r.as_str()) {
                    on.clone()
                } else {
                    Vec::new()
                }
            })
            .collect()
    }

    #[test]
    fn the_real_dialog_its_whole_question_and_its_tabs() {
        let m = menu_of(&aws(), None).expect("a menu");
        assert_eq!(m.question, QUESTION);
        let tab = |name: &str, done, submit| Tab {
            name: name.into(),
            done,
            submit,
        };
        assert_eq!(
            m.tabs,
            [
                tab("Branch commit", true, false),
                tab("AWS spend", false, false),
                tab("7-day rule", false, false),
                tab("Submit", false, true),
            ]
        );
        // No highlight known: the first tab not answered yet.
        assert_eq!(m.current, Some(1));
        assert_eq!(m.options.len(), 5);
        // The rows above the tab row are the context, the tab row and the question are not.
        assert_eq!(m.top, 2);
    }

    #[test]
    fn a_question_wrapped_over_several_rows_reads_as_one() {
        let words: Vec<&str> = QUESTION.split(' ').collect();
        let (a, b, c) = (
            words[..6].join(" "),
            words[6..14].join(" "),
            words[14..].join(" "),
        );
        let screen = over_answers(&["", TAB_ROW, "", a.as_str(), b.as_str(), c.as_str(), ""]);
        assert_eq!(menu_of(&screen, None).expect("a menu").question, QUESTION);
    }

    #[test]
    fn the_highlighted_tab_is_the_current_one_even_when_an_earlier_one_is_unanswered() {
        let screen = over_answers(&["", TAB_ROW, "", QUESTION, ""]);
        let spans = styled(
            &screen,
            |r| r == TAB_ROW,
            vec![fill(TAB_ROW, "☐ 7-day rule")],
        );
        assert_eq!(
            menu_of(&screen, Some(&spans[..])).expect("a menu").current,
            Some(2)
        );
        // The same row in a box: the spans index the raw row, the tab is found by its text.
        let boxed: Vec<String> = screen.iter().map(|r| format!("│ {r}")).collect();
        let boxed_row = boxed
            .iter()
            .find(|r| r.contains("Submit  →"))
            .expect("the tab row")
            .clone();
        let spans = styled(
            &boxed,
            |r| r.contains("Submit  →"),
            vec![fill(&boxed_row, "☐ 7-day rule")],
        );
        assert_eq!(
            menu_of(&boxed, Some(&spans[..])).expect("a menu").current,
            Some(2)
        );
        // A wide name before it moves the columns, not the answer.
        let cjk = "← ☒ 分支提交  ☐ AWS spend  ☐ 7-day rule  ✔ Submit  →";
        let screen = over_answers(&["", cjk, "", QUESTION, ""]);
        let spans = [Vec::new(), vec![fill(cjk, "☐ 7-day rule")]];
        assert_eq!(
            menu_of(&screen, Some(&spans[..])).expect("a menu").current,
            Some(2)
        );
        // A fill on every tab says nothing: the first unanswered one.
        let screen = over_answers(&["", TAB_ROW, "", QUESTION, ""]);
        let all = Span {
            at: 0,
            ..fill(TAB_ROW, TAB_ROW.trim())
        };
        let spans = styled(&screen, |r| r == TAB_ROW, vec![all]);
        assert_eq!(
            menu_of(&screen, Some(&spans[..])).expect("a menu").current,
            Some(1)
        );
    }

    #[test]
    fn the_review_tab_every_question_answered_the_current_tab_is_submit() {
        let m = menu(&[
            "← ☒ A  ☒ B  ✔ Submit  →",
            "",
            "Ready to submit your answers?",
            "",
            "❯ 1. Submit answers",
            "  2. Cancel",
        ]);
        assert_eq!(m.question, "Ready to submit your answers?");
        assert_eq!(m.current, Some(2));
        assert!(m.tabs[2].submit);
    }

    #[test]
    fn one_question_with_its_header_row_has_one_tab_and_no_arrows() {
        let m = menu(&[
            "",
            " ☐ Auth method",
            "",
            "Which login do you want?",
            "",
            "❯ 1. Password",
            "  2. Passkey",
        ]);
        assert_eq!(
            m.tabs,
            [Tab {
                name: "Auth method".into(),
                done: false,
                submit: false
            }]
        );
        assert_eq!(m.question, "Which login do you want?");
    }

    #[test]
    fn two_paragraphs_and_a_list_in_a_question_keep_their_breaks() {
        let m = menu(&[
            "← ☐ Plan  ✔ Submit  →",
            "",
            "The plan has two parts.",
            "",
            "- build the parser",
            "- ship the panel",
            "Which first?",
            "",
            "❯ 1. Parser",
            "  2. Panel",
        ]);
        assert_eq!(
            m.question,
            "The plan has two parts.\n\n- build the parser\n- ship the panel Which first?"
        );
    }

    const PROCEED: [&str; 3] = ["Proceed?", "❯ 1. Yes", "  2. No"];

    fn with_proceed(above: &[&str]) -> Menu {
        let mut screen = above.to_vec();
        screen.extend(PROCEED);
        menu(&screen)
    }

    #[test]
    fn a_todo_list_above_a_prompt_is_no_tab_row() {
        assert!(
            with_proceed(&["● Updated todos", "  ☒ Parse", "  ☐ Write tests", ""])
                .tabs
                .is_empty()
        );
        let m = menu(&[
            "  ☒ Parse",
            "  ☐ Write tests",
            "",
            "Bash command",
            "  cargo test",
            "Do you want to proceed?",
            "> 1. Yes",
            "  2. No",
        ]);
        assert!(m.tabs.is_empty());
        // A single item under a blank row is indented more than a header chip is.
        assert!(with_proceed(&["", "  ☐ Write tests", ""]).tabs.is_empty());
        // A back row is no tab row.
        assert!(with_proceed(&["← Back", ""]).tabs.is_empty());
    }

    #[test]
    fn with_no_tab_row_the_old_rule_holds_one_row_that_ends_in_a_question_mark() {
        let m = menu(&["Run this command", "  rm -rf x", "> 1. Yes", "  2. No"]);
        assert_eq!(m.question, "");
        assert!(m.tabs.is_empty());
        assert_eq!(m.current, None);
        assert_eq!(m.top, m.at);
        assert_eq!(menu(&PERMISSION).question, "Do you want to proceed?");
        assert_eq!(menu(&PERMISSION).top, 5);
    }

    #[test]
    fn the_dialog_keeps_a_long_question_once_and_the_rows_above_it_as_context() {
        let m = menu_of(&aws(), None).expect("a menu");
        assert_eq!(m.question, QUESTION);
        assert_eq!(
            m.context.lines().next(),
            Some("Başlamadan önce 3 karar lazım:")
        );
    }

    #[test]
    fn another_tab_or_an_answered_one_is_another_menu() {
        let a = menu_of(&aws(), None).expect("a menu");
        let answered: Vec<String> = aws()
            .into_iter()
            .map(|r| {
                if r == TAB_ROW {
                    TAB_ROW.replace("☐ AWS spend", "☒ AWS spend")
                } else {
                    r
                }
            })
            .collect();
        let b = menu_of(&answered, None).expect("a menu");
        let asked: Vec<String> = aws()
            .into_iter()
            .map(|r| {
                if r == QUESTION {
                    "Another question?".to_string()
                } else {
                    r
                }
            })
            .collect();
        let c = menu_of(&asked, None).expect("a menu");
        assert!(same_menu(&a, &a));
        assert!(!same_menu(&a, &b));
        assert!(!same_menu(&a, &c));
    }

    #[test]
    fn a_multiple_choice_list_with_a_ticked_first_option_reads_its_tick() {
        let m = menu(&[
            "← ☐ Pick  ✔ Submit  →",
            "",
            "Which ones?",
            "",
            "❯ 1. [✓] Parser",
            "  2. [ ] Tests",
        ]);
        let checked: Vec<Option<bool>> = m.options.iter().map(|o| o.checked).collect();
        assert_eq!(checked, [Some(true), Some(false)]);
    }

    // Answer controls, on the real Claude Code 2.1.319 screens, from the dialog's rule down.
    fn dialog_rule() -> String {
        "─".repeat(110)
    }

    fn extras(tick: impl Fn(usize) -> &'static str, tabs: &str) -> Vec<String> {
        let rule = dialog_rule();
        let mut screen = vec![
            "  Border, Icon. Do nothing else before or after.".to_string(),
            rule.clone(),
            tabs.to_string(),
            String::new(),
            "Which extras do you want?".to_string(),
            String::new(),
            format!("❯ 1. [{}] Shadow", tick(1)),
            "         Adds a drop shadow under the button.".to_string(),
            format!("  2. [{}] Border", tick(2)),
            "         Adds an outline border around the button.".to_string(),
            format!("  3. [{}] Icon", tick(3)),
            "         Adds an icon next to the button label.".to_string(),
            format!("  4. [{}] Type something", tick(4)),
            "     Submit".to_string(),
            rule,
        ];
        screen.extend(rows(&[
            "  5. Chat about this",
            "",
            "Enter to select · Tab/Arrow keys to navigate · Esc to cancel",
            "",
            "",
        ]));
        screen
    }

    const EXTRAS_TABS: &str = "←  ☐ Color  ☐ Extras  ✔ Submit  →";

    fn multi() -> Vec<String> {
        extras(|_| " ", EXTRAS_TABS)
    }

    fn ticked() -> Vec<String> {
        extras(
            |n| if n == 1 || n == 3 { "✔" } else { " " },
            "←  ☒ Color  ☒ Extras  ✔ Submit  →",
        )
    }

    /// The cursor moved to another row of the same screen: on that row and nowhere else.
    fn cursor_on(screen: &[String], row: &str) -> Vec<String> {
        screen
            .iter()
            .map(|r| match r.strip_prefix("❯ ") {
                Some(rest) => format!("  {rest}"),
                None => r.clone(),
            })
            .map(|r| match r.strip_prefix("  ").filter(|_| r.trim() == row) {
                Some(rest) => format!("❯ {rest}"),
                None => r.clone(),
            })
            .collect()
    }

    fn on(screen: &[String], row: &str) -> Menu {
        menu_of(&cursor_on(screen, row), None).expect("a menu")
    }

    fn of(screen: &[String]) -> Menu {
        menu_of(screen, None).expect("a menu")
    }

    const PREVIEW: [&str; 16] = [
        "────────────────────────────────────────────────────────────────────────────────",
        " ☐ Layout ",
        "",
        "Which layout for the report?",
        "",
        "❯ 1. Compact                      ┌────────────────────────────────────────────┐",
        "  2. Wide                         │ ┌──────────────────────────┐               │",
        "  3. Cards                        ├─── ✂ ─── 5 lines hidden ───────────────────┤",
        "                                  └────────────────────────────────────────────┘",
        "",
        "                                  Notes: press n to add notes",
        "",
        "────────────────────────────────────────────────────────────────────────────────",
        "  Chat about this",
        "",
        "Enter to select · ↑/↓ to navigate · n to add notes · Esc to cancel",
    ];

    const SINGLE: [&str; 15] = [
        " ☐ Color ",
        "",
        "Which color should the chart use?",
        "",
        "❯ 1. Red (warm)",
        "     Warm red tone, draws the eye fast and signals urgency.",
        "  2. Green (calm)",
        "     Calm green tone, reads as positive and easy on the eyes.",
        "  3. Blue (cool)",
        "     Cool blue tone, neutral and professional for data.",
        "  4. Type something.",
        "────────────────────────────────────────────────────────────────────────────────",
        "  5. Chat about this",
        "",
        "Enter to select · ↑/↓ to navigate · Esc to cancel",
    ];

    #[test]
    fn a_preview_questions_labels_end_at_its_side_pane_and_a_pick_is_the_number_then_enter() {
        let m = menu(&PREVIEW);
        assert_eq!(labels(&m), ["Compact", "Wide", "Cards"]);
        assert!(m.options.iter().all(|o| o.detail.is_none()));
        assert_eq!(m.question, "Which layout for the report?");
        assert_eq!(keys(&m, "2", false), sent("2\r", true));
        // The plain layout is still the bare number.
        assert_eq!(keys(&menu(&SINGLE), "2", false), sent("2", true));
    }

    #[test]
    fn only_the_dialogs_own_key_hint_marks_the_preview_layout() {
        // A permission prompt whose command holds the words: Enter after the 1 would land in
        // the prompt after it.
        let prompt = menu(&[
            "Bash command",
            "",
            "  grep -rn \"n to add notes\" src/  | head",
            "  Find the hint",
            "",
            "Do you want to proceed?",
            "❯ 1. Yes",
            "  2. Yes, and don't ask again for grep commands",
            "  3. No, and tell Claude what to do differently (esc)",
            "",
            "Esc to cancel",
        ]);
        assert!(!prompt.confirm);
        assert_eq!(keys(&prompt, "1", false), sent("1", true));
        // A tabbed question with the words in the prompt above it and in a label.
        let rule = dialog_rule();
        let short = "─".repeat(80);
        let tabbed = menu(&[
            "> Ask me: n to add notes, or n for notes?",
            "",
            rule.as_str(),
            "←  ☐ Hint  ☐ Place  ✔ Submit  →",
            "",
            "Which hint wording?",
            "",
            "❯ 1. n to add notes",
            "  2. n for notes",
            "  3. Type something.",
            short.as_str(),
            "  4. Chat about this",
            "",
            "Enter to select · Tab/Arrow keys to navigate · Esc to cancel",
        ]);
        assert!(!tabbed.confirm);
        assert_eq!(keys(&tabbed, "1", false), sent("1", true));
    }

    #[test]
    fn the_type_something_row_is_the_answer_field_with_the_cursor_on_it_up_goes_first() {
        let single = rows(&SINGLE);
        let m = of(&single);
        assert_eq!(m.options[3].label, "Type something.");
        assert!(m.options[3].input);
        assert_eq!(m.options.iter().filter(|o| o.input).count(), 1);
        // Its own number only puts Claude Code's cursor in the field: no answer.
        assert_eq!(keys(&m, "4", false), sent("4", false));
        let field = on(&single, "4. Type something.");
        assert_eq!(keys(&field, "2", false), sent("\x1b[A2", true));
        assert_eq!(keys(&field, "2", true), sent("\x1bOA2", true));
        // On Chat about this a number does nothing: Up to the field, Up out of it, the number.
        assert_eq!(
            keys(&on(&single, "5. Chat about this"), "2", false),
            sent("\x1b[A\x1b[A2", true)
        );
    }

    #[test]
    fn a_multiple_choice_question_is_read_its_boxes_its_question_its_submit() {
        let m = of(&multi());
        assert_eq!(m.question, "Which extras do you want?");
        let read: Vec<(&str, Option<bool>)> = m
            .options
            .iter()
            .map(|o| (o.label.as_str(), o.checked))
            .collect();
        assert_eq!(
            read,
            [
                ("Shadow", Some(false)),
                ("Border", Some(false)),
                ("Icon", Some(false)),
                ("Type something", Some(false)),
                ("Chat about this", None),
            ]
        );
        assert!(m.options[3].input);
        assert_eq!(m.submit, Some(4));
    }

    #[test]
    fn ticked_boxes_read_as_checked_and_the_dialogs_tabs_are_never_options() {
        let m = of(&ticked());
        let checked: Vec<Option<bool>> = m.options.iter().map(|o| o.checked).collect();
        assert_eq!(
            checked,
            [Some(true), Some(false), Some(true), Some(false), None]
        );
        assert!(!labels(&m).contains(&"Color"));
        let tabs: Vec<&str> = m.tabs.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(tabs, ["Color", "Extras", "Submit"]);
        // The first tick also flips its tab, so a second tap before the redraw is refused;
        // after it, a tick changes no label and the next tap goes through.
        let one = |tabs: &str| of(&extras(|n| if n == 1 { "✔" } else { " " }, tabs));
        assert!(!same_menu(
            &one("←  ☐ Color  ☒ Extras  ✔ Submit  →"),
            &of(&multi())
        ));
        assert!(same_menu(
            &one("←  ☒ Color  ☒ Extras  ✔ Submit  →"),
            &of(&ticked())
        ));
    }

    #[test]
    fn submit_is_down_to_the_submit_row_and_enter() {
        let multi = multi();
        assert_eq!(
            keys(&of(&multi), SUBMIT, false),
            sent(&format!("{}\r", "\x1b[B".repeat(4)), true)
        );
        assert_eq!(
            keys(&on(&multi, "4. [ ] Type something"), SUBMIT, false),
            sent("\x1b[B\r", true)
        );
        // The cursor on Submit itself: Enter alone; on the answer under it, Up.
        let on_submit = on(&multi, "Submit");
        assert!(!on_submit.options.iter().any(|o| o.selected));
        assert_eq!(keys(&on_submit, SUBMIT, false), sent("\r", true));
        assert_eq!(
            keys(&on(&multi, "5. Chat about this"), SUBMIT, false),
            sent("\x1b[A\r", true)
        );
        // A menu with no Submit row offers none.
        let single = menu(&SINGLE);
        assert_eq!(single.submit, None);
        assert_eq!(keys(&single, SUBMIT, false), None);
    }

    #[test]
    fn a_toggle_is_not_an_answer_its_number_goes_out_and_the_menu_still_waits() {
        let multi = multi();
        assert_eq!(keys(&of(&multi), "1", false), sent("1", false));
        // A ticked box's number unticks it: no answer either.
        assert_eq!(keys(&of(&ticked()), "1", false), sent("1", false));
        // On Submit a number does nothing: two Ups, past the answer field, then the number.
        assert_eq!(
            keys(&on(&multi, "Submit"), "2", false),
            sent("\x1b[A\x1b[A2", false)
        );
        // From the answer field, Up first: else the digit is typed into it.
        assert_eq!(
            keys(&on(&multi, "4. [ ] Type something"), "1", false),
            sent("\x1b[A1", false)
        );
        // On Chat about this a number does nothing either: Up to Submit, then two Ups more.
        assert_eq!(
            keys(&on(&multi, "5. Chat about this"), "2", false),
            sent(&format!("{}2", "\x1b[A".repeat(3)), false)
        );
        // Chat about this is an answer.
        assert_eq!(keys(&of(&multi), "5", false), sent("5", true));
        assert_eq!(keys(&of(&multi), "9", false), None);
    }

    #[test]
    fn the_answer_field_is_known_by_its_place_also_once_it_holds_words() {
        let rule = dialog_rule();
        let m = menu(&[
            rule.as_str(),
            "←  ☐ Color  ☐ Extras  ✔ Submit  →",
            "",
            "Which color should the button be?",
            "",
            "  1. Red (warm)",
            "     A warm, attention-grabbing button color.",
            "  2. Blue (calm)",
            "     A calm, understated button color.",
            "❯ 3. 1",
            rule.as_str(),
            "  4. Chat about this",
            "",
            "Enter to select · Tab/Arrow keys to navigate · ctrl+g to edit in Nvim · Esc to cancel",
        ]);
        let fields: Vec<&str> = m
            .options
            .iter()
            .filter(|o| o.input)
            .map(|o| o.key.as_str())
            .collect();
        assert_eq!(fields, ["3"]);
        assert_eq!(keys(&m, "2", false), sent("\x1b[A2", true));
        assert_eq!(keys(&m, "3", false), sent("\x1b[A3", false));
        // A multiple choice field with words: the box over Submit. From it Up first; from
        // Submit, two Ups.
        let words: Vec<String> = multi()
            .iter()
            .map(|r| r.replace("Type something", "rounded corners"))
            .collect();
        let fields: Vec<String> = of(&words)
            .options
            .into_iter()
            .filter(|o| o.input)
            .map(|o| o.key)
            .collect();
        assert_eq!(fields, ["4"]);
        assert_eq!(
            keys(&on(&words, "4. [ ] rounded corners"), "1", false),
            sent("\x1b[A1", false)
        );
        assert_eq!(
            keys(&on(&words, "Submit"), "2", false),
            sent("\x1b[A\x1b[A2", false)
        );
    }

    #[test]
    fn the_context_leaves_claude_codes_dashed_rules_out() {
        let dashed = "╌".repeat(40);
        let dashes = "-".repeat(40);
        let m = menu(&[
            "Run the migration",
            dashed.as_str(),
            "  bun run migrate",
            dashes.as_str(),
            "Do you want to proceed?",
            "❯ 1. Yes",
            "  2. No",
        ]);
        assert_eq!(
            m.context,
            "Run the migration\nbun run migrate\nDo you want to proceed?"
        );
    }

    /// Two long commands that end alike are one menu to the phone's `sameMenu`, which only reads
    /// the rows just above; what an answer approves is all of the prompt, and the rows under
    /// each option (the scope of a standing permission).
    #[test]
    fn the_seal_binds_the_whole_prompt_and_the_rows_under_each_option() {
        let long = |first: &str| {
            let mut screen = vec!["  Bash command".to_string(), format!("    {first}")];
            screen.extend((1..14).map(|i| format!("    ./deploy.sh --step {i}")));
            screen.extend(rows(&[
                "",
                "  Do you want to proceed?",
                "  > 1. Yes",
                "    2. No",
            ]));
            screen
        };
        let staging = of(&long("cd /srv/staging"));
        let prod = of(&long("cd /srv/prod"));
        assert!(same_menu(&staging, &prod));
        assert_ne!(seal(&staging), seal(&prod));
        assert_eq!(seal(&staging), seal(&of(&long("cd /srv/staging"))));
        let scope = |dir: &str| {
            of(&rows(&[
                "Do you want to proceed?",
                "❯ 1. Yes",
                "  2. Yes, and don't ask again for rm",
                format!("     commands in {dir}").as_str(),
                "  3. No",
            ]))
        };
        assert_ne!(seal(&scope("/tmp")), seal(&scope("/")));
    }

    /// "10" typed would pick 1 first: a key of two digits is none.
    #[test]
    fn an_option_numbered_10_or_more_has_no_key() {
        let mut screen = rows(&["Pick a model?", "❯ 1. model 1"]);
        screen.extend((2..=11).map(|i| format!("  {i}. model {i}")));
        let m = of(&screen);
        assert_eq!(m.options.len(), 11);
        assert_eq!(keys(&m, "10", false), None);
        assert_eq!(keys(&m, "9", false), sent("9", true));
    }

    /// The answer guard's own reading: a seal from another menu types nothing.
    #[test]
    fn an_answer_to_a_menu_that_changed_is_refused() {
        let screen = |command: &str| Snapshot {
            cols: 80,
            rows: 5,
            cursor: (0, 0),
            lines: rows(&[
                "  Bash command",
                command,
                "  Do you want to proceed?",
                "  > 1. Yes",
                "    2. No",
            ]),
            styles: Vec::new(),
            modes: Default::default(),
            title: String::new(),
        };
        let shown = menu_of(&screen("    ls /tmp").lines, None)
            .expect("a menu")
            .view();
        assert_eq!(
            answer_on(&screen("    ls /tmp"), shown.seal, "1"),
            Ok(MenuKeys {
                keys: "1".into(),
                answers: true
            })
        );
        assert_eq!(
            answer_on(&screen("    rm -rf ~/work"), shown.seal, "1"),
            Err(ALREADY_ANSWERED)
        );
        assert_eq!(
            answer_on(&screen("    ls /tmp"), shown.seal, "7"),
            Err(NO_SUCH_ANSWER)
        );
        let mut gone = screen("    ls /tmp");
        gone.lines.truncate(2);
        assert_eq!(answer_on(&gone, shown.seal, "1"), Err(ALREADY_ANSWERED));
    }
}
