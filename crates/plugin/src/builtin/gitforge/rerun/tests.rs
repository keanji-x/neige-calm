use super::*;
use calm_types::forge_action::semantic_payload_hash;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

const HEAD: &str = "2222222222222222222222222222222222222222";
const OTHER: &str = "3333333333333333333333333333333333333333";

fn args() -> Value {
    json!({
        "repo": "owner/repo", "pr": 42, "head_sha": HEAD, "run_id": 100, "run_attempt": 1,
        "reason": "flaky network in shard 3", "idem": "r-1"
    })
}

fn payload(args: &Value) -> PluginForgePayload {
    Rerun::parse(args).expect("valid rerun").payload()
}

fn with(key: &str, value: Value) -> Value {
    let mut args = args();
    args[key] = value;
    args
}

#[test]
fn gh_run_rerun_lowers_an_approval_gated_payload() {
    let payload = payload(&args());
    assert_eq!(
        payload.idem_key,
        r#"gh.run.rerun_failed:v1:["owner/repo",100,1,"r-1"]"#
    );
    assert_eq!(
        payload.argv,
        [
            "sh",
            "-c",
            RERUN_FAILED_SCRIPT,
            "sh",
            "owner/repo",
            "42",
            HEAD,
            "100",
            "1"
        ]
    );
    assert!(RERUN_FAILED_SCRIPT.starts_with(include_str!("../git_actions/actions_prelude.sh")));
    assert_eq!(
        Value::Object(payload.context.clone()),
        json!({ "pr": 42, "head_sha": HEAD, "reason": "flaky network in shard 3" })
    );
    assert!(payload.subject.is_none());
    assert!(
        payload.probe.is_none(),
        "no landed probe: a lost result is unknown"
    );
    assert!(!payload.parked && !payload.replay_any_version);
    assert_eq!(
        payload.approval.expect("approval-gated").title,
        format!(
            "Re-run failed jobs of run 100 attempt 1, owner/repo PR #42 head {HEAD}? Reason: \
             flaky network in shard 3"
        )
    );
}

/// The approval binds the key and the payload hash: the key names the run attempt and the idem,
/// and the hash also covers the PR, head and reason.
#[test]
fn gh_run_rerun_key_and_hash_bind_every_argument() {
    let base = payload(&args());
    for key in ["repo", "run_id", "run_attempt", "idem"] {
        let changed = payload(&with(
            key,
            match key {
                "repo" => json!("owner/other"),
                "idem" => json!("r-2"),
                _ => json!(2),
            },
        ));
        assert_ne!(changed.idem_key, base.idem_key, "{key}");
    }
    for (key, value) in [
        ("pr", json!(43)),
        ("head_sha", json!(OTHER)),
        ("reason", json!("another reason")),
    ] {
        let changed = payload(&with(key, value));
        assert_eq!(changed.idem_key, base.idem_key, "{key}");
        assert_ne!(
            semantic_payload_hash(&changed).unwrap(),
            semantic_payload_hash(&base).unwrap(),
            "{key}"
        );
    }
    assert_eq!(
        payload(&with("head_sha", json!(HEAD.to_ascii_uppercase()))).argv[6],
        HEAD,
        "the head is compared in GitHub's lowercase"
    );
}

#[test]
fn gh_run_rerun_refuses_invalid_arguments() {
    for (key, value) in [
        ("pr", json!(0)),
        ("run_id", json!(0)),
        ("run_attempt", json!(0)),
        ("head_sha", json!("abc")),
        ("reason", json!(" ")),
        ("reason", json!("x".repeat(MAX_REASON_CHARS + 1))),
        ("repo", json!("")),
        ("idem", json!("")),
    ] {
        assert!(
            Rerun::parse(&with(key, value.clone())).is_err(),
            "{key}={value}"
        );
    }
}

/// The descriptor is Planner-only, listed and callable.
#[test]
fn gh_run_rerun_is_a_planner_only_tool() {
    let tool = descriptor();
    assert_eq!(tool.name, RERUN_FAILED);
    assert_eq!(tool.roles, &[CardRole::Planner]);
    assert_eq!(tool.listed_for, &[CardRole::Planner]);
}

/// A fake `gh`: `repo view` resolves the identity, `pr view` prints `$GH_PR_HEAD`, the run read
/// filters `run.json`, and the POST is logged and exits `$GH_POST_STATUS`.
const FAKE_GH: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$GH_FAKE_DIR/calls"
case "$1:$2" in
  repo:view) printf 'github.com\nowner/repo\n' ;;
  pr:view) printf '%s\n' "$GH_PR_HEAD" ;;
  api:*)
    case "$*" in
      *'-X POST'*'/rerun-failed-jobs'*) printf 'post\n' >> "$GH_FAKE_DIR/posts"; exit "$GH_POST_STATUS" ;;
      *'repos/owner/repo/actions/runs/100 '*)
        filter=; prev=; for arg in "$@"; do [ "$prev" = --jq ] && filter=$arg; prev=$arg; done
        jq -c "$filter" "$GH_FAKE_DIR/run.json" ;;
      *) exit 1 ;;
    esac ;;
  *) exit 2 ;;
esac
"#;

struct Ran {
    code: Option<i32>,
    posts: usize,
}

/// Run the lowered script against [`FAKE_GH`] with the PR at `pr_head` and the run as `run`.
fn run(pr_head: &str, run: Value, post_status: i32) -> Ran {
    let dir = tempfile::tempdir().expect("tempdir");
    let gh = dir.path().join("gh");
    std::fs::write(&gh, FAKE_GH).expect("write fake gh");
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    std::fs::write(dir.path().join("run.json"), run.to_string()).expect("write run");
    let argv = payload(&args()).argv;
    let output = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .env("PATH", path_with(dir.path()))
        .env("GH_FAKE_DIR", dir.path())
        .env("GH_PR_HEAD", pr_head)
        .env("GH_POST_STATUS", post_status.to_string())
        .output()
        .expect("run the rerun script");
    let posts = std::fs::read_to_string(dir.path().join("posts"))
        .map(|posts| posts.lines().count())
        .unwrap_or(0);
    Ran {
        code: output.status.code(),
        posts,
    }
}

fn path_with(dir: &Path) -> String {
    format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

fn run_json(head: &str, attempt: u64, status: &str, repo: &str) -> Value {
    json!({
        "id": 100, "head_sha": head, "run_attempt": attempt, "status": status,
        "repository": { "full_name": repo }
    })
}

#[test]
fn gh_run_rerun_posts_once_for_the_approved_completed_attempt() {
    let ran = run(HEAD, run_json(HEAD, 1, "completed", "owner/repo"), 0);
    assert_eq!((ran.code, ran.posts), (Some(0), 1));
    let failed = run(HEAD, run_json(HEAD, 1, "completed", "owner/repo"), 1);
    assert_eq!((failed.code, failed.posts), (Some(1), 1), "a failed POST");
}

#[test]
fn gh_run_rerun_refuses_a_moved_head_or_another_attempt_without_posting() {
    let moved = run(OTHER, run_json(HEAD, 1, "completed", "owner/repo"), 0);
    assert_eq!((moved.code, moved.posts), (Some(20), 0), "PR head moved");
    for (label, run_state) in [
        (
            "advanced attempt",
            run_json(HEAD, 2, "completed", "owner/repo"),
        ),
        (
            "still running",
            run_json(HEAD, 1, "in_progress", "owner/repo"),
        ),
        (
            "another head",
            run_json(OTHER, 1, "completed", "owner/repo"),
        ),
        (
            "another repository",
            run_json(HEAD, 1, "completed", "owner/fork"),
        ),
    ] {
        let ran = run(HEAD, run_state, 0);
        assert_eq!((ran.code, ran.posts), (Some(21), 0), "{label}");
    }
    let unreadable = run(HEAD, json!(null), 0);
    assert_eq!(
        (unreadable.code, unreadable.posts),
        (Some(21), 0),
        "a run without the fields"
    );
}
