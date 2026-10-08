//! How reproducible a capture is, computed from what it declares.
//!
//! The definitions are in the master plan at gate G5.3 and repeated here, because a gate
//! cannot assert against a definition it does not carry:
//!
//! - **Level A, deterministic.** Every executable, runtime, service and env reference the
//!   Setup needs is declared, and preflight can verify each one on the target.
//! - **Level B, best effort.** At least one dependency is assumed present on the host and
//!   is not declared, so preflight can check it but the Setup did not promise it.
//! - **Level C, local snapshot.** At least one required reference could not be resolved to
//!   anything portable, so the Setup is useful only on a machine like the one it was
//!   captured on until the person fills the gap.
//!
//! The level is computed, never declared. A field would be a claim by the artifact, and
//! ADR-015's whole subject is not believing claims by the artifact.

use serde::{Deserialize, Serialize};

use crate::Setup;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Level {
    /// Useful only on a machine like the one it came from.
    LocalSnapshot,
    /// Runnable elsewhere, with something assumed.
    BestEffort,
    /// Everything it needs is declared and checkable.
    Deterministic,
}

/// Why a capture is not Level A. Named, so the interface can tell somebody what to fill in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Gap {
    /// A runtime is needed and no version was captured.
    RuntimeVersionUnknown { runtime: String },
    /// A service has no way to tell whether it is up.
    ServiceReadinessUnknown { service: String },
    /// A path that only exists on the machine it was captured on.
    UnportablePath { what: String, path: String },
    /// A required env reference with no declared provider or default.
    UnresolvedEnvRef { name: String },
    /// A required secret with no provider.
    SecretWithoutProvider { name: String },
}

impl Gap {
    /// Whether this gap makes the Setup useless elsewhere, or merely unpromised.
    ///
    /// The distinction is the difference between Level B and Level C: something assumed
    /// present can be checked and installed, and something unresolvable cannot be acted on
    /// at all until a person supplies it.
    pub fn is_blocking(&self) -> bool {
        matches!(
            self,
            Gap::UnportablePath { .. }
                | Gap::UnresolvedEnvRef { .. }
                | Gap::SecretWithoutProvider { .. }
        )
    }
}

/// Paths that exist only on the machine that captured them.
///
/// A prefix list, and therefore a heuristic. It is deliberately conservative in the
/// direction that costs least: calling a portable path unportable produces a Level C
/// capture and a person who fills in something that was already fine, while the other
/// mistake produces a Setup that fails on the target with no warning.
fn is_unportable(path: &str) -> bool {
    // Both spellings of each: capture keeps the cwd as the shell gave it, and a shell in
    // `$TMPDIR` says `/var/folders/...` while a resolved `/tmp` says `/private/tmp/...`.
    const MACHINE_LOCAL: &[&str] = &[
        "/private/var/folders/",
        "/var/folders/",
        "/tmp/",
        "/private/tmp/",
        "/var/tmp/",
        "/private/var/tmp/",
        "/run/user/",
        "/Volumes/",
        "/media/",
        "/mnt/",
    ];
    MACHINE_LOCAL.iter().any(|p| path.starts_with(p))
}

/// The level, and everything that stops it being higher.
pub fn level_of(setup: &Setup) -> (Level, Vec<Gap>) {
    let mut gaps = Vec::new();

    if is_unportable(&setup.executable) {
        gaps.push(Gap::UnportablePath {
            what: "executable".into(),
            path: setup.executable.clone(),
        });
    }
    if is_unportable(&setup.cwd) {
        gaps.push(Gap::UnportablePath {
            what: "cwd".into(),
            path: setup.cwd.clone(),
        });
    }
    for runtime in &setup.runtimes {
        if runtime.version.is_none() {
            gaps.push(Gap::RuntimeVersionUnknown {
                runtime: runtime.name.clone(),
            });
        }
    }
    for service in &setup.services {
        if service.readiness.is_none() {
            gaps.push(Gap::ServiceReadinessUnknown {
                service: service.name.clone(),
            });
        }
        if is_unportable(&service.cwd) {
            gaps.push(Gap::UnportablePath {
                what: format!("service {}", service.name),
                path: service.cwd.clone(),
            });
        }
    }
    for env in &setup.env_refs {
        // A required reference that nothing declares how to obtain. Not required means the
        // Setup runs without it, which is a decision the capture made rather than a gap.
        if env.required && !setup.overrides.iter().any(|(n, _)| n == &env.name) {
            gaps.push(Gap::UnresolvedEnvRef {
                name: env.name.clone(),
            });
        }
    }
    for secret in &setup.secret_refs {
        if secret.required && secret.provider.is_empty() {
            gaps.push(Gap::SecretWithoutProvider {
                name: secret.name.clone(),
            });
        }
    }

    let level = if gaps.iter().any(Gap::is_blocking) {
        Level::LocalSnapshot
    } else if gaps.is_empty() {
        Level::Deterministic
    } else {
        Level::BestEffort
    };
    (level, gaps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EnvRefEntry, NativeSession, Origin, Runtime, SecretRefEntry, Service};

    fn base() -> Setup {
        Setup {
            name: "fixture".into(),
            origin: Origin::default(),
            executable: "/opt/homebrew/bin/claude".into(),
            argv: vec!["claude".into()],
            argv_bytes: Vec::new(),
            cwd: "/Users/me/code/app".into(),
            inherit: vec!["HOME".into()],
            overrides: Vec::new(),
            env_refs: Vec::new(),
            secret_refs: Vec::new(),
            services: Vec::new(),
            runtimes: Vec::new(),
            native_session: Some(NativeSession {
                kind: "claude".into(),
                value: "s1".into(),
            }),
        }
    }

    /// G5.3: a capture that declares everything it needs.
    #[test]
    fn a_fully_declared_capture_is_deterministic() {
        let mut s = base();
        s.runtimes.push(Runtime {
            name: "node".into(),
            version: Some("22.3.0".into()),
        });
        s.services.push(Service {
            name: "web".into(),
            executable: "/opt/homebrew/bin/pnpm".into(),
            argv: vec!["pnpm".into(), "dev".into()],
            cwd: "/Users/me/code/app".into(),
            readiness: Some("http://localhost:3000/health".into()),
        });
        s.secret_refs.push(SecretRefEntry {
            name: "STRIPE_KEY".into(),
            provider: "keychain".into(),
            required: true,
        });
        let (level, gaps) = level_of(&s);
        assert_eq!(level, Level::Deterministic, "gaps: {gaps:?}");
        assert!(gaps.is_empty());
    }

    /// G5.3: something assumed present on the host and not declared.
    #[test]
    fn an_undeclared_runtime_version_is_best_effort() {
        let mut s = base();
        s.runtimes.push(Runtime {
            name: "node".into(),
            version: None,
        });
        let (level, gaps) = level_of(&s);
        assert_eq!(level, Level::BestEffort);
        assert_eq!(
            gaps,
            vec![Gap::RuntimeVersionUnknown {
                runtime: "node".into()
            }]
        );
        assert!(
            !gaps[0].is_blocking(),
            "an assumption is checkable, not fatal"
        );
    }

    /// G5.3: something that cannot be resolved to anything portable.
    #[test]
    fn a_machine_local_path_is_a_local_snapshot() {
        let mut s = base();
        s.cwd = "/private/var/folders/xy/T/scratch".into();
        let (level, gaps) = level_of(&s);
        assert_eq!(level, Level::LocalSnapshot);
        assert!(gaps.iter().any(Gap::is_blocking));
    }

    /// The same places spelled the way a shell reports them: capture keeps the cwd as
    /// given, so `$TMPDIR` is `/var/folders/...` and a resolved `/tmp` is `/private/tmp`.
    #[test]
    fn a_machine_local_path_is_local_however_it_is_spelled() {
        for cwd in [
            "/var/folders/xy/T/scratch",
            "/private/tmp/x",
            "/private/var/tmp/x",
        ] {
            let mut s = base();
            s.cwd = cwd.into();
            assert_eq!(level_of(&s).0, Level::LocalSnapshot, "{cwd}");
        }
    }

    #[test]
    fn a_required_env_reference_with_nothing_to_resolve_it_is_a_local_snapshot() {
        let mut s = base();
        s.env_refs.push(EnvRefEntry {
            name: "STRIPE_ACCOUNT".into(),
            required: true,
        });
        assert_eq!(level_of(&s).0, Level::LocalSnapshot);

        // Declared as an override, and it is deterministic again.
        s.overrides
            .push(("STRIPE_ACCOUNT".into(), "acct_123".into()));
        assert_eq!(level_of(&s).0, Level::Deterministic);
    }

    #[test]
    fn a_required_secret_with_no_provider_is_a_local_snapshot() {
        let mut s = base();
        s.secret_refs.push(SecretRefEntry {
            name: "STRIPE_KEY".into(),
            provider: String::new(),
            required: true,
        });
        assert_eq!(level_of(&s).0, Level::LocalSnapshot);
    }

    /// The level is computed, so an artifact cannot declare itself deterministic.
    #[test]
    fn the_level_is_not_a_field_the_artifact_can_set() {
        let mut s = base();
        s.cwd = "/tmp/scratch".into();
        let text = s.to_toml().expect("encode");
        assert!(
            !text.contains("level"),
            "the artifact carries a level field, so it can claim one"
        );
        assert_eq!(
            level_of(&Setup::from_toml(&text).expect("decode")).0,
            Level::LocalSnapshot
        );
    }

    /// An optional reference is a decision the capture made, not a gap.
    #[test]
    fn an_optional_reference_does_not_lower_the_level() {
        let mut s = base();
        s.env_refs.push(EnvRefEntry {
            name: "DEBUG".into(),
            required: false,
        });
        s.secret_refs.push(SecretRefEntry {
            name: "OPTIONAL_KEY".into(),
            provider: String::new(),
            required: false,
        });
        assert_eq!(level_of(&s).0, Level::Deterministic);
    }
}
