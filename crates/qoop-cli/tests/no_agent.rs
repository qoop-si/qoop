//! A machine with no agent on it yet, as a new computer often is: `qoop hooks install` (which
//! the install script runs) says qoop sets one up by itself once it is installed, not that a
//! running agent must be started afresh.

#[test]
fn hooks_install_with_no_agent_says_it_is_set_up_later() {
    let out = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"))
        .env("PATH", "/usr/bin:/bin")
        .args(["hooks", "install"])
        .output()
        .expect("run qoop hooks install");
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{said}");
    assert!(
        said.contains("qoop sets up its hook and skill by itself once one is installed"),
        "{said}"
    );
    assert!(!said.contains("an agent already running"), "{said}");
}
