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
        (vec!["ls"], "calm.track.ls", json!({}), Render::Ls),
        (
            vec!["ls", "cards"],
            "calm.track.ls",
            json!({ "path": "cards" }),
            Render::Ls,
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
                (true, Render::Ls | Render::State | Render::Diff | Render::Log) => {
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
    assert!(log.contains(" event=3 active commit 2\n"), "{log}");
    let (cat, _, _) = cli(&boot, &["cat", "track.json"]).await;
    assert!(
        cat.starts_with("{\n  \""),
        "track.json is pretty-printed: {cat}"
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

/// Bind the boot Worker to a running isolated attempt with no plugin grants, as the spawn journal would.
async fn bind_isolated_attempt(boot: &CardBoot) {
    let pool = boot.repo.sqlite_pool().unwrap();
    let (track, card, session) = (boot.track_id.as_str(), &boot.card_id, &boot.session_id);
    let context = json!({"neige_execution":{"version":"isolated-codex-v1","workspace":"empty","plugin_tools":[]}});
    sqlx::query(concat!(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,worker_card_id,",
        "created_at_ms,updated_at_ms) VALUES('iso-cli',?1,'iso-cli','codex','read',?2,'running',?3,1,1)"
    ))
        .bind(track).bind(context.to_string()).bind(card).execute(&pool).await.unwrap();
    let request = json!({"version":"isolated-worker-v1","actor":calm_server::ids::ActorId::KernelDispatcher,"track_id":track,"task_id":"iso-cli","idempotency_key":"iso-cli"});
    let output = json!({"result":{},"data":{"isolated_execution":{"version":"isolated-run-v1","track_id":track,"native_token":"fixture-private-token","admission":"open","provider":{"state":"unprepared"},
        "request":{"identity":{"run_id":"iso-cli-op","attempt_id":"iso-cli","card_id":card,"session_id":session},"workspace":"/workspace","developer_instructions":"fixture"}}},"target_type":"card","target_id":card});
    sqlx::query(concat!(
        "INSERT INTO operations(id,operation_key,kind,idempotency_key,payload_hash,target_type,target_id,",
        "target_json,payload_json,phase,tx_output_json,created_at_ms,updated_at_ms) VALUES('iso-cli-op',",
        "'iso-cli-op','codex-isolated-worker','iso-cli','fixture','card',?1,'{}',?2,'spawn_started',?3,1,1)"
    ))
        .bind(card).bind(request.to_string()).bind(output.to_string()).execute(&pool).await.unwrap();
    sqlx::query("UPDATE worker_sessions SET spawn_op_id='iso-cli-op' WHERE id=?1")
        .bind(session)
        .execute(&pool)
        .await
        .unwrap();
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

    // An isolated Worker's native allowlist has no track views: `neige cat` is refused like the direct call.
    let isolated = boot_with_role(CardRole::Worker).await;
    let (_, _, exit) = cli(&isolated, &["cat", "track.json"]).await;
    assert_eq!(exit, 0, "the unbound Worker reads the view");
    bind_isolated_attempt(&isolated).await;
    assert_same_refusal(
        &isolated,
        &["cat", "track.json"],
        "calm.track.cat",
        json!({ "path": "track.json" }),
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
