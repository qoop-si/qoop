//! Spike: what does one terminal actually cost in memory with the engine ADR-003 chose?
//!
//! Two corrections a second look forced, both of which change the numbers:
//!
//!   1. `getrusage` `maxrss` is a high-water mark, so measuring several configurations in
//!      one process makes every row after the first depend on allocator retention rather
//!      than on cost. Each configuration now runs in its own process, and the measurement
//!      is current resident size, not the peak.
//!   2. The engine allocates scrollback in fixed 1000 row chunks
//!      (`grid/storage.rs`: `MAX_CACHE_SIZE = 1_000`, and `initialize` grows by
//!      `max(additional_rows, MAX_CACHE_SIZE)`), so a scrollback bound expressed in lines
//!      does not bound memory at all below 1000 lines. This spike measures the step.

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;

#[derive(Clone, Default)]
struct Sink;
impl EventListener for Sink {
    fn send_event(&self, _e: Event) {}
}

/// Current resident size, not the peak. `maxrss` cannot be reset, so it is useless for
/// more than one measurement per process.
#[cfg(target_os = "macos")]
fn resident_bytes() -> u64 {
    #[repr(C)]
    #[derive(Default)]
    struct RUsageInfoV0 {
        ri_uuid: [u8; 16],
        ri_user_time: u64,
        ri_system_time: u64,
        ri_pkg_idle_wkups: u64,
        ri_interrupt_wkups: u64,
        ri_pageins: u64,
        ri_wired_size: u64,
        ri_resident_size: u64,
        ri_phys_footprint: u64,
        ri_proc_start_abstime: u64,
        ri_proc_exit_abstime: u64,
    }
    unsafe extern "C" {
        fn proc_pid_rusage(
            pid: libc::c_int,
            flavor: libc::c_int,
            buf: *mut libc::c_void,
        ) -> libc::c_int;
    }
    let mut info = RUsageInfoV0::default();
    // SAFETY: info is a live, correctly sized RUSAGE_INFO_V0 buffer for our own pid.
    let rc = unsafe {
        proc_pid_rusage(
            std::process::id() as libc::c_int,
            0,
            &mut info as *mut _ as *mut libc::c_void,
        )
    };
    if rc == 0 { info.ri_resident_size } else { 0 }
}

#[cfg(not(target_os = "macos"))]
fn resident_bytes() -> u64 {
    let s = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    let pages: u64 = s
        .split_whitespace()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    pages * 4096
}

fn measure(n: usize, cols: usize, rows: usize, history: usize, fill_lines: usize) -> (f64, usize) {
    let before = resident_bytes();
    let mut terms = Vec::with_capacity(n);
    for _ in 0..n {
        let cfg = Config {
            scrolling_history: history,
            ..Config::default()
        };
        let mut t = Term::new(cfg, &TermSize::new(cols, rows), Sink);
        let mut p: Processor = Processor::new();
        for i in 0..fill_lines {
            p.advance(
                &mut t,
                format!("\x1b[32m[{i:06}]\x1b[0m output line with some realistic width\r\n")
                    .as_bytes(),
            );
        }
        terms.push(t);
    }
    let after = resident_bytes();
    let history_size = terms[0].grid().total_lines() - terms[0].grid().screen_lines();
    let per = (after.saturating_sub(before)) as f64 / n as f64;
    std::hint::black_box(&terms);
    (per, history_size)
}

const CASES: &[(usize, usize, usize, usize, &str)] = &[
    (80, 24, 1000, 0, "never scrolled"),
    (80, 24, 1000, 1200, "filled"),
    (200, 50, 1000, 1200, "filled"),
    (200, 50, 200, 1, "one line scrolled, 200 line budget"),
    (200, 50, 200, 400, "filled to a 200 line budget"),
    (200, 50, 1001, 1200, "filled, 1001 lines"),
    (200, 50, 0, 1200, "engine history disabled"),
];

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(idx) = args.get(1).and_then(|a| a.parse::<usize>().ok()) {
        // Child: measure exactly one configuration and print one row.
        let (cols, rows, hist, fill, label) = CASES[idx];
        let (per, hsize) = measure(100, cols, rows, hist, fill);
        println!(
            "{cols:>3}x{rows:<3} scrolling_history={hist:<5} {label:<34} {:>7.0} KiB per terminal (history_size={hsize})",
            per / 1024.0
        );
        return;
    }

    println!(
        "engine: alacritty_terminal 0.26, 100 terminals per measurement, current RSS (not maxrss),\n\
         one configuration per process so no row inherits another's high-water mark"
    );
    let exe = std::env::current_exe().expect("current_exe");
    let mut failed = false;
    for i in 0..CASES.len() {
        let out = std::process::Command::new(&exe).arg(i.to_string()).output();
        match out {
            Ok(o) if o.status.success() => print!("{}", String::from_utf8_lossy(&o.stdout)),
            _ => {
                eprintln!("FAILURE: case {i} did not run");
                failed = true;
            }
        }
    }
    println!(
        "\nthe step between a 200 line budget and a 1000 line budget is the point: the engine\n\
         allocates in 1000 row chunks, so a line count below 1000 buys no memory back"
    );
    if failed {
        std::process::exit(1);
    }
}
