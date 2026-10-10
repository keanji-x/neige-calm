//! #2516: a Claude card resumes the session it last started, by hand and after its PTY was lost.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::ids::{ActorId, TrackId};
use calm_server::model::{RequestTheme, new_id, now_ms};
use calm_server::session_projection_repo::WorkerSessionState;
use serde_json::{Value, json};
use sqlx::Row;
use tower::ServiceExt;

use super::{
    Boot, ENV_LOCK, SpawnCall, body, boot_with_spawn_hook_factory, post, post_restart,
    recording_spawn_hook, response_json, runtime_status,
};

async fn post_hook(boot: &Boot, card_id: &str, payload: Value) -> StatusCode {
    let resp = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/internal/claude/hook?card_id={card_id}"))
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    response_json(resp).await.0
}

fn session_start(session_id: &str, source: &str) -> Value {
    json!({
        "hook_event_name": "SessionStart",
        "session_id": session_id,
        "source": source,
        "cwd": "/workspace",
        "transcript_path": format!("/workspace/.claude/{session_id}.jsonl"),
    })
}

async fn agent_session_ids(boot: &Boot, card_id: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT agent_session_id FROM worker_sessions WHERE card_id = ?1 \
         ORDER BY created_at_ms ASC, id ASC",
    )
    .bind(card_id)
    .fetch_all(boot.repo.pool())
    .await
    .unwrap()
}

/// Claude changes its session id on `/clear`; resuming the id neige minted at spawn would bring
/// back the conversation from before the `/clear` and lose everything after it.
#[tokio::test]
async fn restart_after_clear_resumes_the_session_the_card_last_started() {
    let _guard = ENV_LOCK.lock().await;
    let calls = Arc::new(tokio::sync::Mutex::new(Vec::<SpawnCall>::new()));
    let calls_for_factory = calls.clone();
    let boot =
        boot_with_spawn_hook_factory(move |_, _| recording_spawn_hook(calls_for_factory)).await;
    let (status, created) = post(boot.app.clone(), &boot.track_id, body(None), None, None).await;
    assert_eq!(status, StatusCode::CREATED, "body={created:?}");
    let card_id = created["id"].as_str().unwrap();
    let minted = created["payload"]["claude_session_id"].as_str().unwrap();
    let cleared = "6f1c2a8e-4b7d-4e2a-9c3b-1d5e7f9a0b21";

    assert_eq!(
        post_hook(&boot, card_id, session_start(cleared, "clear")).await,
        StatusCode::OK
    );
    // Exited first, so the restart is the plain resume of an exited card.
    boot.repo
        .session_projection_complete_for_card(card_id, WorkerSessionState::Exited)
        .await
        .unwrap();

    let (status, restarted) = post_restart(boot.app.clone(), card_id).await;
    assert_eq!(status, StatusCode::OK, "body={restarted:?}");
    let program = calls.lock().await.last().unwrap().program.clone();
    assert!(
        program.contains(&format!("--resume='{cleared}'")),
        "a restart must resume the session the card last started: {program}"
    );
    assert!(!program.contains(minted), "{program}");
    assert_eq!(restarted["payload"]["claude_session_id"], cleared);
    assert_eq!(agent_session_ids(&boot, card_id).await, [cleared, cleared]);

    // An id another card's live runtime holds stays rejected, and that card keeps its own id.
    let (status, other) = post(boot.app.clone(), &boot.track_id, body(None), None, None).await;
    assert_eq!(status, StatusCode::CREATED, "body={other:?}");
    let other_id = other["id"].as_str().unwrap();
    let other_session = other["payload"]["claude_session_id"].as_str().unwrap();
    assert_eq!(
        post_hook(&boot, other_id, session_start(cleared, "resume")).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(agent_session_ids(&boot, other_id).await, [other_session]);
}

/// A Claude card a Planner task created, as its scheduler worker op leaves it: the card, its
/// terminal and runtime from the worker composite, and the `claude-worker` op that targets it.
async fn seed_task_worker_card(boot: &Boot) -> String {
    let card_id = new_id();
    let op_id = new_id();
    let mut tx = boot.repo.pool().begin().await.unwrap();
    sqlx::query(
        "INSERT INTO operations (id, operation_key, kind, idempotency_key, payload_hash, \
           target_type, target_id, target_json, payload_json, phase, created_at_ms, updated_at_ms) \
         VALUES (?1, ?1, 'claude-worker', ?2, 'hash', 'card', ?3, ?4, ?5, 'succeeded', ?6, ?6)",
    )
    .bind(&op_id)
    .bind(format!("{}:worker-task", boot.track_id))
    .bind(&card_id)
    .bind(json!({ "type": "card", "id": card_id }).to_string())
    .bind(json!({ "actor": ActorId::KernelDispatcher, "track_id": boot.track_id }).to_string())
    .bind(now_ms())
    .execute(&mut *tx)
    .await
    .unwrap();
    calm_server::db::sqlite::card_with_claude_worker_create_tx(
        &mut tx,
        card_id.clone(),
        &new_id(),
        Some(&op_id),
        TrackId::from(boot.track_id.clone()),
        None,
        None,
        "'/bin/true' --session-id 'worker-session'".into(),
        "/workspace".into(),
        json!({}),
        Some("task prompt".into()),
        None,
        None,
        "/tmp/worker-settings/settings.json".into(),
        "worker-session".into(),
        &boot.state.card_role_cache,
        RequestTheme::default_dark(),
    )
    .await
    .unwrap();
    assert!(
        calm_server::db::sqlite::card_is_worker_spawn_target_tx(&mut tx, &card_id)
            .await
            .unwrap(),
        "the seeded card must be a task worker's card by the kernel's own proof"
    );
    tx.commit().await.unwrap();
    card_id
}

/// After a host crash, boot reconcile marks every lost PTY exited; the owner's Claude card comes
/// back on its own, once, and a Planner task worker's card is left to its Planner.
#[tokio::test]
async fn boot_resumes_owner_created_claude_cards_and_leaves_task_workers_exited() {
    let _guard = ENV_LOCK.lock().await;
    let calls = Arc::new(tokio::sync::Mutex::new(Vec::<SpawnCall>::new()));
    let calls_for_factory = calls.clone();
    let boot =
        boot_with_spawn_hook_factory(move |_, _| recording_spawn_hook(calls_for_factory)).await;
    let (status, owner) = post(boot.app.clone(), &boot.track_id, body(None), None, None).await;
    assert_eq!(status, StatusCode::CREATED, "body={owner:?}");
    let owner_id = owner["id"].as_str().unwrap();
    let owner_terminal = owner["payload"]["terminal_id"].as_str().unwrap();
    let owner_session = owner["payload"]["claude_session_id"].as_str().unwrap();
    let worker_id = seed_task_worker_card(&boot).await;
    let worker_terminal = boot
        .repo
        .terminal_get_by_card(&worker_id)
        .await
        .unwrap()
        .unwrap()
        .id;

    // No supervisor knows either PTY: both are lost.
    let stale = calm_server::reconcile_supervisor_on_boot(&boot.state).await;
    let mut sorted = stale.clone();
    sorted.sort();
    let mut expected = vec![owner_terminal.to_string(), worker_terminal.clone()];
    expected.sort();
    assert_eq!(sorted, expected);
    let spawns_before = calls.lock().await.len();

    // A terminal listed twice still resumes its card once.
    let listed_twice = [stale.clone(), stale].concat();
    calm_server::claude_auto_resume::resume_owner_claude_cards(&boot.state, &listed_twice).await;

    let restarted: Vec<String> = sqlx::query(
        "SELECT json_extract(payload_json, '$.card_id') AS card_id FROM operations \
         WHERE kind = 'claude-restart'",
    )
    .fetch_all(boot.repo.pool())
    .await
    .unwrap()
    .iter()
    .map(|row| row.try_get("card_id").unwrap())
    .collect();
    assert_eq!(restarted, [owner_id]);
    let calls = calls.lock().await;
    assert_eq!(calls.len(), spawns_before + 1);
    assert_eq!(calls.last().unwrap().terminal_id, owner_terminal);
    assert!(
        calls
            .last()
            .unwrap()
            .program
            .contains(&format!("--resume='{owner_session}'")),
        "{}",
        calls.last().unwrap().program
    );
    let owner_runtime = boot
        .repo
        .session_projection_active_for_card(&owner_id.to_string())
        .await
        .unwrap()
        .expect("the resumed card has a live runtime");
    assert_eq!(owner_runtime.status, WorkerSessionState::Running);
    let owner_term = boot
        .repo
        .terminal_get(owner_terminal)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(owner_term.exit_code, None);

    let worker_term = boot
        .repo
        .terminal_get(&worker_terminal)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        worker_term.exit_code,
        Some(-1),
        "a task worker stays exited"
    );
    assert_eq!(runtime_status(&boot.repo, &worker_id).await, "exited");
}

/// The argv `/bin/sh -c` hands the CLI for `program`, as the terminal runs it.
fn shell_argv(program: &str) -> Vec<String> {
    let output = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("set -- {program}; printf '%s\\0' \"$@\""))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout)
        .unwrap()
        .split_terminator('\0')
        .map(ToOwned::to_owned)
        .collect()
}

/// The hook payload is unauthenticated, and Claude's `--session-id` takes a UUID: an id that is not
/// one (here one that reads like an option) is not followed, and the restart resumes the card's own
/// id as one `--resume=<id>` token.
#[tokio::test]
async fn a_session_start_id_that_is_not_a_uuid_is_not_followed() {
    let _guard = ENV_LOCK.lock().await;
    let calls = Arc::new(tokio::sync::Mutex::new(Vec::<SpawnCall>::new()));
    let calls_for_factory = calls.clone();
    let boot =
        boot_with_spawn_hook_factory(move |_, _| recording_spawn_hook(calls_for_factory)).await;
    let (status, created) = post(boot.app.clone(), &boot.track_id, body(None), None, None).await;
    assert_eq!(status, StatusCode::CREATED, "body={created:?}");
    let card_id = created["id"].as_str().unwrap();
    let minted = created["payload"]["claude_session_id"].as_str().unwrap();
    assert_eq!(
        post_hook(&boot, card_id, session_start("--version", "clear")).await,
        StatusCode::OK
    );
    assert_eq!(agent_session_ids(&boot, card_id).await, [minted]);

    let (status, restarted) = post_restart(boot.app.clone(), card_id).await;
    assert_eq!(status, StatusCode::OK, "body={restarted:?}");
    let argv = shell_argv(&calls.lock().await.last().unwrap().program);
    assert!(
        argv.iter().any(|arg| *arg == format!("--resume={minted}")),
        "{argv:?}"
    );
    assert!(!argv.iter().any(|arg| arg == "--version"), "{argv:?}");
}

/// Operation recovery finished the restart a crash interrupted before the auto-resume runs: the
/// card is running again, so the auto-resume leaves it alone.
#[tokio::test]
async fn boot_auto_resume_skips_a_card_already_running_again() {
    let _guard = ENV_LOCK.lock().await;
    let calls = Arc::new(tokio::sync::Mutex::new(Vec::<SpawnCall>::new()));
    let calls_for_factory = calls.clone();
    let boot =
        boot_with_spawn_hook_factory(move |_, _| recording_spawn_hook(calls_for_factory)).await;
    let (status, owner) = post(boot.app.clone(), &boot.track_id, body(None), None, None).await;
    assert_eq!(status, StatusCode::CREATED, "body={owner:?}");
    let owner_id = owner["id"].as_str().unwrap();
    let stale = calm_server::reconcile_supervisor_on_boot(&boot.state).await;
    assert_eq!(stale.len(), 1);
    // What recovery's completed restart leaves: the card running on its terminal again.
    let (status, restarted) = post_restart(boot.app.clone(), owner_id).await;
    assert_eq!(status, StatusCode::OK, "body={restarted:?}");
    let spawns_before = calls.lock().await.len();

    calm_server::claude_auto_resume::resume_owner_claude_cards(&boot.state, &stale).await;

    let restarts: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM operations WHERE kind = 'claude-restart'")
            .fetch_one(boot.repo.pool())
            .await
            .unwrap();
    assert_eq!(restarts, 1, "only the recovered restart");
    assert_eq!(calls.lock().await.len(), spawns_before);
}
