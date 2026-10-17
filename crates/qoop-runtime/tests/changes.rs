//! `Changes`: what a terminal's checkout changed against its default branch, read by git that
//! the checkout's own config cannot make run anything.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use qoop_runtime::control::{ChangedFile, ControlRequest, ControlResponse};
use qoop_testkit::TestDaemon;

const PATIENCE: Duration = Duration::from_secs(15);

/// git for the test's own setup, with no config but the repository's.
fn git(repo: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git")
        .success();
    assert!(ok, "git {args:?}");
}

#[tokio::test]
async fn a_checkout_is_read_against_where_it_left_main_and_runs_nothing_of_its_own() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("repo");
    let marker = dir.path().join("marker");
    std::fs::create_dir(&repo).expect("repo");
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\n").expect("a");
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    git(&repo, &["checkout", "-q", "-b", "feat"]);
    std::fs::write(repo.join("b.txt"), "two\n").expect("b");
    git(&repo, &["add", "b.txt"]);
    git(&repo, &["commit", "-q", "-m", "second"]);
    std::fs::write(repo.join("a.txt"), "one\nmore\n").expect("a again");
    std::fs::write(repo.join("new.txt"), "new\n").expect("new");

    std::fs::write(repo.join("b.txt"), "two\nthree\n").expect("b again");

    // What an agent could write into its own checkout: every command it names leaves a mark,
    // and a filter named `a=b` its own, since `-c` could not even name it.
    let marked = |name: &str, marker: &Path| {
        let hook = dir.path().join(name);
        let script = format!("#!/bin/sh\ntouch '{}'\nexec cat \"$@\"\n", marker.display());
        std::fs::write(&hook, script).expect("hook");
        let exec = std::os::unix::fs::PermissionsExt::from_mode(0o755);
        std::fs::set_permissions(&hook, exec).expect("chmod");
        hook.display().to_string()
    };
    let hook = marked("hook.sh", &marker);
    let marker_ab = dir.path().join("marker-ab");
    let hook_ab = marked("hook-ab.sh", &marker_ab);
    std::fs::write(
        repo.join(".gitattributes"),
        "* diff=y\na.txt filter=x\nb.txt filter=a=b\n",
    )
    .expect("attributes");
    for key in [
        "core.fsmonitor",
        "filter.x.clean",
        "filter.x.smudge",
        "diff.y.textconv",
        "diff.y.command",
        "diff.external",
        "core.pager",
    ] {
        git(&repo, &["config", key, &hook]);
    }
    git(&repo, &["config", "filter.x.required", "true"]);
    let mut config = std::fs::read_to_string(repo.join(".git/config")).expect("config");
    config.push_str(&format!(
        "[filter \"a=b\"]\n\tclean = {hook_ab}\n\tprocess = {hook_ab}\n"
    ));
    std::fs::write(repo.join(".git/config"), config).expect("config");
    // The traps are real: a plain git in that checkout runs both (the filter once the external
    // diff, which takes the files as they are, is out of the way).
    for plain in [&["diff", "main"][..], &["diff", "--no-ext-diff", "main"]] {
        let _ = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(plain)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    assert!(
        marker.exists() && marker_ab.exists(),
        "the checkout's config ran nothing"
    );
    std::fs::remove_file(&marker).expect("reset");
    std::fs::remove_file(&marker_ab).expect("reset");

    let d = TestDaemon::start().await;
    let mut phone = d.attach_phone().await;
    let created = phone
        .control(
            &ControlRequest::CreateTerminal {
                id: "tab".into(),
                cwd: Some(repo.display().to_string()),
                workspace: None,
                name: None,
            },
            PATIENCE,
        )
        .await;
    let Some(ControlResponse::TerminalCreated { terminal, .. }) = created else {
        panic!("no terminal: {created:?}");
    };
    let answer = phone
        .control(
            &ControlRequest::Changes {
                id: "changes".into(),
                terminal,
            },
            PATIENCE,
        )
        .await;
    assert!(
        !marker.exists(),
        "the daemon ran the checkout's own command"
    );
    assert!(!marker_ab.exists(), "the daemon ran the filter named a=b");
    let Some(ControlResponse::Changes {
        root,
        branch,
        base,
        files,
        diff,
        cut,
        pr,
        pr_known,
        ..
    }) = answer
    else {
        panic!("no changes: {answer:?}");
    };
    let real = std::fs::canonicalize(&repo).expect("real");
    assert_eq!(Path::new(&root), real);
    assert_eq!((branch.as_deref(), base.as_str()), (Some("feat"), "main"));
    let file = |path: &str, status: &str| ChangedFile {
        path: path.into(),
        status: status.into(),
    };
    for want in [
        file("a.txt", "M"),
        file("b.txt", "A"),
        file("new.txt", "?"),
        file(".gitattributes", "?"),
    ] {
        assert!(files.contains(&want), "{want:?} in {files:?}");
    }
    assert_eq!(files.len(), 4, "{files:?}");
    // Committed and uncommitted together, from where the branch left main.
    assert!(
        diff.contains("+two\n") && diff.contains("+more\n"),
        "{diff}"
    );
    assert!(!diff.contains("+one\n"), "{diff}");
    assert!(!cut);
    // No GitHub origin, so no gh and no pull request, and nothing known about one.
    assert_eq!((pr, pr_known), (None, false));

    let answer = phone
        .control(
            &ControlRequest::Changes {
                id: "nowhere".into(),
                terminal: 9999,
            },
            PATIENCE,
        )
        .await;
    assert!(
        matches!(answer, Some(ControlResponse::Failed { .. })),
        "{answer:?}"
    );
}
