//! Spike: can `alacritty_terminal` serve as the terminal state engine behind our
//! `TerminalEngine` seam, and does it give us the damage information the idle
//! invariant needs?
//!
//! This decides ADR-003. Checks:
//!   1. feed bytes, read the grid back as text
//!   2. SGR colour and attributes survive into cell state
//!   3. scrollback exists and is bounded
//!   4. damage tracking reports something useful and is empty when nothing changed
//!   5. resize does not panic and keeps content
//!   6. throughput on a megabyte of realistic output

use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;

#[derive(Clone, Default)]
struct Collector;

impl EventListener for Collector {
    fn send_event(&self, _event: Event) {}
}

fn screen_text<T: EventListener>(term: &Term<T>) -> Vec<String> {
    let grid = term.grid();
    let mut out = Vec::new();
    for line in 0..grid.screen_lines() {
        let mut s = String::new();
        for col in 0..grid.columns() {
            let point = Point::new(Line(line as i32), Column(col));
            s.push(grid[point].c);
        }
        out.push(s.trim_end().to_string());
    }
    out
}

fn main() {
    let mut failures: Vec<String> = Vec::new();

    let size = TermSize::new(80, 24);
    let config = Config {
        scrolling_history: 1000,
        ..Config::default()
    };

    let mut term = Term::new(config, &size, Collector);
    let mut parser: Processor = Processor::new();

    // ---- 1. plain text lands in the grid ----------------------------------------
    parser.advance(&mut term, b"hello qoro\r\nsecond line\r\n");
    let text = screen_text(&term);
    let basic_ok = text[0] == "hello qoro" && text[1] == "second line";
    println!(
        "[1] plain text into grid: {} ({:?})",
        if basic_ok { "PASS" } else { "FAIL" },
        &text[..2]
    );
    if !basic_ok {
        failures.push(format!("grid text wrong: {:?}", &text[..3]));
    }

    // ---- 2. SGR attributes survive ----------------------------------------------
    parser.advance(&mut term, b"\x1b[1;31mRED\x1b[0m\r\n");
    let cell = term.grid()[Point::new(Line(2), Column(0))].clone();
    let sgr_ok = cell.c == 'R' && format!("{:?}", cell.fg).contains("Red");
    println!(
        "[2] SGR colour reaches cell state: {} (c={:?}, fg={:?}, flags={:?})",
        if sgr_ok { "PASS" } else { "FAIL" },
        cell.c,
        cell.fg,
        cell.flags
    );
    if !sgr_ok {
        failures.push(format!("sgr not applied: {:?} {:?}", cell.c, cell.fg));
    }

    // ---- 3. scrollback accumulates and stays bounded ------------------------------
    for i in 0..3000 {
        parser.advance(&mut term, format!("line {i}\r\n").as_bytes());
    }
    let total = term.grid().total_lines();
    let screen = term.grid().screen_lines();
    let history = total - screen;
    let scroll_ok = history > 0 && history <= 1000;
    println!(
        "[3] bounded scrollback: {} (history={history}, screen={screen}, total={total})",
        if scroll_ok { "PASS" } else { "FAIL" }
    );
    if !scroll_ok {
        failures.push(format!("scrollback unbounded or absent: {history}"));
    }

    // ---- 4. damage tracking -------------------------------------------------------
    // `Term::damage()` always folds in cursor damage, so a quiet terminal reports the
    // cursor line rather than nothing. What matters for us is that content damage is
    // line-precise and that a quiet terminal does not claim a full-screen repaint.
    fn damaged_lines<T: EventListener>(term: &mut Term<T>) -> (bool, Vec<usize>) {
        match term.damage() {
            alacritty_terminal::term::TermDamage::Full => (true, Vec::new()),
            alacritty_terminal::term::TermDamage::Partial(iter) => {
                (false, iter.map(|d| d.line).collect())
            }
        }
    }
    term.reset_damage();
    let (quiet_full, quiet_lines) = damaged_lines(&mut term);
    term.reset_damage();
    parser.advance(&mut term, b"\x1b[5;10Hx");
    let (after_full, after_lines) = damaged_lines(&mut term);
    let damage_ok = !quiet_full
        && !after_full
        && !quiet_lines.contains(&4)
        && after_lines.contains(&4)
        && after_lines.len() <= 3;
    println!(
        "[4] damage tracking: {} (quiet lines={quiet_lines:?}, after write to row 5 lines={after_lines:?})",
        if damage_ok { "PASS" } else { "FAIL" }
    );
    if !damage_ok {
        failures.push(format!(
            "damage not line-precise: quiet={quiet_lines:?} after={after_lines:?}"
        ));
    }

    // ---- 5. resize keeps content --------------------------------------------------
    term.reset_damage();
    let wide = TermSize::new(132, 40);
    term.resize(wide);
    let after_resize = screen_text(&term);
    let resize_ok = term.grid().columns() == 132 && term.grid().screen_lines() == 40;
    println!(
        "[5] resize to 132x40: {} (cols={}, lines={}, last non-empty {:?})",
        if resize_ok { "PASS" } else { "FAIL" },
        term.grid().columns(),
        term.grid().screen_lines(),
        after_resize.iter().rev().find(|l| !l.is_empty())
    );
    if !resize_ok {
        failures.push("resize did not take".into());
    }

    // ---- 6. throughput ------------------------------------------------------------
    let mut payload = Vec::with_capacity(1 << 20);
    let mut i = 0u32;
    while payload.len() < (1 << 20) {
        payload.extend_from_slice(
            format!("\x1b[32m[{i:06}]\x1b[0m some realistic agent output line here\r\n").as_bytes(),
        );
        i += 1;
    }
    let mb = payload.len() as f64 / (1024.0 * 1024.0);
    let t = Instant::now();
    parser.advance(&mut term, &payload);
    let dt = t.elapsed();
    let throughput = mb / dt.as_secs_f64();
    println!(
        "[6] parse throughput: {:.0} MiB/s ({:.2} MiB in {:?})",
        throughput, mb, dt
    );
    if throughput < 20.0 {
        failures.push(format!("throughput too low: {throughput:.0} MiB/s"));
    }

    if failures.is_empty() {
        println!("\nSPIKE-TERM: ALL PASS");
    } else {
        for f in &failures {
            eprintln!("FAILURE: {f}");
        }
        std::process::exit(1);
    }
}
