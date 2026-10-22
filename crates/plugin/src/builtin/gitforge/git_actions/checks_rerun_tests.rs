//! `after_rerun` lowering; tests/cases/forge_pr_checks_rerun.rs runs the fenced fold, wait and
//! deadline snapshot through the kernel.
use super::*;

fn fenced(extra: Value) -> Value {
    let mut args = json!({
        "repo": "owner/repo",
        "pr": 42,
        "after_rerun": { "run_id": 100, "run_attempt": 1 }
    });
    for (key, value) in extra.as_object().unwrap() {
        args[key] = value.clone();
    }
    args
}

#[test]
fn gh_pr_checks_after_rerun_passes_the_fence_to_the_wait_and_the_deadline_read() {
    let payload = lower("gh_pr_checks", &fenced(json!({}))).expect("lower a fenced wait");
    let unfenced =
        lower("gh_pr_checks", &json!({"repo": "owner/repo", "pr": 42})).expect("lower unfenced");
    let request = r#"{"run_attempt":1,"run_id":100}"#;
    let wait = payload["argv"].as_array().unwrap();
    let read = payload["probe"]["output_probe_argv"].as_array().unwrap();
    assert_eq!(wait.last().unwrap(), request, "{payload}");
    assert_eq!(read.last().unwrap(), request, "{payload}");
    assert_eq!(
        serde_json::from_str::<Value>(request).unwrap(),
        json!({ "run_id": 100, "run_attempt": 1 })
    );
    // Only the fence argument differs from the unfenced call: the same programs, the same
    // event, context and probe, so the persisted event shape is unchanged.
    let mut without_fence = payload.clone();
    *without_fence["argv"]
        .as_array_mut()
        .unwrap()
        .last_mut()
        .unwrap() = json!("null");
    *without_fence["probe"]["output_probe_argv"]
        .as_array_mut()
        .unwrap()
        .last_mut()
        .unwrap() = json!("null");
    without_fence["idem_key"] = unfenced["idem_key"].clone();
    assert_eq!(without_fence, unfenced);
    assert_eq!(
        payload["idem_key"],
        r#"gh_pr_checks_rerun:v2:["owner/repo",42,100,1,false,null]"#
    );
    assert_eq!(payload["replay_any_version"], true);
}

/// A fenced wait never replays an unfenced read (or a released key) of the attempt it waits
/// past, and every fenced argument is encoded injectively.
#[test]
fn gh_pr_checks_after_rerun_key_namespace_is_distinct() {
    let key = |args: Value| {
        lower("gh_pr_checks", &args).unwrap()["idem_key"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let mut fenced_keys = std::collections::BTreeSet::new();
    let mut unfenced_keys = std::collections::BTreeSet::new();
    for attempt in [None, Some("7"), Some("100:1")] {
        for wait_for_all in [false, true] {
            let mut base = json!({"repo": "owner/repo", "pr": 42, "wait_for_all": wait_for_all});
            if let Some(attempt) = &attempt {
                base["attempt"] = json!(attempt);
            }
            unfenced_keys.insert(key(base.clone()));
            for (run_id, run_attempt) in [(100, 1), (100, 2), (101, 1), (1, 100)] {
                let mut args = base.clone();
                args["after_rerun"] = json!({ "run_id": run_id, "run_attempt": run_attempt });
                let key = key(args);
                let encoded = key
                    .strip_prefix("gh_pr_checks_rerun:v2:")
                    .unwrap_or_else(|| panic!("fenced namespace: {key}"));
                assert_eq!(
                    serde_json::from_str::<Value>(encoded).unwrap(),
                    json!(["owner/repo", 42, run_id, run_attempt, wait_for_all, attempt])
                );
                assert!(fenced_keys.insert(key.clone()), "colliding identity: {key}");
            }
        }
    }
    assert_eq!(fenced_keys.len(), 3 * 2 * 4, "{fenced_keys:?}");
    for key in &fenced_keys {
        assert!(!unfenced_keys.contains(key), "{key}");
        for released in ["gh.pr.checks:", "gh_pr_checks:", "gh_pr_checks_all:"] {
            assert!(!key.starts_with(released), "{key}");
        }
    }
}

#[test]
fn gh_pr_checks_after_rerun_is_a_closed_object_of_positive_integers() {
    let rejected = [
        (
            json!(7),
            "after_rerun must be an object {run_id, run_attempt}",
        ),
        (
            json!("100"),
            "after_rerun must be an object {run_id, run_attempt}",
        ),
        (
            json!({"run_id": 100}),
            "after_rerun.run_attempt must be a positive integer",
        ),
        (
            json!({"run_attempt": 1}),
            "after_rerun.run_id must be a positive integer",
        ),
        (
            json!({"run_id": 0, "run_attempt": 1}),
            "after_rerun.run_id must be a positive integer",
        ),
        (
            json!({"run_id": 100, "run_attempt": "1"}),
            "after_rerun.run_attempt must be a positive integer",
        ),
        (
            json!({"run_id": -1, "run_attempt": 1}),
            "after_rerun.run_id must be a positive integer",
        ),
        (
            json!({"run_id": 100, "run_attempt": 1, "head_sha": "abc"}),
            "after_rerun: unknown field `head_sha`; valid: run_attempt, run_id",
        ),
    ];
    for (after_rerun, message) in rejected {
        let err = lower(
            "gh_pr_checks",
            &fenced(json!({ "after_rerun": after_rerun })),
        )
        .expect_err("malformed after_rerun rejected");
        assert_eq!(err, message);
    }
    // Absent and null are the unfenced call.
    assert_eq!(
        lower("gh_pr_checks", &fenced(json!({ "after_rerun": null }))).unwrap(),
        lower("gh_pr_checks", &json!({"repo": "owner/repo", "pr": 42})).unwrap()
    );
}
