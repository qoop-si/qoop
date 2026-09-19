//! The agents panel's signs, live: run it and watch.
//!
//! ```sh
//! cargo run -p qoro-tui --example spinner_demo
//! ```
//!
//! Two working rows turn their braille orbit, the idle row wears the dotted
//! waiting circle, the blocked row pulses, and the done row blinks until it is
//! looked at. Everything on screen comes through the same [`paint`] the
//! interface uses, so what you see is what the panel draws.

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
    for tick in 0..48u64 {
        let message = format!("tick {tick}: orbit ⠉⢸⣀⡇, waiting ◌, stopped □");
        let seen = sidebar::Seen::new();
        let painted = screen::paint(&Frame {
            rows: &rows,
            spaces: &spaces,
            space: Some(1),
            focused: 1,
            snapshot: None,
            cols: 60,
            lines: 16,
            message: Some(&message),
            hint: None,
            fold: sidebar::Fold::all_open(),
            view: sidebar::View::Grouped,
            seen: &seen,
            palette: None,
            prompt: None,
            menu: None,
            collapsed: false,
            reading: None,
            selection: None,
            scroll: 0,
            tick,
            anchor: 0,
            rows_back: 0,
            tiles: Vec::new(),
            behind: &behind,
        });
        print!("\x1b[H\x1b[2J{painted}");
        use std::io::Write;
        std::io::stdout().flush().expect("flush");
        std::thread::sleep(std::time::Duration::from_millis(120));
    }
    println!("{reset}", reset = theme::RESET,);
}
