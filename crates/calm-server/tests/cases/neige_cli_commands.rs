//! #1801 `neige/cli` against a real kernel socket: output and authorization equal a direct `tools/call`
//! on the same token, the `--force` gates refuse before any tool runs, and help is served by the kernel.

#![cfg(unix)]

use crate::support;

use calm_server::mcp_server::cli::help::{self, HelpRequest};
use calm_server::mcp_server::cli::render::{Render, render};
use calm_server::model::CardRole;
use serde_json::{Value, json};
use support::mcp::{
    CardBoot, boot_with_role, call_tool_card_bound, cli_output, neige_cli_via_socket,
};
use support::track_vcs_seed::seed_linear_commits;

const LS: Render = Render::Ls {
    long: false,
    reports: false,
};

async fn cli(boot: &CardBoot, argv: &[&str]) -> (String, String, i64) {
    cli_output(&neige_cli_via_socket(&boot.socket_path, &boot.raw_token, argv).await)
}

async fn direct(boot: &CardBoot, tool: &str, args: Value) -> Value {
    call_tool_card_bound(&boot.socket_path, &boot.raw_token, tool, args).await
}

async fn direct_ok(boot: &CardBoot, tool: &str, args: Value) -> Value {
    let resp = direct(boot, tool, args).await;
    let value = resp["result"]["structuredContent"].clone();
    assert!(!value.is_null(), "{tool} failed directly: {resp:#?}");
    value
}

async fn commit_count(boot: &CardBoot) -> i64 {
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    sqlx::query_scalar("SELECT COUNT(*) FROM track_vcs_commits WHERE track_id = ?1")
        .bind(boot.track_id.as_str())
        .fetch_one(&pool)
        .await
        .expect("count commits")
}

fn commit(boot: &CardBoot, index: usize) -> String {
    format!("{}-admin-commit-{index}", boot.track_id.as_str())
}

#[tokio::test]
async fn cli_output_equals_direct_tool_call() {
    let boot = boot_with_role(CardRole::Planner).await;
    seed_linear_commits(&boot.repo.sqlite_pool().unwrap(), &boot.track_id, 3).await;
    let (c0, c2) = (commit(&boot, 0), commit(&boot, 2));

    // (argv without --json, tool, direct args, render)
    let cases: Vec<(Vec<&str>, &str, Value, Render)> = vec![
        (vec!["ls"], "calm.track.ls", json!({}), LS),
        (
            vec!["ls", "cards"],
            "calm.track.ls",
            json!({ "path": "cards" }),
            LS,
        ),
        (vec!["state"], "calm.track.state", json!({}), Render::State),
        (
            vec!["diff", &c0, &c2],
            "calm.track.diff",
            json!({ "from": c0, "to": c2 }),
            Render::Diff,
        ),
        (
            vec!["diff", &c0, "--path", "file-2.txt"],
            "calm.track.diff",
            json!({ "from": c0, "path": "file-2.txt" }),
            Render::Diff,
        ),
        (vec!["log"], "calm.track.log", json!({}), Render::Log),
        (
            vec!["log", "--limit", "0", "--include-empty"],
            "calm.track.log",
            json!({ "limit": 0, "include_empty": true }),
            Render::Log,
        ),
        (
            vec!["cat", "track.json"],
            "calm.track.cat",
            json!({ "path": "track.json" }),
            Render::Content,
        ),
        (
            vec!["cat-at", &c2, "file-2.txt"],
            "calm.track.cat_at",
            json!({ "commit": c2, "path": "file-2.txt" }),
            Render::Content,
        ),
    ];
    for (argv, tool, args, how) in cases {
        let value = direct_ok(&boot, tool, args).await;
        for json_flag in [false, true] {
            let mut full = argv.clone();
            if json_flag {
                full.insert(0, "--json");
            }
            let (stdout, stderr, exit) = cli(&boot, &full).await;
            assert_eq!((exit, stderr.as_str()), (0, ""), "{full:?}");
            let want = match (json_flag, how) {
                (true, Render::Ls { .. } | Render::State | Render::Diff | Render::Log) => {
                    format!("{value}\n")
                }
                _ => render(how, tool, json_flag, &value).expect("direct result renders"),
            };
            assert_eq!(stdout, want, "{full:?}");
        }
    }

    // The rendered shapes themselves, end to end.
    let (ls, _, _) = cli(&boot, &["ls"]).await;
    assert!(
        ls.lines().any(|l| l == "- track.json") && ls.lines().any(|l| l == "d cards/"),
        "{ls}"
    );
    let (diff, _, _) = cli(&boot, &["diff", &c0, &c2]).await;
    assert!(
        diff.contains("file-0.txt deleted\n") && diff.contains("file-2.txt new\n"),
        "{diff}"
    );
    let (log, _, _) = cli(&boot, &["log"]).await;
    assert!(log.contains(" event=3 commit 2\n"), "{log}");
    let (cat, _, _) = cli(&boot, &["cat", "track.json"]).await;
    assert!(
        cat.starts_with("{\n  \""),
        "track.json is pretty-printed: {cat}"
    );
}

#[tokio::test]
async fn cli_state_text_is_one_fact_per_line() {
    let boot = boot_with_role(CardRole::Planner).await;
    sqlx::query(concat!(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,worker_card_id,",
        "created_at_ms,updated_at_ms) VALUES('fix-login-1',?1,'fix-login','codex','g','{}','running',?2,1,1)"
    ))
    .bind(boot.track_id.as_str())
    .bind(&boot.other_card_id)
    .execute(&boot.repo.sqlite_pool().unwrap())
    .await
    .unwrap();

    let (text, stderr, exit) = cli(&boot, &["state"]).await;
    assert_eq!((exit, stderr.as_str()), (0, ""), "{text}");
    let fact = |label: &str| -> Vec<&str> {
        text.lines()
            .filter(|line| line.split_whitespace().next() == Some(label))
            .collect()
    };
    assert_eq!(
        fact("track"),
        vec![format!("track      {}", boot.track_id.as_str())]
    );
    assert_eq!(fact("title"), vec!["title      mcp-test"]);
    assert_eq!(
        text.lines().filter(|l| l.contains("closed_at")).count(),
        1,
        "{text}"
    );
    assert_eq!(fact("closed_at"), vec!["closed_at  -"]);
    assert_eq!(
        fact("you"),
        vec![format!("you        {} planner", boot.card_id)]
    );
    assert_eq!(fact("report"), vec!["report     none"]);
    assert!(fact("tasks").is_empty(), "{text}");
    assert_eq!(fact("live").len(), 1, "{text}");
    let own: Vec<&str> = text
        .lines()
        .filter(|l| l.contains(&boot.card_id) && !l.starts_with("you"))
        .collect();
    assert_eq!(own.len(), 1, "{text}");
    assert!(
        own[0].contains(" planner ") && own[0].ends_with("  (you)"),
        "{text}"
    );
    let worker: Vec<&str> = text
        .lines()
        .filter(|l| l.contains(&boot.other_card_id))
        .collect();
    assert_eq!(worker.len(), 1, "{text}");
    assert!(
        worker[0].contains(" worker ") && worker[0].ends_with("  task fix-login"),
        "{text}"
    );
    assert!(!text.contains('{'), "text, not JSON: {text}");

    sqlx::query("UPDATE worker_sessions SET state = 'exited' WHERE card_id = ?1")
        .bind(&boot.other_card_id)
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let (text, _, exit) = cli(&boot, &["state"]).await;
    assert_eq!(exit, 0, "{text}");
    let live: Vec<&str> = text
        .lines()
        .skip_while(|l| !l.starts_with("live"))
        .collect();
    assert_eq!(live.len(), 1, "an exited worker is not live: {text}");
    assert!(
        live[0].contains(&boot.card_id) && live[0].ends_with("  (you)"),
        "{text}"
    );
}

/// `tool` refused `argv` exactly as it refuses the direct call on the same token, in both error formats.
async fn assert_same_refusal(boot: &CardBoot, argv: &[&str], tool: &str, args: Value) {
    let resp = direct(boot, tool, args).await;
    let error = resp
        .get("error")
        .unwrap_or_else(|| panic!("direct {tool} was not refused: {resp:#?}"));
    let (message, code) = (
        error["message"].as_str().unwrap(),
        error["code"].as_i64().unwrap(),
    );

    let (stdout, stderr, exit) = cli(boot, argv).await;
    assert_eq!(exit, 4, "{argv:?}: {stderr}");
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        format!("neige: {tool}: {message} (code {code})\n"),
        "{argv:?}"
    );

    let mut json_argv = vec!["--json"];
    json_argv.extend_from_slice(argv);
    let (_, stderr, exit) = cli(boot, &json_argv).await;
    assert_eq!(exit, 4);
    let parsed: Value = serde_json::from_str(&stderr).expect("--json error is JSON");
    assert_eq!(
        parsed["error"]["message"],
        json!(format!("{tool}: {message} (code {code})"))
    );
    assert_eq!(
        parsed["error"]["detail"],
        json!({ "kind": "rpc", "method": tool, "rpc_error": error })
    );
}

#[tokio::test]
async fn cli_authorization_equals_direct_call() {
    let worker = boot_with_role(CardRole::Worker).await;
    assert_same_refusal(
        &worker,
        &["vacuum", "--force"],
        "calm.admin.vacuum",
        json!({}),
    )
    .await;

    let planner = boot_with_role(CardRole::Planner).await;
    assert_same_refusal(
        &planner,
        &["task-completed", "--idempotency-key", "k"],
        "calm.task.complete",
        json!({ "idempotency_key": "k" }),
    )
    .await;
}

#[tokio::test]
async fn force_gate_refuses_before_the_tool() {
    let boot = boot_with_role(CardRole::Planner).await;
    seed_linear_commits(&boot.repo.sqlite_pool().unwrap(), &boot.track_id, 5).await;
    let track = boot.track_id.as_str().to_string();

    let (stdout, stderr, exit) =
        cli(&boot, &["track-gc", "--track-id", &track, "--keep", "1"]).await;
    assert_eq!(
        (exit, stdout.as_str(), stderr.as_str()),
        (
            1,
            "",
            "neige: track-gc is destructive (prunes VCS history + sweeps objects); re-run with --force to confirm\n"
        ),
        "track-gc without --force"
    );
    assert_eq!(
        commit_count(&boot).await,
        5,
        "track-gc pruned without --force"
    );

    let (stdout, stderr, exit) = cli(&boot, &["vacuum"]).await;
    assert_eq!(
        (exit, stdout.as_str(), stderr.as_str()),
        (
            1,
            "",
            "neige: vacuum takes a write lock on the DB and must run in a quiet maintenance window; re-run with --force to confirm\n"
        ),
        "vacuum without --force"
    );

    // The confirmed forms reach the tool.
    let (stdout, _, exit) = cli(
        &boot,
        &["track-gc", "--track-id", &track, "--keep", "1", "--dry-run"],
    )
    .await;
    assert_eq!(exit, 0);
    assert_eq!(
        serde_json::from_str::<Value>(&stdout).unwrap()["pruned_commits"],
        json!(4)
    );
    assert_eq!(commit_count(&boot).await, 5);
    let (stdout, _, exit) = cli(&boot, &["vacuum", "--force"]).await;
    assert_eq!((exit, stdout.as_str()), (0, "{\"ok\":true}\n"));
}

#[tokio::test]
async fn help_and_unknown_commands_are_served_by_the_kernel() {
    let boot = boot_with_role(CardRole::Worker).await;
    let root = help::render(HelpRequest::Root).unwrap();
    assert!(
        root.starts_with("neige\n\n"),
        "no version in the kernel's help: {root}"
    );
    assert!(
        root.contains("--version  (only argument): print the forwarder version"),
        "{root}"
    );
    for (argv, want) in [
        (&["--help"][..], root.clone()),
        (&["-h"][..], root.clone()),
        (&["help"][..], root.clone()),
        (
            &["help", "cat"][..],
            help::render(HelpRequest::Command("cat")).unwrap(),
        ),
        (
            &["cat", "--help"][..],
            help::render(HelpRequest::Command("cat")).unwrap(),
        ),
        (
            &["--json", "track-gc", "-h"][..],
            help::render(HelpRequest::Command("track-gc")).unwrap(),
        ),
    ] {
        assert_eq!(cli(&boot, argv).await, (want, String::new(), 0), "{argv:?}");
    }
    assert!(
        help::render(HelpRequest::Command("track-gc"))
            .unwrap()
            .contains(&format!(
                "[default: {}]",
                calm_server::track_vcs::DEFAULT_TRACK_HISTORY_PRUNE_KEEP
            ))
    );

    let unknown = format!("neige: {}\n", help::unknown_command_message("snow"));
    for argv in [
        &["snow"][..],
        &["snow", "--help"][..],
        &["help", "snow"][..],
    ] {
        assert_eq!(
            cli(&boot, argv).await,
            (String::new(), unknown.clone(), 1),
            "{argv:?}"
        );
    }
    let (_, stderr, exit) = cli(&boot, &["--json", "snow"]).await;
    assert_eq!(exit, 1);
    let parsed: Value = serde_json::from_str(&stderr).expect("JSON usage error");
    assert_eq!(
        parsed["error"]["message"],
        json!(help::unknown_command_message("snow"))
    );
    assert_eq!(parsed["error"]["detail"]["kind"], json!("usage"));
    assert_eq!(
        parsed["error"]["detail"]["usage"],
        json!("neige [--json] <command> [options]")
    );
    let (_, stderr, exit) = cli(&boot, &[]).await;
    assert_eq!(exit, 1);
    assert!(
        stderr.starts_with("neige: missing command; expected `ls`"),
        "{stderr}"
    );
}

/// Give the boot track the report card every production track is created with.
async fn add_report_card(boot: &CardBoot) {
    boot.repo
        .card_create(calm_server::model::NewCard {
            track_id: boot.track_id.clone(),
            title: None,
            kind: "track-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(calm_server::track_report::TrackReportPayload::initial())
                .unwrap(),
        })
        .await
        .unwrap();
}

/// #1838: `neige tag report.md` end to end. The Planner changes tags; only `report.md` takes them.
#[tokio::test]
async fn cli_tag_round_trips_the_planners_report_tags() {
    let boot = boot_with_role(CardRole::Planner).await;
    add_report_card(&boot).await;
    let steps: [(&[&str], &str); 5] = [
        (&["tag", "report.md"], "\n"),
        (
            &["tag", "report.md", "--add", "认证", "--add", "架构"],
            "认证 架构\n",
        ),
        (
            &["tag", "report.md", "--add", "排障", "--add", "认证"],
            "认证 架构 排障\n",
        ),
        (&["tag", "report.md", "--remove", "排障"], "认证 架构\n"),
        (&["tag", "report.md"], "认证 架构\n"),
    ];
    for (argv, want) in steps {
        let (stdout, stderr, exit) = cli(&boot, argv).await;
        assert_eq!(
            (exit, stderr.as_str(), stdout.as_str()),
            (0, "", want),
            "{argv:?}"
        );
    }
    let (stdout, _, exit) = cli(&boot, &["--json", "tag", "report.md"]).await;
    assert_eq!(
        (exit, stdout),
        (0, "{\"tags\":[\"认证\",\"架构\"]}\n".to_string())
    );
    assert_same_refusal(
        &boot,
        &["tag", "track.json", "--add", "x"],
        "calm.report.tag",
        json!({ "path": "track.json", "add": ["x"] }),
    )
    .await;
}

/// #1838: a Worker lists its track's report tags but cannot change them.
#[tokio::test]
async fn cli_tag_lists_for_a_worker_and_refuses_its_changes() {
    let boot = boot_with_role(CardRole::Worker).await;
    add_report_card(&boot).await;
    let (stdout, stderr, exit) = cli(&boot, &["tag", "report.md"]).await;
    assert_eq!((exit, stderr.as_str(), stdout.as_str()), (0, "", "\n"));
    assert_same_refusal(
        &boot,
        &["tag", "report.md", "--add", "认证"],
        "calm.report.tag",
        json!({ "path": "report.md", "add": ["认证"] }),
    )
    .await;
    let (stdout, _, exit) = cli(&boot, &["tag", "report.md"]).await;
    assert_eq!((exit, stdout.as_str()), (0, "\n"));
}
