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
    assert!(
        !all["idem_key"]
            .as_str()
            .unwrap()
            .starts_with("gh.pr.checks:")
    );
    assert!(all.get("compatible_payload_hashes").is_none());
}

#[test]
fn gh_pr_checks_all_identity_encodes_parameters_without_delimiter_collisions() {
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
        let mut args = json!({"repo":repo,"pr":pr,"wait_for_all":true});
        if let Some(attempt) = attempt {
            args["attempt"] = json!(attempt);
        }
        let all = lower("gh_pr_checks", &args).unwrap();
        let key = all["idem_key"].as_str().unwrap();
        let encoded = key.strip_prefix("gh_pr_checks_all:").unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(encoded).unwrap(),
            json!([repo, pr, attempt])
        );
        assert!(keys.insert(key.to_owned()), "colliding identity: {key}");
        args["wait_for_all"] = json!(false);
        let default = lower("gh_pr_checks", &args).unwrap();
        assert_ne!(all["idem_key"], default["idem_key"]);
        assert!(all.get("compatible_payload_hashes").is_none());
    }
}
