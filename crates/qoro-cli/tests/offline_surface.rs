//! Gate G9.1: every subcommand behaves the same with the network gone.
//!
//! The enumeration comes from `docs/cli-surface.md` rather than from a list in this file. A
//! list here is a list that stops covering the surface the first time somebody adds a
//! subcommand, and the point of that document is that it grows with the code.

use std::process::Command;

fn qoro() -> &'static str {
    env!("CARGO_BIN_EXE_qoro")
}

const SURFACE: &str = include_str!("../../../docs/cli-surface.md");

/// The commands the document lists, as arguments.
///
/// Commands that start a daemon or attach a terminal are not run here: they do not
/// terminate on their own and their behaviour under a blackholed network is a different
/// gate. They are still required to be in the table, which is what the coverage assertion
/// below checks.
fn runnable_from_the_document() -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for line in SURFACE.lines() {
        let Some(rest) = line.strip_prefix("| `qoro") else {
            continue;
        };
        // A row marked yes in the network column is the declared exception: it reaches
        // the network on purpose and fails closed without it, so byte-identical output
        // is asserted for it in `update_fails_closed_with_the_network_gone` instead.
        if line.trim_end().ends_with("| yes |") {
            continue;
        }
        let Some(end) = rest.find('`') else { continue };
        let raw: Vec<&str> = rest[..end].split_whitespace().collect();
        // An optional flag group like `[--json]` runs as its bare form: running
        // it literally would only ever exercise the unknown-flag error path.
        // `--json` variants are covered by their own tests. An optional
        // positional like `[agent...]` is a placeholder instead: the bare form
        // would install into the real home, so the row is skipped like the
        // `<file>` rows below rather than run.
        if raw
            .iter()
            .any(|a| a.contains('[') && !a.trim_matches(|c| c == '[' || c == ']').starts_with('-'))
        {
            continue;
        }
        let args: Vec<String> = raw
            .into_iter()
            .filter(|a| !a.contains('[') && !a.contains(']'))
            .map(|a| a.to_string())
            .collect();
        // `<file>` placeholders are filled in by the caller; a command with one is run
        // against a real artifact below rather than skipped.
        if args.iter().any(|a| a.contains('<')) {
            continue;
        }
        // `daemon` never returns, and `relay` starts one: its output is whatever the daemon
        // says to a client rather than a result of its own, and running it here left two
        // daemons alive per run against runtime directories the test had already deleted.
        // Both are still required to be in the table, which the agreement assertion below
        // covers.
        if matches!(args.first().map(String::as_str), Some("daemon" | "relay")) {
            continue;
        }
        if args.is_empty() {
            continue;
        }
        out.push(args);
    }
    out
}

fn run(args: &[String], offline: bool) -> (String, String, Option<i32>) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(
        dir.path(),
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
    )
    .expect("chmod");
    let mut command = Command::new(qoro());
    command.args(args).env("QORO_RUNTIME_DIR", dir.path());
    if offline {
        // Blackholed: no proxy, no resolver, nowhere to reach. A subcommand that consulted
        // a control plane would notice.
        command
            .env("http_proxy", "http://127.0.0.1:1")
            .env("https_proxy", "http://127.0.0.1:1")
            .env("all_proxy", "http://127.0.0.1:1")
            .env("QORO_CONTROL_PLANE", "http://127.0.0.1:1")
            .env("RES_OPTIONS", "timeout:1 attempts:1");
    }
    let out = command.output().expect("run qoro");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

#[test]
fn every_documented_subcommand_is_identical_with_the_network_gone() {
    let commands = runnable_from_the_document();
    assert!(
        commands.len() >= 4,
        "only {} runnable subcommands were found in the document, so it is not being read",
        commands.len()
    );
    for args in commands {
        let (online_out, _, online_code) = run(&args, false);
        let (offline_out, _, offline_code) = run(&args, true);
        assert_eq!(
            online_out,
            offline_out,
            "`qoro {}` printed something different with the network gone",
            args.join(" ")
        );
        assert_eq!(
            online_code,
            offline_code,
            "`qoro {}` exited differently with the network gone",
            args.join(" ")
        );
    }
}

/// The one yes row: `qoro update` fails closed with the network gone, having touched
/// nothing. It names GitHub in the failure, exits nonzero, and leaves no notice behind.
#[test]
fn update_fails_closed_with_the_network_gone() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(
        dir.path(),
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
    )
    .expect("chmod");
    let mut command = Command::new(qoro());
    command
        .args(["update", "--check"])
        .env("QORO_RUNTIME_DIR", dir.path())
        .env("http_proxy", "http://127.0.0.1:1")
        .env("https_proxy", "http://127.0.0.1:1")
        .env("all_proxy", "http://127.0.0.1:1")
        .env("QORO_CONTROL_PLANE", "http://127.0.0.1:1")
        .env("RES_OPTIONS", "timeout:1 attempts:1");
    let out = command.output().expect("run qoro");
    assert!(
        !out.status.success(),
        "`qoro update --check` succeeded with the network gone, so the failure is silent"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("GitHub"),
        "`qoro update --check` failed without saying what it could not reach: {stderr}"
    );
    assert!(
        !dir.path().join("update-available").exists(),
        "a failed check left a notice behind, and the interface would advertise a version nobody saw"
    );
}

/// The Setup subcommands, which take a file, run against a real artifact.
#[test]
fn the_setup_subcommands_are_identical_with_the_network_gone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let artifact = dir.path().join("s.toml");
    let setup = qoro_setup::Setup {
        name: "offline".into(),
        origin: qoro_setup::Origin::default(),
        executable: "/bin/echo".into(),
        argv: vec!["echo".into()],
        argv_bytes: Vec::new(),
        cwd: "/tmp".into(),
        inherit: Vec::new(),
        overrides: Vec::new(),
        env_refs: Vec::new(),
        secret_refs: Vec::new(),
        services: Vec::new(),
        runtimes: Vec::new(),
        native_session: None,
    };
    std::fs::write(&artifact, setup.to_toml().expect("encode")).expect("write");
    let path = artifact.display().to_string();

    for verb in ["show", "check"] {
        let args = vec!["setup".to_string(), verb.to_string(), path.clone()];
        let (online_out, _, online_code) = run(&args, false);
        let (offline_out, _, offline_code) = run(&args, true);
        assert_eq!(
            online_out, offline_out,
            "`qoro setup {verb}` differed offline"
        );
        assert_eq!(
            online_code, offline_code,
            "`qoro setup {verb}` differed offline"
        );
    }
}

/// The subcommand names in one of this crate's two enumerations of the surface.
///
/// Both sides are parsed rather than listed, and that is the whole point of the assertion
/// below. The first version of this test walked a hardcoded `["daemon", "reset", "setup"]`
/// while its own doc comment claimed that a subcommand missing from the table is a
/// subcommand nobody checks. It was not true: `qoro relay` was added for milestone 6, was
/// printed by `--help`, was absent from the table, and this test stayed green because
/// `relay` was not one of the three words it knew about. A list in a test stops covering the
/// surface the first time somebody adds to it, which is the reason the document exists.
fn verbs(text: &str) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for line in text.lines() {
        // The help prints `  qoro <verb>` and the table prints `| `qoro <verb>` |`.
        let line = line.trim_start().trim_start_matches("| ");
        let Some(rest) = line
            .strip_prefix("qoro ")
            .or_else(|| line.strip_prefix("`qoro "))
        else {
            continue;
        };
        // Exactly one space, because the help aligns its description column: the bare
        // `qoro` row is `qoro` followed by twelve spaces and then the sentence "attach to a
        // terminal", and taking the first word after the name would read "attach" as a
        // subcommand that does not exist.
        if rest.starts_with(' ') {
            continue;
        }
        let Some(word) = rest.split_whitespace().next() else {
            continue;
        };
        // A flag is not a subcommand, and the flags are asserted separately: `--help` does
        // not print every flag it accepts, so comparing those two sets would be comparing
        // two different things.
        let word = word.trim_end_matches('`');
        if word.starts_with('-') || word.is_empty() {
            continue;
        }
        out.insert(word.to_string());
    }
    out
}

/// The new rows are actually enumerated as runnable commands, not just
/// present in the table: the `[--json]` group must parse away to a bare
/// `agents` invocation rather than a literal `[--json]` flag.
#[test]
fn the_new_rows_are_enumerated_as_their_bare_forms() {
    let commands = runnable_from_the_document();
    assert!(
        commands.contains(&vec!["agents".to_string()]),
        "bare `qoro agents` is not enumerated: {commands:?}"
    );
    assert!(
        !commands.iter().any(|c| c.iter().any(|a| a.contains('['))),
        "an optional group leaked into the enumeration: {commands:?}"
    );
    assert!(
        commands.contains(&vec!["master".to_string(), "brief".to_string()]),
        "`qoro master brief` is not enumerated: {commands:?}"
    );
}

/// The document and the binary agree about what exists.
///
/// A subcommand missing from the table is a subcommand nobody checks, and a row with no
/// subcommand is a table that has drifted from the code.
#[test]
fn the_document_and_the_help_output_describe_the_same_surface() {
    let (help, _, _) = run(&["--help".to_string()], false);
    let from_help = verbs(&help);
    let from_document = verbs(SURFACE);
    assert!(
        !from_help.is_empty(),
        "the help output named no subcommands at all, so this assertion is vacuous: {help}"
    );
    assert_eq!(
        from_help, from_document,
        "`qoro --help` and docs/cli-surface.md name different subcommands"
    );
    for flag in ["--hello", "--version", "--help"] {
        assert!(
            SURFACE.contains(&format!("`qoro {flag}`")),
            "{flag} is not in the document"
        );
    }
}
