//! Every agent session on this computer, as `AgentSessions` and `AgentActivity` read it.
//!
//! The index (`qoop_parsed::index`) is brought up to date by `qoop sessions-index`, a process
//! of its own, one at a time: a first read of a large history takes about a minute and holds
//! hundreds of megabytes at its peak (699 MB measured), which go back to the system when that
//! process ends instead of staying with the daemon. A request only ever reads the last
//! finished index: one that finds it older than [`STALE`] starts the next refresh and answers
//! from the one it has; with none yet it says it is still reading, and the client asks again.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use qoop_parsed::index::{self, Index};

use crate::control::{ControlResponse, SessionRow};

/// How old an index may be before a request asks for a fresh one. Refreshes follow the asks:
/// none runs while no one looks.
const STALE: Duration = Duration::from_secs(30);
/// Sessions a list holds at most.
const MOST: u32 = 200;
/// Days an activity covers at most.
const MOST_DAYS: u32 = 366;
/// How long one look at a directory may take: a dead mount answers late or never.
const PROBE: Duration = Duration::from_secs(2);

#[derive(Default)]
pub struct Sessions {
    /// Where the index is kept; unset (tests, a daemon without a state dir) means never read.
    path: OnceLock<PathBuf>,
    shared: Arc<Shared>,
}

#[derive(Default)]
struct Shared {
    last: Mutex<Option<(Arc<Index>, Instant)>>,
    refreshing: AtomicBool,
}

/// Clears `refreshing` however the refresh ends, a panic included: a flag left set would
/// answer every later request with "still reading".
struct Done<'a>(&'a AtomicBool);

impl Drop for Done<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Shared {
    fn publish(&self, index: Index, at: Instant) {
        if let Ok(mut last) = self.last.lock() {
            *last = Some((Arc::new(index), at));
        }
    }
}

impl Sessions {
    /// The file the index is read from and written to, beside the daemon's other state.
    pub fn place(&self, path: PathBuf) {
        let _ = self.path.set(path);
    }

    /// The last finished index, starting a refresh when it is missing or stale.
    fn current(&self) -> Option<Arc<Index>> {
        let last = self.shared.last.lock().ok()?.clone();
        if last.as_ref().is_none_or(|(_, at)| at.elapsed() > STALE)
            && let Some(path) = self.path.get()
        {
            self.refresh(path.clone(), last.is_none());
        }
        last.map(|(index, _)| index)
    }

    fn refresh(&self, path: PathBuf, first: bool) {
        if self.shared.refreshing.swap(true, Ordering::AcqRel) {
            return;
        }
        let shared = self.shared.clone();
        let spawned = std::thread::Builder::new()
            .name("qoop-sessions".into())
            .spawn(move || {
                let _done = Done(&shared.refreshing);
                // What the last run wrote is shown while it is brought up to date, marked
                // stale so the next ask after this refresh starts another.
                if first {
                    let saved = Index::load(&path);
                    if !saved.summaries().is_empty() {
                        let old = Instant::now()
                            .checked_sub(STALE)
                            .unwrap_or_else(Instant::now);
                        shared.publish(saved, old);
                    }
                }
                if read_sessions(&path) {
                    shared.publish(Index::load(&path), Instant::now());
                }
            });
        if spawned.is_err() {
            self.shared.refreshing.store(false, Ordering::Release);
        }
    }
}

/// Runs `qoop sessions-index <path>` with this daemon's own binary and waits for it. Its
/// failures go to the daemon's log; the index kept is then the one before.
fn read_sessions(path: &Path) -> bool {
    let ran = std::env::current_exe().and_then(|exe| {
        Command::new(exe)
            .arg("sessions-index")
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()
    });
    match ran {
        Ok(status) if status.success() => true,
        Ok(status) => {
            crate::say!("qoopd: reading the agent sessions ended with {status}");
            false
        }
        Err(e) => {
            crate::say!("qoopd: the agent sessions could not be read: {e}");
            false
        }
    }
}

/// Whether each directory is there, in one look through the probe pool: a dead mount holds
/// that one probe and never the listing, and a busy pool is no answer. `None`: no answer.
async fn are_dirs(dirs: Vec<String>) -> Option<Vec<bool>> {
    if dirs.is_empty() {
        return Some(Vec::new());
    }
    crate::blocking::try_probe(PROBE, move || there(&dirs))
        .await
        .ok()
}

fn there(dirs: &[String]) -> Vec<bool> {
    dirs.iter().map(|d| Path::new(d).is_dir()).collect()
}

/// `dirs` as asked and as the disk names them: an agent records the real path, and a tab
/// reached through a link (`/tmp` for `/private/tmp`) would otherwise match nothing. No answer
/// in time leaves them as asked.
async fn spellings(dirs: Vec<String>) -> Vec<String> {
    if dirs.is_empty() {
        return dirs;
    }
    let asked = dirs.clone();
    crate::blocking::try_probe(PROBE, move || with_real(asked))
        .await
        .unwrap_or(dirs)
}

/// `dirs` and the real paths they name, each once.
fn with_real(dirs: Vec<String>) -> Vec<String> {
    let mut out = dirs.clone();
    for real in dirs.iter().filter_map(|d| std::fs::canonicalize(d).ok()) {
        let real = real.display().to_string();
        if !out.contains(&real) {
            out.push(real);
        }
    }
    out
}

/// `AgentSessions`: the newest sessions in `dirs`, within the range when one is given.
pub async fn list(
    id: String,
    sessions: &Sessions,
    dirs: Vec<String>,
    from_ms: Option<i64>,
    to_ms: Option<i64>,
    limit: u32,
) -> ControlResponse {
    let Some(index) = sessions.current() else {
        return ControlResponse::AgentSessions {
            id,
            reading: true,
            sessions: Vec::new(),
        };
    };
    let dirs = spellings(dirs).await;
    let mut rows: Vec<SessionRow> = {
        let all = index::latest(&index.summaries());
        index::sessions(&all, &dirs, from_ms, to_ms, limit.min(MOST) as usize)
            .into_iter()
            .map(|l| {
                let s = l.summary;
                SessionRow {
                    key: s.key.clone(),
                    agent: s.agent.as_str().to_string(),
                    id: s.id.clone(),
                    cwd: s.cwd.clone(),
                    title: s.title.clone(),
                    started_at: s.started_at,
                    updated_at: s.updated_at,
                    tokens: l.tokens,
                    prompts: l.prompts,
                    resume: resume_line(s.agent.as_str(), &s.id),
                    gone: false,
                }
            })
            .collect()
    };
    // Only the rows sent are looked at, each directory once. No answer is not "gone": the row
    // keeps its resume, and a start there says what is wrong.
    let mut cwds: Vec<String> = rows.iter().filter_map(|r| r.cwd.clone()).collect();
    cwds.sort();
    cwds.dedup();
    let there = are_dirs(cwds.clone()).await;
    for row in &mut rows {
        let Some(cwd) = &row.cwd else {
            row.resume = None;
            continue;
        };
        let gone = there
            .as_ref()
            .is_some_and(|there| cwds.binary_search(cwd).is_ok_and(|n| !there[n]));
        if gone {
            row.gone = true;
            row.resume = None;
        }
    }
    ControlResponse::AgentSessions {
        id,
        reading: false,
        sessions: rows,
    }
}

/// `AgentActivity`: the last `days` days in the asker's zone.
pub fn activity(id: String, sessions: &Sessions, days: u32, tz_offset_min: i32) -> ControlResponse {
    let Some(index) = sessions.current() else {
        return ControlResponse::AgentActivity {
            id,
            reading: true,
            days: Vec::new(),
        };
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    // A zone is within a day of UTC; anything else is not one.
    let tz = tz_offset_min.clamp(-24 * 60, 24 * 60);
    let all = index::latest(&index.summaries());
    ControlResponse::AgentActivity {
        id,
        reading: false,
        days: index::activity(&all, days.min(MOST_DAYS), tz, now),
    }
}

/// The line that brings a session back, as the agent's manifest words it (`claude --resume
/// <id>`, `opencode --standalone --session <id>`), each word quoted for the shell.
fn resume_line(program: &str, id: &str) -> Option<String> {
    qoop_detect::resume_argv(program, Some(id)).map(|argv| crate::daemon::shell_line(&argv))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resume_line_quotes_what_the_shell_would_read_as_more() {
        assert_eq!(
            resume_line("claude", "4f0ea2d4-8e4e-40e8-bfdf-06c66a11e60f").as_deref(),
            Some("claude --resume 4f0ea2d4-8e4e-40e8-bfdf-06c66a11e60f")
        );
        let odd = resume_line("codex", "a b;rm").unwrap();
        assert!(odd.ends_with("'a b;rm'"), "{odd}");
        assert_eq!(resume_line("zsh", "x"), None);
    }

    #[test]
    fn a_daemon_without_a_place_never_reads_and_says_it_is_reading() {
        let sessions = Sessions::default();
        assert!(sessions.current().is_none());
        assert!(!sessions.shared.refreshing.load(Ordering::Acquire));
    }

    // The pure halves: through the pool, a busy pool is no answer by design, which a test
    // binary full of other probes would hit.
    #[test]
    fn a_link_to_a_directory_matches_the_real_path_an_agent_records() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(dir.path(), &link).unwrap();
        let both = with_real(vec![link.display().to_string()]);
        let real = std::fs::canonicalize(dir.path()).unwrap();
        assert!(both.contains(&real.display().to_string()), "{both:?}");
        let asked = [link.display().to_string(), "/no/such/dir".to_string()];
        assert_eq!(there(&asked), [true, false]);
    }
}
