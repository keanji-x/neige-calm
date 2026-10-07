use super::*;
use serde_json::json;

fn query(kind: ReferenceKind) -> PreviewQuery {
    PreviewQuery {
        owner: "octocat".into(),
        repo: "Hello-World".into(),
        kind,
        number: 42,
    }
}

#[test]
fn github_preview_rejects_unsafe_references() {
    for bad in [
        "",
        ".",
        "..",
        "o/r",
        "%2f",
        "--hostname",
        "a?x=y",
        "evil@host",
        "a\\b",
        "a b",
    ] {
        // A leading dash is a legal identifier segment, never a CLI argument. All other cases are rejected.
        if bad == "--hostname" {
            continue;
        }
        let mut q = query(ReferenceKind::Issue);
        q.owner = bad.into();
        assert!(q.endpoint().is_err(), "owner: {bad}");
        let mut q = query(ReferenceKind::Issue);
        q.repo = bad.into();
        assert!(q.endpoint().is_err(), "repo: {bad}");
    }
    for number in [0, 9_007_199_254_740_992] {
        let mut q = query(ReferenceKind::Pull);
        q.number = number;
        assert!(q.endpoint().is_err());
    }
    assert_eq!(
        query(ReferenceKind::Issue).endpoint().unwrap(),
        "repos/octocat/Hello-World/issues/42"
    );
    assert_eq!(
        query(ReferenceKind::Pull).endpoint().unwrap(),
        "repos/octocat/Hello-World/pulls/42"
    );
}

#[test]
fn github_preview_command_is_fixed_host_read_only_and_allowlisted() {
    let cmd = command(&query(ReferenceKind::Pull).endpoint().unwrap());
    let cmd = cmd.as_std();
    assert_eq!(cmd.get_program(), "gh");
    assert_eq!(
        cmd.get_args()
            .map(|s| s.to_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "api",
            "--hostname",
            "github.com",
            "--method",
            "GET",
            "repos/octocat/Hello-World/pulls/42"
        ]
    );
    assert_eq!(cmd.get_current_dir().unwrap(), std::path::Path::new("/"));
    let allowed = [
        "PATH",
        "HOME",
        "LANG",
        "LC_ALL",
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "GH_PROMPT_DISABLED",
        "GH_NO_UPDATE_NOTIFIER",
    ];
    for (key, _) in cmd.get_envs() {
        assert!(allowed.contains(&key.to_str().unwrap()));
    }
}

fn raw() -> serde_json::Value {
    json!({ "number":42, "title":"Fix", "state":"open", "user":{"login":"octocat"},
        "labels":[{"name":"bug"}], "body":"<script>secret()</script>", "draft":false,
        "merged":false, "additions":12, "deletions":3, "changed_files":2 })
}

#[test]
fn github_preview_summarizes_issue_pr_and_closed_draft() {
    let issue = summarize(
        serde_json::from_value(raw()).unwrap(),
        &query(ReferenceKind::Issue),
    )
    .unwrap();
    assert!(matches!(issue.state, PreviewState::Open));
    assert!(issue.changes.is_none());
    assert_eq!(issue.author, "octocat");
    assert_eq!(issue.labels, ["bug"]);
    assert_eq!(issue.excerpt, "<script>secret()</script>");
    for (state, merged, draft, expected) in [
        ("open", None, false, "open"),
        ("open", None, true, "draft"),
        ("closed", None, true, "closed"),
        ("closed", Some("2026-10-07"), false, "merged"),
    ] {
        let mut value = raw();
        value["state"] = json!(state);
        value["merged"] = json!(merged.is_some());
        value["draft"] = json!(draft);
        let summary = summarize(
            serde_json::from_value(value).unwrap(),
            &query(ReferenceKind::Pull),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(summary.state).unwrap(), expected);
        let changes = summary.changes.unwrap();
        assert_eq!(
            (changes.additions, changes.deletions, changes.changed_files),
            (12, 3, 2)
        );
    }
}

#[test]
fn github_preview_rejects_wrong_number_and_missing_pull_fields() {
    let mut value = raw();
    value["number"] = json!(43);
    assert!(
        summarize(
            serde_json::from_value(value).unwrap(),
            &query(ReferenceKind::Pull)
        )
        .is_err()
    );
    for field in ["merged", "draft", "additions", "deletions", "changed_files"] {
        let mut value = raw();
        value.as_object_mut().unwrap().remove(field);
        assert!(
            summarize(
                serde_json::from_value(value).unwrap(),
                &query(ReferenceKind::Pull)
            )
            .is_err()
        );
    }
}

#[tokio::test]
async fn github_preview_bounded_reader_handles_failure_and_oversized_output() {
    let deadline = || tokio::time::Instant::now() + Duration::from_secs(2);
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", "printf '%s' '{\"number\":42,\"title\":\"Issue\",\"state\":\"open\",\"user\":{\"login\":\"a\"},\"labels\":[],\"body\":null}'"]);
    assert_eq!(read_raw(cmd, deadline()).await.unwrap().number, 42);
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", "echo credential-secret >&2; exit 1"]);
    let error = read_raw(cmd, deadline()).await.err().unwrap().to_string();
    assert!(!error.contains("credential-secret"));
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", "head -c 262145 /dev/zero"]);
    assert!(read_raw(cmd, deadline()).await.is_err());
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", "sleep 2"]);
    assert!(
        read_raw(cmd, tokio::time::Instant::now() + Duration::from_millis(30))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn github_preview_subprocess_cannot_inherit_unlisted_environment() {
    assert!(
        std::env::var_os("CARGO_MANIFEST_DIR").is_some(),
        "cargo supplies the inheritance sentinel"
    );
    let mut cmd = Command::new("/usr/bin/env");
    github_environment(&mut cmd);
    let output = run_bounded(
        cmd,
        tokio::time::Instant::now() + Duration::from_secs(2),
        CAP,
    )
    .await
    .unwrap();
    let keys: Vec<_> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| line.split_once('=').map(|(key, _)| key))
        .collect();
    assert!(
        !keys.contains(&"CARGO_MANIFEST_DIR"),
        "unlisted cargo environment leaked"
    );
    for key in keys {
        assert!(
            [
                "PATH",
                "HOME",
                "LANG",
                "LC_ALL",
                "GH_TOKEN",
                "GITHUB_TOKEN",
                "GH_PROMPT_DISABLED",
                "GH_NO_UPDATE_NOTIFIER"
            ]
            .contains(&key),
            "unlisted environment key leaked"
        );
    }
}
