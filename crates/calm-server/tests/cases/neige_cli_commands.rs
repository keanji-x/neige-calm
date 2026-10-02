//! #1801 `neige/cli` against a real kernel socket: output and authorization equal a direct `tools/call`
//! on the same token, the `--force` gates refuse before any tool runs, and help is served by the kernel.

#![cfg(unix)]

use crate::support;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::card_with_codex_create_tx;
use calm_server::ids::ActorId;
use calm_server::mcp_server::cli::help::{self, HelpRequest};
use calm_server::mcp_server::cli::render::{Render, render};
use calm_server::model::CardRole;
use calm_server::session_projection_repo::WorkerSessionState;
use calm_types::event::TaskContextRef;
use calm_types::task_recovery::{TASK_IN_TRACK_ROUTE, TaskAttemptOrigin, TaskRecoveryConstraint};
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
    assert_eq!(fact("tasks"), vec!["tasks      fix-login running"]);
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
        worker[0].contains(" worker ") && worker[0].ends_with("  session starting"),
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

/// Stamp an ungated task execution on its worker card.
async fn stamp_task(boot: &CardBoot, id: &str, key: &str, status: &str, worker: &str) {
    sqlx::query(concat!(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,worker_card_id,",
        "created_at_ms,updated_at_ms) VALUES(?1,?2,?3,'codex','g','{}',?4,?5,1,1)"
    ))
    .bind(id)
    .bind(boot.track_id.as_str())
    .bind(key)
    .bind(status)
    .bind(worker)
    .execute(boot.sqlx.pool())
    .await
    .unwrap();
}

#[tokio::test]
async fn cli_state_marks_stored_readers_without_live_workers() {
    let boot = boot_with_role(CardRole::Planner).await;
    stamp_task(&boot, "reader-1", "reader", "pending", &boot.other_card_id).await;
    stamp_task(&boot, "exited-1", "exited", "failed", &boot.other_card_id).await;
    stamp_task(&boot, "writer-1", "writer", "pending", &boot.other_card_id).await;
    sqlx::query("UPDATE tasks SET access = 'read_only' WHERE id IN ('reader-1','exited-1')")
        .execute(boot.sqlx.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE tasks SET worker_card_id = NULL WHERE id IN ('reader-1','writer-1')")
        .execute(boot.sqlx.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE worker_sessions SET state = 'exited' WHERE card_id = ?1")
        .bind(&boot.other_card_id)
        .execute(boot.sqlx.pool())
        .await
        .unwrap();
    let (text, stderr, exit) = cli(&boot, &["state"]).await;
    assert_eq!((exit, stderr.as_str()), (0, ""));
    let rows: Vec<_> = text
        .lines()
        .skip_while(|row| !row.starts_with("tasks"))
        .take_while(|row| !row.starts_with("live"))
        .collect();
    assert_eq!(
        rows,
        [
            "tasks      exited failed read_only",
            "           reader pending read_only",
            "           writer pending"
        ]
    );
    assert!(!text.contains(&boot.other_card_id), "{text}");
    let (json_text, stderr, exit) = cli(&boot, &["state", "--json"]).await;
    assert_eq!((exit, stderr.as_str()), (0, ""));
    let state: Value = serde_json::from_str(&json_text).unwrap();
    assert_eq!(
        state["tasks"],
        json!([
            {"key":"exited","status":"failed","worker_card_id":boot.other_card_id,"access":"read_only"},
            {"key":"reader","status":"pending","worker_card_id":null,"access":"read_only"},
            {"key":"writer","status":"pending","worker_card_id":null,"access":"read_write"}
        ])
    );
    assert_eq!(json_text, format!("{state}\n"));
}

/// The worker's own `neige task-completed`: the production report path that ends its task.
async fn report_completed(boot: &CardBoot, worker_token: &str, task_id: &str) {
    let argv = ["task-completed", "--attempt-id", task_id, "--result", "{}"];
    let (_, stderr, exit) =
        cli_output(&neige_cli_via_socket(&boot.socket_path, worker_token, &argv).await);
    assert_eq!((exit, stderr.as_str()), (0, ""), "{task_id}");
}

/// The `neige state` live row of `card`; exactly one.
fn live_row<'a>(text: &'a str, card: &str) -> &'a str {
    let rows: Vec<&str> = text
        .lines()
        .skip_while(|line| !line.starts_with("live"))
        .filter(|line| line.contains(card))
        .collect();
    assert_eq!(rows.len(), 1, "{card}: {text}");
    rows[0]
}

/// #1932: a worker's report ends its task in the report transaction while its session lives on;
/// `neige state` names the session's status and the task's apart.
#[tokio::test]
async fn cli_state_shows_a_reported_task_done_beside_its_live_worker_session() {
    let boot = boot_with_role(CardRole::Planner).await;
    let worker = boot.other_card_id.as_str();
    boot.sqlx
        .session_projection_set_status_for_card(worker, WorkerSessionState::Running)
        .await
        .unwrap();
    stamp_task(&boot, "fix-login-1", "fix-login", "running", worker).await;
    report_completed(&boot, &boot.other_raw_token, "fix-login-1").await;

    // The task settled; the worker session did not end with it.
    let (status, finished_at): (String, Option<i64>) =
        sqlx::query_as("SELECT status, finished_at_ms FROM tasks WHERE id = 'fix-login-1'")
            .fetch_one(boot.sqlx.pool())
            .await
            .unwrap();
    assert_eq!(status, "done");
    assert!(finished_at.is_some());
    let session: String =
        sqlx::query_scalar("SELECT state FROM worker_sessions WHERE card_id = ?1")
            .bind(worker)
            .fetch_one(boot.sqlx.pool())
            .await
            .unwrap();
    assert_eq!(session, "running");

    let (text, stderr, exit) = cli(&boot, &["state"]).await;
    assert_eq!((exit, stderr.as_str()), (0, ""), "{text}");
    assert!(
        text.contains("\ntasks      fix-login done\nlive "),
        "{text}"
    );
    assert!(
        live_row(&text, worker).ends_with("  worker   codex  session running"),
        "{text}"
    );
}

/// #1932: `tasks` holds each key's current execution only, so a key whose earlier attempt failed
/// on a still-live card shows once, with its current attempt's status.
#[tokio::test]
async fn cli_state_shows_a_key_once_at_its_current_attempt() {
    let boot = boot_with_role(CardRole::Planner).await;
    let first = boot.other_card_id.as_str();
    let retry = calm_server::model::new_id();
    let mut tx = boot.sqlx.pool().begin().await.unwrap();
    let (_, _, retry_token) = card_with_codex_create_tx(
        &mut tx,
        retry.clone(),
        &calm_server::model::new_id(),
        None,
        boot.track_id.clone(),
        None,
        None,
        "/workspace".into(),
        json!({}),
        None,
        None,
        None,
        CardRole::Worker,
        true,
        &boot.card_role_cache,
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    for card in [first, retry.as_str()] {
        boot.sqlx
            .session_projection_set_status_for_card(card, WorkerSessionState::Running)
            .await
            .unwrap();
    }
    stamp_task(&boot, "fix-login-1", "fix-login", "failed", first).await;
    let origin = TaskAttemptOrigin::Recovery {
        previous_attempt_id: "fix-login-1".into(),
        idempotency_key: "recover-fix-login".into(),
        request_fingerprint: "fixture".into(),
        reason: "fixture recovery".into(),
        actor: ActorId::User,
        constraint: TaskRecoveryConstraint::V1 {
            refs: vec![TaskContextRef {
                track_id: boot.track_id.clone(),
                block_id: "blk-fix-login".into(),
                rev: 1,
                hash: "fixture".into(),
                is_root: true,
            }],
            spawn: TASK_IN_TRACK_ROUTE.into(),
            declared_by: "user".into(),
        },
    };
    sqlx::query(concat!(
        "INSERT INTO task_attempt_allocations(attempt_id,track_id,key,generation,origin_json,",
        "created_at_ms) VALUES('fix-login-2',?1,'fix-login',2,?2,2)"
    ))
    .bind(boot.track_id.as_str())
    .bind(serde_json::to_string(&origin).unwrap())
    .execute(boot.sqlx.pool())
    .await
    .unwrap();
    stamp_task(&boot, "fix-login-2", "fix-login", "running", &retry).await;
    sqlx::query("UPDATE tasks SET access = 'read_only' WHERE id = 'fix-login-2'")
        .execute(boot.sqlx.pool())
        .await
        .unwrap();
    report_completed(&boot, &retry_token.unwrap(), "fix-login-2").await;

    let (text, _, exit) = cli(&boot, &["state"]).await;
    assert_eq!(exit, 0, "{text}");
    assert!(
        text.contains("\ntasks      fix-login done read_only\nlive "),
        "{text}"
    );
    assert!(
        live_row(&text, &retry).ends_with("  session running"),
        "{text}"
    );
    assert!(
        live_row(&text, first).ends_with("  session running"),
        "{text}"
    );
    assert_eq!(
        text.lines()
            .filter(|line| line.contains("fix-login"))
            .count(),
        1
    );
    let (json_text, stderr, exit) = cli(&boot, &["--json", "state"]).await;
    assert_eq!((exit, stderr.as_str()), (0, ""));
    let state: Value = serde_json::from_str(&json_text).unwrap();
    assert_eq!(
        state["tasks"],
        json!([
            {"key":"fix-login","status":"done","worker_card_id":retry,"access":"read_only"}
        ])
    );
}

/// #1944: the Planner reads every current task's status from `neige state` alone: a pending key,
/// a done key whose worker session lives on, and a done key whose worker session has exited.
#[tokio::test]
async fn cli_state_lists_every_current_task_whatever_its_worker_session() {
    let boot = boot_with_role(CardRole::Planner).await;
    let live = boot.other_card_id.as_str();
    let gone = calm_server::model::new_id();
    let mut tx = boot.sqlx.pool().begin().await.unwrap();
    let (_, _, gone_token) = card_with_codex_create_tx(
        &mut tx,
        gone.clone(),
        &calm_server::model::new_id(),
        None,
        boot.track_id.clone(),
        None,
        None,
        "/workspace".into(),
        json!({}),
        None,
        None,
        None,
        CardRole::Worker,
        true,
        &boot.card_role_cache,
        calm_server::routes::theme::RequestTheme::default_dark(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    for card in [live, gone.as_str()] {
        boot.sqlx
            .session_projection_set_status_for_card(card, WorkerSessionState::Running)
            .await
            .unwrap();
    }
    stamp_task(&boot, "fix-login-1", "fix-login", "running", live).await;
    stamp_task(&boot, "add-test-1", "add-test", "running", &gone).await;
    report_completed(&boot, &boot.other_raw_token, "fix-login-1").await;
    report_completed(&boot, &gone_token.unwrap(), "add-test-1").await;
    boot.sqlx
        .session_projection_set_status_for_card(&gone, WorkerSessionState::Exited)
        .await
        .unwrap();
    sqlx::query(concat!(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,",
        "created_at_ms,updated_at_ms) VALUES('docs-1',?1,'docs','codex','g','{}','pending',1,1)"
    ))
    .bind(boot.track_id.as_str())
    .execute(boot.sqlx.pool())
    .await
    .unwrap();

    let (text, stderr, exit) = cli(&boot, &["state"]).await;
    assert_eq!((exit, stderr.as_str()), (0, ""), "{text}");
    let tasks: Vec<&str> = text
        .lines()
        .skip_while(|line| !line.starts_with("tasks"))
        .take_while(|line| !line.starts_with("live"))
        .collect();
    assert_eq!(
        tasks,
        vec![
            "tasks      add-test done",
            "           docs pending",
            "           fix-login done",
        ],
        "{text}"
    );
    assert!(
        live_row(&text, live).ends_with("  session running"),
        "{text}"
    );
    assert!(
        !text.contains(gone.as_str()),
        "an exited worker is not live: {text}"
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
        &["task-completed", "--attempt-id", "k"],
        "calm.task.complete",
        json!({ "attempt_id": "k" }),
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
