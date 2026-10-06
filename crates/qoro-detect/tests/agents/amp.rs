//! Amp has no screen rules of its own: only how to resume it.

#[test]
fn it_resumes_by_thread_or_the_last_one() {
    assert_eq!(
        qoro_detect::resume_argv("amp", Some("T-1")),
        Some(vec![
            "amp".into(),
            "threads".into(),
            "continue".into(),
            "T-1".into()
        ])
    );
    assert_eq!(
        qoro_detect::resume_argv("amp", None),
        Some(vec![
            "amp".into(),
            "threads".into(),
            "continue".into(),
            "--last".into()
        ])
    );
}

#[test]
fn it_is_read_by_the_generic_rules() {
    let rules = qoro_detect::rules_for("amp").expect("rules");
    assert_eq!(rules.id, "generic");
}
