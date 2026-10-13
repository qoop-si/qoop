//! The search palette: one key at the door.
//!
//! `ctrl-a space` opens it over the terminal, listing what needs a person before
//! a character is typed, so the fastest path is also the right one. Typing
//! filters spaces, agents, tabs and actions together; `enter` goes to the
//! selected row and `esc` is back to the terminal.
//!
//! Pure state and filtering here. The caller paints the box and acts on the
//! picked item, because only it knows the screen size and the daemon.

use qoop_runtime::control::{AgentRow, WorkspaceRow};

/// What the palette offers: a terminal to move to, a space to enter, or an
/// action the interface knows how to take.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Terminal(u32),
    Space(i64),
    NewTerminal,
    NewSpace,
    /// A new terminal running this agent: the command is typed into the new
    /// shell once the tab opens, the same path `qoop new -- <agent>` takes.
    NewAgent(String),
    /// A new terminal running an agent the table does not know: the palette
    /// asks which command to type, so any agent works, not just the listed ones.
    NewAgentAsk,
    CopyMode,
    Resize,
    Equalize,
    /// Put the tab the keyboard is in to sleep, as `ctrl-a Z` does.
    Sleep,
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

/// Which kind of row the palette shows, cycled with tab: everything, or one kind.
///
/// Named on the query row so the list never shrinks without saying why.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    Tabs,
    Spaces,
    Agents,
    Actions,
}

impl Filter {
    /// In the order tab walks them, which is the order the query row shows them.
    pub const ORDER: [Filter; 5] = [
        Filter::All,
        Filter::Tabs,
        Filter::Spaces,
        Filter::Agents,
        Filter::Actions,
    ];

    /// The word on the query row.
    pub fn name(self) -> &'static str {
        match self {
            Filter::All => "all",
            Filter::Tabs => "tab",
            Filter::Spaces => "space",
            Filter::Agents => "agent",
            Filter::Actions => "action",
        }
    }

    /// Whether a row of this item stays under this filter. By what the row does, not by
    /// the header it sits under, so a new section can never slip past a filter.
    fn keeps(self, item: &Item) -> bool {
        let agent = matches!(item, Item::NewAgent(_) | Item::NewAgentAsk);
        match self {
            Filter::All => true,
            Filter::Tabs => matches!(item, Item::Terminal(_)),
            Filter::Spaces => matches!(item, Item::Space(_)),
            Filter::Agents => agent,
            Filter::Actions => !agent && !matches!(item, Item::Terminal(_) | Item::Space(_)),
        }
    }
}

/// Open palette state: the query and which filtered row is selected.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Palette {
    pub query: String,
    pub selected: usize,
    /// The kind of row on show; `All` every time the palette opens.
    pub filter: Filter,
    /// The config's `[agents]`, read when the palette opened, so an edit shows the next
    /// time without the file being read per frame.
    pub agents: Vec<crate::config::Launcher>,
}

impl Palette {
    /// An opened palette: empty query, first row selected, which is the most
    /// urgent thing on the daemon.
    pub fn open() -> Self {
        Palette::default()
    }

    /// An opened palette that also offers the person's own agents.
    pub fn with_agents(agents: Vec<crate::config::Launcher>) -> Self {
        Palette {
            agents,
            ..Palette::default()
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

    /// The next filter, or the one before, and the selection back to the top: the list
    /// it was on is not the list on screen any more.
    pub fn cycle(&mut self, forward: bool) {
        let at = Filter::ORDER
            .iter()
            .position(|f| *f == self.filter)
            .unwrap_or(0);
        let len = Filter::ORDER.len();
        let next = if forward { at + 1 } else { at + len - 1 };
        self.filter = Filter::ORDER[next % len];
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
        for agent in listed.iter().filter(|a| {
            !matches!(
                a.state.as_str(),
                "blocked" | "failed" | "working" | "starting" | "done"
            ) && !crate::sidebar::is_new(a, focused, seen, unseen)
        }) {
            out.push(terminal_entry(agent, spaces, "idle"));
        }
        // Sleeping tabs, one per tab, below everything that runs: picking one shows what it
        // last drew, and waking it is a separate key.
        for space in spaces {
            for tab in space
                .sleeping
                .iter()
                .filter(|t| t.lead.is_none_or(|lead| lead == t.terminal))
            {
                let name = tab.label();
                out.push(Entry {
                    item: Item::Terminal(tab.terminal),
                    section: "sleeping",
                    text: format!("{} {name}", space.name),
                    sub: "asleep".to_string(),
                    space: Some(space.id),
                    space_len: crate::width::width(&space.name),
                    state: None,
                });
            }
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
        // palette, so the next time needs no palette. The agent row takes no key: it
        // asks which command to type, so an agent nobody listed still starts.
        for (item, text, key) in [
            (Item::CopyMode, "copy mode: select and search history", "["),
            (Item::Resize, "resize panes", "e"),
            (Item::Equalize, "equalize panes", "="),
            (Item::NewTerminal, "new terminal here", "c"),
            (Item::NewAgentAsk, "new agent terminal...", ""),
            (Item::Sleep, "sleep this tab", "Z"),
            (Item::NewSpace, "new space", "w"),
        ] {
            // Under the agent filter the ask row leads the agents it offers, not a `do`
            // header of its own.
            let section = if item == Item::NewAgentAsk && self.filter == Filter::Agents {
                "agents"
            } else {
                "do"
            };
            out.push(Entry {
                item,
                section,
                text: text.to_string(),
                sub: if key.is_empty() {
                    "type any agent".to_string()
                } else {
                    crate::keys::named(&format!("ctrl-a {key}"))
                },
                space: None,
                space_len: 0,
                state: None,
            });
        }
        // One row per known agent, but only while filtering or when agents are what is
        // asked for: the empty query stays short with the single agent row above, and
        // typing a name finds its own.
        if !self.query.is_empty() || self.filter == Filter::Agents {
            let needle = self.query.to_lowercase();
            // The person's own first: picking one types its command, not its name.
            for agent in &self.agents {
                let text = format!("new {} terminal", agent.name);
                if text.to_lowercase().contains(&needle) {
                    out.push(Entry {
                        item: Item::NewAgent(agent.command.clone()),
                        section: "agents",
                        text,
                        sub: agent.command.clone(),
                        space: None,
                        space_len: 0,
                        state: None,
                    });
                }
            }
            for name in qoop_domain::program::agent_names() {
                let text = format!("new {name} terminal");
                // The person's own entry of the same name already offered it, their way.
                let theirs = self
                    .agents
                    .iter()
                    .any(|a| a.name.eq_ignore_ascii_case(name));
                if !theirs && text.contains(&needle) {
                    out.push(Entry {
                        item: Item::NewAgent(name.to_string()),
                        section: "agents",
                        text,
                        sub: "agent".to_string(),
                        space: None,
                        space_len: 0,
                        state: None,
                    });
                }
            }
        }
        let query = self.query.to_lowercase();
        out.into_iter()
            .filter(|e| self.filter.keeps(&e.item))
            .filter(|e| {
                query.is_empty()
                    || e.text.to_lowercase().contains(&query)
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
            keep_awake: false,
            id,
            name: name.to_string(),
            terminals: 1,
            asleep: false,
            sleeping: Vec::new(),
            closable: true,
            ink: None,
            link: None,
            goal: None,
        }
    }

    fn agent(terminal: u32, workspace: i64, program: &str, state: &str) -> AgentRow {
        AgentRow {
            keep_awake: false,
            background: 0,
            subagents: 0,
            task: None,
            news: false,
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
            pgid: None,
            discards: None,
        }
    }

    fn shell(terminal: u32, workspace: i64) -> AgentRow {
        AgentRow {
            keep_awake: false,
            background: 0,
            subagents: 0,
            task: None,
            news: false,
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
            pgid: None,
            discards: None,
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
    fn each_filter_keeps_its_own_kind_and_tab_comes_back_round() {
        let (rows, spaces) = sample();
        let seen = crate::sidebar::Seen::new();
        let unseen = crate::sidebar::Unseen::new();
        let mut palette = Palette::with_agents(vec![crate::config::Launcher {
            name: "claude".to_string(),
            command: "claude --fast".to_string(),
        }]);
        let mut shown = Vec::new();
        for _ in Filter::ORDER {
            palette.cycle(true);
            shown.push((
                palette.filter,
                palette.items(&rows, &spaces, 0, &seen, &unseen),
            ));
        }
        assert_eq!(palette.filter, Filter::All, "five steps come back to all");
        for (filter, entries) in &shown {
            assert!(!entries.is_empty(), "{filter:?} left nothing");
            assert!(
                entries.iter().all(|e| filter.keeps(&e.item)),
                "{filter:?} let in another kind"
            );
        }
        let of = |f: Filter| &shown.iter().find(|(g, _)| *g == f).unwrap().1;
        // Agents on an empty query: the ask row first, all under one header, and the
        // person's own claude offered once, their way.
        let agents = of(Filter::Agents);
        assert_eq!(agents[0].item, Item::NewAgentAsk);
        assert!(agents.iter().all(|e| e.section == "agents"));
        assert_eq!(
            agents
                .iter()
                .filter(|e| e.text == "new claude terminal")
                .count(),
            1
        );
        assert!(
            agents
                .iter()
                .any(|e| e.item == Item::NewAgent("claude --fast".to_string()))
        );
        assert!(
            !of(Filter::Actions)
                .iter()
                .any(|e| e.item == Item::NewAgentAsk)
        );
        assert!(
            of(Filter::Tabs)
                .iter()
                .all(|e| matches!(e.item, Item::Terminal(_)))
        );
        // Back from all is the last one.
        palette.cycle(false);
        assert_eq!(palette.filter, Filter::Actions);
        assert_eq!(palette.selected, 0);
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

    /// A `[agents]` entry is found by its name and starts its command, ahead of the
    /// known agents.
    #[test]
    fn a_configured_agent_is_found_by_name_and_types_its_command() {
        let (rows, spaces) = sample();
        let mut palette = Palette::with_agents(vec![crate::config::Launcher {
            name: "claudec".to_string(),
            command: "claude --dangerously-skip-permissions".to_string(),
        }]);
        for c in "claudec".chars() {
            palette.push(c);
        }
        assert_eq!(
            palette.picked(
                &rows,
                &spaces,
                0,
                &crate::sidebar::Seen::new(),
                &crate::sidebar::Unseen::new()
            ),
            Some(Item::NewAgent(
                "claude --dangerously-skip-permissions".to_string()
            ))
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

    /// A sleeping space's tabs are listed once each, under their own name, so a jump can
    /// wake them; the `sleep this tab` row is there for the awake one.
    #[test]
    fn a_sleeping_spaces_tabs_are_listed_under_sleeping() {
        let mut asleep = space(2, "api");
        asleep.asleep = true;
        asleep.sleeping = vec![qoop_runtime::control::SleepingTab {
            lead: None,
            terminal: 7,
            session: None,
            title: None,
            agent: Some("claude".into()),
            cwd: "/tmp".into(),
        }];
        let entries = Palette::open().items(
            &[],
            &[space(1, "default"), asleep],
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
        assert_eq!(hits[0].text, "api claude");
        assert!(entries.iter().any(|e| e.item == Item::Sleep));
    }

    /// The empty query stays short: one agent row that asks which command,
    /// and no preset rows. Twenty preset rows would bury the tabs the palette
    /// is for.
    #[test]
    fn an_empty_query_shows_one_agent_row_and_no_presets() {
        let (rows, spaces) = sample();
        let entries = Palette::open().items(
            &rows,
            &spaces,
            0,
            &crate::sidebar::Seen::new(),
            &crate::sidebar::Unseen::new(),
        );
        assert!(
            entries
                .iter()
                .filter(|e| matches!(e.item, Item::NewAgent(_)))
                .collect::<Vec<_>>()
                .is_empty(),
            "presets wait for a query: {entries:?}"
        );
        let ask = entries
            .iter()
            .find(|e| e.item == Item::NewAgentAsk)
            .expect("the generic agent row");
        assert_eq!(ask.text, "new agent terminal...");
        assert_eq!(entries.last().unwrap().item, Item::NewSpace);
    }

    /// Typing a name finds its own row, so `clau` starts claude. Terminals
    /// already running that agent still list first: the palette moves to what
    /// exists before offering to start another.
    #[test]
    fn typing_an_agent_name_finds_its_own_row() {
        let (rows, spaces) = sample();
        let mut palette = Palette::open();
        for c in "clau".chars() {
            palette.push(c);
        }
        let entries = palette.items(
            &rows,
            &spaces,
            0,
            &crate::sidebar::Seen::new(),
            &crate::sidebar::Unseen::new(),
        );
        let found = entries
            .iter()
            .find(|e| e.item == Item::NewAgent("claude".to_string()))
            .expect("new claude terminal");
        assert_eq!(found.text, "new claude terminal");
        // The full row text matches nothing already running, so enter starts it.
        let mut palette = Palette::open();
        for c in "new claude terminal".chars() {
            palette.push(c);
        }
        assert_eq!(
            palette.picked(
                &rows,
                &spaces,
                0,
                &crate::sidebar::Seen::new(),
                &crate::sidebar::Unseen::new()
            ),
            Some(Item::NewAgent("claude".to_string()))
        );
    }

    /// `agent` finds the generic row, which is the door for an agent nobody
    /// listed: the prompt after it takes any command.
    #[test]
    fn typing_agent_finds_the_generic_row() {
        let (rows, spaces) = sample();
        let mut palette = Palette::open();
        for c in "agent".chars() {
            palette.push(c);
        }
        let entries = palette.items(
            &rows,
            &spaces,
            0,
            &crate::sidebar::Seen::new(),
            &crate::sidebar::Unseen::new(),
        );
        assert!(
            entries.iter().any(|e| e.item == Item::NewAgentAsk),
            "{entries:?}"
        );
    }

    /// An unknown name matches nothing in the palette itself: it starts
    /// through the generic row, not through a row of its own.
    #[test]
    fn an_unknown_agent_matches_no_row_of_its_own() {
        let (rows, spaces) = sample();
        let mut palette = Palette::open();
        for c in "zzz-no-such-agent".chars() {
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
    }
}
