//! The local confirmation ledger.
//!
//! ADR-015. A Setup whose content hash is not in here requires explicit confirmation before
//! it runs, and the decision never reads the artifact.
//!
//! Its integrity requirement is one directional and therefore cheap: **absence fails
//! closed**. A deleted, corrupted or unreadable ledger means nothing is trusted and every
//! Setup prompts again, which is annoying and safe. There is no signature and no tamper
//! detection, because a same-uid process that can write the ledger can also run the command
//! directly (ADR-012), so a signature would protect against nothing while implying it
//! protected against something.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// What has been confirmed on this machine.
pub struct ConfirmationLedger {
    path: PathBuf,
    hashes: BTreeSet<String>,
}

impl ConfirmationLedger {
    /// Open, treating anything unreadable as empty.
    ///
    /// Failing closed is the whole design. A corrupted ledger that returned an error would
    /// have to be handled by every caller, and the handling that is correct in every case
    /// is "trust nothing", so it happens here once.
    pub fn open(path: &Path) -> Self {
        let hashes = std::fs::read_to_string(path)
            .map(|text| {
                text.lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !l.starts_with('#'))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            path: path.to_path_buf(),
            hashes,
        }
    }

    pub fn is_confirmed(&self, content_hash: &str) -> bool {
        self.hashes.contains(content_hash)
    }

    /// Record a confirmation. Written whole, so a partial write leaves the previous file.
    pub fn confirm(&mut self, content_hash: &str) -> Result<(), LedgerError> {
        self.hashes.insert(content_hash.to_string());
        let mut text = String::from("# Setups confirmed on this machine, by content hash.\n");
        for hash in &self.hashes {
            text.push_str(hash);
            text.push('\n');
        }
        let temp = self.path.with_extension("tmp");
        std::fs::write(&temp, text)?;
        std::fs::rename(&temp, &self.path)?;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.hashes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hashes.is_empty()
    }
}

/// Why a Setup was not run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Not confirmed, and there is somebody to ask.
    NeedsConfirmation { prompt: String },
    /// Not confirmed, and there is nobody to ask.
    ///
    /// A typed error rather than running: a daemon that ran unconfirmed Setups whenever no
    /// client was attached would make "unattended" the way to bypass the prompt.
    NeedsConfirmationNoClient { name: String },
}

/// Whether this Setup may run now.
pub fn admit(
    setup: &crate::Setup,
    ledger: &ConfirmationLedger,
    interactive_client: bool,
) -> Result<(), Refusal> {
    if ledger.is_confirmed(&setup.content_hash()) {
        return Ok(());
    }
    if !interactive_client {
        return Err(Refusal::NeedsConfirmationNoClient {
            name: setup.name.clone(),
        });
    }
    Err(Refusal::NeedsConfirmation {
        prompt: setup.confirmation_text(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Origin, Setup};

    fn setup(name: &str) -> Setup {
        Setup {
            name: name.into(),
            origin: Origin {
                machine: "laptop-a1".into(),
                user: "alice".into(),
                captured_at_unix: 1,
            },
            executable: "/bin/echo".into(),
            argv: vec!["echo".into(), "hi".into()],
            argv_bytes: Vec::new(),
            cwd: "/tmp".into(),
            inherit: Vec::new(),
            overrides: Vec::new(),
            env_refs: Vec::new(),
            secret_refs: Vec::new(),
            services: Vec::new(),
            runtimes: Vec::new(),
            native_session: None,
        }
    }

    #[test]
    fn an_unconfirmed_setup_is_refused_with_the_prompt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ledger = ConfirmationLedger::open(&dir.path().join("confirmed"));
        let s = setup("a");
        match admit(&s, &ledger, true) {
            Err(Refusal::NeedsConfirmation { prompt }) => {
                assert!(prompt.contains("/bin/echo"));
                assert!(prompt.contains("unverified"));
            }
            other => panic!("an unconfirmed Setup was admitted: {other:?}"),
        }
    }

    #[test]
    fn a_confirmed_setup_runs_and_an_edited_one_does_not() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("confirmed");
        let mut ledger = ConfirmationLedger::open(&path);
        let s = setup("a");
        ledger.confirm(&s.content_hash()).expect("confirm");
        assert_eq!(admit(&s, &ledger, true), Ok(()));

        let mut edited = s.clone();
        edited.argv.push("--dangerously-skip-permissions".into());
        assert!(
            admit(&edited, &ledger, true).is_err(),
            "editing a confirmed Setup did not invalidate the confirmation"
        );
    }

    /// The decision does not read the artifact. A Setup claiming local origin is refused
    /// exactly like any other.
    #[test]
    fn a_setup_claiming_it_came_from_this_machine_is_still_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ledger = ConfirmationLedger::open(&dir.path().join("confirmed"));
        let mut s = setup("hostile");
        s.origin.machine = hostname_like();
        s.origin.user = std::env::var("USER").unwrap_or_default();
        assert!(
            admit(&s, &ledger, true).is_err(),
            "a Setup that claimed local origin ran without confirmation"
        );
    }

    fn hostname_like() -> String {
        std::process::Command::new("hostname")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "localhost".into())
    }

    /// With nobody to ask, the answer is a typed error and not a run.
    #[test]
    fn an_unconfirmed_setup_with_no_client_fails_rather_than_running() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ledger = ConfirmationLedger::open(&dir.path().join("confirmed"));
        assert_eq!(
            admit(&setup("a"), &ledger, false),
            Err(Refusal::NeedsConfirmationNoClient { name: "a".into() })
        );
    }

    /// Absence fails closed, and so does corruption.
    #[test]
    fn a_missing_or_corrupt_ledger_trusts_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("confirmed");
        let s = setup("a");

        assert!(ConfirmationLedger::open(&path).is_empty());

        let mut ledger = ConfirmationLedger::open(&path);
        ledger.confirm(&s.content_hash()).expect("confirm");
        assert!(ConfirmationLedger::open(&path).is_confirmed(&s.content_hash()));

        // Unreadable, as a permissions problem or a truncated disk would leave it.
        std::fs::write(&path, [0xff, 0xfe, 0xfd]).expect("corrupt");
        let reopened = ConfirmationLedger::open(&path);
        assert!(
            !reopened.is_confirmed(&s.content_hash()),
            "a corrupt ledger still trusted something"
        );

        std::fs::remove_file(&path).expect("remove");
        assert!(!ConfirmationLedger::open(&path).is_confirmed(&s.content_hash()));
    }

    #[test]
    fn a_confirmation_survives_reopening() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("confirmed");
        let a = setup("a");
        let b = setup("b");
        {
            let mut ledger = ConfirmationLedger::open(&path);
            ledger.confirm(&a.content_hash()).expect("confirm");
            ledger.confirm(&b.content_hash()).expect("confirm");
        }
        let ledger = ConfirmationLedger::open(&path);
        assert_eq!(ledger.len(), 2);
        assert!(ledger.is_confirmed(&a.content_hash()));
        assert!(ledger.is_confirmed(&b.content_hash()));
    }
}
