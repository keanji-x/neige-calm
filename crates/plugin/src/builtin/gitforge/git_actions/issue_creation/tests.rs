use super::*;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn caller(card: &str) -> ForgeCallerScope {
    ForgeCallerScope {
        plugin_id: "gitforge".into(),
        track_id: "track".into(),
        card_id: card.into(),
    }
}

#[test]
fn issue_create_identity_normalizes_and_binds_request_and_caller() {
    let args = json!({"repo":"Owner/Repo","title":"Title","body":"Body","idem":"a:b"});
    let first = create(&args, &caller("a")).unwrap();
    let mut alias = args.clone();
    alias["repo"] = json!("GITHUB.COM/owner/repo");
    assert_eq!(first, create(&alias, &caller("a")).unwrap());
    assert_ne!(
        first["probe"],
        create(&args, &caller("b")).unwrap()["probe"]
    );
    for field in ["title", "body"] {
        let mut changed = args.clone();
        changed[field] = json!("Changed");
        let second = create(&changed, &caller("a")).unwrap();
        assert_eq!(first["idem_key"], second["idem_key"]);
        assert_ne!(first["context"], second["context"]);
    }
    for repo in [
        "https://github.com/o/r",
        "../r",
        "o/../r",
        "--hostname/o/r",
        "host:123/o/r",
        "o/r?x=y",
        "/o/r",
    ] {
        let mut invalid = args.clone();
        invalid["repo"] = json!(repo);
        assert!(create(&invalid, &caller("a")).is_err(), "{repo}");
    }
    for field in ["title", "body", "idem"] {
        let mut invalid = args.clone();
        invalid[field] = json!(" \n ");
        assert!(create(&invalid, &caller("a")).is_err());
    }
    assert!(super::super::lower("gh_issue_create", &args).is_err());
}

#[test]
fn issue_create_probe_exact_match_and_complete_pagination() {
    let tmp = tempfile::tempdir().unwrap();
    let gh = tmp.path().join("gh");
    // Only serve pages. The real production probe owns matching/validation.
    std::fs::write(&gh, "#!/bin/sh\npage=1\nfor arg in \"$@\"; do case \"$arg\" in page=*) page=${arg#page=} ;; esac; done\ncat \"$(dirname \"$0\")/page$page.json\"\n").unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let payload = create(
        &json!({"repo":"owner/repo","title":"Title","body":"Body","idem":"one"}),
        &caller("a"),
    )
    .unwrap();
    let input: Value = serde_json::from_str(payload["argv"][6].as_str().unwrap()).unwrap();
    let row = json!({"number":731,"html_url":"https://github.com/owner/repo/issues/731","state":"closed","title":input["title"],"body":input["body"],"labels":[]});
    let mut pr = row.clone();
    pr["pull_request"] = json!({"url":"pull"});
    std::fs::write(
        tmp.path().join("page1.json"),
        json!(vec![pr; 100]).to_string(),
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("page2.json"),
        json!([row.clone()]).to_string(),
    )
    .unwrap();
    let argv: Vec<_> = payload["probe"]["probe_argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let output = Command::new(argv[0])
        .args(&argv[1..])
        .env(
            "PATH",
            format!(
                "{}:{}",
                tmp.path().display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({"issue_number":731,"issue_url":"https://github.com/owner/repo/issues/731"})
    );
    assert_eq!(
        payload["probe"]["probe_argv"],
        payload["probe"]["output_probe_argv"]
    );
    let mut missing_body = row.clone();
    missing_body.as_object_mut().unwrap().remove("body");
    let mut null_body = row.clone();
    null_body["body"] = Value::Null;
    for (name, inventory, expected_code) in [
        ("missing body alone", json!([missing_body.clone()]), 3),
        (
            "missing body with match",
            json!([missing_body, row.clone()]),
            3,
        ),
        ("explicit null with match", json!([null_body, row]), 0),
    ] {
        std::fs::write(tmp.path().join("page1.json"), inventory.to_string()).unwrap();
        let output = Command::new(argv[0])
            .args(&argv[1..])
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    tmp.path().display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(expected_code),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if expected_code == 0 {
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout).unwrap(),
                json!({"issue_number":731,"issue_url":"https://github.com/owner/repo/issues/731"})
            );
        } else {
            assert!(output.stdout.is_empty(), "{name}");
        }
    }
}
