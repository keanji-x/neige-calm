use super::super::test_helpers::*;
use super::*;
use crate::builtin::gitforge::git_actions::lower;

const HEAD: &str = "2222222222222222222222222222222222222222";

#[test]
fn lowers_gh_ci_failure_evidence() {
    let payload = lower(
        "gh_ci_failure_evidence",
        &json!({ "repo": "owner/repo", "pr": "42", "head_sha": HEAD, "attempt": 3 }),
    )
    .expect("lower gh ci failure evidence");
    assert!(
        CI_EVIDENCE_SCRIPT.starts_with(include_str!("../actions_prelude.sh"))
            && CI_EVIDENCE_SCRIPT.contains("resolve_repo \"$1\" || exit 1"),
        "the read resolves its identity through the shared Actions prelude"
    );
    assert_eq!(
        payload["argv"],
        json!([
            "sh",
            "-c",
            CI_EVIDENCE_SCRIPT,
            "sh",
            "owner/repo",
            HEAD,
            checks_sanitizer()
        ])
    );
    assert_eq!(
        payload["idem_key"],
        format!("gh_ci_failure_evidence:v1:[\"owner/repo\",42,\"{HEAD}\",\"3\"]")
    );
    assert_eq!(event_kind(&payload), "forge.ci.evidence.read");
    assert_eq!(
        payload["context"],
        json!({ "pr_number": 42, "head_sha": HEAD })
    );
    assert_eq!(
        (&payload["subject"], &payload["probe"], &payload["parked"]),
        (&Value::Null, &Value::Null, &json!(false)),
        "a probe-free, unparked read"
    );
    assert_eq!(payload["replay_any_version"], true);
    assert_eq!(payload.as_object().unwrap().len(), 8, "{payload}");
    assert_no_reserved_context(&payload, &["track_id", "artifact_path"]);
    assert_supported_event_kind(&payload);
}

/// `replay_any_version` replays by the key alone, so distinct arguments never share a key.
#[test]
fn gh_ci_failure_evidence_key_encodes_every_argument_injectively() {
    let key = |args: Value| {
        lower("gh_ci_failure_evidence", &args).unwrap()["idem_key"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let other = "3333333333333333333333333333333333333333";
    let keys = [
        key(json!({ "repo": "o/r", "pr": 1, "head_sha": HEAD })),
        key(json!({ "repo": "o/r", "pr": 1, "head_sha": HEAD, "attempt": "a" })),
        key(json!({ "repo": "o/r", "pr": 1, "head_sha": HEAD, "attempt": "a:b" })),
        key(json!({ "repo": "o/r:1", "pr": 1, "head_sha": HEAD })),
        key(json!({ "repo": "o/r", "pr": 11, "head_sha": HEAD })),
        key(json!({ "repo": "o/r", "pr": 1, "head_sha": other })),
    ];
    let distinct: std::collections::BTreeSet<_> = keys.iter().collect();
    assert_eq!(distinct.len(), keys.len(), "{keys:?}");
}

#[test]
fn gh_ci_failure_evidence_rejects_a_head_that_is_not_a_full_sha() {
    for head_sha in ["main", "2222222", &"g".repeat(40)] {
        let err = lower(
            "gh_ci_failure_evidence",
            &json!({ "repo": "o/r", "pr": 1, "head_sha": head_sha }),
        )
        .expect_err("non-SHA head rejected");
        assert_eq!(err, "head_sha must be a full commit SHA");
    }
}
