//! `qoop update` leaves the hooks and skills current, the way `scripts/install.sh` does.
//!
//! The whole chain runs for real inside one temp dir: an installed copy of this build, a
//! fake release served from `file://`, and a home with one agent in it. Nothing reaches the
//! person's own home, runtime directory or configuration.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

const ASSETS: [&str; 4] = [
    "qoop-aarch64-apple-darwin.tar.gz",
    "qoop-x86_64-apple-darwin.tar.gz",
    "qoop-x86_64-unknown-linux-gnu.tar.gz",
    "qoop-aarch64-unknown-linux-gnu.tar.gz",
];

/// `qoop` as installed on the fake machine, with its environment.
fn qoop(root: &Path, exe: &Path, args: &[&str]) -> std::process::Output {
    qoop_testkit::sandboxed(exe)
        .args(args)
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("QOOP_RUNTIME_DIR", root.join("rt"))
        // An update walks the saved remotes: this one is empty, never the person's own.
        .env("QOOP_REMOTES", root.join("remotes.json"))
        .env(
            "QOOP_RELEASE_BASE",
            format!("file://{}", root.join("rel").display()),
        )
        // No agent binary on the path: an agent is present only by its directory.
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("run qoop")
}

fn line_of(status: &std::process::Output, agent: &str) -> String {
    String::from_utf8_lossy(&status.stdout)
        .lines()
        .find(|l| l.trim_start().starts_with(agent))
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[test]
fn an_update_leaves_hooks_and_skills_current() {
    let root = tempfile::Builder::new()
        .prefix("qu")
        .tempdir()
        .expect("root");
    let root = root.path();
    for dir in ["home/.claude", "bin", "rel/tags", "pack", "rt"] {
        std::fs::create_dir_all(root.join(dir)).expect("dir");
    }
    std::fs::set_permissions(root.join("rt"), std::fs::Permissions::from_mode(0o700))
        .expect("chmod");
    std::fs::write(root.join("remotes.json"), "[]").expect("remotes");

    // The installed build, and the "newer release": says 9.9.9, then is this build.
    let exe = root.join("bin/qoop");
    std::fs::copy(env!("CARGO_BIN_EXE_qoop"), &exe).expect("install");
    let fresh = root.join("pack/qoop");
    std::fs::write(
        &fresh,
        format!(
            "#!/bin/sh\n[ \"$1\" = --version ] && {{ echo 'qoop 9.9.9'; exit 0; }}\n\
             exec '{}' \"$@\"\n",
            env!("CARGO_BIN_EXE_qoop")
        ),
    )
    .expect("fresh");
    std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let tarball = root.join("pack/release.tar.gz");
    let tar = Command::new("tar")
        .arg("-czf")
        .arg(&tarball)
        .arg("qoop")
        .current_dir(root.join("pack"))
        .status()
        .expect("tar");
    assert!(tar.success());
    let assets: Vec<_> = ASSETS
        .iter()
        .map(|name| serde_json::json!({"name": name, "url": format!("file://{}", tarball.display())}))
        .collect();
    std::fs::write(
        root.join("rel/latest"),
        serde_json::json!({"tag_name": "v9.9.9", "assets": assets}).to_string(),
    )
    .expect("release");

    let hooks = qoop(root, &exe, &["hooks", "status"]);
    assert_eq!(line_of(&hooks, "claude"), "claude     not installed");
    assert_eq!(line_of(&hooks, "codex"), "codex      not present");

    let update = qoop(root, &exe, &["update"]);
    let said = String::from_utf8_lossy(&update.stdout).into_owned()
        + &String::from_utf8_lossy(&update.stderr);
    assert!(update.status.success(), "{said}");
    assert!(said.contains("updated qoop"), "{said}");
    assert!(
        said.contains("this machine: no daemon running, the next qoop starts 9.9.9"),
        "{said}"
    );
    assert!(
        said.contains("claude     installed"),
        "its output is shown: {said}"
    );

    let hooks = qoop(root, &exe, &["hooks", "status"]);
    assert_eq!(line_of(&hooks, "claude"), "claude     installed");
    assert_eq!(line_of(&hooks, "codex"), "codex      not present");
    assert!(
        !root.join("home/.codex").exists(),
        "an absent agent got files"
    );
    let skills = qoop(root, &exe, &["skills", "status"]);
    assert_eq!(line_of(&skills, "claude"), "claude     installed");
    assert!(
        line_of(&skills, "qass").starts_with("qass       installed, in its folder"),
        "{}",
        String::from_utf8_lossy(&skills.stdout)
    );
}
