//! The left column at frame zero: run it and read.
//!
//! ```sh
//! cargo run -p qoro-tui --example spinner_demo
//! ```
//!
//! Every state wears its dot, an unread done wears the done sage, and at frame zero
//! nothing moves. Everything on screen comes through the same [`paint`] the interface
//! uses, so what you see is what the column draws.

use qoro_runtime::control::{AgentRow, WorkspaceRow};
use qoro_tui::screen::{self, Frame};
use qoro_tui::{sidebar, theme};

fn space(id: i64, name: &str) -> WorkspaceRow {
    WorkspaceRow {
        id,
        name: name.to_string(),
        terminals: 4,
        asleep: false,
        sleeping: Vec::new(),
        closable: true,
    }
}

fn agent(terminal: u32, workspace: i64, program: &str, state: &str) -> AgentRow {
    AgentRow {
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
        panes: std::collections::BTreeSet::new(),
        focused: 1,
        armed: None,
        snapshot: None,
        cols: 60,
        lines: 16,
        message: Some(message),
        hint: None,
        view: sidebar::View::Tree,
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
        image_notice: None,
        chrome: Default::default(),
    });
    print!("{painted}");
    use std::io::Write;
    std::io::stdout().flush().expect("flush");
    println!("{reset}", reset = theme::RESET,);
}
