use super::*;
#[test]
fn rejects_unknown_tool() {
    let err = lower("git.push", &json!({})).expect_err("unknown tool rejected");
    assert!(err.contains("unknown gitforge tool"));
}

#[test]
fn rejects_missing_required_arg() {
    let err = lower("git_commit", &json!({ "message": "m" }))
        .expect_err("missing required argument rejected");
    assert!(err.contains("idem"));
}

/// The SHAs are placed in the compare API path, so only full commit ids are accepted.
#[test]
fn gh_pr_diff_rejects_non_sha_commits() {
    let full = "a".repeat(40);
    for bad in [
        "main",
        "abc1234",
        "../../user",
        &format!("{full}?x=1"),
        &"a".repeat(41),
    ] {
        for key in ["base_sha", "head_sha"] {
            let mut args = json!({"repo":"o/r","pr":1,"base_sha":full,"head_sha":full});
            args[key] = json!(bad);
            let err = lower("gh_pr_diff", &args).expect_err("non-SHA commit rejected");
            assert_eq!(err, format!("{key} must be a full commit SHA"));
        }
    }
    let sha256 = "b".repeat(64);
    lower(
        "gh_pr_diff",
        &json!({"repo":"o/r","pr":1,"base_sha":full,"head_sha":sha256}),
    )
    .expect("SHA-1 and SHA-256 commit ids are accepted");
}

#[test]
fn gh_pr_checks_rejects_non_boolean_wait_policy_and_separates_all_mode() {
    for value in [json!(null), json!(1), json!("true")] {
        assert!(
            lower(
                "gh_pr_checks",
                &json!({"repo":"o/r","pr":1,"wait_for_all":value})
            )
            .is_err()
        );
    }
    let default = lower("gh_pr_checks", &json!({"repo":"o/r","pr":1})).unwrap();
    let explicit = lower(
        "gh_pr_checks",
        &json!({"repo":"o/r","pr":1,"wait_for_all":false}),
    )
    .unwrap();
    let all = lower(
        "gh_pr_checks",
        &json!({"repo":"o/r","pr":1,"wait_for_all":true}),
    )
    .unwrap();
    assert_eq!(default, explicit);
    assert_ne!(default["idem_key"], all["idem_key"]);
    for payload in [&default, &all] {
        assert_eq!(payload["replay_any_version"], true);
    }
}

#[test]
fn gh_pr_checks_identity_encodes_parameters_without_delimiter_collisions() {
    let cases = [
        ("owner/repo", 42, Some("43")),
        ("owner/repo:42", 43, None),
        ("owner/repo", 42, Some("43:44")),
        ("owner/repo:42:43", 44, None),
        ("owner/repo\",42,\"attempt", 42, Some("a\"],\\\n:null")),
        ("owner/repo", 42, Some("null")),
        ("owner/repo", 42, None),
    ];
    let mut keys = std::collections::BTreeSet::new();
    for (repo, pr, attempt) in cases {
        let mut args = json!({"repo":repo,"pr":pr});
        if let Some(attempt) = attempt {
            args["attempt"] = json!(attempt);
        }
        for (wait_for_all, prefix) in [(true, "gh_pr_checks_all:v3:"), (false, "gh_pr_checks:v3:")]
        {
            args["wait_for_all"] = json!(wait_for_all);
            let payload = lower("gh_pr_checks", &args).unwrap();
            let key = payload["idem_key"].as_str().unwrap();
            let encoded = key.strip_prefix(prefix).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(encoded).unwrap(),
                json!([repo, pr, attempt])
            );
            assert!(keys.insert(key.to_owned()), "colliding identity: {key}");
        }
    }
}

/// `replay_any_version` replays whatever row holds the key, so no current key may take a form a
/// released version wrote: `gh.pr.checks:{repo}:{pr}[:{attempt}]` or `gh_pr_checks_all:[…]`.
#[test]
fn gh_pr_checks_keys_never_take_a_released_form() {
    for (repo, pr, attempt) in [
        ("owner/repo", 42, None),
        ("owner/repo", 42, Some(json!("7"))),
        ("owner/repo", 42, Some(json!(43))),
        ("owner/repo:42", 43, None),
        ("o/r", 1, Some(json!("upgrade"))),
    ] {
        for wait_for_all in [false, true] {
            let mut args = json!({"repo": repo, "pr": pr, "wait_for_all": wait_for_all});
            if let Some(attempt) = &attempt {
                args["attempt"] = attempt.clone();
            }
            let payload = lower("gh_pr_checks", &args).unwrap();
            let key = payload["idem_key"].as_str().unwrap();
            assert!(
                !key.starts_with("gh.pr.checks:"),
                "released text form: {key}"
            );
            assert!(
                !key.starts_with("gh_pr_checks_all:["),
                "released all-mode form: {key}"
            );
        }
    }
}
