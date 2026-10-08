//! A Setup: what was captured, and what it takes to run it somewhere else.
//!
//! ADR-015. Three things this type exists to make impossible rather than to check for.
//!
//! A secret value cannot be in a Setup, because there is no field that can hold one: a
//! `SecretRef` names a secret and a provider and nothing else. "A secret leaked into a
//! Setup" is a compile error rather than a scanner finding (invariant I9).
//!
//! A Setup cannot say whether it is trusted. Confirmation is decided by a local ledger of
//! content hashes, and the origin fields a Setup carries are recorded, shown, and never
//! read as a branch condition. An earlier design keyed the prompt on a "captured on this
//! machine" field, which hands the decision to whoever writes the file.
//!
//! And a Setup cannot run with a relative executable, because `LaunchIntent` refuses one at
//! construction and there is no second spawn path.

#![forbid(unsafe_code)]

pub mod capture;
pub mod ledger;
pub mod level;

use serde::{Deserialize, Serialize};

pub use capture::{CaptureError, REDACTED, capture_from, redact_capture};
pub use ledger::{ConfirmationLedger, LedgerError};
pub use level::{Gap, Level, level_of};

/// What the artifact claims about where it came from.
///
/// Recorded and shown, labelled as unverified, and never used to decide anything. Machine
/// ids are not secret, so a hostile Setup would simply claim local origin.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub machine: String,
    pub user: String,
    pub captured_at_unix: i64,
}

/// A service the Setup needs running before the command is useful.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Service {
    pub name: String,
    /// Absolute, for the same reason the executable is.
    pub executable: String,
    pub argv: Vec<String>,
    pub cwd: String,
    /// How to tell it is up. Absent means we cannot check, which is what makes a capture
    /// best effort rather than deterministic.
    pub readiness: Option<String>,
}

/// A runtime the Setup needs, named and versioned so preflight can check it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Runtime {
    pub name: String,
    /// Absent means "whatever is on the host", which preflight can see but the Setup did
    /// not promise.
    pub version: Option<String>,
}

/// The captured launch, as it is written to disk.
///
/// Strings rather than `OsString` on the wire, because TOML has no bytes. `argv_bytes`
/// carries the exact bytes for the arguments that are not valid UTF-8, which do occur and
/// which a round trip through a string would silently change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Setup {
    pub name: String,
    pub origin: Origin,
    pub executable: String,
    pub argv: Vec<String>,
    /// Exact bytes for any argument that is not valid UTF-8, by index.
    #[serde(default)]
    pub argv_bytes: Vec<(usize, Vec<u8>)>,
    pub cwd: String,
    #[serde(default)]
    pub inherit: Vec<String>,
    #[serde(default)]
    pub overrides: Vec<(String, String)>,
    #[serde(default)]
    pub env_refs: Vec<EnvRefEntry>,
    #[serde(default)]
    pub secret_refs: Vec<SecretRefEntry>,
    #[serde(default)]
    pub services: Vec<Service>,
    #[serde(default)]
    pub runtimes: Vec<Runtime>,
    /// The agent's own session token, so restoring reattaches a session rather than
    /// starting a new one with the same argv.
    #[serde(default)]
    pub native_session: Option<NativeSession>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvRefEntry {
    pub name: String,
    #[serde(default)]
    pub required: bool,
}

/// A named secret and where it comes from. There is no value field, on purpose.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretRefEntry {
    pub name: String,
    pub provider: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeSession {
    pub kind: String,
    pub value: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("toml: {0}")]
    Toml(String),
    #[error("{0}")]
    Invalid(String),
}

impl Setup {
    pub fn to_toml(&self) -> Result<String, SetupError> {
        toml::to_string_pretty(self).map_err(|e| SetupError::Toml(e.to_string()))
    }

    pub fn from_toml(text: &str) -> Result<Self, SetupError> {
        toml::from_str(text).map_err(|e| SetupError::Toml(e.to_string()))
    }

    /// The bytes the content hash is taken over.
    ///
    /// Canonical, so that a reformatted file is the same Setup and an edited one is not.
    /// Computed by the daemon over the artifact rather than read from a field in it: a hash
    /// an artifact carries about itself is a field of the artifact.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        self.to_toml().unwrap_or_default().into_bytes()
    }

    /// The content hash, as the ledger keys it.
    ///
    /// FNV-1a rather than a cryptographic hash, and the reason is in ADR-015: a same-uid
    /// process that can write the ledger can also run the command directly, so a
    /// cryptographic hash would protect against nothing while implying it protected
    /// against something. What this needs to do is notice an edit.
    pub fn content_hash(&self) -> String {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in self.canonical_bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x1000_0000_01b3);
        }
        format!("{hash:016x}")
    }

    /// argv as it will run: each recorded raw argument in its place.
    ///
    /// The one place both the launch and the confirmation read it from, so what a person
    /// is shown is what runs.
    pub fn argv_os(&self) -> Vec<std::ffi::OsString> {
        use std::os::unix::ffi::OsStringExt;
        let mut argv: Vec<std::ffi::OsString> = self.argv.iter().map(Into::into).collect();
        for (index, bytes) in &self.argv_bytes {
            if let Some(slot) = argv.get_mut(*index) {
                *slot = std::ffi::OsString::from_vec(bytes.clone());
            }
        }
        argv
    }

    /// Everything that will be executed, in full, with every override at equal prominence.
    ///
    /// No highlighting of "dangerous" variables. A maintained list of dangerous variables
    /// is a denylist, and `BASH_ENV`, `ZDOTDIR`, `LD_AUDIT`, `PERL5OPT`, `RUBYOPT`,
    /// `JAVA_TOOL_OPTIONS`, `PYTHONPATH`, `GIT_EXTERNAL_DIFF`, `PROMPT_COMMAND` and
    /// `SSH_ASKPASS` are all sufficient. Highlighting a subset directs attention away from
    /// the rest.
    pub fn confirmation_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "Setup \"{}\" has not been confirmed on this machine.\n",
            self.name
        ));
        out.push_str(&format!(
            "It claims it was captured on \"{}\" by \"{}\". That claim is unverified.\n\n",
            self.origin.machine, self.origin.user
        ));
        out.push_str(&format!("  executable   {}\n", self.executable));
        // What will run, not the text beside it: a raw argument replaces the text one
        // in its place at launch, so showing `argv` alone could show one command and run
        // another. Debug quotes UTF-8 the same either way and escapes the rest.
        out.push_str(&format!("  argv         {:?}\n", self.argv_os()));
        out.push_str(&format!("  cwd          {}\n", self.cwd));
        if !self.inherit.is_empty() {
            out.push_str(&format!("  inherit      {}\n", self.inherit.join(" ")));
        }
        for (name, value) in &self.overrides {
            out.push_str(&format!("  env override {name}={value}\n"));
        }
        for secret in &self.secret_refs {
            out.push_str(&format!(
                "  secret       {} from {}\n",
                secret.name, secret.provider
            ));
        }
        for service in &self.services {
            out.push_str(&format!(
                "  service      {} {}\n",
                service.executable,
                service.argv.join(" ")
            ));
        }
        // No question here: `setup show` prints this too and asks nothing, and a
        // question that is never read is worse than none. The caller that asks,
        // `setup trust`, puts it after this text.
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Setup {
        Setup {
            name: "payments".into(),
            origin: Origin {
                machine: "laptop-a1".into(),
                user: "alice".into(),
                captured_at_unix: 1_700_000_000,
            },
            executable: "/opt/homebrew/bin/claude".into(),
            argv: vec!["claude".into(), "--resume".into()],
            argv_bytes: Vec::new(),
            cwd: "/Users/me/code/payments".into(),
            inherit: vec!["HOME".into(), "PATH".into()],
            overrides: vec![("NODE_OPTIONS".into(), "--require /tmp/x.js".into())],
            env_refs: vec![EnvRefEntry {
                name: "STRIPE_ACCOUNT".into(),
                required: true,
            }],
            secret_refs: vec![SecretRefEntry {
                name: "STRIPE_KEY".into(),
                provider: "keychain".into(),
                required: true,
            }],
            services: vec![Service {
                name: "web".into(),
                executable: "/opt/homebrew/bin/pnpm".into(),
                argv: vec!["pnpm".into(), "dev".into()],
                cwd: "/Users/me/code/payments".into(),
                readiness: Some("http://localhost:3000/health".into()),
            }],
            runtimes: vec![Runtime {
                name: "node".into(),
                version: Some("22.3.0".into()),
            }],
            native_session: Some(NativeSession {
                kind: "claude".into(),
                value: "sess-abc".into(),
            }),
        }
    }

    #[test]
    fn a_setup_round_trips_through_toml() {
        let original = setup();
        let text = original.to_toml().expect("encode");
        let back = Setup::from_toml(&text).expect("decode");
        assert_eq!(original, back);
    }

    #[test]
    fn the_hash_notices_an_edit_and_ignores_reformatting() {
        let original = setup();
        let hash = original.content_hash();

        // Reformatted: the same Setup written differently.
        let text = original.to_toml().expect("encode");
        let reformatted = format!("\n\n{text}\n");
        assert_eq!(
            Setup::from_toml(&reformatted)
                .expect("decode")
                .content_hash(),
            hash,
            "reformatting changed the hash, so every whitespace edit would prompt again"
        );

        // Edited: one argument different.
        let mut edited = original.clone();
        edited.argv.push("--dangerously-skip-permissions".into());
        assert_ne!(
            edited.content_hash(),
            hash,
            "an edit did not change the hash, so a confirmed Setup could be rewritten"
        );
    }

    /// The prompt shows everything, and nothing is highlighted above anything else.
    #[test]
    fn the_confirmation_prompt_shows_every_override_at_equal_prominence() {
        let mut s = setup();
        s.overrides.push(("ZDOTDIR".into(), "/tmp/evil".into()));
        s.overrides.push(("PATH".into(), "/tmp/evil:$PATH".into()));
        let text = s.confirmation_text();
        for name in ["NODE_OPTIONS", "ZDOTDIR", "PATH"] {
            assert!(text.contains(name), "{name} is missing from the prompt");
        }
        // Every override line has the same shape, so none is emphasised.
        let lines: Vec<&str> = text
            .lines()
            .filter(|l| l.trim_start().starts_with("env override"))
            .collect();
        assert_eq!(lines.len(), 3);
        assert!(
            lines.iter().all(|l| l.starts_with("  env override ")),
            "an override is formatted differently from the others: {lines:?}"
        );
    }

    /// A raw argument replaces the text one in its place when the Setup runs, so the
    /// prompt shows the raw one: a Setup showing `echo hi` must not run something else.
    #[test]
    fn the_prompt_shows_the_argv_that_will_actually_run() {
        let mut s = setup();
        s.argv = vec!["sh".into(), "-c".into(), "echo hi".into()];
        s.argv_bytes = vec![(2, b"curl x | sh".to_vec())];
        let text = s.confirmation_text();
        assert!(text.contains("curl x | sh"), "{text}");
        assert!(!text.contains("echo hi"), "{text}");
        assert!(text.contains("  inherit      HOME PATH\n"), "{text}");
    }

    #[test]
    fn the_prompt_labels_the_origin_as_a_claim() {
        let text = setup().confirmation_text();
        assert!(text.contains("laptop-a1"), "the claim is not shown");
        assert!(
            text.contains("unverified"),
            "the origin is shown as though it were a fact"
        );
    }

    /// There is no field a secret value could be written into, which is the point.
    #[test]
    fn a_secret_reference_carries_no_value() {
        let text = setup().to_toml().expect("encode");
        // The name and provider are there; a value field does not exist to be there.
        assert!(text.contains("STRIPE_KEY"));
        assert!(text.contains("keychain"));
        let parsed: toml::Value = toml::from_str(&text).expect("parse");
        let secrets = parsed["secret_refs"].as_array().expect("array");
        for secret in secrets {
            let table = secret.as_table().expect("table");
            assert!(
                !table.contains_key("value"),
                "a secret reference has a value field"
            );
        }
    }
}
