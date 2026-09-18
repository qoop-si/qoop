//! `qoro master brief`: the shipped supervisor instructions.

use std::process::Command;

fn qoro() -> &'static str {
    env!("CARGO_BIN_EXE_qoro")
}

#[test]
fn master_brief_prints_the_shipped_instructions() {
    let out = Command::new(qoro())
        .args(["master", "brief"])
        .output()
        .expect("run qoro");
    assert!(out.status.success(), "qoro master brief failed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Report-only"),
        "the brief lost its autonomy rule: {stdout:?}"
    );
    assert!(
        stdout.contains("master.json"),
        "the brief lost its file contract: {stdout:?}"
    );
}

#[test]
fn master_without_a_verb_names_the_usage() {
    for args in [vec!["master"], vec!["master", "spawn"]] {
        let out = Command::new(qoro()).args(&args).output().expect("run qoro");
        assert!(!out.status.success(), "{args:?} was accepted");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("usage"), "no usage hint: {stderr:?}");
    }
}
