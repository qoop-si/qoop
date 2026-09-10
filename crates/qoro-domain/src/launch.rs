//! Launch intent: the only way to start a process anywhere in the codebase.
//!
//! Invariant I7. Restore is correct by construction because there is no second spawn path
//! that could forget argv or environment. Persisting argv and
//! displaying it without ever using it is the worst of both: the data model promises
//! "your command will come back" and silently does not.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// A reference to an environment variable, never its value.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnvRef {
    pub name: String,
    pub required: bool,
}

/// A reference to a secret, never its value.
///
/// Invariant I9. There is deliberately no variant of this type that can hold a value, so
/// "a secret leaked into a Setup" is a compile error rather than a scanner finding.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SecretRef {
    pub name: String,
    pub provider: String,
    pub required: bool,
}

/// A native agent session reference, for example the token a coding agent CLI accepts on
/// its own resume flag.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NativeSessionRef {
    pub kind: String,
    pub value: String,
}

/// What the child's environment is built from.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnvPlan {
    /// Names inherited from the daemon's environment, as an allowlist rather than a
    /// denylist: a new dangerous variable added by some future toolchain is excluded by
    /// default rather than needing to be remembered.
    pub inherit: Vec<String>,
    /// Explicit non-secret overrides.
    pub overrides: Vec<(String, String)>,
    /// Referenced variables that must exist on the target, checked by preflight.
    pub env_refs: Vec<EnvRef>,
    /// Referenced secrets, resolved only into the `execve` call.
    pub secret_refs: Vec<SecretRef>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LaunchIntentError {
    #[error("executable must be an absolute path, got {0:?}")]
    RelativeExecutable(PathBuf),
    #[error("cwd must be an absolute path, got {0:?}")]
    RelativeCwd(PathBuf),
    #[error("argv must not be empty")]
    EmptyArgv,
}

/// The only spawn description in the system.
///
/// `argv` is `Vec<OsString>` and not `Vec<String>` on purpose: argv on Unix is bytes, not
/// UTF-8, and a `String` here would silently mangle a legitimate command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchIntent {
    executable: PathBuf,
    argv: Vec<OsString>,
    cwd: PathBuf,
    pub env: EnvPlan,
    /// The reference passed at spawn. Empty for the first launch of an agent, which is the
    /// common case. Restore reads the agent's observed reference first and falls back to
    /// this one.
    pub native_session_ref: Option<NativeSessionRef>,
}

impl LaunchIntent {
    /// The only constructor. It rejects a relative executable, which is what stops a Setup
    /// from resolving to a different binary on every machine (ADR-015), and it rejects a
    /// relative cwd, which would resolve against whatever the daemon's cwd happens to be.
    pub fn new(
        executable: impl Into<PathBuf>,
        argv: Vec<OsString>,
        cwd: impl Into<PathBuf>,
    ) -> Result<Self, LaunchIntentError> {
        let executable = executable.into();
        let cwd = cwd.into();
        if !executable.is_absolute() {
            return Err(LaunchIntentError::RelativeExecutable(executable));
        }
        if !cwd.is_absolute() {
            return Err(LaunchIntentError::RelativeCwd(cwd));
        }
        if argv.is_empty() {
            return Err(LaunchIntentError::EmptyArgv);
        }
        Ok(Self {
            executable,
            argv,
            cwd,
            env: EnvPlan::default(),
            native_session_ref: None,
        })
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }
    pub fn argv(&self) -> &[OsString] {
        &self.argv
    }
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent() -> LaunchIntent {
        LaunchIntent::new(
            "/bin/sh",
            vec![OsString::from("sh"), OsString::from("-i")],
            "/tmp",
        )
        .unwrap()
    }

    #[test]
    fn a_relative_executable_is_refused() {
        let err = LaunchIntent::new("claude", vec![OsString::from("claude")], "/tmp").unwrap_err();
        assert!(matches!(err, LaunchIntentError::RelativeExecutable(_)));
    }

    #[test]
    fn a_relative_cwd_is_refused() {
        let err = LaunchIntent::new("/bin/sh", vec![OsString::from("sh")], "relative").unwrap_err();
        assert!(matches!(err, LaunchIntentError::RelativeCwd(_)));
    }

    #[test]
    fn empty_argv_is_refused() {
        assert_eq!(
            LaunchIntent::new("/bin/sh", vec![], "/tmp").unwrap_err(),
            LaunchIntentError::EmptyArgv
        );
    }

    #[test]
    fn argv_carries_bytes_that_are_not_utf8() {
        // A `Vec<String>` cannot hold this, and mangling it would change what runs.
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let weird = OsString::from_vec(vec![0x66, 0x6f, 0xff, 0x6f]);
            let i = LaunchIntent::new("/bin/sh", vec![OsString::from("sh"), weird.clone()], "/tmp")
                .unwrap();
            assert_eq!(i.argv()[1], weird);
        }
    }

    #[test]
    fn a_secret_ref_cannot_carry_a_value() {
        // There is no field to put one in. This test documents that the absence is
        // deliberate, so a future field addition has to argue with it.
        let s = SecretRef {
            name: "STRIPE_KEY".into(),
            provider: "environment".into(),
            required: true,
        };
        let json = format!("{s:?}");
        assert!(!json.contains("value"));
    }

    #[test]
    fn env_plan_starts_empty_rather_than_inheriting_everything() {
        // An allowlist that defaults to empty is the only version of this that is safe by
        // default: a denylist has to be remembered, and the whole environment is the
        // failure mode ADR-015 is about.
        assert!(intent().env.inherit.is_empty());
    }
}
