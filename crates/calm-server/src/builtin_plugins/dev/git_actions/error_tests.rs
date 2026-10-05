use super::*;
#[test]
fn rejects_unknown_tool() {
    let err = lower("git.push", &json!({})).expect_err("unknown tool rejected");
    assert!(err.contains("unknown git-forge tool"));
}

#[test]
fn rejects_missing_required_arg() {
    let err = lower("git_commit", &json!({ "message": "m" }))
        .expect_err("missing required argument rejected");
    assert!(err.contains("idem"));
}
