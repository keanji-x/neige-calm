use super::test_helpers::*;
use super::*;

#[test]
fn lowers_git_worktree_add() {
    let payload = lower(
        "git.worktree.add",
        &json!({ "target": "/tmp/wt", "branch": "wt-x" }),
    )
    .expect("lower worktree add");
    assert_eq!(
        payload,
        json!({
            "argv": [
                "sh",
                "-c",
                format!("{FORGE_SHELL_PRELUDE}\nneige_git worktree add \"$@\""),
                "sh",
                "/tmp/wt",
                "-b",
                "wt-x"
            ],
            "idem_key": "git.worktree.add:/tmp/wt",
            "event_spec": {
                "event_kind": "worktree.provisioned",
                "fields": {}
            },
            "subject": null,
            "context": { "path": "/tmp/wt" },
            "probe": null,
            "parked": false
        })
    );
    assert_no_reserved_context(&payload, &["track_id", "card_id"]);
}

#[test]
fn lowers_git_commit() {
    let expected_probe_script = format!("{FORGE_SHELL_PRELUDE}\n{GIT_COMMIT_PROBE_SCRIPT}");
    let expected_commit_script = format!("{FORGE_SHELL_PRELUDE}\n{GIT_COMMIT_SCRIPT}");
    let expected_output_probe_script = git_commit_output_probe_script();
    let payload = lower(
        "git.commit",
        &json!({
            "message": "neige: worker card-1 @ track track-1",
            "idem": "step-1",
            "branch": "neige/track-1/card-1"
        }),
    )
    .expect("lower commit");
    assert_eq!(
        payload,
        json!({
            "argv": [
                "sh",
                "-c",
                expected_commit_script,
                "sh",
                "neige: worker card-1 @ track track-1",
                "neige/track-1/card-1"
            ],
            "idem_key": "git.commit:step-1",
            "event_spec": {
                "event_kind": "worktree.committed",
                "fields": {
                    "branch": { "json_field": { "path": "/branch" } },
                    "commit_sha": { "json_field": { "path": "/commit" } }
                }
            },
            "subject": null,
            "context": {},
            "probe": {
                "probe_argv": [
                    "sh",
                    "-c",
                    expected_probe_script,
                    "sh"
                ],
                "output_probe_argv": [
                    "sh",
                    "-c",
                    expected_output_probe_script,
                    "sh",
                    "neige/track-1/card-1"
                ]
            },
            "parked": false
        })
    );
    assert_no_reserved_context(&payload, &["track_id", "card_id"]);
    assert_supported_event_kind(&payload);
    assert!(expected_commit_script.contains("neige_git add -A || exit 1"));
    assert!(expected_commit_script.contains("neige_git commit -m \"$1\" || exit 1"));
    assert!(!expected_commit_script.contains("|| true"));
    let rendered = serde_json::to_string(&payload).expect("payload json");
    for needle in ["worktree.committed", "neige: worker "] {
        assert!(
            rendered.contains(needle),
            "git.commit lowering missing needle {needle:?}: {rendered}"
        );
    }
}

#[test]
fn lowers_git_commit_with_runtime_branch_default() {
    let payload = lower(
        "git.commit",
        &json!({
            "message": "neige: worker card-1 @ track track-1",
            "idem": "step-1"
        }),
    )
    .expect("lower commit");
    assert_eq!(
        payload["argv"],
        json!([
            "sh",
            "-c",
            format!("{FORGE_SHELL_PRELUDE}\n{GIT_COMMIT_SCRIPT}"),
            "sh",
            "neige: worker card-1 @ track track-1"
        ])
    );
    assert_eq!(
        payload["probe"]["output_probe_argv"],
        json!(["sh", "-c", git_commit_output_probe_script(), "sh"])
    );
    assert_eq!(payload["event_spec"]["event_kind"], "worktree.committed");
}

#[test]
fn git_commit_lowering_uses_shared_scripts_as_drift_lock() {
    let payload = lower(
        "git.commit",
        &json!({
            "message": "neige: worker card-1 @ track track-1",
            "idem": "step-1",
            "branch": "neige/track-1/card-1"
        }),
    )
    .expect("lower commit");

    assert_eq!(
        payload["argv"][2],
        format!("{FORGE_SHELL_PRELUDE}\n{GIT_COMMIT_SCRIPT}")
    );
    assert_eq!(
        payload["probe"]["probe_argv"][2],
        format!("{FORGE_SHELL_PRELUDE}\n{GIT_COMMIT_PROBE_SCRIPT}")
    );
    assert_eq!(
        payload["probe"]["output_probe_argv"][2],
        format!("{FORGE_SHELL_PRELUDE}\n{GIT_COMMIT_OUTPUT_PROBE_SCRIPT}")
    );
}

#[test]
fn git_commit_output_probe_json_escapes_branch_argument() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    run_git(temp_dir.path(), ["init"]);
    run_git(
        temp_dir.path(),
        ["config", "user.email", "git-forge@example.test"],
    );
    run_git(temp_dir.path(), ["config", "user.name", "Git Forge"]);
    std::fs::write(temp_dir.path().join("README.md"), "init\n").expect("write readme");
    run_git(temp_dir.path(), ["add", "README.md"]);
    run_git(temp_dir.path(), ["commit", "-m", "init"]);

    let branch = "feature/quote\"and\nline\twith\rreturn";
    let output = std::process::Command::new("sh")
        .args(["-c", &git_commit_output_probe_script(), "sh", branch])
        .current_dir(temp_dir.path())
        .output()
        .expect("run output probe");
    assert!(
        output.status.success(),
        "output probe failed: status={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: Value = serde_json::from_slice(&output.stdout).expect("probe JSON");
    assert!(parsed["commit"].as_str().is_some_and(is_hex_sha));
    assert_eq!(parsed["branch"], branch);
}

#[test]
fn lowers_gh_pr_create() {
    let expected_probe_script = "n=$(gh pr list --repo \"$2\" --head \"$1\" --base \"$3\" --state open --json number --jq 'length' 2>/dev/null) || exit 3; case \"$n\" in '') exit 3 ;; 0) exit 1 ;; *) exit 0 ;; esac";
    let payload = lower(
        "gh.pr.create",
        &json!({
            "repo": "owner/repo",
            "head": "feature",
            "base": "main",
            "title": "Title",
            "body": "Body"
        }),
    )
    .expect("lower gh pr create");
    assert_eq!(
        payload,
        json!({
            "argv": [
                "gh",
                "pr",
                "create",
                "--repo",
                "owner/repo",
                "--head",
                "feature",
                "--base",
                "main",
                "--title",
                "Title",
                "--body",
                "Body"
            ],
            "idem_key": "gh.pr.create:owner/repo:main:feature",
            "event_spec": {
                "event_kind": "forge.pr.opened",
                "fields": {
                    "head_sha": { "json_field": { "path": "/headRefOid" } },
                    "pr_number": { "json_field": { "path": "/number" } }
                }
            },
            "subject": null,
            "context": {},
            "probe": {
                "probe_argv": [
                    "sh",
                    "-c",
                    expected_probe_script,
                    "sh",
                    "feature",
                    "owner/repo",
                    "main"
                ],
                "output_probe_argv": [
                    "gh",
                    "pr",
                    "list",
                    "--repo",
                    "owner/repo",
                    "--head",
                    "feature",
                    "--base",
                    "main",
                    "--state",
                    "open",
                    "--json",
                    "number,headRefOid",
                    "--jq",
                    ".[0]"
                ]
            },
            "parked": true
        })
    );
    assert_no_reserved_context(&payload, &["track_id"]);
    assert_supported_event_kind(&payload);
    assert!(payload["probe"]["output_probe_argv"].is_array());
}

#[test]
fn lowers_gh_pr_list() {
    let payload = lower(
        "gh.pr.list",
        &json!({
            "repo": "owner/repo",
            "base": "main",
            "head": "feature"
        }),
    )
    .expect("lower gh pr list");
    assert_eq!(
        payload,
        json!({
            "argv": [
                "gh",
                "pr",
                "list",
                "--repo",
                "owner/repo",
                "--base",
                "main",
                "--head",
                "feature",
                "--state",
                "open",
                "--json",
                "number",
                "--jq",
                "[.[].number]"
            ],
            "idem_key": "gh.pr.list:owner/repo:main:feature",
            "event_spec": {
                "event_kind": "forge.scan.completed",
                "fields": {
                    "overlapping_prs": { "json_field": { "path": "" } }
                }
            },
            "subject": null,
            "context": {},
            "probe": {
                "probe_argv": [
                    "gh",
                    "pr",
                    "list",
                    "--repo",
                    "owner/repo",
                    "--limit",
                    "1"
                ],
                "output_probe_argv": [
                    "gh",
                    "pr",
                    "list",
                    "--repo",
                    "owner/repo",
                    "--base",
                    "main",
                    "--head",
                    "feature",
                    "--state",
                    "open",
                    "--json",
                    "number",
                    "--jq",
                    "[.[].number]"
                ]
            },
            "parked": true
        })
    );
    assert_no_reserved_context(&payload, &["track_id"]);
    assert_supported_event_kind(&payload);
}

#[test]
fn lowers_gh_pr_diff() {
    let payload = lower(
        "gh.pr.diff",
        &json!({
            "repo": "owner/repo",
            "pr": "42",
            "base_sha": "base123",
            "head_sha": "head456"
        }),
    )
    .expect("lower gh pr diff");
    assert_eq!(
        payload,
        json!({
            "argv": [
                "gh",
                "pr",
                "diff",
                "42",
                "--repo",
                "owner/repo",
                "--patch"
            ],
            "idem_key": "gh.pr.diff:owner/repo:42:base123:head456",
            "event_spec": {
                "event_kind": "forge.pr.diff.read",
                "fields": {}
            },
            "subject": null,
            "context": {
                "pr_number": 42,
                "base_sha": "base123",
                "head_sha": "head456"
            },
            "probe": null,
            "parked": false
        })
    );
    assert_no_reserved_context(&payload, &["track_id", "artifact_path"]);
    assert_supported_event_kind(&payload);
}

#[test]
fn lowers_gh_pr_checks() {
    let payload = lower(
        "gh.pr.checks",
        &json!({
            "repo": "owner/repo",
            "pr": 42
        }),
    )
    .expect("lower gh pr checks");
    let attempt_payload = lower(
        "gh.pr.checks",
        &json!({
            "repo": "owner/repo",
            "pr": 42,
            "attempt": 7
        }),
    )
    .expect("lower gh pr checks with attempt");
    // tests/cases/forge_pr_checks.rs runs the filter through the gh shim; here the read and its
    // output probe must carry the same one.
    let jq = payload["argv"][9]
        .as_str()
        .expect("gh.pr.checks --jq filter");
    let expected_payload = |idem_key: &str| {
        json!({
            "argv": [
                "gh",
                "pr",
                "view",
                "42",
                "--repo",
                "owner/repo",
                "--json",
                "statusCheckRollup",
                "--jq",
                jq
            ],
            "idem_key": idem_key,
            "event_spec": {
                "event_kind": "forge.pr.checks",
                "fields": {
                    "conclusion": { "json_field": { "path": "/conclusion" } }
                }
            },
            "subject": null,
            "context": { "pr_number": 42 },
            "probe": {
                "probe_argv": [
                    "gh",
                    "pr",
                    "view",
                    "42",
                    "--repo",
                    "owner/repo",
                    "--json",
                    "state"
                ],
                "output_probe_argv": [
                    "gh",
                    "pr",
                    "view",
                    "42",
                    "--repo",
                    "owner/repo",
                    "--json",
                    "statusCheckRollup",
                    "--jq",
                    jq
                ]
            },
            "parked": false
        })
    };
    assert_eq!(payload, expected_payload("gh.pr.checks:owner/repo:42"));
    assert_eq!(
        attempt_payload,
        expected_payload("gh.pr.checks:owner/repo:42:7")
    );
    assert_no_reserved_context(&payload, &["track_id"]);
    assert_no_reserved_context(&attempt_payload, &["track_id"]);
    assert_supported_event_kind(&payload);
    assert_supported_event_kind(&attempt_payload);
}

#[test]
fn lowers_gh_pr_merge() {
    let payload = lower(
        "gh.pr.merge",
        &json!({
            "repo": "owner/repo",
            "pr": 42,
            "phase": "impl",
            "slice_id": "809"
        }),
    )
    .expect("lower gh pr merge");
    assert_eq!(
        payload,
        json!({
            "argv": [
                "gh",
                "pr",
                "merge",
                "42",
                "--repo",
                "owner/repo",
                "--squash",
                "--delete-branch"
            ],
            "idem_key": "gh.pr.merge:owner/repo:42",
            "event_spec": {
                "event_kind": "forge.pr.merged",
                "fields": {
                    "head_sha": { "json_field": { "path": "/headRefOid" } },
                    "merge_sha": { "json_field": { "path": "/mergeCommit/oid" } }
                }
            },
            "subject": {
                "phase": "impl",
                "slice_id": "809",
                "pr_number": 42
            },
            "context": {},
            "probe": {
                "probe_argv": [
                    "sh",
                    "-c",
                    PR_MERGE_PROBE_SCRIPT,
                    "sh",
                    "42",
                    "owner/repo"
                ],
                "output_probe_argv": [
                    "gh",
                    "pr",
                    "view",
                    "42",
                    "--repo",
                    "owner/repo",
                    "--json",
                    "headRefOid,mergeCommit"
                ]
            },
            "parked": true
        })
    );
    assert_no_reserved_context(&payload, &["track_id", "subject"]);
    assert_supported_event_kind(&payload);

    let payload = lower(
        "gh.pr.merge",
        &json!({
            "repo": "owner/repo",
            "pr": 42,
            "phase": "impl",
            "slice_id": "809",
            "expected_head_sha": "abc123"
        }),
    )
    .expect("lower gh pr merge with expected head sha");
    assert_eq!(
        payload,
        json!({
            "argv": [
                "gh",
                "pr",
                "merge",
                "42",
                "--repo",
                "owner/repo",
                "--squash",
                "--delete-branch",
                "--match-head-commit",
                "abc123"
            ],
            "idem_key": "gh.pr.merge:owner/repo:42:abc123",
            "event_spec": {
                "event_kind": "forge.pr.merged",
                "fields": {
                    "head_sha": { "json_field": { "path": "/headRefOid" } },
                    "merge_sha": { "json_field": { "path": "/mergeCommit/oid" } }
                }
            },
            "subject": {
                "phase": "impl",
                "slice_id": "809",
                "pr_number": 42
            },
            "context": {},
            "probe": {
                "probe_argv": [
                    "sh",
                    "-c",
                    PR_MERGE_HEAD_MATCH_PROBE_SCRIPT,
                    "sh",
                    "42",
                    "owner/repo",
                    "abc123"
                ],
                "output_probe_argv": [
                    "gh",
                    "pr",
                    "view",
                    "42",
                    "--repo",
                    "owner/repo",
                    "--json",
                    "headRefOid,mergeCommit"
                ]
            },
            "parked": true
        })
    );
    assert_no_reserved_context(&payload, &["track_id", "subject"]);
    assert_supported_event_kind(&payload);
}

#[test]
#[cfg(unix)]
fn pr_merge_head_match_probe_checks_state_and_head() {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = std::env::temp_dir().join(format!(
        "git-forge-head-probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time after epoch")
            .as_nanos()
    ));
    std::fs::create_dir(&temp_dir).expect("create temp dir");
    let gh_path = temp_dir.join("gh");
    std::fs::write(
        &gh_path,
        "#!/bin/sh\nprintf '%s\\n' \"$GH_FAKE_JSON\"\nexit ${GH_FAKE_STATUS:-0}\n",
    )
    .expect("write fake gh");
    let mut permissions = std::fs::metadata(&gh_path)
        .expect("fake gh metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&gh_path, permissions).expect("chmod fake gh");

    let path = format!(
        "{}:{}",
        temp_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let run_probe = |json: &str| -> i32 {
        std::process::Command::new("sh")
            .args([
                "-c",
                PR_MERGE_HEAD_MATCH_PROBE_SCRIPT,
                "sh",
                "42",
                "owner/repo",
                "abc123",
            ])
            .env("PATH", &path)
            .env("GH_FAKE_JSON", json)
            .status()
            .expect("run head-match probe")
            .code()
            .expect("probe exits normally")
    };

    assert_eq!(run_probe(r#"{"headRefOid":"abc123","state":"MERGED"}"#), 0);
    assert_eq!(run_probe(r#"{"state":"MERGED","headRefOid":"def456"}"#), 1);
    assert_eq!(run_probe(r#"{"state":"OPEN","headRefOid":"abc123"}"#), 1);

    std::fs::remove_dir_all(temp_dir).expect("remove temp dir");
}

#[test]
fn lowers_gh_issue_view() {
    let payload = lower(
        "gh.issue.view",
        &json!({
            "repo": "owner/repo",
            "issue": "808"
        }),
    )
    .expect("lower gh issue view");
    assert_eq!(
        payload,
        json!({
            "argv": [
                "gh",
                "issue",
                "view",
                "808",
                "--repo",
                "owner/repo",
                "--json",
                "body",
                "--jq",
                ".body"
            ],
            "idem_key": "gh.issue.view:v2:owner/repo:808",
            "event_spec": {
                "event_kind": "forge.issue.read",
                "fields": {}
            },
            "subject": null,
            "context": {
                "issue_number": 808
            },
            "probe": null,
            "parked": false
        })
    );
    assert_no_reserved_context(&payload, &["track_id", "artifact_path"]);
    assert_supported_event_kind(&payload);
}

#[test]
fn lowers_gh_issue_close() {
    let expected_probe_script = "out=$(gh issue view \"$1\" --repo \"$2\" --json state 2>/dev/null) || exit 3; case \"$out\" in *'\"state\":\"CLOSED\"'*) exit 0 ;; *) exit 1 ;; esac";
    let payload = lower(
        "gh.issue.close",
        &json!({
            "repo": "owner/repo",
            "issue": 808
        }),
    )
    .expect("lower gh issue close");
    assert_eq!(
        payload,
        json!({
            "argv": [
                "gh",
                "issue",
                "close",
                "808",
                "--repo",
                "owner/repo"
            ],
            "idem_key": "gh.issue.close:owner/repo:808",
            "event_spec": {
                "event_kind": "forge.issue.closed",
                "fields": {}
            },
            "subject": null,
            "context": { "issue_number": 808 },
            "probe": {
                "probe_argv": [
                    "sh",
                    "-c",
                    expected_probe_script,
                    "sh",
                    "808",
                    "owner/repo"
                ]
            },
            "parked": true
        })
    );
    assert!(payload["probe"]["output_probe_argv"].is_null());
    assert_no_reserved_context(&payload, &["track_id"]);
    assert_supported_event_kind(&payload);
}
