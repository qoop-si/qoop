//! Phase 0 measurements, built only with the `bench` feature: a JSONL log, the frame CPU
//! clock, the time from process start to the first frame with content, and a driver that
//! types a command into the panes and quits when it has finished.
//!
//! Flags (bench builds only):
//!   --log FILE            write measurements as JSONL
//!   --run CMD             once every pane shows content, type CMD into the first
//!                         --run-panes panes and quit when all of them print `_DONE<pid>_`
//!   --run-panes N
//!   --exit-on-first-content
//!   --minimize-after-ms N minimize the window N ms after the first content

use std::cell::{Cell, RefCell};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::{Context, Window};
use tokio::sync::mpsc::UnboundedSender;

use crate::spike_link::Up;
use crate::view::{Pane, Spike};

static LOG: Mutex<Option<BufWriter<File>>> = Mutex::new(None);

thread_local! {
    /// Per pane: the run's command is going in it.
    static BUSY: RefCell<[bool; 4]> = const { RefCell::new([false; 4]) };
    /// Every pane has content and the frame showing it has not been logged yet.
    static FIRST: Cell<bool> = const { Cell::new(false) };
    static QUIT_ON_FIRST: Cell<bool> = const { Cell::new(false) };
}

fn line(value: serde_json::Value) {
    if let Some(out) = LOG.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        let _ = writeln!(out, "{value}");
    }
}

fn flush() {
    if let Some(out) = LOG.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        let _ = out.flush();
    }
}

/// A line for the log, flushed: these are rare, and a run that is killed keeps them.
pub fn note(value: impl FnOnce() -> serde_json::Value) {
    if LOG.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
        line(value());
        flush();
    }
}

/// CPU time this thread has used. Wall time would count the frames the window waits on
/// the display for; the gate is about work.
fn thread_cpu() -> Duration {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, writable timespec, and CLOCK_THREAD_CPUTIME_ID is a clock
    // macOS has had since 10.12.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    if rc != 0 {
        return Duration::ZERO;
    }
    Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}

/// How long ago the kernel started this process, from its own record of the start
/// (`proc_pidinfo`), so the time before `main` runs is counted too.
fn since_process_start() -> Option<f64> {
    let mut info = zeroed_bsdinfo();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: the buffer is a proc_bsdinfo of exactly `size` bytes, which is what
    // PROC_PIDTBSDINFO writes; the call reads nothing from it.
    let wrote = unsafe {
        libc::proc_pidinfo(
            std::process::id() as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    if wrote != size {
        return None;
    }
    let start = UNIX_EPOCH
        + Duration::from_secs(info.pbi_start_tvsec)
        + Duration::from_micros(info.pbi_start_tvusec);
    let since = SystemTime::now().duration_since(start).ok()?;
    Some(since.as_secs_f64() * 1000.0)
}

fn zeroed_bsdinfo() -> libc::proc_bsdinfo {
    // SAFETY: proc_bsdinfo is a plain C struct of integers and byte arrays, for which all
    // zero bytes is a valid value.
    unsafe { std::mem::zeroed() }
}

/// A frame's CPU time from the start of the element's prepaint to the end of its paint,
/// without what other elements did in between.
pub struct Frame {
    wall: Instant,
    cpu: Duration,
    since: Duration,
}

impl Frame {
    pub fn start() -> Self {
        Frame {
            wall: Instant::now(),
            cpu: Duration::ZERO,
            since: thread_cpu(),
        }
    }

    pub fn pause(&mut self) {
        self.cpu += thread_cpu().saturating_sub(self.since);
    }

    pub fn resume(&mut self) {
        self.since = thread_cpu();
    }

    pub fn cpu_us(&self) -> u64 {
        (self.cpu + thread_cpu().saturating_sub(self.since)).as_micros() as u64
    }
}

/// A pane finished painting: log the frame, and the first frame with content.
pub fn painted(frame: Frame, pane: usize, shown: bool, window: &mut Window) {
    if !shown {
        return;
    }
    if LOG.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
        line(serde_json::json!({
            "ev": "frame",
            "pane": pane,
            "busy": BUSY.with_borrow(|b| b.get(pane).copied().unwrap_or(false)),
            "cpu_us": frame.cpu_us(),
            "wall_us": frame.wall.elapsed().as_micros() as u64,
            "at_ms": since_process_start(),
        }));
    }
    if pane == 0 && FIRST.replace(false) {
        note(|| serde_json::json!({"ev": "first_content", "pane": 0, "ms": since_process_start()}));
        if QUIT_ON_FIRST.get() {
            window.on_next_frame(|_, cx| cx.quit());
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    run: Option<String>,
    run_panes: usize,
    minimize_after: Option<Duration>,
}

impl Options {
    /// Take one bench flag; `Ok(false)` when the flag is not one of these.
    pub fn flag(
        &mut self,
        flag: &str,
        value: &mut dyn FnMut() -> Result<String, String>,
    ) -> Result<bool, String> {
        let number = |v: String| v.parse::<u64>().map_err(|e| format!("{flag}: {e}"));
        match flag {
            "--log" => {
                let path = value()?;
                let file = File::create(Path::new(&path)).map_err(|e| format!("{path}: {e}"))?;
                *LOG.lock().unwrap_or_else(|e| e.into_inner()) = Some(BufWriter::new(file));
            }
            "--run" => self.run = Some(value()?),
            "--run-panes" => self.run_panes = number(value()?)? as usize,
            "--exit-on-first-content" => QUIT_ON_FIRST.set(true),
            "--minimize-after-ms" => {
                self.minimize_after = Some(Duration::from_millis(number(value()?)?))
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}

/// The measured run, driven by what the panes show.
pub struct Driver {
    options: Options,
    content_seen: bool,
    started: Option<Instant>,
    running: [bool; 4],
    marker: String,
}

impl Driver {
    pub fn new(options: Options) -> Self {
        Driver {
            options,
            content_seen: false,
            started: None,
            running: [false; 4],
            marker: format!("_DONE{}_", std::process::id()),
        }
    }

    pub fn on_screen(
        &mut self,
        panes: &[Pane],
        up: &UnboundedSender<Up>,
        window: &mut Window,
        cx: &mut Context<Spike>,
    ) {
        let has_content = |p: &Pane| {
            p.screen
                .as_ref()
                .is_some_and(|s| s.lines.iter().any(|l| !l.trim().is_empty()))
        };
        if !self.content_seen && panes.iter().all(has_content) {
            self.content_seen = true;
            FIRST.set(true);
            if let Some(after) = self.options.minimize_after {
                cx.spawn_in(window, async move |_, cx| {
                    cx.background_executor().timer(after).await;
                    let _ = cx.update(|window, _| window.minimize_window());
                })
                .detach();
            }
            if let Some(command) = self.options.run.clone() {
                // `\137` is `_`: the typed command line never reads `_DONE<pid>_`, only its
                // output does, and the pid keeps an earlier run's marker from counting.
                let typed = format!(
                    "{command}; printf '\\137DONE%s\\137\\n' {}\r",
                    std::process::id()
                );
                let take = self.options.run_panes.clamp(1, panes.len());
                for (i, pane) in panes.iter().enumerate().take(take) {
                    if let Some(terminal) = pane.terminal {
                        self.running[i] = true;
                        let _ = up.send(Up::Input(terminal, typed.clone().into_bytes()));
                    }
                }
                BUSY.set(self.running);
                self.started = Some(Instant::now());
                note(|| serde_json::json!({"ev": "run_start", "cmd": command}));
            }
        }
        let Some(started) = self.started else {
            return;
        };
        for (i, pane) in panes.iter().enumerate() {
            let Some(screen) = pane.screen.as_ref().filter(|_| self.running[i]) else {
                continue;
            };
            if screen.lines.iter().any(|l| l.trim_end() == self.marker) {
                self.running[i] = false;
                let real = screen
                    .lines
                    .iter()
                    .rev()
                    .find_map(|l| l.trim().strip_prefix("real")?.trim().parse::<f64>().ok());
                note(|| {
                    serde_json::json!({
                        "ev": "done",
                        "pane": i,
                        "wall_ms": started.elapsed().as_secs_f64() * 1000.0,
                        "time_real_s": real,
                        "cols": screen.cols,
                        "rows": screen.rows,
                    })
                });
            }
        }
        BUSY.set(self.running);
        if self.running.iter().all(|r| !r) {
            self.started = None;
            // A moment for the last screen to be painted and logged.
            cx.spawn(async move |_, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(300))
                    .await;
                flush();
                cx.update(|cx| cx.quit());
            })
            .detach();
        }
    }
}
