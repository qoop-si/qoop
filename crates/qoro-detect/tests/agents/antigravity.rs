//! Antigravity CLI has no screen rules of its own: only how to resume it.

#[test]
fn it_resumes_only_a_reported_conversation() {
    assert_eq!(
        qoro_detect::resume_argv("antigravity", Some("abc")),
        Some(vec!["agy".into(), "--conversation".into(), "abc".into()])
    );
    assert_eq!(
        qoro_detect::resume_argv("antigravity", None),
        None,
        "no latest line: only a reported conversation is resumed"
    );
}

#[test]
fn it_is_read_by_the_generic_rules() {
    let rules = qoro_detect::rules_for("antigravity").expect("rules");
    assert_eq!(rules.id, "generic");
}
