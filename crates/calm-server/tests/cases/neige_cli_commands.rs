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
        (vec!["track", "ls"], "neige_track_ls", json!({}), LS),
        (
            vec!["track", "ls", "cards"],
            "neige_track_ls",
            json!({ "path": "cards" }),
            LS,
        ),
        (
            vec!["track", "status"],
            "neige_track_status",
            json!({}),
            Render::Status,
        ),
        (
            vec!["track", "diff", &c0, &c2],
            "neige_track_diff",
            json!({ "from": c0, "to": c2 }),
            Render::Diff,
        ),
        (
            vec!["track", "diff", &c0, "--path", "file-2.txt"],
            "neige_track_diff",
            json!({ "from": c0, "path": "file-2.txt" }),
            Render::Diff,
        ),
        (
            vec!["track", "log"],
            "neige_track_log",
            json!({}),
            Render::Log,
        ),
        (
            vec!["track", "log", "--cursor", &c2, "--include-empty"],
            "neige_track_log",
            json!({ "cursor": c2, "include_empty": true }),
            Render::Log,
        ),
        (
            vec!["track", "cat", "track.json"],
            "neige_track_cat",
            json!({ "path": "track.json" }),
            Render::Content,
        ),
        (
            vec!["track", "show", &c2, "file-2.txt"],
            "neige_track_show",
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
                (true, Render::Ls { .. } | Render::Status | Render::Diff | Render::Log) => {
                    format!("{value}\n")
                }
                _ => render(how, tool, json_flag, &value).expect("direct result renders"),
            };
            assert_eq!(stdout, want, "{full:?}");
        }
    }

    // The rendered shapes themselves, end to end.
    let (ls, _, _) = cli(&boot, &["track", "ls"]).await;
    assert!(
        ls.lines().any(|l| l == "- track.json") && ls.lines().any(|l| l == "d cards/"),
        "{ls}"
    );
    let (diff, _, _) = cli(&boot, &["track", "diff", &c0, &c2]).await;
    assert!(
        diff.contains("file-0.txt deleted\n") && diff.contains("file-2.txt new\n"),
        "{diff}"
    );
    let (log, _, _) = cli(&boot, &["track", "log"]).await;
    assert!(log.contains(" event=3 commit 2\n"), "{log}");
    let (cat, _, _) = cli(&boot, &["track", "cat", "track.json"]).await;
    assert!(
        cat.starts_with("{\n  \""),
        "track.json is pretty-printed: {cat}"
    );
}

/// #2087 C2 (§5): `neige_track_log` pages 50 commits at a time by `cursor`, newest first, and
/// never drops a commit; `limit` and `truncated` are gone and a cursor it did not mint is refused.
#[tokio::test]
async fn track_log_pages_by_cursor_without_losing_a_commit() {
    let boot = boot_with_role(CardRole::Planner).await;
    seed_linear_commits(&boot.repo.sqlite_pool().unwrap(), &boot.track_id, 120).await;
    let (mut seen, mut cursor, mut sizes) = (Vec::new(), None::<String>, Vec::new());
    loop {
        let args = cursor
            .as_ref()
            .map_or(json!({}), |c| json!({ "cursor": c }));
        let page = direct_ok(&boot, "neige_track_log", args).await;
        assert!(page.get("truncated").is_none(), "{page}");
        let commits = page["commits"].as_array().unwrap();
        sizes.push(commits.len());
        seen.extend(
            commits
                .iter()
                .map(|c| c["hash"].as_str().unwrap().to_string()),
        );
        match &page["next_cursor"] {
            Value::String(next) => {
                assert_eq!(Some(next.as_str()), seen.last().map(String::as_str));
                cursor = Some(next.clone());
            }
            Value::Null => break,
            other => panic!("next_cursor {other}"),
        }
    }
    assert_eq!(sizes, [50, 50, 20]);
    let expected: Vec<String> = (0..120).rev().map(|index| commit(&boot, index)).collect();
    assert_eq!(seen, expected, "every commit once, newest first");

    let (text, stderr, exit) = cli(&boot, &["track", "log", "--cursor", &commit(&boot, 20)]).await;
    assert_eq!((exit, stderr.as_str()), (0, ""), "{text}");
    assert_eq!(
        text.lines().count(),
        21,
        "20 commits, then the cursor line: {text}"
    );
    assert!(text.ends_with("next_cursor: null\n"), "{text}");

    for (args, refusal) in [
        (json!({ "cursor": "nope" }), "names no row of this listing"),
        (json!({ "limit": 5 }), "unknown argument `limit`"),
    ] {
        let resp = direct(&boot, "neige_track_log", args).await;
        assert_eq!(resp["error"]["code"], -32602, "{resp}");
        let message = resp["error"]["message"].as_str().unwrap();
        assert!(
            message.starts_with("neige_track_log: ") && message.contains(refusal),
            "{message}"
        );
    }
}

/// A `neige_track_log` page also ends early at its 32 KiB byte budget, and the next page resumes
/// after the last commit it emitted.
#[tokio::test]
async fn track_log_page_ends_early_at_the_byte_budget() {
    let boot = boot_with_role(CardRole::Planner).await;
    let pool = boot.repo.sqlite_pool().unwrap();
    seed_linear_commits(&pool, &boot.track_id, 50).await;
    sqlx::query("UPDATE track_vcs_commits SET message = ?1 WHERE track_id = ?2")
        .bind("m".repeat(1024))
        .bind(boot.track_id.as_str())
        .execute(&pool)
        .await
        .unwrap();
    let (mut seen, mut cursor, mut sizes) = (Vec::new(), None::<String>, Vec::new());
    loop {
        let args = cursor
            .as_ref()
            .map_or(json!({}), |c| json!({ "cursor": c }));
        let page = direct_ok(&boot, "neige_track_log", args).await;
        let commits = page["commits"].as_array().unwrap();
        let bytes: usize = commits
            .iter()
            .map(|c| serde_json::to_vec(c).unwrap().len() + 1)
            .sum();
        assert!(bytes <= 32 * 1024, "{bytes} bytes");
        sizes.push(commits.len());
        seen.extend(
            commits
                .iter()
                .map(|c| c["hash"].as_str().unwrap().to_string()),
        );
        match &page["next_cursor"] {
            Value::String(next) => {
                assert_eq!(Some(next.as_str()), seen.last().map(String::as_str));
                cursor = Some(next.clone());
            }
            Value::Null => break,
            other => panic!("next_cursor {other}"),
        }
    }
    assert!(
        sizes.len() > 1 && sizes[0] < 50,
        "the byte budget ended the page: {sizes:?}"
    );
    let expected: Vec<String> = (0..50).rev().map(|index| commit(&boot, index)).collect();
    assert_eq!(seen, expected, "every commit once, newest first");
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

    let (text, stderr, exit) = cli(&boot, &["track", "status"]).await;
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
    assert_eq!(
        fact("tasks"),
        vec!["tasks      fix-login running start=checkout"]
    );
    assert_eq!(fact("sessions").len(), 1, "{text}");
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
    let (text, _, exit) = cli(&boot, &["track", "status"]).await;
    assert_eq!(exit, 0, "{text}");
    let live: Vec<&str> = text
        .lines()
        .skip_while(|l| !l.starts_with("sessions"))
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
    sqlx::query("UPDATE tasks SET start = 'upstream' WHERE id = 'writer-1'")
        .execute(boot.sqlx.pool())
        .await
        .unwrap();
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
    let (text, stderr, exit) = cli(&boot, &["track", "status"]).await;
    assert_eq!((exit, stderr.as_str()), (0, ""));
    let rows: Vec<_> = text
        .lines()
        .skip_while(|row| !row.starts_with("tasks"))
        .take_while(|row| !row.starts_with("sessions"))
        .collect();
    assert_eq!(
        rows,
        [
            "tasks      exited failed read_only start=checkout",
            "           reader pending read_only start=checkout",
            "           writer pending start=upstream"
        ]
    );
    assert!(!text.contains(&boot.other_card_id), "{text}");
    let (json_text, stderr, exit) = cli(&boot, &["track", "status", "--json"]).await;
    assert_eq!((exit, stderr.as_str()), (0, ""));
    let state: Value = serde_json::from_str(&json_text).unwrap();
    assert_eq!(
        state["tasks"],
        json!([
            {"key":"exited","status":"failed","worker_card_id":boot.other_card_id,"access":"read_only","start":"checkout"},
            {"key":"reader","status":"pending","worker_card_id":null,"access":"read_only","start":"checkout"},
            {"key":"writer","status":"pending","worker_card_id":null,"access":"read_write","start":"upstream"}
        ])
    );
    assert_eq!(json_text, format!("{state}\n"));
}

/// The worker's own `neige task done`: the production report path that ends its task.
async fn report_completed(boot: &CardBoot, worker_token: &str, task_id: &str) {
    let argv = ["task", "done", "--attempt-id", task_id, "--result", "{}"];
    let (_, stderr, exit) =
        cli_output(&neige_cli_via_socket(&boot.socket_path, worker_token, &argv).await);
    assert_eq!((exit, stderr.as_str()), (0, ""), "{task_id}");
}

/// The `neige track status` live row of `card`; exactly one.
fn live_row<'a>(text: &'a str, card: &str) -> &'a str {
    let rows: Vec<&str> = text
        .lines()
        .skip_while(|line| !line.starts_with("sessions"))
        .filter(|line| line.contains(card))
        .collect();
    assert_eq!(rows.len(), 1, "{card}: {text}");
    rows[0]
}

/// #1932: a worker's report ends its task in the report transaction while its session lives on;
/// `neige track status` names the session's status and the task's apart.
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

    let (text, stderr, exit) = cli(&boot, &["track", "status"]).await;
    assert_eq!((exit, stderr.as_str()), (0, ""), "{text}");
    assert!(
        text.contains("\ntasks      fix-login done start=checkout\nsessions "),
        "{text}"
    );
    assert!(
        live_row(&text, worker).ends_with("  worker   codex  session open"),
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

    let (text, _, exit) = cli(&boot, &["track", "status"]).await;
    assert_eq!(exit, 0, "{text}");
    assert!(
        text.contains("\ntasks      fix-login done read_only start=checkout\nsessions "),
        "{text}"
    );
    assert!(
        live_row(&text, &retry).ends_with("  session open"),
        "{text}"
    );
    assert!(live_row(&text, first).ends_with("  session open"), "{text}");
    assert_eq!(
        text.lines()
            .filter(|line| line.contains("fix-login"))
            .count(),
        1
    );
    let (json_text, stderr, exit) = cli(&boot, &["--json", "track", "status"]).await;
    assert_eq!((exit, stderr.as_str()), (0, ""));
    let state: Value = serde_json::from_str(&json_text).unwrap();
    assert_eq!(
        state["tasks"],
        json!([
            {"key":"fix-login","status":"done","worker_card_id":retry,"access":"read_only","start":"checkout"}
        ])
    );
}

/// #1944: the Planner reads every current task's status from `neige track status` alone: a pending key,
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

    let (text, stderr, exit) = cli(&boot, &["track", "status"]).await;
    assert_eq!((exit, stderr.as_str()), (0, ""), "{text}");
    let tasks: Vec<&str> = text
        .lines()
        .skip_while(|line| !line.starts_with("tasks"))
        .take_while(|line| !line.starts_with("sessions"))
        .collect();
    assert_eq!(
        tasks,
        vec![
            "tasks      add-test done start=checkout",
            "           docs pending start=checkout",
            "           fix-login done start=checkout",
        ],
        "{text}"
    );
    assert!(live_row(&text, live).ends_with("  session open"), "{text}");
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
    assert!(
        message.starts_with(&format!("{tool}: ")),
        "§5: the refusal is led by the tool name: {message}"
    );

    let (stdout, stderr, exit) = cli(boot, argv).await;
    assert_eq!(exit, 4, "{argv:?}: {stderr}");
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        format!("neige: {message} (code {code})\n"),
        "{argv:?}"
    );

    let mut json_argv = vec!["--json"];
    json_argv.extend_from_slice(argv);
    let (_, stderr, exit) = cli(boot, &json_argv).await;
    assert_eq!(exit, 4);
    let parsed: Value = serde_json::from_str(&stderr).expect("--json error is JSON");
    assert_eq!(
        parsed["error"]["message"],
        json!(format!("{message} (code {code})"))
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
        &["admin", "vacuum", "--force"],
        "neige_admin_vacuum",
        json!({}),
    )
    .await;

    let planner = boot_with_role(CardRole::Planner).await;
    assert_same_refusal(
        &planner,
        &["task", "done", "--attempt-id", "k"],
        "neige_task_done",
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
        cli(&boot, &["admin", "gc", "--track-id", &track, "--keep", "1"]).await;
    assert_eq!(
        (exit, stdout.as_str(), stderr.as_str()),
        (
            1,
            "",
            "neige: admin gc is destructive (prunes VCS history + sweeps objects); re-run with --force to confirm\n"
        ),
        "admin gc without --force"
    );
    assert_eq!(
        commit_count(&boot).await,
        5,
        "admin gc pruned without --force"
    );

    let (stdout, stderr, exit) = cli(&boot, &["admin", "vacuum"]).await;
    assert_eq!(
        (exit, stdout.as_str(), stderr.as_str()),
        (
            1,
            "",
            "neige: admin vacuum write-locks the DB and must run in a quiet maintenance window; re-run with --force to confirm\n"
        ),
        "admin vacuum without --force"
    );

    // The confirmed forms reach the tool.
    let (stdout, _, exit) = cli(
        &boot,
        &[
            "admin",
            "gc",
            "--track-id",
            &track,
            "--keep",
            "1",
            "--dry-run",
        ],
    )
    .await;
    assert_eq!(exit, 0);
    assert_eq!(
        serde_json::from_str::<Value>(&stdout).unwrap()["pruned_commits"],
        json!(4)
    );
    assert_eq!(commit_count(&boot).await, 5);
    let (stdout, _, exit) = cli(&boot, &["admin", "vacuum", "--force"]).await;
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
    let cat = help::render(HelpRequest::Command("track", "cat")).unwrap();
    assert!(
        cat.contains("`track cat` is a view; a report write needs `neige_report_read` first."),
        "{cat}"
    );
    let track = help::render(HelpRequest::Object("track")).unwrap();
    let gc = help::render(HelpRequest::Command("admin", "gc")).unwrap();
    for (argv, want) in [
        (&["--help"][..], root.clone()),
        (&["-h"][..], root.clone()),
        (&["help"][..], root.clone()),
        (&["help", "track", "cat"][..], cat.clone()),
        (&["track", "cat", "--help"][..], cat.clone()),
        (&["track", "cat", "report.md", "-h"][..], cat.clone()),
        (&["help", "track"][..], track.clone()),
        (&["track", "--help"][..], track.clone()),
        (&["--json", "admin", "gc", "-h"][..], gc.clone()),
        (
            &["tool", "ls", "--help"][..],
            help::render(HelpRequest::Object("tool")).unwrap(),
        ),
    ] {
        assert_eq!(cli(&boot, argv).await, (want, String::new(), 0), "{argv:?}");
    }
    assert!(gc.contains("--keep <count>"), "{gc}");
    assert!(!gc.contains("[default:"), "{gc}");
    for action in ["ls", "cat", "show", "diff", "log", "status", "close"] {
        assert!(track.contains(&format!("\n  {action} ")), "{track}");
    }

    for action in ["done", "fail"] {
        let text = help::render(HelpRequest::Command("task", action)).unwrap();
        assert!(text.contains("report_received"), "{text}");
        assert_eq!(
            cli(&boot, &["task", action, "--help"]).await,
            (text, String::new(), 0)
        );
    }
    // A retired outcome action is no alias: the usage error lists the actions.
    for retired in ["complete", "report-success", "report-failure"] {
        assert!(help::render(HelpRequest::Command("task", retired)).is_none());
        let (stdout, stderr, exit) =
            cli(&boot, &["task", retired, "--attempt-id", "retired"]).await;
        assert_eq!(
            (stdout.as_str(), stderr, exit),
            (
                "",
                format!("neige: {}\n", help::unknown_action_message("task", retired)),
                1
            ),
            "{retired}"
        );
    }

    // An old one-word spelling is no alias: the usage error lists the objects.
    for (word, argv) in [
        ("snow", &["snow"][..]),
        ("snow", &["snow", "--help"][..]),
        ("snow", &["help", "snow"][..]),
        ("cat", &["cat", "report.md"][..]),
        ("cat", &["cat", "report.md", "--help"][..]),
        ("cat-at", &["help", "cat-at"][..]),
        (
            "task-report-success",
            &["task-report-success", "--attempt-id", "retired"][..],
        ),
        (
            "task-report-failure",
            &["task-report-failure", "--attempt-id", "retired"][..],
        ),
        (
            "task-completed",
            &["task-completed", "--attempt-id", "retired"][..],
        ),
        (
            "task-failed",
            &["task-failed", "--attempt-id", "retired"][..],
        ),
    ] {
        let (stdout, stderr, exit) = cli(&boot, argv).await;
        assert_eq!(
            (stdout.as_str(), stderr.clone(), exit),
            (
                "",
                format!("neige: {}\n", help::unknown_command_message(word)),
                1
            ),
            "{argv:?}"
        );
        assert!(
            stderr.contains("Objects: track, report, mail, task, admin, tool\n"),
            "{stderr}"
        );
    }
    let (_, stderr, exit) = cli(&boot, &["track", "cat-at", "c", "p"]).await;
    assert_eq!(
        (stderr.as_str(), exit),
        (
            "neige: unknown action `cat-at` for `neige track`; expected one of: ls, cat, show, diff, log, status, close\n",
            1
        )
    );
    let (_, stderr, exit) = cli(&boot, &["help", "track", "cat-at"]).await;
    assert_eq!(exit, 1);
    assert!(
        stderr.contains("expected one of: ls, cat, show"),
        "{stderr}"
    );
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
        json!("neige [--json] <object> <action> [options]")
    );
    let (_, stderr, exit) = cli(&boot, &["--json", "track", "cat"]).await;
    assert_eq!(exit, 1);
    let parsed: Value = serde_json::from_str(&stderr).expect("JSON usage error");
    assert_eq!(
        parsed["error"]["message"],
        json!("track cat requires <path>")
    );
    assert_eq!(
        parsed["error"]["detail"]["usage"],
        json!(help::usage_line(Some("neige_track_cat")))
    );
    let (_, stderr, exit) = cli(&boot, &[]).await;
    assert_eq!(exit, 1);
    assert!(
        stderr.starts_with("neige: missing command; expected `neige <object> <action>`"),
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

/// #1838: `neige report tag report.md` end to end. The Planner changes tags; only `report.md` takes them.
#[tokio::test]
async fn cli_tag_round_trips_the_planners_report_tags() {
    let boot = boot_with_role(CardRole::Planner).await;
    add_report_card(&boot).await;
    let steps: [(&[&str], &str); 5] = [
        (&["report", "tag", "report.md"], "\n"),
        (
            &[
                "report",
                "tag",
                "report.md",
                "--add",
                "认证",
                "--add",
                "架构",
            ],
            "认证 架构\n",
        ),
        (
            &[
                "report",
                "tag",
                "report.md",
                "--add",
                "排障",
                "--add",
                "认证",
            ],
            "认证 架构 排障\n",
        ),
        (
            &["report", "tag", "report.md", "--remove", "排障"],
            "认证 架构\n",
        ),
        (&["report", "tag", "report.md"], "认证 架构\n"),
    ];
    for (argv, want) in steps {
        let (stdout, stderr, exit) = cli(&boot, argv).await;
        assert_eq!(
            (exit, stderr.as_str(), stdout.as_str()),
            (0, "", want),
            "{argv:?}"
        );
    }
    let (stdout, _, exit) = cli(&boot, &["--json", "report", "tag", "report.md"]).await;
    assert_eq!(
        (exit, stdout),
        (0, "{\"tags\":[\"认证\",\"架构\"]}\n".to_string())
    );
    assert_same_refusal(
        &boot,
        &["report", "tag", "track.json", "--add", "x"],
        "neige_report_tag",
        json!({ "path": "track.json", "add": ["x"] }),
    )
    .await;
}

/// #1838: a Worker lists its track's report tags but cannot change them.
#[tokio::test]
async fn cli_tag_lists_for_a_worker_and_refuses_its_changes() {
    let boot = boot_with_role(CardRole::Worker).await;
    add_report_card(&boot).await;
    let (stdout, stderr, exit) = cli(&boot, &["report", "tag", "report.md"]).await;
    assert_eq!((exit, stderr.as_str(), stdout.as_str()), (0, "", "\n"));
    assert_same_refusal(
        &boot,
        &["report", "tag", "report.md", "--add", "认证"],
        "neige_report_tag",
        json!({ "path": "report.md", "add": ["认证"] }),
    )
    .await;
    let (stdout, _, exit) = cli(&boot, &["report", "tag", "report.md"]).await;
    assert_eq!((exit, stdout.as_str()), (0, "\n"));
}
