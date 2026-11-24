//! qass's ledger, on `ctrl-a Q`: how qass stands and Pause or Start, the link to its questions in
//! the Inbox, what it holds now, what it did today as the daemon recorded it, the schedules, the
//! rules as a count, and a line to ask it. `t` opens its own terminal.
//!
//! The phone's qass tab says the same in the same words (`PhoneQass.tsx`, `qassLedger.ts`).
//! There is no reason line under what it did: the daemon records none (plan D1).

use std::cell::Cell;

use crate::inbox::Inbox;
use crate::inbox_pane::{Env, Hit, PlaceHit, Row, clock, flow, unit, wrapped};
use crate::mux::machine_of;
use crate::places::{K, PlaceOut, finish, keys_of, machine_name};
use crate::theme::Role;

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

/// How qass stands, from the rows and the spaces.
pub fn stand(env: &Env) -> Stand {
    if env.hand.qass.is_some() {
        if env.hand.watching == Some(false) {
            return Stand::Paused;
        }
        let agents: Vec<_> = env
            .rows
            .iter()
            .filter(|r| r.agent && !env.ctx.qass_spaces.contains(&r.workspace))
            .collect();
        let mut machines: Vec<usize> = env
            .spaces
            .iter()
            .map(|s| machine_of(s.id))
            .chain(std::iter::once(0))
            .collect();
        machines.sort_unstable();
        machines.dedup();
        let of = machines.len();
        let on = machines
            .iter()
            .filter(|m| !env.ctx.lost.contains(m))
            .count();
        let watched = agents
            .iter()
            .filter(|r| !env.ctx.lost.contains(&machine_of(r.workspace)))
            .count();
        return Stand::Watching {
            agents: watched,
            on,
            of,
        };
    }
    match env
        .spaces
        .iter()
        .find(|s| s.name == crate::sidebar::QASS_SPACE && machine_of(s.id) == 0)
    {
        None => Stand::None,
        Some(space) if space.asleep || space.sleeping.iter().any(|t| t.agent.is_some()) => {
            Stand::Asleep
        }
        Some(_) => Stand::Stopped,
    }
}

/// Whether the watching switch can be turned from here: qass is paused, or it watches and the
/// switch has answered (an older qoop has none, and `p` would do nothing).
fn can_flip(stand: &Stand, env: &Env) -> bool {
    match stand {
        Stand::Paused => true,
        Stand::Watching { .. } => env.hand.watching.is_some(),
        _ => false,
    }
}

/// An act as the daemon recorded it ("wrote to T4"), its first letter raised.
pub fn act_words(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |c| {
        c.to_uppercase().chain(chars).collect::<String>()
    })
}

fn plural(n: usize, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

/// The line under the title: what qass watches and where it does not, or why it does not run.
pub fn said(stand: &Stand, env: &Env) -> String {
    match stand {
        Stand::None => "not started".to_string(),
        Stand::Asleep => "asleep: it sleeps when idle. s wakes it".to_string(),
        Stand::Paused => "paused: it wakes only when you write to it".to_string(),
        Stand::Stopped => {
            let when = env.beside.reach.qass_seen();
            format!(
                "stopped, {when}. What it held is back in your Inbox, and new questions come straight to you"
            )
        }
        Stand::Watching { agents, on, of } => {
            let mut said = format!("watching {}", plural(*agents, "agent"));
            if *of > 1 {
                if on < of {
                    said.push_str(&format!(" on {on} of {of} computers"));
                } else {
                    said.push_str(&format!(" on {of} computers"));
                }
            }
            if on < of {
                let lost: Vec<String> = env
                    .ctx
                    .lost
                    .iter()
                    .map(|m| machine_name(env.spaces, *m))
                    .collect();
                said.push_str(&format!("; {} not reached", lost.join(" and ")));
            }
            said
        }
    }
}

/// What the rules file counts, in words.
pub fn rules_words(rules: Option<crate::answers::Lessons>) -> String {
    match rules {
        None => "lessons.md could not be read".to_string(),
        Some(l) if l.all == 0 => "none written yet".to_string(),
        Some(l) => format!(
            "{}: {} it leaves to you, {} it answers alone",
            plural(l.all, "rule"),
            l.yours,
            l.alone
        ),
    }
}

/// The ledger as this client keeps it: the ask line, and where the body is scrolled to.
#[derive(Debug, Default)]
pub struct Ledger {
    /// Where the person is typing a line for qass, and what they have typed.
    typing: bool,
    text: String,
    offset: Cell<usize>,
    max: Cell<usize>,
}

/// The most characters one line to qass carries.
const ASK_MOST: usize = 400;

impl Ledger {
    pub fn scroll(&self, down: bool) {
        let off = self.offset.get();
        self.offset.set(if down {
            (off + 3).min(self.max.get())
        } else {
            off.saturating_sub(3)
        });
    }

    pub fn typing(&self) -> bool {
        self.typing
    }

    /// What is on the ask line, kept by the pane when the ledger is put away.
    pub fn draft(&self) -> &str {
        &self.text
    }

    /// A ledger that comes back with the line that was left on its ask line.
    pub fn with_draft(text: String) -> Ledger {
        Ledger {
            text,
            ..Ledger::default()
        }
    }

    /// A line that could not be sent now is back on the ask line.
    pub fn keep(&mut self, text: String) {
        self.text = text;
        self.typing = true;
    }

    pub fn click(&mut self, hit: PlaceHit, env: &Env) -> PlaceOut {
        match hit {
            PlaceHit::Watch if can_flip(&stand(env), env) => PlaceOut::Watch,
            PlaceHit::Start if !stand(env).runs() => PlaceOut::StartQass,
            PlaceHit::Questions => PlaceOut::Questions,
            PlaceHit::Terminal => env
                .hand
                .qass
                .map_or(PlaceOut::Stay, |(t, _)| PlaceOut::Jump(t)),
            PlaceHit::Ask if stand(env).runs() => {
                self.typing = true;
                PlaceOut::Stay
            }
            _ => PlaceOut::Stay,
        }
    }

    pub fn key(&mut self, bytes: &[u8], env: &Env) -> PlaceOut {
        for key in keys_of(bytes) {
            if self.typing {
                match key {
                    K::Esc => self.typing = false,
                    K::Enter => {
                        let text = self.text.trim().to_string();
                        if !text.is_empty() {
                            self.text.clear();
                            self.typing = false;
                            return PlaceOut::Ask(text);
                        }
                    }
                    K::Back => {
                        self.text.pop();
                    }
                    K::Clear => self.text.clear(),
                    K::Char(c) if self.text.chars().count() < ASK_MOST => self.text.push(c),
                    _ => {}
                }
                continue;
            }
            let stand = stand(env);
            match key {
                K::Esc => return PlaceOut::Closed,
                K::Char('p') if can_flip(&stand, env) => return PlaceOut::Watch,
                K::Char('s') if !stand.runs() => return PlaceOut::StartQass,
                K::Char('i') => return PlaceOut::Questions,
                K::Char('t') => {
                    if let Some((terminal, _)) = env.hand.qass {
                        return PlaceOut::Jump(terminal);
                    }
                }
                K::Char('a') if stand.runs() => self.typing = true,
                K::Char('j') | K::Down => {
                    self.offset.set((self.offset.get() + 1).min(self.max.get()));
                }
                K::Char('k') | K::Up => self.offset.set(self.offset.get().saturating_sub(1)),
                _ => {}
            }
        }
        PlaceOut::Stay
    }

    /// The ledger painted into `cols` by `rows`.
    pub fn view(
        &self,
        env: &Env,
        inbox: &Inbox,
        cols: usize,
        rows: usize,
    ) -> crate::inbox_pane::View {
        let stand = stand(env);
        let runs = stand.runs();
        let mut body: Vec<Row> = Vec::new();

        // The title, and the one control: Pause or Resume while it runs, Start while it does not.
        let control = match &stand {
            Stand::Paused => Some(("p resume", PlaceHit::Watch)),
            Stand::Watching { .. } if can_flip(&stand, env) => Some(("p pause", PlaceHit::Watch)),
            Stand::Watching { .. } => None,
            Stand::Asleep => Some(("s wake qass", PlaceHit::Start)),
            _ => Some(("s start qass", PlaceHit::Start)),
        };
        let mut title = vec![crate::inbox_pane::Unit {
            role: Role::Title,
            bold: true,
            text: "\u{25c6} qass".into(),
            hit: None,
        }];
        title.extend(control.map(|(text, hit)| unit(Role::Focused, text, Some(Hit::Place(hit)))));
        body.extend(flow(title, 1, 3, cols));
        let words = said(&stand, env);
        body.extend(wrapped(
            &words,
            3,
            cols,
            if matches!(stand, Stand::Stopped) {
                Role::Failed
            } else {
                Role::FaintText
            },
            false,
        ));

        // The link to its questions.
        let n = inbox.items.iter().filter(|i| i.kind.from_qass()).count();
        if n > 0 {
            body.push(Row::blank());
            let text = format!(
                "{n} question{} from qass {} in your Inbox  i opens From qass",
                if n == 1 { "" } else { "s" },
                if n == 1 { "waits" } else { "wait" }
            );
            let mut link = wrapped(&text, 1, cols, Role::Blocked, false);
            for r in &mut link {
                r.hits.push((0, cols, Hit::Place(PlaceHit::Questions)));
            }
            body.extend(link);
        }

        // Now.
        body.push(Row::blank());
        body.extend(wrapped("now", 1, cols, Role::Title, true));
        let held: Vec<String> = inbox
            .with_qass
            .iter()
            .map(|t| {
                let who = env.who_slot(&crate::answers::Slot::Agent(*t));
                let ask = env
                    .rows
                    .iter()
                    .find(|r| r.terminal == *t)
                    .and_then(|r| r.menu.as_ref())
                    .map(|m| m.question.clone())
                    .filter(|q| !q.is_empty());
                match ask {
                    Some(q) => format!("holding a question for {who}: {q}"),
                    None => format!("holding a question for {who}"),
                }
            })
            .collect();
        for line in &held {
            body.extend(wrapped(line, 3, cols, Role::Text, false));
        }
        if held.is_empty() {
            let idle = match &stand {
                Stand::Watching { .. } => "holding nothing for you",
                Stand::Paused => "paused, holding nothing",
                _ => "not running, so nothing is held",
            };
            body.extend(wrapped(idle, 3, cols, Role::FaintText, false));
        }
        if let crate::board::Read::Items(items) = &env.beside.board.read {
            let doing = items
                .iter()
                .filter(|i| !i.hidden && i.state == "doing")
                .count();
            if doing > 0 {
                body.extend(wrapped(
                    &format!("{} doing on the board", plural(doing, "item")),
                    3,
                    cols,
                    Role::FaintText,
                    false,
                ));
            }
        }

        // Did today.
        body.push(Row::blank());
        match (env.beside.acts, env.beside.acts_failed) {
            (None, Some((why, old))) => {
                body.extend(wrapped("did today", 1, cols, Role::Title, true));
                let words = if old {
                    "qoop's daemon on this computer is too old to say what qass did: restart qoop to update it".to_string()
                } else {
                    format!("what qass did could not be read: {why}")
                };
                body.extend(wrapped(&words, 3, cols, Role::Failed, false));
            }
            (None, None) => {
                body.extend(wrapped("did today", 1, cols, Role::Title, true));
                body.extend(wrapped("reading", 3, cols, Role::FaintText, false));
            }
            (Some(acts), _) => {
                body.extend(wrapped(
                    &format!("did today {}", acts.len()),
                    1,
                    cols,
                    Role::Title,
                    true,
                ));
                if acts.is_empty() {
                    body.extend(wrapped(
                        "nothing yet today",
                        3,
                        cols,
                        Role::FaintText,
                        false,
                    ));
                }
                for act in acts {
                    body.extend(wrapped(
                        &format!("{}  {}", clock(act.at), act_words(&act.text)),
                        3,
                        cols,
                        Role::Text,
                        false,
                    ));
                }
            }
        }

        // Schedules.
        {
            let list = env.beside.schedules.unwrap_or(&[]);
            body.push(Row::blank());
            body.extend(wrapped(
                &format!("schedules {}", list.len()),
                1,
                cols,
                Role::Title,
                true,
            ));
            let zone = qoop_runtime::schedule::system_zone();
            let mut ordered: Vec<_> = list.iter().collect();
            ordered.sort_by_key(|s| {
                if s.paused || s.next_at.is_none() {
                    i64::MAX
                } else {
                    s.next_at.unwrap_or(i64::MAX)
                }
            });
            body.extend(wrapped(
                "add or change them with qoop schedule",
                3,
                cols,
                Role::FaintText,
                false,
            ));
            for s in ordered {
                let next = match (s.paused, s.next_at) {
                    (true, _) => "paused".to_string(),
                    (false, Some(at)) => {
                        format!("next {}", crate::schedules::moment(at, env.now_unix, zone))
                    }
                    (false, None) => "done".to_string(),
                };
                body.extend(wrapped(
                    &format!("{}  {}, {next}", s.name, s.when),
                    3,
                    cols,
                    Role::Text,
                    false,
                ));
            }
        }

        // Rules.
        body.push(Row::blank());
        body.extend(wrapped("rules", 1, cols, Role::Title, true));
        body.extend(wrapped(
            &rules_words(env.beside.rules),
            3,
            cols,
            Role::Text,
            false,
        ));

        // The ask line, and the keys.
        body.push(Row::blank());
        let ask = if self.typing {
            format!("ask qass \u{25b8} {}\u{258c}", self.text)
        } else if runs && !self.text.is_empty() {
            format!("ask qass \u{25b8} {}  (a edits it)", self.text)
        } else if runs {
            "ask qass \u{25b8} a types here, enter sends".to_string()
        } else {
            "qass is stopped. s starts it and opens its terminal".to_string()
        };
        let mut ask_rows = wrapped(
            &ask,
            1,
            cols,
            if self.typing {
                Role::Focused
            } else {
                Role::FaintText
            },
            self.typing,
        );
        for r in &mut ask_rows {
            r.hits.push((0, cols, Hit::Place(PlaceHit::Ask)));
        }
        body.extend(ask_rows);

        let mut foot = Vec::new();
        if self.typing {
            foot.push(unit(Role::FaintText, "enter sends", None));
            foot.push(unit(Role::FaintText, "esc stops typing", None));
        } else {
            foot.extend(
                control.map(|(text, hit)| unit(Role::FaintText, text, Some(Hit::Place(hit)))),
            );
            if n > 0 {
                foot.push(unit(
                    Role::FaintText,
                    "i questions",
                    Some(Hit::Place(PlaceHit::Questions)),
                ));
            }
            if runs {
                foot.push(unit(
                    Role::FaintText,
                    "a ask",
                    Some(Hit::Place(PlaceHit::Ask)),
                ));
                foot.push(unit(
                    Role::FaintText,
                    "t its terminal",
                    Some(Hit::Place(PlaceHit::Terminal)),
                ));
            }
            foot.push(unit(Role::FaintText, "j k scroll", None));
            foot.push(unit(Role::FaintText, "esc back", None));
        }
        let (view, max) = finish(body, foot, cols, rows, self.offset.get());
        self.max.set(max);
        self.offset.set(self.offset.get().min(max));
        view
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::places::fixture::world;

    /// The rows as one line of single spaces: a phrase may wrap, and is checked whole.
    fn text(view: &crate::inbox_pane::View) -> String {
        view.plain
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn painted(
        ledger: &Ledger,
        w: &crate::places::fixture::World,
        cols: usize,
        rows: usize,
    ) -> String {
        w.with(|env| {
            let inbox = Inbox::build(env.rows, env.cards, env.hand, env.ctx, &[]);
            text(&ledger.view(env, &inbox, cols, rows))
        })
    }

    /// `t` opens qass's own terminal while qass runs, and does nothing when it does not.
    #[test]
    fn t_opens_qass_own_terminal() {
        let mut w = world();
        let mut ledger = Ledger::default();
        assert_eq!(w.with(|env| ledger.key(b"t", env)), PlaceOut::Jump(10));
        w.hand.qass = None;
        assert_eq!(w.with(|env| ledger.key(b"t", env)), PlaceOut::Stay);
    }

    /// A running qass offers Pause (Resume while paused) and no Start; a stopped one hides
    /// Pause, offers Start and says what became of what it held.
    #[test]
    fn a_stopped_qass_hides_pause_and_shows_start() {
        let mut w = world();
        let ledger = Ledger::default();
        let running = painted(&ledger, &w, 104, 40);
        assert!(running.contains("p pause"), "{running}");
        assert!(!running.contains("start qass"), "{running}");
        assert!(
            running.contains("watching 3 agents on 1 of 2 computers; build-node not reached"),
            "{running}"
        );

        w.hand.watching = Some(false);
        let paused = painted(&ledger, &w, 104, 40);
        assert!(
            paused.contains("p resume") && !paused.contains("p pause"),
            "{paused}"
        );

        w.hand.qass = None;
        w.rows.retain(|r| r.terminal != 10);
        let stopped = painted(&ledger, &w, 104, 40);
        assert!(stopped.contains("s start qass"), "{stopped}");
        assert!(
            !stopped.contains("p pause") && !stopped.contains("p resume"),
            "{stopped}"
        );
        assert!(!stopped.contains("t its terminal"), "{stopped}");
        assert!(
            stopped.contains("stopped, not seen running since this window opened"),
            "{stopped}"
        );
        assert!(stopped.contains("back in your Inbox"), "{stopped}");

        // The keys follow: `p` does nothing, `s` and enter start it, and `t` opens nothing.
        let mut ledger = Ledger::default();
        assert_eq!(w.with(|env| ledger.key(b"p", env)), PlaceOut::Stay);
        assert_eq!(w.with(|env| ledger.key(b"s", env)), PlaceOut::StartQass);
        assert_eq!(
            w.with(|env| ledger.key(b"\r", env)),
            PlaceOut::Stay,
            "enter is no alias for Start: a stray key must not wake qass"
        );
        assert_eq!(w.with(|env| ledger.key(b"t", env)), PlaceOut::Stay);
        // Running: `p` turns the watching switch, `s` does nothing.
        w.hand.qass = Some((10, Some(1)));
        w.hand.watching = Some(true);
        assert_eq!(w.with(|env| ledger.key(b"p", env)), PlaceOut::Watch);
        assert_eq!(w.with(|env| ledger.key(b"s", env)), PlaceOut::Stay);
    }

    /// What it shows: its questions as a link, what it holds, what it did as recorded with no
    /// reason line, the schedules and the rules as a count.
    #[test]
    fn the_ledger_says_what_qass_holds_did_and_runs() {
        let w = world();
        let ledger = Ledger::default();
        let t = painted(&ledger, &w, 104, 60);
        assert!(
            t.contains("1 question from qass waits in your Inbox"),
            "{t}"
        );
        assert!(
            t.contains("holding a question for crw / credit system"),
            "{t}"
        );
        assert!(t.contains("did today 3"), "{t}");
        assert!(
            t.contains("Wrote to T4") && t.contains("Verified task 7") && t.contains("Closed E3"),
            "{t}"
        );
        assert!(!t.contains("by rule"), "no reason line: {t}");
        assert!(
            t.contains("schedules 3") && t.contains("nightly-qoop") && t.contains("paused"),
            "{t}"
        );
        assert!(
            t.contains("12 rules: 3 it leaves to you, 8 it answers alone"),
            "{t}"
        );
        assert!(t.contains("2 items doing on the board"), "{t}");
    }

    /// The ask line: `a`, words, enter sends them; escape leaves what was typed unsent.
    #[test]
    fn the_ask_line_sends_what_was_typed() {
        let w = world();
        let mut ledger = Ledger::default();
        assert_eq!(w.with(|env| ledger.key(b"a", env)), PlaceOut::Stay);
        assert!(ledger.typing());
        assert_eq!(
            w.with(|env| ledger.key(b"what is T4 doing", env)),
            PlaceOut::Stay
        );
        assert_eq!(
            w.with(|env| ledger.key(b"\r", env)),
            PlaceOut::Ask("what is T4 doing".into())
        );
        assert!(!ledger.typing());
    }

    /// Nothing is cut at any width: every word of what it did and every schedule is on screen
    /// whole, no ellipsis, and no row is wider than the pane.
    #[test]
    fn nothing_is_cut_at_any_width() {
        let w = world();
        let ledger = Ledger::default();
        for (cols, rows) in [(41, 200), (63, 200), (80, 200), (160, 200)] {
            let t = painted(&ledger, &w, cols, rows);
            assert!(!t.contains('\u{2026}'), "{cols}: an ellipsis\n{t}");
            let joined = t.clone();
            for word in [
                "watching",
                "holding",
                "credit",
                "nightly-qoop",
                "weekly-report",
                "mondays",
                "08:30",
                "answers",
                "alone",
            ] {
                assert!(joined.contains(word), "{cols}: lost {word}\n{t}");
            }
            let view = w.with(|env| {
                let inbox = Inbox::build(env.rows, env.cards, env.hand, env.ctx, &[]);
                ledger.view(env, &inbox, cols, rows)
            });
            for row in &view.plain {
                assert!(crate::width::width(row) <= cols, "{cols}: wide row {row:?}");
            }
        }
    }

    /// Pause is offered only when the watching switch has answered: an older qoop has none and
    /// `p` would do nothing.
    #[test]
    fn pause_is_offered_only_when_the_switch_is_known() {
        let mut w = world();
        w.hand.watching = None;
        let ledger = Ledger::default();
        let t = painted(&ledger, &w, 104, 40);
        assert!(!t.contains("p pause"), "{t}");
        let mut ledger = Ledger::default();
        assert_eq!(w.with(|env| ledger.key(b"p", env)), PlaceOut::Stay);
        assert_eq!(
            w.with(|env| ledger.click(PlaceHit::Watch, env)),
            PlaceOut::Stay
        );
        w.hand.watching = Some(true);
        assert_eq!(w.with(|env| ledger.key(b"p", env)), PlaceOut::Watch);
    }

    /// Schedules are always drawn, with where to change them, even with none.
    #[test]
    fn schedules_are_always_drawn_with_where_to_change_them() {
        let mut w = world();
        w.schedules = Some(Vec::new());
        let t = painted(&Ledger::default(), &w, 104, 60);
        assert!(t.contains("schedules 0"), "{t}");
        assert!(t.contains("add or change them with qoop schedule"), "{t}");
        w.schedules = None;
        let t = painted(&Ledger::default(), &w, 104, 60);
        assert!(
            t.contains("schedules 0") && t.contains("qoop schedule"),
            "{t}"
        );
    }

    /// What qass did could not be read: the ledger says why in words, never "reading" forever.
    #[test]
    fn a_failed_read_of_what_qass_did_is_said_in_words() {
        let mut w = world();
        w.acts = None;
        let reading = painted(&Ledger::default(), &w, 104, 60);
        assert!(reading.contains("did today reading"), "{reading}");
        let said = w.with(|env| {
            let b = crate::places::Beside {
                acts_failed: Some(("unknown variant `QassActs`", true)),
                ..*env.beside
            };
            let env = Env { beside: &b, ..*env };
            let inbox = Inbox::build(env.rows, env.cards, env.hand, env.ctx, &[]);
            Ledger::default()
                .view(&env, &inbox, 104, 60)
                .plain
                .join(" ")
        });
        let said = said.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            said.contains(
                "daemon on this computer is too old to say what qass did: restart qoop to update it"
            ),
            "{said}"
        );
        assert!(!said.contains("did today reading"), "{said}");
    }

    /// What was left on the ask line comes back when the ledger is opened again.
    #[test]
    fn a_draft_on_the_ask_line_survives_closing_the_ledger() {
        let w = world();
        let mut ledger = Ledger::default();
        w.with(|env| ledger.key(b"a", env));
        w.with(|env| ledger.key(b"what is T4 doing", env));
        let kept = ledger.draft().to_string();
        let back = Ledger::with_draft(kept);
        let t = painted(&back, &w, 104, 60);
        assert!(t.contains("what is T4 doing"), "{t}");
        // And the pane keeps it across put away and open.
        let mut pane = crate::inbox_pane::Pane::new();
        pane.open_place(crate::places::Place::Ledger(ledger), false);
        pane.close();
        assert_eq!(pane.ask_draft, "what is T4 doing");
    }
}
