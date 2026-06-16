//! #741 §1.3 — the durable codex worker-liveness feeder (T2, OBSERVATIONAL).
//!
//! A long-lived task, spawned at dispatcher construction behind the SAME
//! kill-switch as the reaper (`NEIGE_REAPER_DISABLED`), that subscribes to the
//! shared codex daemon notification stream and push-feeds the durable
//! `worker_sessions.{last_activity_ms,last_thread_status}` columns (added inert
//! in 741-1) keyed by codex `thread_id`.
//!
//! It writes ONLY those two `worker_sessions`-only columns (never
//! `updated_at_ms`, never `runtimes`) via
//! [`SessionRepo::session_record_activity_by_thread`], so it is parity-safe.
//! Nothing CONSUMES these columns yet — the reaper does not read them until
//! 741-3. This slice only keeps them fresh.

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::broadcast::error::RecvError;

use crate::codex_appserver::{Notification, ThreadActiveFlag, ThreadStatus};
use crate::db::prelude::*;
use crate::model::now_ms;

/// Map a `thread/status/changed` raw `status` JSON (`{ "type": "active",
/// "activeFlags": [...] }`, design §1.3) to the short string persisted in
/// `last_thread_status`. Pure + total: any shape that fails to parse degrades
/// to `"active"` (conservative — recent traffic implies the thread is busy,
/// and the next clean `ThreadStatusChanged` / the authoritative live
/// `thread_read` pull corrects it).
///
/// The mapping:
/// * `active` + `waitingOnUserInput` ⇒ `"waitingOnUserInput"`
/// * `active` + `waitingOnApproval`  ⇒ `"waitingOnApproval"`
///   (user-input wins if BOTH are set — the stronger human-block signal)
/// * `active` (no flag)              ⇒ `"active"`
/// * `idle` / `systemError` / `notLoaded` ⇒ that `type`
pub fn status_str_from_value(status: &Value) -> &'static str {
    match serde_json::from_value::<ThreadStatus>(status.clone()) {
        Ok(parsed) => status_str_from_thread_status(&parsed),
        // Unknown / malformed status shape: treat as active (recent traffic),
        // never drop the activity stamp.
        Err(_) => "active",
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
/// * `TurnStarted` / `TurnCompleted` ⇒ `"active"` (recent traffic; the next
///   `ThreadStatusChanged` / the reaper's authoritative live `thread_read`
///   corrects the status).
///
/// Everything else is DROPPED (`None`): per-token `item/*` deltas
/// (`item/agentMessage/delta`, …), `thread/started`, and any `Other` method.
/// Turn granularity fully satisfies the reaper's 15-min DEADLINE pre-gate, so a
/// `worker_sessions` write per agent-message chunk would be pure
/// `begin_immediate_tx` write contention with no consumer benefit.
fn stamp_status_for(notification: &Notification) -> Option<&'static str> {
    match notification {
        Notification::ThreadStatusChanged { status, .. } => Some(status_str_from_value(status)),
        Notification::TurnStarted { .. } | Notification::TurnCompleted { .. } => Some("active"),
        Notification::Item { .. }
        | Notification::ThreadStarted { .. }
        | Notification::Other { .. } => None,
    }
}

/// Run the durable liveness feeder loop until the notification channel closes.
/// Each event is classified by [`stamp_status_for`]; only turn-boundary + status
/// events stamp `worker_sessions.{last_activity_ms,last_thread_status}` keyed by
/// `thread_id`, the rest are dropped (no write).
pub async fn run_liveness_feeder(
    repo: Arc<dyn Repo>,
    mut rx: tokio::sync::broadcast::Receiver<Notification>,
) {
    loop {
        match rx.recv().await {
            Ok(notification) => {
                let Some(status_str) = stamp_status_for(&notification) else {
                    continue;
                };
                let Some(thread_id) = notification.thread_id() else {
                    continue;
                };
                if let Err(e) = repo
                    .session_record_activity_by_thread(thread_id, now_ms(), status_str)
                    .await
                {
                    tracing::warn!(
                        target = "liveness_feeder",
                        %thread_id,
                        error = %e,
                        "durable liveness write failed (observational; ignored)"
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

    #[test]
    fn malformed_status_degrades_to_active() {
        // Unknown `type` / missing fields ⇒ conservative "active".
        assert_eq!(status_str_from_value(&json!({ "type": "wat" })), "active");
        assert_eq!(status_str_from_value(&Value::Null), "active");
        assert_eq!(status_str_from_value(&json!({ "no": "type" })), "active");
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

    #[test]
    fn stamps_turn_started_and_completed_active() {
        let started = Notification::TurnStarted {
            thread_id: "t1".into(),
            turn: json!({ "id": "turn-1" }),
        };
        let completed = Notification::TurnCompleted {
            thread_id: "t1".into(),
            turn: json!({ "id": "turn-1" }),
        };
        assert_eq!(stamp_status_for(&started), Some("active"));
        assert_eq!(stamp_status_for(&completed), Some("active"));
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
}
