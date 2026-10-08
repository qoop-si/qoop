//! `qoop remote ls` with nothing added: standard output is the list, so it stays empty and the
//! sentence goes to stderr. qass's watch reads standard output and followed a machine it
//! called `no` while the sentence was on it.

#[test]
fn no_remotes_print_nothing_on_standard_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"))
        .args(["remote", "ls"])
        .env("XDG_CONFIG_HOME", dir.path())
        .env("HOME", dir.path())
        .output()
        .expect("run qoop");
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "",
        "the list is empty"
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("no remotes"));
}
