//! #741 §1.3 — the durable codex worker-liveness feeder (T2, OBSERVATIONAL).
//!
//! A long-lived task, spawned at dispatcher construction behind the SAME
//! kill-switch as the reaper (`NEIGE_REAPER_DISABLED`), that subscribes to the
//! shared codex daemon notification stream and push-feeds the durable
//! `worker_sessions.{last_activity_ms,last_thread_status}` columns keyed by
//! codex `thread_id`.
//!
//! It writes ONLY those two columns — plus, on a `turn/completed` whose
//! `turn.status` is `completed`, the monotone
//! `worker_sessions.last_turn_completed_ms` (#1722 §4.2.1) — via
//! [`SessionRepo::session_record_activity_by_thread`]; never `updated_at_ms`,
//! which orders projection reads.
//!
//! Both columns gate `Reaper::sweep_all` (`reaper/mod.rs`): stop stamping them
//! and a live but quiet codex session ages past the deadline and is reapable.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::broadcast::error::RecvError;

use crate::codex_appserver::{Notification, ThreadActiveFlag, ThreadStatus};
use crate::db::prelude::*;
use crate::model::now_ms;

/// Map a `thread/status/changed` raw `status` JSON (`{ "type": "active",
/// "activeFlags": [...] }`, design §1.3) to the short string persisted in
/// `last_thread_status`. Pure + total: any shape that fails to parse degrades
/// to `"unknown"` (#1722 §4.2.1 — fail-closed: `active` is the track
/// activity projector's `working` evidence, so a status shape this binary
/// does not know must not light an idle interactive card; `"unknown"` is
/// outside every projector predicate and, for the reaper, gets the same
/// 900 s time pre-gate as `idle` because the stamp still writes
/// `last_activity_ms`).
///
/// The mapping:
/// * `active` + `waitingOnUserInput` ⇒ `"waitingOnUserInput"`
/// * `active` + `waitingOnApproval`  ⇒ `"waitingOnApproval"`
///   (user-input wins if BOTH are set — the stronger human-block signal)
/// * `active` (no flag)              ⇒ `"active"`
/// * `idle` / `systemError` / `notLoaded` ⇒ that `type`
/// * anything else                   ⇒ `"unknown"`
pub fn status_str_from_value(status: &Value) -> &'static str {
    match serde_json::from_value::<ThreadStatus>(status.clone()) {
        Ok(parsed) => status_str_from_thread_status(&parsed),
        Err(_) => "unknown",
    }
}

/// The pure core of [`status_str_from_value`] over the typed [`ThreadStatus`].
fn status_str_from_thread_status(status: &ThreadStatus) -> &'static str {
    match status {
        ThreadStatus::Active { active_flags } => {
            if active_flags.contains(&ThreadActiveFlag::WaitingOnUserInput) {
                "waitingOnUserInput"
            } else if active_flags.contains(&ThreadActiveFlag::WaitingOnApproval) {
                "waitingOnApproval"
            } else {
                "active"
            }
        }
        ThreadStatus::Idle => "idle",
        ThreadStatus::SystemError => "systemError",
        ThreadStatus::NotLoaded => "notLoaded",
    }
}

/// The per-notification feeder decision: which `last_thread_status` to stamp,
/// or `None` to DROP the event without touching `worker_sessions`.
///
/// Stamped ONLY on TURN-BOUNDARY + STATUS events:
/// * `ThreadStatusChanged` ⇒ the precise mapped status;
/// * `TurnStarted` ⇒ `"active"`;
/// * `TurnCompleted` ⇒ by the turn's `status` ([`turn_completed_status`]):
///   `failed` ⇒ `"systemError"`, otherwise `"idle"`. This makes
///   `turn/completed` the LAST stamp of a turn (#1722 §4.2.1): on the
///   installed codex the value a finished shared-daemon thread rests at
///   used to be `active` (F2.29), which the activity projector would read
///   as still working.
///
/// Everything else is DROPPED (`None`): per-token `item/*` deltas
/// (`item/agentMessage/delta`, …), `thread/started`, and any `Other` method.
/// Turn granularity fully satisfies the reaper's 15-min DEADLINE pre-gate, so a
/// `worker_sessions` write per agent-message chunk would be pure
/// `begin_immediate_tx` write contention with no consumer benefit.
fn stamp_status_for(notification: &Notification) -> Option<&'static str> {
    match notification {
        Notification::ThreadStatusChanged { status, .. } => Some(status_str_from_value(status)),
        Notification::TurnStarted { .. } => Some("active"),
        Notification::TurnCompleted { turn, .. } => Some(turn_completed_status(turn)),
        Notification::Item { .. }
        | Notification::ThreadStarted { .. }
        | Notification::Other { .. } => None,
    }
}

/// The stamp for a `turn/completed` `turn` object by its `status`
/// (`completed` / `failed` / `interrupted`, F2.7). `failed` keeps the
/// `systemError` codex sends just BEFORE the failed `turn/completed`
/// (`harness/run_loop.rs`, the systemError branch) instead of overwriting it
/// with `idle`; `completed`, `interrupted`, and any shape without a
/// recognisable status all rest at `idle` — the turn is over either way.
fn turn_completed_status(turn: &Value) -> &'static str {
    match turn.get("status").and_then(Value::as_str) {
        Some("failed") => "systemError",
        _ => "idle",
    }
}

/// Is this a `turn/completed` whose turn actually COMPLETED (`status =
/// completed`)? Only then does the stamp also raise
/// `last_turn_completed_ms` (#1722 §4.2.1 (2)); an interrupted or failed
/// turn stamps its status but leaves the completion column alone.
fn completed_turn(notification: &Notification) -> bool {
    matches!(
        notification,
        Notification::TurnCompleted { turn, .. }
            if turn.get("status").and_then(Value::as_str) == Some("completed")
    )
}

/// A stamp whose durable write failed once, kept for ONE replay on the same
/// thread's next notification (#1722 §4.2.1 — no timer, no queue: one slot
/// per thread, at most two attempts per stamp).
#[derive(Debug, Clone, Copy)]
struct PendingStamp {
    at_ms: i64,
    status: &'static str,
    /// `Some(at_ms)` when the stamp came from a completed turn — the replay
    /// must raise `last_turn_completed_ms` exactly as the first attempt
    /// would have.
    turn_completed_ms: Option<i64>,
}

/// Run the durable liveness feeder loop until the notification channel closes.
/// Each event is classified by [`stamp_status_for`]; only turn-boundary + status
/// events stamp `worker_sessions.{last_activity_ms,last_thread_status}` keyed by
/// `thread_id`, the rest are dropped (no write).
pub async fn run_liveness_feeder(
    repo: Arc<dyn Repo>,
    rx: tokio::sync::broadcast::Receiver<Notification>,
) {
    run_feeder_loop(rx, |thread_id, at_ms, status, turn_completed_ms| {
        let repo = repo.clone();
        async move {
            repo.session_record_activity_by_thread(&thread_id, at_ms, status, turn_completed_ms)
                .await
        }
    })
    .await
}

/// The feeder loop over an injectable durable writer (`(thread_id, at_ms,
/// status, turn_completed_ms) → Result`), so the replay policy can be driven
/// by tests without a repo that fails on demand. Production passes
/// `SessionRepo::session_record_activity_by_thread`. `turn_completed_ms` is
/// `Some(at_ms)` for a completed turn's stamp and `None` otherwise.
///
/// Write-failure policy: a failed write is remembered per thread and replayed
/// FIRST when that thread's next stampable notification arrives (with its
/// original timestamp, so ordering is preserved); a replay that fails again
/// is dropped with a warning. The new stamp is then written as usual and, if
/// it fails, takes the slot. No timers, no queues.
async fn run_feeder_loop<W, Fut, E>(
    mut rx: tokio::sync::broadcast::Receiver<Notification>,
    mut write: W,
) where
    W: FnMut(String, i64, &'static str, Option<i64>) -> Fut,
    Fut: Future<Output = Result<(), E>>,
    E: std::fmt::Display,
{
    let mut pending: HashMap<String, PendingStamp> = HashMap::new();
    loop {
        match rx.recv().await {
            Ok(notification) => {
                let Some(status_str) = stamp_status_for(&notification) else {
                    continue;
                };
                let Some(thread_id) = notification.thread_id() else {
                    continue;
                };
                if let Some(prev) = pending.remove(thread_id)
                    && let Err(e) = write(
                        thread_id.to_string(),
                        prev.at_ms,
                        prev.status,
                        prev.turn_completed_ms,
                    )
                    .await
                {
                    tracing::warn!(
                        target = "liveness_feeder",
                        %thread_id,
                        status = prev.status,
                        error = %e,
                        "durable liveness replay failed (second consecutive failure); stamp dropped"
                    );
                }
                let at_ms = now_ms();
                let turn_completed_ms = completed_turn(&notification).then_some(at_ms);
                if let Err(e) =
                    write(thread_id.to_string(), at_ms, status_str, turn_completed_ms).await
                {
                    tracing::warn!(
                        target = "liveness_feeder",
                        %thread_id,
                        status = status_str,
                        error = %e,
                        "durable liveness write failed; will replay on this thread's next notification"
                    );
                    pending.insert(
                        thread_id.to_string(),
                        PendingStamp {
                            at_ms,
                            status: status_str,
                            turn_completed_ms,
                        },
                    );
                }
            }
            Err(RecvError::Lagged(n)) => {
                // We missed `n` notifications. The columns are best-effort
                // recency hints (the live `thread_read` pull is authoritative
                // for the reaper), so a lag is benign — log and keep going.
                tracing::warn!(
                    target = "liveness_feeder",
                    skipped = n,
                    "liveness feeder lagged; missed activity notifications"
                );
            }
            Err(RecvError::Closed) => break,
        }
    }
}

/// Spawn [`run_liveness_feeder`] on the runtime, returning the join handle.
/// The caller MUST take `rx` via `SharedCodexAppServer::subscribe_notifications`
/// BEFORE the `Arc<SharedCodexAppServer>` is moved into the provider registry.
pub fn spawn_liveness_feeder(
    repo: Arc<dyn Repo>,
    rx: tokio::sync::broadcast::Receiver<Notification>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run_liveness_feeder(repo, rx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ===================================================================
    // status_str_from_value — the pure §1.3 mapping (its own unit test).
    // ===================================================================

    #[test]
    fn maps_active_no_flags_to_active() {
        let v = json!({ "type": "active", "activeFlags": [] });
        assert_eq!(status_str_from_value(&v), "active");
    }

    #[test]
    fn maps_active_waiting_on_user_input() {
        let v = json!({ "type": "active", "activeFlags": ["waitingOnUserInput"] });
        assert_eq!(status_str_from_value(&v), "waitingOnUserInput");
    }

    #[test]
    fn maps_active_waiting_on_approval() {
        let v = json!({ "type": "active", "activeFlags": ["waitingOnApproval"] });
        assert_eq!(status_str_from_value(&v), "waitingOnApproval");
    }

    #[test]
    fn maps_active_both_flags_prefers_user_input() {
        // Both flags set: user-input is the stronger human-block signal.
        let v = json!({
            "type": "active",
            "activeFlags": ["waitingOnApproval", "waitingOnUserInput"]
        });
        assert_eq!(status_str_from_value(&v), "waitingOnUserInput");
    }

    #[test]
    fn maps_idle() {
        let v = json!({ "type": "idle" });
        assert_eq!(status_str_from_value(&v), "idle");
    }

    #[test]
    fn maps_system_error() {
        let v = json!({ "type": "systemError" });
        assert_eq!(status_str_from_value(&v), "systemError");
    }

    #[test]
    fn maps_not_loaded() {
        let v = json!({ "type": "notLoaded" });
        assert_eq!(status_str_from_value(&v), "notLoaded");
    }

    /// #1722 §4.2.1 — an unparsable status shape is `"unknown"`, never
    /// `"active"` (which the activity projector reads as working).
    #[test]
    fn unknown_status_shape_stamps_unknown() {
        assert_eq!(
            status_str_from_value(&json!({ "type": "somethingNew" })),
            "unknown"
        );
        assert_eq!(status_str_from_value(&json!({ "type": "wat" })), "unknown");
        assert_eq!(status_str_from_value(&Value::Null), "unknown");
        assert_eq!(status_str_from_value(&json!({ "no": "type" })), "unknown");
        let n = Notification::ThreadStatusChanged {
            thread_id: "t1".into(),
            status: json!({ "type": "somethingNew" }),
        };
        assert_eq!(stamp_status_for(&n), Some("unknown"));
    }

    // ===================================================================
    // stamp_status_for — the per-notification feeder decision: turn/status
    // events stamp, everything else (item/*, thread/started, Other) drops.
    // ===================================================================

    #[test]
    fn stamps_thread_status_changed_with_mapped_status() {
        let n = Notification::ThreadStatusChanged {
            thread_id: "t1".into(),
            status: json!({ "type": "active", "activeFlags": ["waitingOnApproval"] }),
        };
        assert_eq!(stamp_status_for(&n), Some("waitingOnApproval"));
    }

    // ===================================================================
    // #1722 §4.2.1 — turn boundaries. Each sequence test drives the real
    // feeder loop through a recording writer and asserts the LAST value the
    // thread rests at, so a `turn/completed` that stamped `active` again
    // (the pre-#1722 shape) reddens the test rather than a helper.
    // ===================================================================

    /// One successful durable write as the recording writer saw it:
    /// `(thread_id, at_ms, status, turn_completed_ms)`.
    type Write = (String, i64, &'static str, Option<i64>);

    /// Run `notifications` through [`run_feeder_loop`] with a writer that
    /// records `(thread_id, at_ms, status)` for every successful write and
    /// fails whenever `fail(thread_id, status)` says so (a failed attempt is
    /// NOT recorded). Returns the recorded writes in order.
    async fn drive(
        notifications: Vec<Notification>,
        mut fail: impl FnMut(&str, &'static str) -> bool + Send + 'static,
    ) -> Vec<Write> {
        use std::sync::{Arc, Mutex};
        let writes: Arc<Mutex<Vec<Write>>> = Arc::default();
        let (tx, rx) = tokio::sync::broadcast::channel(64);
        for n in notifications {
            tx.send(n).unwrap();
        }
        drop(tx); // the loop exits on `Closed` once the backlog is drained
        let sink = writes.clone();
        run_feeder_loop(rx, move |thread_id, at_ms, status, turn_completed_ms| {
            let failed = fail(&thread_id, status);
            if !failed {
                sink.lock()
                    .unwrap()
                    .push((thread_id, at_ms, status, turn_completed_ms));
            }
            async move {
                if failed {
                    Err(std::io::Error::other("injected write failure"))
                } else {
                    Ok(())
                }
            }
        })
        .await;
        writes.lock().unwrap().clone()
    }

    fn status_changed(thread_id: &str, status: Value) -> Notification {
        Notification::ThreadStatusChanged {
            thread_id: thread_id.into(),
            status,
        }
    }

    fn turn_completed(thread_id: &str, status: &str) -> Notification {
        Notification::TurnCompleted {
            thread_id: thread_id.into(),
            turn: json!({ "id": "turn-1", "status": status, "items": [] }),
        }
    }

    fn statuses(writes: &[Write]) -> Vec<&'static str> {
        writes.iter().map(|(_, _, s, _)| *s).collect()
    }

    /// `[status idle, turn/completed{completed}] → idle`.
    #[tokio::test]
    async fn turn_completed_stamps_idle() {
        let writes = drive(
            vec![
                status_changed("t1", json!({ "type": "idle" })),
                turn_completed("t1", "completed"),
            ],
            |_, _| false,
        )
        .await;
        assert_eq!(statuses(&writes), ["idle", "idle"]);
        assert_eq!(writes.last().map(|w| w.2), Some("idle"));
        // Only the COMPLETED turn's stamp carries the completion instant.
        assert_eq!(writes[0].3, None, "a status stamp never completes a turn");
        assert_eq!(
            writes[1].3,
            Some(writes[1].1),
            "turn/completed{{completed}} carries its own at_ms as the completion"
        );
        // `interrupted` rests at idle too; a missing status still means the
        // turn is over.
        let writes = drive(
            vec![
                turn_completed("t1", "interrupted"),
                Notification::TurnCompleted {
                    thread_id: "t1".into(),
                    turn: json!({ "id": "turn-2" }),
                },
            ],
            |_, _| false,
        )
        .await;
        assert_eq!(statuses(&writes), ["idle", "idle"]);
        assert!(
            writes.iter().all(|w| w.3.is_none()),
            "an interrupted or status-less turn never completes: {writes:?}"
        );
    }

    /// `[status systemError, turn/completed{failed}] → systemError`.
    #[tokio::test]
    async fn failed_turn_keeps_system_error() {
        let writes = drive(
            vec![
                status_changed("t1", json!({ "type": "systemError" })),
                turn_completed("t1", "failed"),
            ],
            |_, _| false,
        )
        .await;
        assert_eq!(statuses(&writes), ["systemError", "systemError"]);
        assert_eq!(writes.last().map(|w| w.2), Some("systemError"));
        assert!(
            writes.iter().all(|w| w.3.is_none()),
            "a failed turn never writes last_turn_completed_ms"
        );
    }

    /// `[turn/started] → active`.
    #[tokio::test]
    async fn turn_started_stamps_active() {
        let writes = drive(
            vec![Notification::TurnStarted {
                thread_id: "t1".into(),
                turn: json!({ "id": "turn-1" }),
            }],
            |_, _| false,
        )
        .await;
        assert_eq!(statuses(&writes), ["active"]);
        let started = Notification::TurnStarted {
            thread_id: "t1".into(),
            turn: json!({ "id": "turn-1" }),
        };
        assert_eq!(stamp_status_for(&started), Some("active"));
    }

    // ===================================================================
    // #1722 §4.2.1 (3) — failed-write replay: one slot per thread, replayed
    // before that thread's next stamp, dropped after a second failure.
    // ===================================================================

    #[tokio::test]
    async fn failed_write_is_replayed_before_the_threads_next_stamp() {
        let mut attempts = 0usize;
        let writes = drive(
            vec![
                turn_completed("t1", "completed"), // fails once
                // other thread: no replay
                status_changed("t2", json!({ "type": "active", "activeFlags": [] })),
                Notification::TurnStarted {
                    thread_id: "t1".into(),
                    turn: json!({ "id": "turn-2" }),
                }, // replays t1's idle first, then stamps active
            ],
            move |thread_id, _| {
                attempts += 1;
                thread_id == "t1" && attempts == 1
            },
        )
        .await;
        let order: Vec<(&str, &str)> = writes.iter().map(|(t, _, s, _)| (t.as_str(), *s)).collect();
        assert_eq!(
            order,
            [("t2", "active"), ("t1", "idle"), ("t1", "active")],
            "the failed idle stamp is replayed on t1's next notification, before the new stamp"
        );
        // The replay carries the ORIGINAL timestamp, never a fresher one.
        let t1: Vec<(i64, Option<i64>)> = writes
            .iter()
            .filter(|(t, _, _, _)| t == "t1")
            .map(|(_, at, _, done)| (*at, *done))
            .collect();
        assert!(
            t1[0].0 <= t1[1].0,
            "replayed at_ms {} > new at_ms {}",
            t1[0].0,
            t1[1].0
        );
        // The replayed completed-turn stamp still carries its ORIGINAL
        // completion instant; the fresh `turn/started` stamp carries none.
        assert_eq!(t1[0].1, Some(t1[0].0));
        assert_eq!(t1[1].1, None);
    }

    #[tokio::test]
    async fn stamp_is_dropped_after_two_consecutive_failures() {
        let mut t1_attempts = 0usize;
        let writes = drive(
            vec![
                turn_completed("t1", "completed"), // fails
                Notification::TurnStarted {
                    thread_id: "t1".into(),
                    turn: json!({ "id": "turn-2" }),
                }, // replay fails again → dropped; new stamp succeeds
                turn_completed("t1", "completed"), // no replay pending any more
            ],
            move |thread_id, _| {
                if thread_id != "t1" {
                    return false;
                }
                t1_attempts += 1;
                t1_attempts <= 2
            },
        )
        .await;
        assert_eq!(
            statuses(&writes),
            ["active", "idle"],
            "the twice-failed idle stamp is gone; nothing is retried a third time"
        );
    }

    #[test]
    fn drops_item_token_delta_events() {
        // Per-token `item/*` deltas must NOT stamp — turn granularity is enough
        // for the DEADLINE pre-gate, and per-token writes are pure contention.
        let n = Notification::Item {
            method: "item/agentMessage/delta".into(),
            params: json!({ "threadId": "t1", "delta": "x" }),
        };
        assert_eq!(stamp_status_for(&n), None);
    }

    #[test]
    fn drops_thread_started_and_other_events() {
        let started = Notification::ThreadStarted {
            params: json!({ "thread": { "id": "t1" } }),
        };
        let other = Notification::Other {
            method: "some/unmodeled".into(),
            params: json!({ "threadId": "t1" }),
        };
        assert_eq!(stamp_status_for(&started), None);
        assert_eq!(stamp_status_for(&other), None);
    }

    // ===================================================================
    // #1722 §4.2.1 (2) — `last_turn_completed_ms`, through the REAL repo
    // writer: written only for a completed turn, raised monotonically.
    // ===================================================================

    /// Mint area → track → codex card → running session through the
    /// production creation helpers (`session_start_runtime_tx` links
    /// `cards.session_id`); returns `(repo, session id)`.
    async fn seed_codex_session(thread_id: &str) -> (Arc<crate::db::sqlite::SqlxRepo>, String) {
        use crate::db::sqlite::{
            SqlxRepo, area_create_tx, card_create_with_id_tx, session_start_runtime_tx,
            track_create_tx,
        };
        use crate::model::{CardRole, NewArea, NewCard, NewTrack, RequestTheme};
        use crate::session_projection_repo::{
            AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
        };
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let mut tx = repo.pool().begin().await.unwrap();
        let area = area_create_tx(
            &mut tx,
            NewArea {
                name: "a".into(),
                color: "#fff".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
        let track = track_create_tx(
            &mut tx,
            NewTrack {
                template_input: None,
                area_id: area.id.clone(),
                title: "t".into(),
                sort: None,
                cwd: "/tmp".into(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            },
            None,
            &crate::db::sqlite::TrackWorkspacePlan::AttachedFromCwd,
            None,
            repo.track_area_cache(),
        )
        .await
        .unwrap();
        let card = card_create_with_id_tx(
            &mut tx,
            "card-feeder".to_string(),
            NewCard {
                track_id: track.id.clone(),
                title: None,
                kind: "codex".into(),
                sort: None,
                payload: json!({}),
            },
            CardRole::Worker,
            true,
            repo.card_role_cache(),
        )
        .await
        .unwrap();
        let session_id = "ws-feeder".to_string();
        session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit {
                id: session_id.clone(),
                card_id: card.id.as_str().to_string(),
                kind: WorkerSessionKind::CodexCard,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Running,
                terminal_run_id: None,
                thread_id: Some(thread_id.to_string()),
                session_id: None,
                active_turn_id: None,
                handle_state_json: None,
                spawn_op_id: None,
                now_ms: now_ms(),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (repo, session_id)
    }

    async fn last_turn_completed_ms(
        repo: &crate::db::sqlite::SqlxRepo,
        session_id: &str,
    ) -> Option<i64> {
        sqlx::query_scalar("SELECT last_turn_completed_ms FROM worker_sessions WHERE id = ?1")
            .bind(session_id)
            .fetch_one(repo.pool())
            .await
            .unwrap()
    }

    async fn last_activity(repo: &crate::db::sqlite::SqlxRepo, session_id: &str) -> (i64, String) {
        let row: (i64, String) = sqlx::query_as(
            "SELECT last_activity_ms, last_thread_status FROM worker_sessions WHERE id = ?1",
        )
        .bind(session_id)
        .fetch_one(repo.pool())
        .await
        .unwrap();
        row
    }

    /// `completed@t2` then `completed@t1 < t2` does not lower the column;
    /// `interrupted@t3` and `failed@t4` stamp their status but do not write
    /// it — both through the storage writer with pinned instants and through
    /// the real feeder loop over the same repo.
    #[tokio::test]
    async fn last_turn_completed_only_on_completed_and_monotone() {
        let (repo, ws) = seed_codex_session("th-done").await;
        assert_eq!(last_turn_completed_ms(&repo, &ws).await, None);

        // Storage layer, pinned instants.
        repo.session_record_activity_by_thread("th-done", 2_000, "idle", Some(2_000))
            .await
            .unwrap();
        assert_eq!(last_turn_completed_ms(&repo, &ws).await, Some(2_000));
        // A late replay of an OLDER completion never lowers it.
        repo.session_record_activity_by_thread("th-done", 1_000, "idle", Some(1_000))
            .await
            .unwrap();
        assert_eq!(last_turn_completed_ms(&repo, &ws).await, Some(2_000));
        assert_eq!(last_activity(&repo, &ws).await, (1_000, "idle".into()));
        // A stamp without a completion (interrupted turn → idle, failed turn
        // → systemError, plain status) leaves the column alone.
        repo.session_record_activity_by_thread("th-done", 3_000, "idle", None)
            .await
            .unwrap();
        repo.session_record_activity_by_thread("th-done", 4_000, "systemError", None)
            .await
            .unwrap();
        assert_eq!(last_turn_completed_ms(&repo, &ws).await, Some(2_000));
        assert_eq!(
            last_activity(&repo, &ws).await,
            (4_000, "systemError".into())
        );

        // The real loop over the real writer: only `turn/completed{completed}`
        // moves the column, and it moves it forward.
        let (tx, rx) = tokio::sync::broadcast::channel(16);
        tx.send(turn_completed("th-done", "completed")).unwrap();
        tx.send(turn_completed("th-done", "interrupted")).unwrap();
        tx.send(turn_completed("th-done", "failed")).unwrap();
        tx.send(status_changed(
            "th-done",
            json!({ "type": "active", "activeFlags": [] }),
        ))
        .unwrap();
        drop(tx);
        let repo_dyn: Arc<dyn crate::db::Repo> = repo.clone();
        run_liveness_feeder(repo_dyn, rx).await;
        let completed_at = last_turn_completed_ms(&repo, &ws)
            .await
            .expect("the completed turn wrote the column");
        assert!(
            completed_at > 2_000,
            "moved forward past the pinned value: {completed_at}"
        );
        let (activity_ms, status) = last_activity(&repo, &ws).await;
        assert_eq!(status, "active", "the later stamps still landed");
        assert!(activity_ms >= completed_at);
        // The three later notifications did not move the completion column.
        let again = last_turn_completed_ms(&repo, &ws).await.unwrap();
        assert_eq!(again, completed_at);
    }
}
