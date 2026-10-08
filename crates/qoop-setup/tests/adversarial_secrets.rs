//! Gate G5.2: an adversarial fixture, and zero plaintext secret values by exact match.
//!
//! Four places a secret really turns up, and each one is a different mechanism rather than
//! a different case of the same one:
//!
//! - in the environment, which is the case the type system already prevents,
//! - in argv, which it does not,
//! - inside a URL, where it is a substring of something that is not a secret,
//! - inside a shell command string, where it is a substring of something that is also code.
//!
//! The last three are the reason this gate exists. A design whose only defence is "a secret
//! is a `SecretRef`" is a design that never looks at the other three.

use qoop_setup::{Origin, Setup, redact_capture};

/// The values that must not appear anywhere, ever, by exact match.
const SECRETS: &[&str] = &[
    "sk-live-51H8ZqLkjhgfdsaPOIUYTREWQ",
    "ghp_16C7e42F292c6912E7710c838347Ae178B4a",
    "AKIAIOSFODNN7EXAMPLE",
    "hunter2-the-database-password",
];

fn adversarial() -> Setup {
    Setup {
        name: "adversarial".into(),
        origin: Origin {
            machine: "laptop-a1".into(),
            user: "alice".into(),
            captured_at_unix: 1,
        },
        executable: "/opt/homebrew/bin/node".into(),
        argv: vec![
            "node".into(),
            "server.js".into(),
            format!("--api-key={}", SECRETS[0]),
            format!(
                "--callback=https://api.example.com/v1?token={}&x=1",
                SECRETS[1]
            ),
            format!(
                "--pre=sh -c 'export AWS_ACCESS_KEY_ID={} && ./run.sh'",
                SECRETS[2]
            ),
        ],
        argv_bytes: Vec::new(),
        cwd: "/Users/me/code/app".into(),
        inherit: vec!["HOME".into(), "PATH".into()],
        overrides: vec![
            (
                "DATABASE_URL".into(),
                format!("postgres://app:{}@db/app", SECRETS[3]),
            ),
            ("NODE_OPTIONS".into(), "--enable-source-maps".into()),
        ],
        env_refs: Vec::new(),
        secret_refs: Vec::new(),
        services: Vec::new(),
        runtimes: Vec::new(),
        native_session: None,
    }
}

#[test]
fn a_capture_carrying_secrets_in_four_places_writes_none_of_them() {
    let (redacted, found) = redact_capture(adversarial(), SECRETS);

    let artifact = redacted.to_toml().expect("encode");
    for secret in SECRETS {
        assert!(
            !artifact.contains(secret),
            "the artifact contains a plaintext secret: {secret}"
        );
    }
    assert_eq!(
        found.len(),
        SECRETS.len(),
        "not every secret was found: {found:?}"
    );
}

/// Redaction has to leave a trace, or an absence of evidence and a redaction look the same.
#[test]
fn every_redaction_says_where_it_happened() {
    let (redacted, found) = redact_capture(adversarial(), SECRETS);
    let artifact = redacted.to_toml().expect("encode");
    assert!(
        artifact.contains("[redacted]"),
        "a secret was removed with no trace"
    );
    assert!(found.iter().any(|f| f.contains("argv")));
    assert!(found.iter().any(|f| f.contains("overrides")));
}

/// What is left is still the command that was captured, minus the values.
#[test]
fn redaction_keeps_the_shape_of_what_was_captured() {
    let (redacted, _) = redact_capture(adversarial(), SECRETS);
    assert_eq!(redacted.executable, "/opt/homebrew/bin/node");
    assert_eq!(redacted.argv.len(), 5);
    assert!(redacted.argv[2].starts_with("--api-key="));
    assert!(
        redacted.argv[3].contains("https://api.example.com/v1"),
        "the URL was destroyed rather than redacted: {}",
        redacted.argv[3]
    );
    assert!(
        redacted.argv[4].contains("./run.sh"),
        "the command string was destroyed rather than redacted: {}",
        redacted.argv[4]
    );
    assert!(
        redacted
            .overrides
            .iter()
            .any(|(n, v)| n == "NODE_OPTIONS" && v == "--enable-source-maps"),
        "an override with no secret in it was changed"
    );
}

/// A capture with nothing to hide is unchanged, so redaction cannot be the thing that
/// breaks an ordinary Setup.
#[test]
fn a_capture_with_no_secrets_is_untouched() {
    let mut clean = adversarial();
    clean.argv = vec!["node".into(), "server.js".into()];
    clean.overrides = vec![("NODE_OPTIONS".into(), "--enable-source-maps".into())];
    let (redacted, found) = redact_capture(clean.clone(), SECRETS);
    assert_eq!(redacted, clean);
    assert!(found.is_empty());
}
