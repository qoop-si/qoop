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
        // On the store pool, not a raw blocking thread: a hung `qoro.db` then holds the
        // pool's threads and no more, where every call used to add one blocked thread.
        let work = crate::blocking::run_within(STORE_DEADLINE, move || {
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

    /// Record an agent's own id for the conversation in a terminal.
    pub async fn set_agent_session(&self, id: i64, session: String) -> Result<(), HandleError> {
        self.run(move |s| s.set_agent_session(id, &session)).await
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
