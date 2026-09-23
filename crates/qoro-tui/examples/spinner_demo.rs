//! The agents panel, static: run it and read.
//!
//! ```sh
//! cargo run -p qoro-tui --example spinner_demo
//! ```
//!
//! Every state says its word on the right, an unread done says `new`, and no
//! tick ever changes a cell. Everything on screen comes through the same
//! [`paint`] the interface uses, so what you see is what the panel draws.

use qoro_runtime::control::{AgentRow, WorkspaceRow};
use qoro_tui::screen::{self, Frame};
use qoro_tui::{sidebar, theme};

fn space(id: i64, name: &str) -> WorkspaceRow {
    WorkspaceRow {
        id,
        name: name.to_string(),
        terminals: 4,
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
    }
}

fn main() {
    let spaces = vec![space(1, "qoro"), space(2, "task")];
    let rows = vec![
        agent(1, 1, "opencode", "working"),
        agent(2, 1, "claude", "starting"),
        agent(3, 2, "codex", "idle"),
        agent(4, 1, "gemini", "blocked"),
        agent(5, 2, "amp", "done"),
    ];
    let behind = qoro_tui::run::Behind::default();
    let message = "static: working says working, unread done says new";
    let seen = sidebar::Seen::new();
    let unseen = sidebar::Unseen::new();
    let painted = screen::paint(&Frame {
        rows: &rows,
        spaces: &spaces,
        space: Some(1),
        panes: &std::collections::BTreeSet::new(),
        focused: 1,
        snapshot: None,
        cols: 60,
        lines: 16,
        message: Some(message),
        hint: None,
        view: sidebar::View::Grouped,
        seen: &seen,
        unseen: &unseen,
        palette: None,
        prompt: None,
        menu: None,
        collapsed: false,
        reading: None,
        selection: None,
        scroll: 0,
        tick: 0,
        now_unix: 0,
        sidebar_width: sidebar::WIDTH,
        anchor: 0,
        rows_back: 0,
        tiles: Vec::new(),
        behind: &behind,
    });
    print!("{painted}");
    use std::io::Write;
    std::io::stdout().flush().expect("flush");
    println!("{reset}", reset = theme::RESET,);
}
