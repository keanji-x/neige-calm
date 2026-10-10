//! #1755, #2492: the quiet-worker detector against production task, card, session and terminal
//! rows and a live renderer entry whose last-output instant and the sweep clock the test sets. The
//! routing cases deliver through the production watcher (conversation create and planner-input
//! send over a fake app-server); the gating cases record what the detector hands over.
use super::task_terminal::{Worker, worker_running};
use super::terminal_support::Harness;
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::card_with_terminal_create_tx;
use calm_server::error::CalmError;
use calm_server::model::{CardRole, new_id, now_ms};
use calm_server::session_projection_repo::WorkerSessionState;
use calm_server::terminal_renderer::{RendererConfig, RendererEntry, TerminalExitInfo};
use calm_server::worker_quiet::{QuietWorkerInbox, WorkerQuietDetector};
use calm_server::worker_watch::{WorkerWatcher, watch_text, watcher_card_id};
use futures::future::BoxFuture;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const QUIET: Duration = Duration::from_secs(5);
const QUIET_MS: i64 = 5_000;
const T0: i64 = 1_000_000;

/// What the detector handed over, as `(track_id, episode_key, text)`.
#[derive(Default)]
struct Recorder(Mutex<Vec<(String, String, String)>>);

impl QuietWorkerInbox for Recorder {
    fn deliver<'a>(
        &'a self,
        track_id: &'a str,
        episode_key: &'a str,
        text: String,
    ) -> BoxFuture<'a, calm_server::error::Result<()>> {
        self.0
            .lock()
            .unwrap()
            .push((track_id.to_owned(), episode_key.to_owned(), text));
        Box::pin(async { Ok(()) })
    }
}

/// The production watcher behind one refused first delivery.
struct FailFirst {
    watcher: WorkerWatcher,
    failed: AtomicBool,
}

impl QuietWorkerInbox for FailFirst {
    fn deliver<'a>(
        &'a self,
        track_id: &'a str,
        episode_key: &'a str,
        text: String,
    ) -> BoxFuture<'a, calm_server::error::Result<()>> {
        if !self.failed.swap(true, Ordering::SeqCst) {
            return Box::pin(async {
                Err(CalmError::ServiceUnavailable(
                    "first delivery refused".into(),
                ))
            });
        }
        Box::pin(self.watcher.deliver(track_id, episode_key, text))
    }
}

fn detector(h: &Harness, inbox: Arc<dyn QuietWorkerInbox>) -> WorkerQuietDetector {
    WorkerQuietDetector::new(
        h.sql.clone(),
        h.state.terminal_renderer.clone(),
        inbox,
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

/// Every persisted `track.wake_requested`, of any source.
async fn wake_events(h: &Harness) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'track.wake_requested'")
        .fetch_one(h.sql.pool())
        .await
        .unwrap()
}

/// The keys `card` took planner input under, oldest first: one per message it was sent.
async fn input_keys(h: &Harness, card: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT idempotency_key FROM planner_input_idempotency WHERE card_id = ?1 ORDER BY id",
    )
    .bind(card)
    .fetch_all(h.sql.pool())
    .await
    .unwrap()
}

/// `harness.user_message.enqueued` rows of `card`: its first message and every send that queued.
async fn enqueued(h: &Harness, card: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM events \
          WHERE kind = 'harness.user_message.enqueued' AND scope_card = ?1",
    )
    .bind(card)
    .fetch_one(h.sql.pool())
    .await
    .unwrap()
}

/// Wait until `needle` reached `card`: a turn the fake app-server started, or the card's persisted
/// queue (the fake never completes a turn, so later messages wait there).
async fn await_delivered(h: &Harness, card: &str, needle: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let mut texts: Vec<String> = h
            .state
            .shared_codex_appserver
            .started_turns_for_test()
            .into_iter()
            .flat_map(|(_, items)| items)
            .filter_map(|item| match item {
                calm_server::codex_appserver::InputItem::Text { text } => Some(text),
                _ => None,
            })
            .collect();
        let states: Vec<Option<String>> =
            sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE card_id = ?1")
                .bind(card)
                .fetch_all(h.sql.pool())
                .await
                .unwrap();
        for state in states.into_iter().flatten() {
            let parsed: Value = serde_json::from_str(&state).unwrap();
            for obs in parsed["pending_queue"]
                .as_array()
                .cloned()
                .unwrap_or_default()
            {
                texts.push(obs["text"].as_str().unwrap_or_default().to_owned());
            }
        }
        if texts.iter().any(|text| text.contains(needle)) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{needle:?} never reached the watcher: {texts:#?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Sweep once past every fixture's threshold and require that only the positive control is handed
/// over, with its watch message.
async fn assert_only_control_delivered(h: &Harness, control: &Worker) {
    let recorder = Arc::new(Recorder::default());
    let mut d = detector(h, recorder.clone());
    let key = format!("{}:{T0}", control.task);
    assert_eq!(d.tick(T0 + 10 * QUIET_MS).await, vec![key.clone()]);
    let text = watch_text(&task_key(h, &control.task).await, &control.task, 5).unwrap();
    assert_eq!(
        *recorder.0.lock().unwrap(),
        vec![(h.track.clone(), key, text)]
    );
}

async fn task_key(h: &Harness, attempt_id: &str) -> String {
    h.sql.task_get(attempt_id).await.unwrap().unwrap().key
}

#[tokio::test]
async fn a_quiet_worker_goes_to_the_tracks_watcher_once_per_episode_never_to_the_planner() {
    let h = Harness::start_with_fake_codex().await;
    let (w, entry) = quiet_worker(&h, "claude").await;
    let key = task_key(&h, &w.task).await;
    let watcher = Arc::new(WorkerWatcher::new(&h.state));
    let mut d = detector(&h, watcher.clone());
    let card = watcher_card_id(&h.track);

    // Output is recent: nothing is handed over, and no watcher exists yet.
    assert!(d.tick(T0 + QUIET_MS - 1).await.is_empty());
    assert!(h.sql.card_get(&card).await.unwrap().is_none());

    // Quiet for the threshold: the watcher is created, a plain Assistant, and sent the episode.
    let first = format!("{}:{T0}", w.task);
    assert_eq!(d.tick(T0 + QUIET_MS).await, vec![first.clone()]);
    let (kind, role, profile): (String, String, String) = sqlx::query_as(
        "SELECT kind, role, json_extract(payload, '$.harness_profile') FROM cards WHERE id = ?1",
    )
    .bind(&card)
    .fetch_one(h.sql.pool())
    .await
    .unwrap();
    assert_eq!(
        (kind.as_str(), role.as_str(), profile.as_str()),
        ("codex", "assistant", "assistant")
    );
    let first_text = watch_text(&key, &w.task, 5).unwrap();
    assert!(first_text.contains(&format!("Task {key} (attempt_id {})", w.task)));
    assert_eq!(input_keys(&h, &card).await, vec![first.clone()]);
    assert_eq!(
        enqueued(&h, &card).await,
        2,
        "its first message and the episode's"
    );
    await_delivered(&h, &card, &first_text).await;

    // The same episode is never handed over again, and a replay under its key queues nothing.
    assert!(d.tick(T0 + 4 * QUIET_MS).await.is_empty());
    watcher
        .deliver(&h.track, &first, first_text.clone())
        .await
        .unwrap();
    assert_eq!(input_keys(&h, &card).await, vec![first.clone()]);
    assert_eq!(enqueued(&h, &card).await, 2);

    // New output starts a new episode; its own quiet goes to the same watcher.
    let t1 = T0 + 10 * QUIET_MS;
    entry.last_output_ms.store(t1, Ordering::SeqCst);
    assert!(d.tick(t1 + 1_000).await.is_empty());
    let second = format!("{}:{t1}", w.task);
    assert_eq!(d.tick(t1 + 2 * QUIET_MS).await, vec![second.clone()]);
    assert_eq!(input_keys(&h, &card).await, vec![first, second]);
    assert_eq!(enqueued(&h, &card).await, 3);
    let assistants: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM cards WHERE track_id = ?1 AND role = 'assistant'")
            .bind(&h.track)
            .fetch_one(h.sql.pool())
            .await
            .unwrap();
    assert_eq!(assistants, 1, "one watcher per Track");

    // The Planner is never woken by the detector itself.
    assert_eq!(wake_events(&h).await, 0);
}

#[tokio::test]
async fn a_failed_delivery_stays_due_and_is_retried_under_the_episode_key() {
    let h = Harness::start_with_fake_codex().await;
    let (w, _entry) = quiet_worker(&h, "codex").await;
    let mut d = detector(
        &h,
        Arc::new(FailFirst {
            watcher: WorkerWatcher::new(&h.state),
            failed: AtomicBool::new(false),
        }),
    );
    let card = watcher_card_id(&h.track);
    assert!(d.tick(T0 + QUIET_MS).await.is_empty());
    assert!(h.sql.card_get(&card).await.unwrap().is_none());
    let episode = format!("{}:{T0}", w.task);
    assert_eq!(d.tick(T0 + 2 * QUIET_MS).await, vec![episode.clone()]);
    assert_eq!(input_keys(&h, &card).await, vec![episode]);
    assert_eq!(wake_events(&h).await, 0);
}

#[tokio::test]
async fn a_quiet_codex_worker_is_handed_over_too() {
    let h = Harness::start().await;
    let (w, _entry) = quiet_worker(&h, "codex").await;
    let recorder = Arc::new(Recorder::default());
    let mut d = detector(&h, recorder);
    assert_eq!(
        d.tick(T0 + QUIET_MS).await,
        vec![format!("{}:{T0}", w.task)]
    );
}

#[tokio::test]
async fn a_task_without_an_agent_worker_is_not_handed_over() {
    let h = Harness::start().await;
    let (control, _control) = quiet_worker(&h, "claude").await;
    // A terminal task's command may run silently for long; quiet says nothing about it.
    let (_terminal, _terminal_entry) = quiet_worker(&h, "terminal").await;
    // A child-Track route row: its worker is another Track, never this card.
    let (child, _child_entry) = quiet_worker(&h, "codex").await;
    sqlx::query("UPDATE tasks SET spawn = ?2 WHERE id = ?1")
        .bind(&child.task)
        .bind(calm_types::task_recovery::TASK_CHILD_TRACK_ROUTE)
        .execute(h.sql.pool())
        .await
        .unwrap();
    assert_only_control_delivered(&h, &control).await;
}

#[tokio::test]
async fn a_task_that_is_not_running_is_not_handed_over() {
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
    assert_only_control_delivered(&h, &control).await;
}

#[tokio::test]
async fn a_superseded_attempt_is_not_handed_over() {
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
    assert_only_control_delivered(&h, &control).await;
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
async fn a_card_that_is_not_the_task_worker_is_not_handed_over() {
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
    assert_only_control_delivered(&h, &control).await;
}

#[tokio::test]
async fn a_worker_without_a_live_readable_printing_pty_is_not_handed_over() {
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
    assert_only_control_delivered(&h, &control).await;
}
