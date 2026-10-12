//! A machine with no agent on it yet, as a new computer often is: `qoop hooks install` (which
//! the install script runs) says what to run once one is installed, not that a running agent
//! must be started afresh.

use std::process::Command;

#[test]
fn hooks_install_with_no_agent_says_what_to_run_later() {
    let out = Command::from(qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop")))
        .env("PATH", "/usr/bin:/bin")
        .args(["hooks", "install"])
        .output()
        .expect("run qoop hooks install");
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{said}");
    assert!(said.contains("claude     not present, skipped"), "{said}");
    assert!(
        said.contains("once one is installed, run `qoop hooks install` and `qoop skills install`"),
        "{said}"
    );
    assert!(!said.contains("an agent already running"), "{said}");
}
