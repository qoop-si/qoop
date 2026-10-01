//! The store, seen from the runtime.
//!
//! `qoro-store` is synchronous and knows nothing about tokio, which is deliberate: a store
//! that spawns its own tasks is a store that can be called from the reactor by accident.
//! This is the other half of that decision. Every call goes to the blocking pool with a
//! deadline (ADR-011), and the deadline is what keeps a hung filesystem under the database
//! from becoming a hung daemon.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use qoro_store::{Store, StoreError, TerminalRow};

/// How long a command answer is kept before the ledger forgets it.
///
/// The same window the ledger had when it lived in the event log (ADR-017). A client's
/// retry is seconds behind the answer it missed, so this is about forensics on a reap
/// rather than about the retry, and a table that is never trimmed is the one row per
/// mutating command that would grow for as long as the machine is switched on.
pub const LEDGER_RETENTION_SECONDS: i64 = 30 * 24 * 60 * 60;

/// How long a metadata write may take before the caller stops waiting for it.
///
/// A create is acknowledged only when it is durable (ADR-005 rule 2), so this is how long a
/// client can be left waiting on one. Past it the answer is a refusal, which a client can
/// retry, and not silence.
pub const STORE_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum HandleError {
    #[error("the store did not answer within {STORE_DEADLINE:?}")]
    Timeout,
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Writes this process still owes the disk: every store call and every history writer.
/// Per process rather than per daemon, which is the same thing outside the tests. An exit
/// waits for them, because `process::exit` runs no destructors and a buffered history or a
/// close in flight would otherwise be lost with the process.
static OWED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// One write owed, for as long as it is held. Taken before the task that does the write
/// is spawned, so an exit can never look between the spawn and the task's first step.
pub(crate) struct Owed;

impl Owed {
    pub(crate) fn new() -> Self {
        OWED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Owed
    }
}

impl Drop for Owed {
    fn drop(&mut self) {
        OWED.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Wait, until `deadline`, for every owed write to land.
pub(crate) async fn settle_owed(deadline: tokio::time::Instant) {
    while OWED.load(std::sync::atomic::Ordering::SeqCst) > 0
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let left = OWED.load(std::sync::atomic::Ordering::SeqCst);
    if left > 0 {
        eprintln!("qorod: exiting with {left} writes to disk unfinished");
    }
}

#[derive(Clone)]
pub struct StoreHandle {
    inner: Arc<Mutex<Store>>,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl StoreHandle {
    pub fn new(store: Store) -> Self {
        Self {
            inner: Arc::new(Mutex::new(store)),
        }
    }

    async fn run<T, F>(&self, f: F) -> Result<T, HandleError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Store) -> Result<T, StoreError> + Send + 'static,
    {
        let inner = self.inner.clone();
        // Owed from here until the closure is done, whether or not this caller is still
        // waiting: a call past its deadline still runs, and an exit must wait for it.
        let owed = Owed::new();
        // On the store pool, not a raw blocking thread: a hung `qoro.db` then holds the
        // pool's threads and no more, where every call used to add one blocked thread.
        let work = crate::blocking::run_within(STORE_DEADLINE, move || {
            let _owed = owed;
            // A panic under the lock leaves SQLite consistent (an unfinished transaction
            // rolls back when its statement drops), so the lock is taken back rather than
            // failing every later call for the rest of the daemon's life.
            let mut guard = inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            f(&mut guard)
        });
        match work.await {
            Ok(result) => Ok(result?),
            // Out of time, or the job itself panicked. Reported as a timeout rather than
            // unwrapped, because taking the daemon down with it would turn one failed write
            // into every terminal ending.
            Err(_) => Err(HandleError::Timeout),
        }
    }

    /// The daemon's workspace, created on first use.
    ///
    /// One per daemon for now. It exists from the first terminal rather than from the first
    /// time somebody asks for one, because a terminal with no workspace has nowhere to
    /// record its layout and the foreign key would refuse it.
    pub async fn open_workspace(&self) -> Result<i64, HandleError> {
        self.run(move |s| {
            if let Some(first) = s.workspaces()?.first() {
                return Ok(first.id);
            }
            s.create_workspace("default", now())
        })
        .await
    }

    /// Every workspace this daemon holds.
    pub async fn workspaces(&self) -> Result<Vec<qoro_store::WorkspaceRow>, HandleError> {
        self.run(move |s| s.workspaces()).await
    }

    pub async fn create_workspace(&self, name: String) -> Result<i64, HandleError> {
        self.run(move |s| s.create_workspace(&name, now())).await
    }

    pub async fn rename_workspace(&self, id: i64, name: String) -> Result<usize, HandleError> {
        self.run(move |s| s.rename_workspace(id, &name)).await
    }

    pub async fn delete_workspace(&self, id: i64) -> Result<usize, HandleError> {
        self.run(move |s| s.delete_workspace(id)).await
    }

    pub async fn create_terminal(
        &self,
        id: i64,
        workspace: i64,
        cwd: String,
        argv: String,
    ) -> Result<i64, HandleError> {
        self.run(move |s| s.create_terminal(id, workspace, &cwd, &argv, now()))
            .await
    }

    /// Record a terminal and the answer to the command that asked for it, in one
    /// transaction.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_terminal_answering(
        &self,
        key: String,
        id: i64,
        workspace: i64,
        cwd: String,
        argv: String,
        title: Option<String>,
        abandoned: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<i64, HandleError> {
        self.run(move |s| {
            s.create_terminal_answering(
                Some(&key),
                id,
                workspace,
                &cwd,
                &argv,
                title.as_deref(),
                Some(&abandoned),
                now(),
            )
        })
        .await
    }

    /// The terminals asleep, as `(terminal, workspace, since)`, the latest first.
    pub async fn asleep(&self) -> Result<Vec<(i64, i64, i64)>, HandleError> {
        self.run(|s| s.asleep()).await
    }

    /// Put terminals to sleep with their agents written down; one transaction.
    pub async fn sleep_terminals(
        &self,
        checkpoints: Vec<qoro_store::Checkpoint>,
    ) -> Result<(), HandleError> {
        self.run(move |s| s.sleep_terminals(now(), &checkpoints))
            .await
    }

    /// Take the sleep marks off terminals. Answers how many had one.
    pub async fn unmark(&self, terminals: Vec<i64>) -> Result<usize, HandleError> {
        self.run(move |s| s.unmark(&terminals)).await
    }

    /// Write the terminals' agents down as a sleep does, marking nothing asleep.
    pub async fn checkpoint(
        &self,
        checkpoints: Vec<qoro_store::Checkpoint>,
    ) -> Result<(), HandleError> {
        self.run(move |s| s.checkpoint(&checkpoints)).await
    }

    /// Record an agent's own id for the conversation in a terminal.
    pub async fn set_agent_session(&self, id: i64, session: String) -> Result<(), HandleError> {
        self.run(move |s| s.set_agent_session(id, &session)).await
    }

    /// Forget a conversation id whose agent is gone, if the row still holds it.
    pub async fn forget_agent_session(&self, id: i64, session: String) -> Result<(), HandleError> {
        self.run(move |s| s.forget_agent_session(id, &session))
            .await
    }

    /// Record where a terminal actually is, so a restart opens it there.
    pub async fn set_cwd(&self, id: i64, cwd: String) -> Result<(), HandleError> {
        self.run(move |s| s.set_cwd(id, &cwd)).await
    }

    /// Record which agent a terminal is running, and its own id for the conversation.
    pub async fn set_agent(
        &self,
        id: i64,
        agent: Option<String>,
        agent_session: Option<String>,
    ) -> Result<(), HandleError> {
        self.run(move |s| s.set_agent(id, agent.as_deref(), agent_session.as_deref()))
            .await
    }

    /// Give a terminal the name a person typed, so a restart brings the name back too.
    pub async fn set_title(&self, id: i64, title: String) -> Result<(), HandleError> {
        self.run(move |s| s.set_title(id, &title)).await
    }

    /// Every conversation a person named, as `(session, title)`.
    pub async fn session_titles(&self) -> Result<Vec<(String, String)>, HandleError> {
        self.run(|s| s.session_titles()).await
    }

    /// Name a conversation, or with `None` give it back its agent's own title.
    pub async fn set_session_title(
        &self,
        session: String,
        title: Option<String>,
    ) -> Result<(), HandleError> {
        self.run(move |s| s.set_session_title(&session, title.as_deref()))
            .await
    }

    /// Move an open terminal to another workspace. Answers whether a row moved.
    pub async fn move_terminal(&self, id: i64, workspace: i64) -> Result<bool, HandleError> {
        self.run(move |s| s.move_terminal(id, workspace)).await
    }

    /// Forget a terminal's name, so a cleared tab comes back cleared too.
    pub async fn clear_title(&self, id: i64) -> Result<(), HandleError> {
        self.run(move |s| s.clear_title(id)).await
    }

    pub async fn close_terminal(&self, id: i64) -> Result<bool, HandleError> {
        self.run(move |s| s.close_terminal(id, now())).await
    }

    pub async fn max_terminal_id(&self) -> Result<i64, HandleError> {
        self.run(|s| s.max_terminal_id()).await
    }

    pub async fn open_terminals(&self, workspace: i64) -> Result<Vec<TerminalRow>, HandleError> {
        self.run(move |s| s.open_terminals(workspace)).await
    }

    /// Every stored split tree, as the JSON it was written as.
    pub async fn splits(&self) -> Result<Vec<String>, HandleError> {
        self.run(move |s| s.splits()).await
    }

    /// Replace every stored split tree in one transaction.
    pub async fn set_splits(&self, trees: Vec<String>) -> Result<(), HandleError> {
        self.run(move |s| s.set_splits(&trees)).await
    }

    /// Write down what a command with this key produced, so a retry of it returns the same
    /// answer instead of doing the work again.
    pub async fn record_applied(
        &self,
        key: String,
        ok: bool,
        detail: serde_json::Value,
    ) -> Result<(), HandleError> {
        self.run(move |s| s.record_applied(&key, ok, &detail, now()))
            .await
    }

    /// What a command with this key already produced, if it has been applied.
    pub async fn applied(
        &self,
        key: String,
    ) -> Result<Option<(bool, serde_json::Value)>, HandleError> {
        self.run(move |s| s.applied(&key)).await
    }

    /// Undo a create whose shell never started: its row and its answer, in one transaction.
    pub async fn abandon_create(&self, key: String, id: i64) -> Result<(), HandleError> {
        self.run(move |s| s.abandon_create(&key, id, now())).await
    }

    /// Forget what a retry can no longer be waiting for.
    ///
    /// Once per start, on the store pool like every other write.
    pub async fn prune_applied(&self) -> Result<usize, HandleError> {
        self.run(move |s| s.prune_applied(now() - LEDGER_RETENTION_SECONDS))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_create_is_durable_before_it_is_acknowledged() {
        let handle = StoreHandle::new(Store::in_memory().expect("open"));
        let ws = handle
            .create_workspace("w".into())
            .await
            .expect("workspace");
        let t = handle
            .create_terminal(1, ws, "/tmp".into(), "/bin/sh".into())
            .await
            .expect("terminal");
        // Read back through a separate call, which is what "durable" has to mean from the
        // outside: the row is there for the next reader, not just for the writer.
        let open = handle.open_terminals(ws).await.expect("list");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, t);
    }

    /// One panic under the lock must not take the store away for the rest of the daemon's
    /// life: the next call works, rather than timing out forever with a message about disks.
    #[tokio::test]
    async fn a_panic_under_the_lock_does_not_poison_the_store_for_good() {
        let handle = StoreHandle::new(Store::in_memory().expect("open"));
        let panicked = handle
            .run(|_| -> Result<(), StoreError> { panic!("a bug under the lock") })
            .await;
        assert!(panicked.is_err());
        handle
            .create_workspace("w".into())
            .await
            .expect("the store works again after a panic");
    }

    #[tokio::test]
    async fn a_terminal_in_a_missing_workspace_is_refused_rather_than_silently_dropped() {
        let handle = StoreHandle::new(Store::in_memory().expect("open"));
        let err = handle
            .create_terminal(1, 404, "/tmp".into(), "/bin/sh".into())
            .await
            .expect_err("a missing workspace is not a create");
        assert!(matches!(
            err,
            HandleError::Store(StoreError::NoWorkspace(404))
        ));
    }
}
