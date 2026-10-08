//! Capturing a running terminal into a Setup, and turning one back into a launch.
//!
//! The round trip is the point. `LaunchIntent` is the only way to start a process anywhere
//! in the codebase (invariant I7), so a Setup that restores through it cannot forget argv
//! or environment the way a second spawn path would: argv
//! persisted and displayed but never used.
//!
//! Secrets are captured as references and never as values, which is not a filter but a type:
//! there is no field on a `SecretRef` that can hold one.

use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use qoop_domain::{EnvPlan, EnvRef, LaunchIntent, NativeSessionRef, SecretRef};

use crate::{EnvRefEntry, NativeSession, Origin, SecretRefEntry, Setup};

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("{0}")]
    Invalid(String),
}

/// Environment names the daemon injects. Never captured, by name.
///
/// ADR-015: variables the daemon itself injects, including the capability token, are never
/// shown as user authored overrides and are excluded from capture. Capturing them would put
/// one terminal's token into another terminal's Setup, which is the one value in the
/// environment that must not travel.
pub const DAEMON_INJECTED: &[&str] = &["QOOP_TOKEN", "QOOP_TERMINAL"];

/// Build a Setup from a launch intent.
pub fn capture_from(
    name: &str,
    intent: &LaunchIntent,
    origin: Origin,
) -> Result<Setup, CaptureError> {
    let mut argv = Vec::with_capacity(intent.argv().len());
    let mut argv_bytes = Vec::new();
    for (i, arg) in intent.argv().iter().enumerate() {
        match arg.clone().into_string() {
            Ok(text) => argv.push(text),
            Err(raw) => {
                // Not valid UTF-8, which does happen and which a round trip through a
                // string would silently change. The lossy form goes in `argv` so a person
                // can read it, and the exact bytes go alongside so restore is exact.
                argv.push(String::from_utf8_lossy(raw.as_bytes()).into_owned());
                argv_bytes.push((i, raw.as_bytes().to_vec()));
            }
        }
    }

    Ok(Setup {
        name: name.to_string(),
        origin,
        executable: intent.executable().display().to_string(),
        argv,
        argv_bytes,
        cwd: intent.cwd().display().to_string(),
        inherit: intent
            .env
            .inherit
            .iter()
            .filter(|n| !DAEMON_INJECTED.contains(&n.as_str()))
            .cloned()
            .collect(),
        overrides: intent
            .env
            .overrides
            .iter()
            .filter(|(n, _)| !DAEMON_INJECTED.contains(&n.as_str()))
            .cloned()
            .collect(),
        env_refs: intent
            .env
            .env_refs
            .iter()
            .map(|r| EnvRefEntry {
                name: r.name.clone(),
                required: r.required,
            })
            .collect(),
        secret_refs: intent
            .env
            .secret_refs
            .iter()
            .map(|r| SecretRefEntry {
                name: r.name.clone(),
                provider: r.provider.clone(),
                required: r.required,
            })
            .collect(),
        services: Vec::new(),
        runtimes: Vec::new(),
        native_session: intent.native_session_ref.as_ref().map(|s| NativeSession {
            kind: s.kind.clone(),
            value: s.value.clone(),
        }),
    })
}

impl Setup {
    /// Turn this Setup back into the only thing that can start a process.
    ///
    /// A relative executable is refused here, by `LaunchIntent`, which is the same refusal
    /// a live spawn gets because it is the same constructor (gate G5.5).
    pub fn to_launch_intent(&self) -> Result<LaunchIntent, CaptureError> {
        let mut intent = LaunchIntent::new(
            PathBuf::from(&self.executable),
            self.argv_os(),
            PathBuf::from(&self.cwd),
        )
        .map_err(|e| CaptureError::Invalid(e.to_string()))?;
        intent.env = EnvPlan {
            inherit: self.inherit.clone(),
            overrides: self.overrides.clone(),
            env_refs: self
                .env_refs
                .iter()
                .map(|r| EnvRef {
                    name: r.name.clone(),
                    required: r.required,
                })
                .collect(),
            secret_refs: self
                .secret_refs
                .iter()
                .map(|r| SecretRef {
                    name: r.name.clone(),
                    provider: r.provider.clone(),
                    required: r.required,
                })
                .collect(),
        };
        intent.native_session_ref = self.native_session.as_ref().map(|s| NativeSessionRef {
            kind: s.kind.clone(),
            value: s.value.clone(),
        });
        Ok(intent)
    }
}

/// What redaction leaves behind, so an absence of evidence and a redaction do not look the
/// same.
pub const REDACTED: &str = "[redacted]";

/// Remove known secret values from a capture, wherever they appear.
///
/// This is a backstop, not the mechanism. The mechanism is that a secret is a `SecretRef`
/// with no field that can hold a value, and the mechanism covers the environment. It does
/// not cover argv, and it does not cover a secret that is a substring of a URL or of a
/// shell command string, which is where they actually turn up.
///
/// Substring replacement rather than dropping the field: a URL with its token removed is
/// still a URL somebody can read, and a command string with its key removed is still the
/// command that ran. Dropping the whole field would make the artifact useless in exactly
/// the cases this is for.
///
/// Returns what was changed, by field, because a person restoring this Setup has to know
/// what they will be asked to supply.
pub fn redact_capture(mut setup: Setup, secrets: &[&str]) -> (Setup, Vec<String>) {
    let mut found = Vec::new();
    let scrub = |text: &mut String, field: &str, found: &mut Vec<String>| {
        for secret in secrets {
            if secret.is_empty() {
                continue;
            }
            if text.contains(secret) {
                *text = text.replace(secret, REDACTED);
                found.push(field.to_string());
            }
        }
    };

    scrub(&mut setup.executable, "executable", &mut found);
    scrub(&mut setup.cwd, "cwd", &mut found);
    for (i, arg) in setup.argv.iter_mut().enumerate() {
        scrub(arg, &format!("argv[{i}]"), &mut found);
    }
    for (name, value) in setup.overrides.iter_mut() {
        let field = format!("overrides.{name}");
        scrub(value, &field, &mut found);
    }
    for service in setup.services.iter_mut() {
        let name = service.name.clone();
        scrub(
            &mut service.executable,
            &format!("service.{name}.executable"),
            &mut found,
        );
        for (i, arg) in service.argv.iter_mut().enumerate() {
            scrub(arg, &format!("service.{name}.argv[{i}]"), &mut found);
        }
        if let Some(readiness) = service.readiness.as_mut() {
            scrub(readiness, &format!("service.{name}.readiness"), &mut found);
        }
    }
    if let Some(session) = setup.native_session.as_mut() {
        scrub(&mut session.value, "native_session", &mut found);
    }
    (setup, found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    fn intent() -> LaunchIntent {
        let mut i = LaunchIntent::new(
            "/opt/homebrew/bin/claude",
            vec![
                OsString::from("claude"),
                OsString::from("--resume"),
                OsString::from_vec(vec![0x2d, 0x2d, 0xff, 0xfe]),
            ],
            "/Users/me/code/app",
        )
        .expect("intent");
        i.env = EnvPlan {
            inherit: vec!["HOME".into(), "PATH".into(), "QOOP_TOKEN".into()],
            overrides: vec![
                ("NODE_OPTIONS".into(), "--require /tmp/x.js".into()),
                ("QOOP_TERMINAL".into(), "7".into()),
            ],
            env_refs: vec![EnvRef {
                name: "STRIPE_ACCOUNT".into(),
                required: true,
            }],
            secret_refs: vec![SecretRef {
                name: "STRIPE_KEY".into(),
                provider: "keychain".into(),
                required: true,
            }],
        };
        i.native_session_ref = Some(NativeSessionRef {
            kind: "claude".into(),
            value: "sess-abc".into(),
        });
        i
    }

    /// G5.1: capture then restore preserves the executable, argv as bytes, cwd, the
    /// references, and the native session.
    #[test]
    fn a_capture_restores_exactly_what_it_captured() {
        let original = intent();
        let setup = capture_from("app", &original, Origin::default()).expect("capture");
        let text = setup.to_toml().expect("encode");
        let restored = Setup::from_toml(&text)
            .expect("decode")
            .to_launch_intent()
            .expect("restore");

        assert_eq!(restored.executable(), original.executable());
        assert_eq!(restored.cwd(), original.cwd());
        assert_eq!(
            restored.argv(),
            original.argv(),
            "argv did not survive as bytes"
        );
        assert_eq!(restored.env.env_refs, original.env.env_refs);
        assert_eq!(restored.env.secret_refs, original.env.secret_refs);
        assert_eq!(
            restored.native_session_ref, original.native_session_ref,
            "restoring started a new session instead of reattaching one"
        );
    }

    /// The one argument that is not valid UTF-8 comes back byte for byte.
    #[test]
    fn an_argument_that_is_not_text_survives_the_round_trip() {
        let original = intent();
        let setup = capture_from("app", &original, Origin::default()).expect("capture");
        assert_eq!(setup.argv_bytes.len(), 1, "the raw bytes were not recorded");
        let restored = Setup::from_toml(&setup.to_toml().expect("encode"))
            .expect("decode")
            .to_launch_intent()
            .expect("restore");
        assert_eq!(restored.argv()[2].as_bytes(), &[0x2d, 0x2d, 0xff, 0xfe]);
    }

    /// The daemon's own variables are not somebody's Setup.
    #[test]
    fn the_capability_token_is_never_captured() {
        let setup = capture_from("app", &intent(), Origin::default()).expect("capture");
        let text = setup.to_toml().expect("encode");
        assert!(
            !text.contains("QOOP_TOKEN"),
            "a terminal's capability token was captured into a Setup"
        );
        assert!(!text.contains("QOOP_TERMINAL"));
        assert!(
            text.contains("NODE_OPTIONS"),
            "an ordinary override was dropped"
        );
    }

    /// G5.5: a relative executable is refused, by the same constructor a live spawn uses.
    #[test]
    fn a_setup_with_a_relative_executable_is_refused() {
        let mut setup = capture_from("app", &intent(), Origin::default()).expect("capture");
        setup.executable = "claude".into();
        let err = setup
            .to_launch_intent()
            .expect_err("a relative path is refused");
        assert!(
            format!("{err}").contains("absolute"),
            "the refusal does not say why: {err}"
        );
    }

    #[test]
    fn a_setup_with_a_relative_cwd_is_refused() {
        let mut setup = capture_from("app", &intent(), Origin::default()).expect("capture");
        setup.cwd = "code/app".into();
        assert!(setup.to_launch_intent().is_err());
    }
}
