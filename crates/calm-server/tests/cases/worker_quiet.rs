//! #1755: the quiet-worker detector against production task, card, session and terminal rows and
//! a live renderer entry whose last-output instant and the sweep clock the test sets.
use super::task_terminal::{Worker, worker_running};
use super::terminal_support::Harness;
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::card_with_terminal_create_tx;
use calm_server::model::{CardRole, new_id, now_ms};
use calm_server::session_projection_repo::WorkerSessionState;
use calm_server::terminal_renderer::{RendererConfig, RendererEntry, TerminalExitInfo};
use calm_server::worker_quiet::{WorkerQuietDetector, wake_text};
use calm_types::observation::WORKER_QUIET_WAKE_SOURCE;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

const QUIET: Duration = Duration::from_secs(5);
const QUIET_MS: i64 = 5_000;
const T0: i64 = 1_000_000;

fn detector(h: &Harness) -> WorkerQuietDetector {
    WorkerQuietDetector::new(
        h.sql.clone(),
        h.state.events.clone(),
        h.state.write().clone(),
        h.state.terminal_renderer.clone(),
        QUIET,
    )
}

/// A renderer entry for `terminal` whose last output was at `last_output_ms` (`0` = none yet).
fn live_entry(h: &Harness, terminal: &str, last_output_ms: i64) -> Arc<RendererEntry> {
    let entry = h.state.terminal_renderer.insert_test_entry(RendererConfig {
        terminal_id: terminal.to_owned(),
        cols: 80,
        rows: 24,
        buffer_bytes: 8192,
        terminal_fg: (220, 220, 220),
        terminal_bg: (15, 20, 24),
        program: "/bin/sh".into(),
        args: vec![],
        envs: vec![],
        cwd: h.root.path().to_str().unwrap().to_owned(),
        supervisor_sock: std::path::PathBuf::new(),
    });
    entry.last_output_ms.store(last_output_ms, Ordering::SeqCst);
    entry
}

async fn quiet_worker(h: &Harness, kind: &str) -> (Worker, Arc<RendererEntry>) {
    let w = worker_running(h, kind, &h.track, None).await;
    let entry = live_entry(h, &w.terminal, T0);
    (w, entry)
}

/// Every persisted `track.wake_requested` as `(source, key, text)`, oldest first.
async fn wakes(h: &Harness) -> Vec<(String, String, String)> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT payload FROM events WHERE kind = 'track.wake_requested' ORDER BY id",
    )
    .fetch_all(h.sql.pool())
    .await
    .unwrap();
    rows.into_iter()
        .map(|(payload,)| {
            let v: Value = serde_json::from_str(&payload).unwrap();
            assert_eq!(v["track_id"], json!(h.track), "{v}");
            (
                v["source"].as_str().unwrap().to_owned(),
                v["key"].as_str().unwrap().to_owned(),
                v["text"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// Sweep once past every fixture's threshold and require that only the positive control woke.
async fn assert_only_control_wakes(h: &Harness, control: &Worker) {
    let mut d = detector(h);
    let key = format!("{}:{T0}", control.task);
    assert_eq!(d.tick(T0 + 10 * QUIET_MS).await, vec![key.clone()]);
    let keys: Vec<String> = wakes(h).await.into_iter().map(|(_, key, _)| key).collect();
    assert_eq!(keys, vec![key]);
}

async fn task_key(h: &Harness, attempt_id: &str) -> String {
    h.sql.task_get(attempt_id).await.unwrap().unwrap().key
}

#[tokio::test]
async fn a_quiet_running_worker_wakes_once_per_quiet_episode() {
    let h = Harness::start().await;
    let (w, entry) = quiet_worker(&h, "claude").await;
    let key = task_key(&h, &w.task).await;
    let mut d = detector(&h);

    // Output is recent: no wake.
    assert!(d.tick(T0 + QUIET_MS - 1).await.is_empty());
    assert!(wakes(&h).await.is_empty());

    // Quiet for the threshold: exactly one wake naming the attempt and the episode.
    let first = format!("{}:{T0}", w.task);
    assert_eq!(d.tick(T0 + QUIET_MS).await, vec![first.clone()]);
    let expected = (
        WORKER_QUIET_WAKE_SOURCE.to_owned(),
        first.clone(),
        wake_text(&key, &w.task, 5),
    );
    assert_eq!(wakes(&h).await, vec![expected.clone()]);

    // The same episode never wakes again.
    assert!(d.tick(T0 + 4 * QUIET_MS).await.is_empty());
    assert_eq!(wakes(&h).await.len(), 1);

    // New output starts a new episode; its own quiet wakes again.
    let t1 = T0 + 10 * QUIET_MS;
    entry.last_output_ms.store(t1, Ordering::SeqCst);
    assert!(d.tick(t1 + 1_000).await.is_empty());
    let second = format!("{}:{t1}", w.task);
    assert_eq!(d.tick(t1 + 2 * QUIET_MS).await, vec![second.clone()]);
    assert_eq!(
        wakes(&h).await,
        vec![
            expected,
            (
                WORKER_QUIET_WAKE_SOURCE.to_owned(),
                second,
                wake_text(&key, &w.task, 10),
            ),
        ]
    );
}

#[tokio::test]
async fn a_quiet_codex_worker_wakes_too() {
    let h = Harness::start().await;
    let (w, _entry) = quiet_worker(&h, "codex").await;
    let mut d = detector(&h);
    assert_eq!(
        d.tick(T0 + QUIET_MS).await,
        vec![format!("{}:{T0}", w.task)]
    );
}

#[tokio::test]
async fn a_task_that_is_not_running_does_not_wake() {
    let h = Harness::start().await;
    let (control, _control) = quiet_worker(&h, "claude").await;
    let mut workers = Vec::new();
    for status in ["done", "failed", "canceled", "verifying"] {
        let (w, entry) = quiet_worker(&h, "claude").await;
        sqlx::query("UPDATE tasks SET status = ?2, finished_at_ms = ?3 WHERE id = ?1")
            .bind(&w.task)
            .bind(status)
            .bind((status != "verifying").then(now_ms))
            .execute(h.sql.pool())
            .await
            .unwrap();
        workers.push((w, entry));
    }
    assert_only_control_wakes(&h, &control).await;
}

#[tokio::test]
async fn a_superseded_attempt_does_not_wake() {
    use calm_types::task_recovery::{
        TASK_IN_TRACK_ROUTE, TaskAttemptOrigin, TaskRecoveryConstraint,
    };
    let h = Harness::start().await;
    let (control, _control) = quiet_worker(&h, "claude").await;
    let (w, _entry) = quiet_worker(&h, "claude").await;
    let task = h.sql.task_get(&w.task).await.unwrap().unwrap();
    let mut tx = h.sql.pool().begin().await.unwrap();
    sqlx::query("UPDATE tasks SET status='failed', finished_at_ms=?2 WHERE id=?1")
        .bind(&w.task)
        .bind(now_ms())
        .execute(&mut *tx)
        .await
        .unwrap();
    let origin = TaskAttemptOrigin::Recovery {
        previous_attempt_id: w.task.clone(),
        idempotency_key: "next-generation".into(),
        request_fingerprint: "fingerprint".into(),
        reason: "test explicit new attempt".into(),
        actor: calm_server::ids::ActorId::User,
        constraint: TaskRecoveryConstraint::V1 {
            refs: vec![calm_types::event::TaskContextRef {
                track_id: h.track.clone().into(),
                block_id: "b_1000".into(),
                rev: 1,
                hash: "0".repeat(64),
                is_root: true,
            }],
            spawn: TASK_IN_TRACK_ROUTE.into(),
            declared_by: "user".into(),
        },
    };
    let recovered = format!("{}:2", w.task);
    sqlx::query(
        "INSERT INTO task_attempt_allocations \
         (attempt_id,track_id,key,generation,origin_json,created_at_ms) VALUES (?1,?2,?3,2,?4,0)",
    )
    .bind(&recovered)
    .bind(&h.track)
    .bind(&task.key)
    .bind(serde_json::to_string(&origin).unwrap())
    .execute(&mut *tx)
    .await
    .unwrap();
    // The new attempt runs and names the old worker card: that card belongs to the superseded
    // execution, so the tools cannot reach it by the new attempt_id either.
    sqlx::query(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,worker_card_id,\
         declared_by,created_at_ms,updated_at_ms) \
         VALUES (?1,?2,?3,'claude','test','[]','running',?4,'user',?5,?5)",
    )
    .bind(&recovered)
    .bind(&h.track)
    .bind(&task.key)
    .bind(&w.card)
    .bind(now_ms())
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_only_control_wakes(&h, &control).await;
}

/// A terminal card the Planner or the owner opened (no worker-spawn operation).
async fn manual_terminal(h: &Harness) -> (String, String) {
    let (card, session) = (new_id(), new_id());
    let mut tx = h.sql.pool().begin().await.unwrap();
    let (_, terminal) = card_with_terminal_create_tx(
        &mut tx,
        card.clone(),
        &session,
        None,
        h.track.clone().into(),
        None,
        None,
        "/bin/sh".into(),
        h.root.path().to_str().unwrap().to_owned(),
        json!({}),
        CardRole::Worker,
        true,
        &CardRoleCache::new(),
        calm_server::routes::theme::RequestTheme::default_dark(),
        true,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (card, terminal.id)
}

#[tokio::test]
async fn a_card_that_is_not_the_task_worker_does_not_wake() {
    let h = Harness::start().await;
    let (control, _control) = quiet_worker(&h, "claude").await;
    // A quiet terminal card with no task at all.
    let (_, idle) = manual_terminal(&h).await;
    let _idle = live_entry(&h, &idle, T0);
    // A running task whose row names a manually opened terminal card as its worker.
    let (card, terminal) = manual_terminal(&h).await;
    let _entry = live_entry(&h, &terminal, T0);
    sqlx::query(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,worker_card_id,\
         declared_by,created_at_ms,updated_at_ms) \
         VALUES (?1,?2,?3,'terminal','test','[]','running',?4,'user',?5,?5)",
    )
    .bind(format!("{}:manual", h.track))
    .bind(&h.track)
    .bind("manual")
    .bind(&card)
    .bind(now_ms())
    .execute(h.sql.pool())
    .await
    .unwrap();
    assert_only_control_wakes(&h, &control).await;
}

#[tokio::test]
async fn a_worker_without_a_live_readable_printing_pty_does_not_wake() {
    let h = Harness::start().await;
    let (control, _control) = quiet_worker(&h, "claude").await;
    // No renderer entry.
    let _missing = worker_running(&h, "claude", &h.track, None).await;
    // An entry that never printed.
    let silent = worker_running(&h, "claude", &h.track, None).await;
    let _silent = live_entry(&h, &silent.terminal, 0);
    // An entry whose process exited.
    let (_, exited) = quiet_worker(&h, "claude").await;
    *exited.exit.lock().unwrap() = Some(TerminalExitInfo {
        code: Some(0),
        pty_seq: 0,
        render_rev: 0,
        exited_at: std::time::SystemTime::now(),
    });
    // An entry whose screen can no longer be read (an attach-only reattach invalidates its view).
    let (_, unreadable) = quiet_worker(&h, "claude").await;
    unreadable
        .handle
        .model_view
        .lock()
        .unwrap()
        .invalidate("attach-only reattach");
    // A worker session that has ended.
    let (ended, _ended) = quiet_worker(&h, "claude").await;
    h.sql
        .session_projection_set_status_for_card(&ended.card, WorkerSessionState::Exited)
        .await
        .unwrap();
    assert_only_control_wakes(&h, &control).await;
}
