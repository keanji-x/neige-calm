use super::*;
use crate::builtin_plugins::dev::git_actions::lower;

fn args() -> Value {
    json!({"repo":"owner/repo", "issue":42, "body":"A progress update", "idem":"update-1"})
}

#[test]
fn issue_comment_is_parked_and_body_sensitive() {
    let original = lower("gh.issue.comment", &args()).unwrap();
    assert_eq!(original["parked"], true);
    assert_eq!(
        &original["argv"].as_array().unwrap()[..7],
        &json!([
            "gh",
            "issue",
            "comment",
            "42",
            "--repo",
            "owner/repo",
            "--body"
        ])
        .as_array()
        .unwrap()[..]
    );
    let posted = original["argv"][7].as_str().unwrap();
    assert!(posted.starts_with("A progress update\n\n<!-- neige:issue-comment:"));
    assert_eq!(original, lower("gh.issue.comment", &args()).unwrap());
    let mut changed = args();
    changed["body"] = json!("Different update");
    let other = lower("gh.issue.comment", &changed).unwrap();
    assert_eq!(original["idem_key"], other["idem_key"]);
    assert_ne!(original["context"], other["context"]);
    assert_ne!(original["probe"], other["probe"]);
    changed["idem"] = json!("update-2");
    assert_ne!(
        original["idem_key"],
        lower("gh.issue.comment", &changed).unwrap()["idem_key"]
    );
}

#[test]
fn issue_comment_rejects_invalid_arguments() {
    for field in ["repo", "issue", "body", "idem"] {
        let mut input = args();
        input.as_object_mut().unwrap().remove(field);
        assert!(lower("gh.issue.comment", &input).is_err(), "{field}");
    }
    for field in ["repo", "body", "idem"] {
        for bad in [json!(" \n\t"), Value::Null, json!(42)] {
            let mut input = args();
            input[field] = bad;
            assert!(lower("gh.issue.comment", &input).is_err(), "{field}");
        }
    }
    for issue in [json!(0), json!(-1), json!(1.5), Value::Null] {
        let mut input = args();
        input["issue"] = issue;
        assert!(lower("gh.issue.comment", &input).is_err());
        assert!(lower("gh.issue.comments", &input).is_err());
    }
}

#[test]
fn issue_discussion_and_body_reads_can_refresh() {
    for tool in ["gh.issue.comments", "gh.issue.view"] {
        let original = lower(tool, &args()).unwrap();
        assert_eq!(original["parked"], false);
        assert_eq!(original["probe"], Value::Null);
        let mut input = args();
        input["attempt"] = json!(2);
        let fresh = lower(tool, &input).unwrap();
        assert_ne!(original["idem_key"], fresh["idem_key"]);
        assert_eq!(original["argv"], fresh["argv"]);
        assert_eq!(fresh, lower(tool, &input).unwrap());
    }
}

#[test]
fn issue_comment_probe_matches_exact_body_and_preserves_unknown() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let gh = tmp.path().join("gh");
    // Execute the real generated jq expression against controlled server data.
    std::fs::write(
        &gh,
        "#!/bin/sh\n[ -f \"$2.fail\" ] && exit 1\njq \"$9\" \"$2.json\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut input = args();
    input["body"] = json!("Quotes \" \\ newline\n$(touch forbidden) `touch forbidden` 你好");
    let payload = lower("gh.issue.comment", &input).unwrap();
    let posted = payload["argv"][7].as_str().unwrap();
    let probe = payload["probe"]["probe_argv"].as_array().unwrap();
    let run = |data: Value| {
        std::fs::write(tmp.path().join("view.json"), data.to_string()).unwrap();
        let out = std::process::Command::new("sh")
            .args(probe.iter().skip(1).map(|v| v.as_str().unwrap()))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    tmp.path().display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .current_dir(tmp.path())
            .output()
            .unwrap();
        assert!(!tmp.path().join("forbidden").exists());
        out.status.code().unwrap()
    };
    assert_eq!(run(json!({"comments":[{"body":posted}]})), 0);
    assert_eq!(run(json!({"comments":[{"body":input["body"]}]})), 1);
    assert_eq!(
        run(json!({"comments":[{"body":format!("prefix{posted}")}]})),
        1
    );
    assert_eq!(run(json!({"comments":[]})), 1);
    assert_eq!(run(json!({"comments":null})), 3);
    std::fs::write(tmp.path().join("view.fail"), "fail").unwrap();
    assert_eq!(run(json!({"comments":[{"body":posted}]})), 3);
}
