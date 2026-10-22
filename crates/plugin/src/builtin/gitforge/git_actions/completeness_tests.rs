//! Exercise lowered production scripts; the fake forge only supplies API responses.
use super::*;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

struct Forge {
    dir: tempfile::TempDir,
}

impl Forge {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let gh = dir.path().join("gh");
        std::fs::write(
            &gh,
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$FORGE_TEST_DIR/calls"
case "$1:$2" in
  repo:view) printf 'github.com\nowner/repo\n' ;;
  api:graphql)
    n=$(cat "$FORGE_TEST_DIR/count")
    n=$((n+1)); printf '%s' "$n" > "$FORGE_TEST_DIR/count"
    if [ -f "$FORGE_TEST_DIR/page.$n" ]; then
      cat "$FORGE_TEST_DIR/page.$n"
    else cat "$FORGE_TEST_DIR/page.1"; fi ;;
  pr:view)
    case "$*" in
      *'--jq .headRefOid'*) cat "$FORGE_TEST_DIR/head" ;;
      *) if [ -f "$FORGE_TEST_DIR/pr" ]; then cat "$FORGE_TEST_DIR/pr"; else
           n=$(cat "$FORGE_TEST_DIR/count"); file="$FORGE_TEST_DIR/page.$n"
           [ -f "$file" ] || file="$FORGE_TEST_DIR/page.1"
           jq -c '.[0].data.repository.pullRequest | {headRefOid,mergeStateStatus}' "$file"
         fi ;;
    esac ;;
  pr:merge) printf 'merged\n' ;;
  *) exit 1 ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let fx = Self { dir };
        fx.write("count", "0");
        fx.write("head", "abc");
        fx
    }

    fn write(&self, name: &str, value: &str) {
        std::fs::write(self.dir.path().join(name), value).unwrap();
    }

    fn page(&self, n: usize, value: &Value) {
        self.write(&format!("page.{n}"), &value.to_string());
    }

    fn run(&self, argv: &Value) -> std::process::Output {
        let argv: Vec<String> = serde_json::from_value(argv.clone()).unwrap();
        Command::new("timeout")
            .args(["10", &argv[0]])
            .args(&argv[1..])
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.dir.path().display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("FORGE_TEST_DIR", self.dir.path())
            .output()
            .unwrap()
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("calls")).unwrap()
    }
}

fn page(state: &str, suite_status: &str) -> Value {
    json!([{"data":{"repository":{"pullRequest":{
        "headRefOid":"abc", "mergeable":"MERGEABLE", "mergeStateStatus":state,
        "commits":{"nodes":[{"commit":{
            "oid":"abc",
            "checkSuites":{"totalCount":1,"nodes":[{"id":"suite","status":suite_status}],"suitePageInfo":{"hasNextPage":false}},
            "statusCheckRollup":{"contexts":{"totalCount":1,
                "nodes":[{"__typename":"CheckRun","id":"lint","name":"lint","status":"COMPLETED","conclusion":"SUCCESS"}],
                "pageInfo":{"hasNextPage":false,"endCursor":null}}}
        }}]}
    }}}}])
}

fn checks() -> Value {
    lower(
        "gh_pr_checks",
        &json!({"repo":"owner/repo","pr":42,"wait_for_all":true}),
    )
    .unwrap()
}

#[test]
fn ci_completeness_wait_does_not_report_first_check_as_green() {
    // CLEAN alone is insufficient: a workflow can still be creating optional jobs.
    for (state, suite) in [("BLOCKED", "COMPLETED"), ("CLEAN", "IN_PROGRESS")] {
        let fx = Forge::new();
        fx.page(1, &page(state, suite));
        fx.page(2, &page("CLEAN", "COMPLETED"));
        let mut argv = checks()["argv"].clone();
        argv[6] = json!(0.to_string());
        let out = fx.run(&argv);
        assert!(out.status.success(), "{out:?}");
        assert_eq!(
            std::fs::read_to_string(fx.dir.path().join("count")).unwrap(),
            "2",
            "premature success: {state}/{suite}: {out:?}"
        );
    }
}

#[test]
fn ci_completeness_read_rejects_missing_or_duplicate_contexts() {
    for duplicate in [false, true] {
        let fx = Forge::new();
        let mut p = page("CLEAN", "COMPLETED");
        let contexts = &mut p[0]["data"]["repository"]["pullRequest"]["commits"]["nodes"][0]["commit"]
            ["statusCheckRollup"]["contexts"];
        contexts["totalCount"] = json!(2);
        if duplicate {
            let node = contexts["nodes"][0].clone();
            contexts["nodes"].as_array_mut().unwrap().push(node);
        }
        fx.page(1, &p);
        let out = fx.run(&checks()["probe"]["output_probe_argv"]);
        assert!(
            !out.status.success(),
            "incomplete evidence accepted: {out:?}"
        );
        assert!(out.stdout.is_empty());
    }
}

#[test]
fn ci_completeness_merge_rejects_non_clean_or_running_suite() {
    for (state, suite) in [
        ("BLOCKED", "COMPLETED"),
        ("UNKNOWN", "COMPLETED"),
        ("CLEAN", "IN_PROGRESS"),
    ] {
        let fx = Forge::new();
        fx.page(1, &page(state, suite));
        fx.write(
            "pr",
            &json!({"headRefOid":"abc","mergeStateStatus":state}).to_string(),
        );
        let payload = lower("gh_pr_merge", &json!({"repo":"owner/repo","pr":42})).unwrap();
        let out = fx.run(&payload["argv"]);
        assert!(
            !out.status.success(),
            "unsafe merge allowed: {state}/{suite}: {out:?}"
        );
        assert!(!fx.calls().lines().any(|s| s.starts_with("pr merge")));
    }
}

#[test]
fn ci_completeness_merge_pins_head_even_without_caller_expectation() {
    let fx = Forge::new();
    fx.page(1, &page("CLEAN", "COMPLETED"));
    let payload = lower("gh_pr_merge", &json!({"repo":"owner/repo","pr":42})).unwrap();
    let out = fx.run(&payload["argv"]);
    assert!(out.status.success(), "{out:?}");
    assert!(
        fx.calls()
            .lines()
            .any(|s| s.starts_with("pr merge") && s.contains("--match-head-commit abc")),
        "{}",
        fx.calls()
    );
}

#[test]
fn ci_completeness_read_rejects_truncated_suites_and_api_errors() {
    for kind in [
        "suite_count",
        "suite_page",
        "missing_suites",
        "errors",
        "changing_count",
        "changing_state",
    ] {
        let fx = Forge::new();
        let mut p = page("CLEAN", "COMPLETED");
        let commit =
            &mut p[0]["data"]["repository"]["pullRequest"]["commits"]["nodes"][0]["commit"];
        match kind {
            "suite_count" => commit["checkSuites"]["totalCount"] = json!(2),
            "suite_page" => commit["checkSuites"]["suitePageInfo"]["hasNextPage"] = json!(true),
            "missing_suites" => commit["checkSuites"] = Value::Null,
            "errors" => p[0]["errors"] = json!([{"message":"partial response"}]),
            _ => {
                let mut next = p[0].clone();
                p[0]["data"]["repository"]["pullRequest"]["commits"]["nodes"][0]["commit"]["statusCheckRollup"]
                    ["contexts"]["pageInfo"]["hasNextPage"] = json!(true);
                if kind == "changing_count" {
                    next["data"]["repository"]["pullRequest"]["commits"]["nodes"][0]["commit"]["statusCheckRollup"]
                        ["contexts"]["totalCount"] = json!(2);
                } else {
                    next["data"]["repository"]["pullRequest"]["mergeStateStatus"] =
                        json!("BLOCKED");
                }
                p.as_array_mut().unwrap().push(next);
            }
        }
        fx.page(1, &p);
        let out = fx.run(&checks()["probe"]["output_probe_argv"]);
        assert!(!out.status.success(), "{kind}: {out:?}");
        assert!(out.stdout.is_empty());
    }
}

#[test]
fn ci_completeness_merge_rejects_failed_checks_empty_and_head_changes() {
    for kind in [
        "failed",
        "empty",
        "head_move",
        "explicit_head",
        "state_move",
    ] {
        let fx = Forge::new();
        let mut p = page("CLEAN", "COMPLETED");
        let contexts = &mut p[0]["data"]["repository"]["pullRequest"]["commits"]["nodes"][0]["commit"]
            ["statusCheckRollup"]["contexts"];
        let mut args = json!({"repo":"owner/repo","pr":42});
        match kind {
            "failed" => contexts["nodes"][0]["conclusion"] = json!("FAILURE"),
            "empty" => {
                contexts["nodes"] = json!([]);
                contexts["totalCount"] = json!(0);
            }
            "head_move" => fx.write("pr", r#"{"headRefOid":"new","mergeStateStatus":"CLEAN"}"#),
            "state_move" => fx.write("pr", r#"{"headRefOid":"abc","mergeStateStatus":"BLOCKED"}"#),
            "explicit_head" => args["expected_head_sha"] = json!("other"),
            _ => unreachable!(),
        }
        fx.page(1, &p);
        let out = fx.run(&lower("gh_pr_merge", &args).unwrap()["argv"]);
        assert!(!out.status.success(), "{kind}: {out:?}");
        assert!(
            !fx.calls().lines().any(|s| s.starts_with("pr merge")),
            "{kind}"
        );
    }
}

#[test]
fn ci_completeness_read_accepts_complete_multiple_pages() {
    let fx = Forge::new();
    let mut p = page("CLEAN", "COMPLETED");
    let mut next = p[0].clone();
    let path = "/data/repository/pullRequest/commits/nodes/0/commit/statusCheckRollup/contexts";
    let first = p[0].pointer_mut(path).unwrap();
    first["totalCount"] = json!(2);
    first["pageInfo"] = json!({"hasNextPage":true,"endCursor":"cursor"});
    let second = next.pointer_mut(path).unwrap();
    second["totalCount"] = json!(2);
    second["nodes"][0]["id"] = json!("test");
    second["nodes"][0]["name"] = json!("test");
    p.as_array_mut().unwrap().push(next);
    fx.page(1, &p);
    let out = fx.run(&checks()["probe"]["output_probe_argv"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["conclusion"],
        "success"
    );
}

#[test]
fn ci_completeness_query_reserves_pageinfo_for_context_pagination() {
    // gh's findEndCursor uses the first pageInfo hasNextPage and endCursor it sees.
    // Suite metadata must not steal the contexts cursor (cli/cli pagination.go).
    assert!(PR_CHECKS_QUERY.contains("suitePageInfo:pageInfo{hasNextPage}"));
    assert_eq!(PR_CHECKS_QUERY.matches(" pageInfo{").count(), 1);
}
