//! The search palette: one key at the door.
//!
//! `ctrl-a space` opens it over the terminal, listing what needs a person before
//! a character is typed, so the fastest path is also the right one. Typing
//! filters spaces, agents, tabs and actions together; `enter` goes to the
//! selected row and `esc` is back to the terminal.
//!
//! Pure state and filtering here. The caller paints the box and acts on the
//! picked item, because only it knows the screen size and the daemon.

use qoro_runtime::control::{AgentRow, WorkspaceRow};

/// What the palette offers: a terminal to move to, a space to enter, or an
/// action the interface knows how to take.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Terminal(u32),
    Space(i64),
    NewTerminal,
    NewSpace,
    CopyMode,
    Resize,
    Equalize,
}

/// One filtered row: what it does, which section it is under, and what to draw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub item: Item,
    pub section: &'static str,
    pub text: String,
    pub sub: String,
    /// The space this row belongs to, so the painter can put its colour on the part of
    /// `text` that names it, the way both panels in the column do.
    pub space: Option<i64>,
    /// How many columns of `text` are that space's name.
    pub space_len: usize,
    /// What `sub` is saying, when it is a state, so it can be painted as one.
    pub state: Option<String>,
}

/// Open palette state: the query and which filtered row is selected.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Palette {
    pub query: String,
    pub selected: usize,
}

impl Palette {
    /// An opened palette: empty query, first row selected, which is the most
    /// urgent thing on the daemon.
    pub fn open() -> Self {
        Palette {
            query: String::new(),
            selected: 0,
        }
    }

    /// Type a character. The selection returns to the top match: the list it
    /// was on belongs to a query that no longer exists.
    pub fn push(&mut self, c: char) {
        // Control characters never reach here through the caller, but a paste
        // can carry them, and a newline in the query would paint a broken box.
        if !c.is_control() {
            self.query.push(c);
            self.selected = 0;
        }
    }

    /// Rub one out. An empty query stays open: closing is `esc`, not backspace.
    pub fn backspace(&mut self) {
        self.query.pop();
        self.selected = 0;
    }

    /// Move the selection, clamped to the rows there are.
    pub fn move_by(&mut self, delta: isize, count: usize) {
        if count == 0 {
            self.selected = 0;
            return;
        }
        let next = self.selected as isize + delta;
        self.selected = next.clamp(0, count as isize - 1) as usize;
    }

    /// The rows matching the query, most urgent first.
    ///
    /// Empty query is the whole palette: needs-you, new (finished while
    /// away), running, finished, idle (shells included, they are tabs
    /// somebody opened), every space, then the two things the interface can
    /// start. A query keeps the same order and drops what does not contain
    /// it. Same order as the sidebar priority view: news above running.
    pub fn items(
        &self,
        rows: &[AgentRow],
        spaces: &[WorkspaceRow],
        focused: u32,
        seen: &crate::sidebar::Seen,
        unseen: &crate::sidebar::Unseen,
    ) -> Vec<Entry> {
        let mut out = Vec::new();
        let listed: Vec<&AgentRow> = rows.iter().collect();
        for agent in listed
            .iter()
            .filter(|a| matches!(a.state.as_str(), "blocked" | "failed"))
        {
            out.push(terminal_entry(agent, spaces, "needs you"));
        }
        for agent in listed
            .iter()
            .filter(|a| crate::sidebar::is_new(a, focused, seen, unseen))
        {
            out.push(terminal_entry(agent, spaces, "new"));
        }
        for agent in listed
            .iter()
            .filter(|a| matches!(a.state.as_str(), "working" | "starting"))
        {
            out.push(terminal_entry(agent, spaces, "running"));
        }
        for agent in listed
            .iter()
            .filter(|a| a.state == "done" && !crate::sidebar::is_new(a, focused, seen, unseen))
        {
            out.push(terminal_entry(agent, spaces, "finished"));
        }
        // Slept tabs get their own calm bucket, named, between finished and
        // idle: folding them into "idle" would lie about whether anything
        // runs there, and the catch-all below must not list them twice.
        for agent in listed.iter().filter(|a| a.state == "sleeping") {
            out.push(terminal_entry(agent, spaces, "sleeping"));
        }
        for agent in listed.iter().filter(|a| {
            !matches!(
                a.state.as_str(),
                "blocked" | "failed" | "working" | "starting" | "done" | "sleeping"
            ) && !crate::sidebar::is_new(a, focused, seen, unseen)
        }) {
            out.push(terminal_entry(agent, spaces, "idle"));
        }
        for space in spaces {
            out.push(Entry {
                item: Item::Space(space.id),
                section: "spaces",
                text: space.name.clone(),
                sub: "space".to_string(),
                space: Some(space.id),
                space_len: crate::width::width(&space.name),
                state: None,
            });
        }
        // The things the interface can do, each with the key that does it without the
        // palette, so the next time needs no palette.
        for (item, text, key) in [
            (Item::CopyMode, "copy mode: select and search history", "["),
            (Item::Resize, "resize panes", "e"),
            (Item::Equalize, "equalize panes", "="),
            (Item::NewTerminal, "new terminal here", "c"),
            (Item::NewSpace, "new space", "w"),
        ] {
            out.push(Entry {
                item,
                section: "do",
                text: text.to_string(),
                sub: crate::keys::named(&format!("ctrl-a {key}")),
                space: None,
                space_len: 0,
                state: None,
            });
        }
        let query = self.query.to_lowercase();
        if query.is_empty() {
            return out;
        }
        out.into_iter()
            .filter(|e| {
                e.text.to_lowercase().contains(&query)
                    || e.sub.to_lowercase().contains(&query)
                    || e.section.contains(&query)
            })
            .collect()
    }

    /// The selected row, if the filter left any.
    pub fn picked(
        &self,
        rows: &[AgentRow],
        spaces: &[WorkspaceRow],
        focused: u32,
        seen: &crate::sidebar::Seen,
        unseen: &crate::sidebar::Unseen,
    ) -> Option<Item> {
        self.items(rows, spaces, focused, seen, unseen)
            .get(self.selected)
            .map(|e| e.item.clone())
    }
}

/// One terminal as a palette row.
fn terminal_entry(agent: &AgentRow, spaces: &[WorkspaceRow], section: &'static str) -> Entry {
    let space = spaces
        .iter()
        .find(|s| s.id == agent.workspace)
        .map(|s| s.name.clone())
        .unwrap_or_else(|| agent.workspace.to_string());
    Entry {
        item: Item::Terminal(agent.terminal),
        section,
        text: format!("{space} {}", label(agent)),
        sub: agent.state.clone(),
        space: Some(agent.workspace),
        space_len: crate::width::width(&space),
        state: Some(agent.state.clone()),
    }
}

/// What a tab is called, the same way the tab bar calls it.
fn label(agent: &AgentRow) -> String {
    match (&agent.program, &agent.behind) {
        (Some(program), _) => program
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or(program)
            .to_string(),
        (None, Some(wrapper)) => format!("behind {wrapper}"),
        (None, None) => format!("term {}", agent.terminal),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn space(id: i64, name: &str) -> WorkspaceRow {
        WorkspaceRow {
            id,
            name: name.to_string(),
            terminals: 1,
        }
    }

    fn agent(terminal: u32, workspace: i64, program: &str, state: &str) -> AgentRow {
        AgentRow {
            terminal,
            name: None,
            workspace,
            agent: true,
            program: Some(program.to_string()),
            behind: None,
            state: state.to_string(),
            generation: None,
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
        }
    }

    fn shell(terminal: u32, workspace: i64) -> AgentRow {
        AgentRow {
            terminal,
            name: None,
            workspace,
            agent: false,
            program: Some("sh".to_string()),
            behind: None,
            state: "idle".to_string(),
            generation: None,
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
        }
    }

    fn sample() -> (Vec<AgentRow>, Vec<WorkspaceRow>) {
        (
            vec![
                agent(1, 1, "codex", "blocked"),
                agent(2, 2, "claude", "failed"),
                agent(3, 1, "claude", "working"),
                shell(4, 1),
                agent(5, 2, "claude", "done"),
                agent(6, 2, "claude", "idle"),
            ],
            vec![space(1, "app"), space(2, "webshop")],
        )
    }

    #[test]
    fn an_empty_query_lists_what_needs_you_first() {
        let (rows, spaces) = sample();
        let entries = Palette::open().items(
            &rows,
            &spaces,
            0,
            &crate::sidebar::Seen::new(),
            &crate::sidebar::Unseen::new(),
        );
        assert_eq!(entries[0].section, "needs you");
        assert_eq!(entries[0].item, Item::Terminal(1));
        assert_eq!(entries[1].item, Item::Terminal(2));
        // A daemon done nobody focused is news, above running like the sidebar.
        assert_eq!(entries[2].section, "new");
        assert_eq!(entries[2].item, Item::Terminal(5));
        assert_eq!(entries[3].section, "running");
        // Shells are tabs too, so the palette finds them under idle.
        assert!(entries.iter().any(|e| e.item == Item::Terminal(4)));
        // Spaces and both actions close the list.
        assert!(entries.iter().any(|e| e.item == Item::Space(1)));
        assert!(entries.iter().any(|e| e.item == Item::NewTerminal));
        assert_eq!(entries.last().unwrap().item, Item::NewSpace);
    }

    #[test]
    fn copy_mode_resize_and_equalize_are_found_by_name_with_their_keys() {
        let (rows, spaces) = sample();
        for (query, item, key) in [
            ("copy", Item::CopyMode, "ctrl-a ["),
            ("resize", Item::Resize, "ctrl-a e"),
            ("equal", Item::Equalize, "ctrl-a ="),
        ] {
            let mut palette = Palette::open();
            for c in query.chars() {
                palette.push(c);
            }
            let entries = palette.items(
                &rows,
                &spaces,
                0,
                &crate::sidebar::Seen::new(),
                &crate::sidebar::Unseen::new(),
            );
            let found = entries.iter().find(|e| e.item == item).expect(query);
            assert_eq!(found.sub, key);
        }
    }

    #[test]
    fn typing_filters_spaces_agents_and_actions_together() {
        let (rows, spaces) = sample();
        let mut palette = Palette::open();
        for c in "webshop".chars() {
            palette.push(c);
        }
        let entries = palette.items(
            &rows,
            &spaces,
            0,
            &crate::sidebar::Seen::new(),
            &crate::sidebar::Unseen::new(),
        );
        assert!(!entries.is_empty());
        assert!(
            entries
                .iter()
                .all(|e| e.text.to_lowercase().contains("webshop")),
            "{entries:?}"
        );
        // The blocked codex in app is gone, the failed claude in webshop stays.
        assert!(!entries.iter().any(|e| e.item == Item::Terminal(1)));
        assert!(entries.iter().any(|e| e.item == Item::Terminal(2)));
    }

    #[test]
    fn matching_is_case_insensitive_and_covers_state() {
        let (rows, spaces) = sample();
        let mut palette = Palette::open();
        for c in "BLOCKED".chars() {
            palette.push(c);
        }
        let entries = palette.items(
            &rows,
            &spaces,
            0,
            &crate::sidebar::Seen::new(),
            &crate::sidebar::Unseen::new(),
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].item, Item::Terminal(1));
    }

    #[test]
    fn typing_resets_the_selection_and_movement_clamps() {
        let (rows, spaces) = sample();
        let mut palette = Palette::open();
        let count = palette
            .items(
                &rows,
                &spaces,
                0,
                &crate::sidebar::Seen::new(),
                &crate::sidebar::Unseen::new(),
            )
            .len();
        palette.move_by(99, count);
        assert_eq!(palette.selected, count - 1);
        palette.move_by(-99, count);
        assert_eq!(palette.selected, 0);
        palette.move_by(3, count);
        palette.push('x');
        assert_eq!(palette.selected, 0, "typing keeps a stale selection");
        palette.move_by(1, 0);
        assert_eq!(palette.selected, 0);
    }

    #[test]
    fn backspace_on_an_empty_query_keeps_the_palette_open() {
        let mut palette = Palette::open();
        palette.backspace();
        assert_eq!(palette.query, "");
        palette.push('a');
        palette.backspace();
        assert_eq!(palette.query, "");
    }

    #[test]
    fn picked_follows_the_selection() {
        let (rows, spaces) = sample();
        let mut palette = Palette::open();
        assert_eq!(
            palette.picked(
                &rows,
                &spaces,
                0,
                &crate::sidebar::Seen::new(),
                &crate::sidebar::Unseen::new()
            ),
            Some(Item::Terminal(1)),
            "opens on what needs you"
        );
        let count = palette
            .items(
                &rows,
                &spaces,
                0,
                &crate::sidebar::Seen::new(),
                &crate::sidebar::Unseen::new(),
            )
            .len();
        palette.move_by((count - 1) as isize, count);
        assert_eq!(
            palette.picked(
                &rows,
                &spaces,
                0,
                &crate::sidebar::Seen::new(),
                &crate::sidebar::Unseen::new()
            ),
            Some(Item::NewSpace)
        );
    }

    #[test]
    fn picked_is_none_when_nothing_matches() {
        let (rows, spaces) = sample();
        let mut palette = Palette::open();
        for c in "zzz-no-such-thing".chars() {
            palette.push(c);
        }
        assert!(
            palette
                .items(
                    &rows,
                    &spaces,
                    0,
                    &crate::sidebar::Seen::new(),
                    &crate::sidebar::Unseen::new()
                )
                .is_empty()
        );
        assert_eq!(
            palette.picked(
                &rows,
                &spaces,
                0,
                &crate::sidebar::Seen::new(),
                &crate::sidebar::Unseen::new()
            ),
            None
        );
    }

    #[test]
    fn an_agent_in_an_unknown_space_still_matches() {
        let rows = vec![agent(9, 77, "codex", "blocked")];
        let entries = Palette::open().items(
            &rows,
            &[],
            0,
            &crate::sidebar::Seen::new(),
            &crate::sidebar::Unseen::new(),
        );
        assert!(entries[0].text.contains("77"), "{entries:?}");
    }

    /// A slept tab gets its own calm bucket, named, exactly once: the idle
    /// catch-all must not list it a second time.
    #[test]
    fn a_sleeping_terminal_is_listed_once_as_sleeping() {
        let rows = vec![agent(7, 1, "claude", "sleeping")];
        let entries = Palette::open().items(
            &rows,
            &[],
            0,
            &crate::sidebar::Seen::new(),
            &crate::sidebar::Unseen::new(),
        );
        let hits: Vec<_> = entries
            .iter()
            .filter(|e| e.item == Item::Terminal(7))
            .collect();
        assert_eq!(hits.len(), 1, "listed exactly once: {entries:?}");
        assert_eq!(hits[0].section, "sleeping");
    }
}
