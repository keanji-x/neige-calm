//! The Codex arm's [`PlannerEvent`]s: the app-server's daemon-wide notifications, mapped one for
//! one (#1981 S4). Every notification maps to an event, so the run loop's thread filter sees
//! exactly the frames it saw before the mapping existed. The snapshot write sees them too, except
//! a reply delta (#1923), which the run loop keeps in memory only.

use serde_json::Value;
use tokio::sync::broadcast::{self, error::RecvError};

use crate::codex_appserver::Notification;
use crate::harness::planner_event::{ItemPhase, PlannerEvent, PlannerEventKind};
use crate::shared_codex_appserver::{SharedCodexAppServer, other_turn_id};

/// A subscription to the Codex daemon's notifications, received as [`PlannerEvent`]s.
pub struct CodexEvents(broadcast::Receiver<Notification>);

impl CodexEvents {
    pub fn subscribe(daemon: &SharedCodexAppServer) -> Self {
        Self(daemon.subscribe_notifications())
    }

    /// Cancel safe: the mapping after the inner `recv` never awaits.
    pub async fn recv(&mut self) -> Result<PlannerEvent, RecvError> {
        self.0.recv().await.map(planner_event)
    }
}

/// The event one Codex notification is. The thread is the notification's own
/// ([`Notification::thread_id`]), so a `thread/started` is acted on only when the run loop's
/// thread already equals the one it names.
pub(crate) fn planner_event(notification: Notification) -> PlannerEvent {
    let thread_id = notification.thread_id().map(ToOwned::to_owned);
    let kind = match notification {
        Notification::ThreadStarted { .. } => PlannerEventKind::ThreadStarted,
        Notification::ThreadStatusChanged { status, .. } => {
            match status.get("type").and_then(Value::as_str) {
                Some("systemError") => PlannerEventKind::ThreadSystemError,
                Some("idle") => PlannerEventKind::ThreadIdle,
                _ => PlannerEventKind::Ignored,
            }
        }
        Notification::TurnStarted { turn, .. } => match turn.get("id").and_then(Value::as_str) {
            Some(turn_id) => PlannerEventKind::TurnStarted {
                turn_id: turn_id.to_owned(),
            },
            None => {
                tracing::debug!(?turn, "planner harness ignoring TurnStarted without id");
                PlannerEventKind::Ignored
            }
        },
        Notification::TurnCompleted { turn, .. } => PlannerEventKind::TurnCompleted { turn },
        Notification::Item { method, params } => match method.as_str() {
            "item/started" => PlannerEventKind::Item {
                phase: ItemPhase::Started,
                params,
            },
            "item/completed" => PlannerEventKind::Item {
                phase: ItemPhase::Completed,
                params,
            },
            "item/agentMessage/delta" => reply_delta(&params),
            // Every other delta and `item/*` frame is not stored.
            _ => PlannerEventKind::Ignored,
        },
        Notification::Other { method, params } => match method.as_str() {
            approval if approval.starts_with("approval/") => PlannerEventKind::Approval { method },
            "turn/aborted" => match other_turn_id(&params) {
                Some(turn_id) => PlannerEventKind::TurnAborted {
                    turn_id: turn_id.to_owned(),
                },
                None => {
                    tracing::debug!("planner harness ignoring turn/aborted without a turn id");
                    PlannerEventKind::Ignored
                }
            },
            "turn/plan/updated" => PlannerEventKind::PlanUpdated { params },
            "thread/tokenUsage/updated" => PlannerEventKind::TokenUsage { params },
            _ => PlannerEventKind::Ignored,
        },
    };
    PlannerEvent { thread_id, kind }
}

/// `item/agentMessage/delta` carries `threadId`, `turnId`, `itemId` and `delta`, all required by
/// the app-server schema; a frame missing one is not a delta the run loop can place.
fn reply_delta(params: &Value) -> PlannerEventKind {
    let field = |name: &str| {
        params
            .get(name)
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    };
    match (field("turnId"), field("itemId"), field("delta")) {
        (Some(turn_id), Some(item_id), Some(delta)) => PlannerEventKind::ReplyDelta {
            turn_id,
            item_id,
            delta,
        },
        _ => {
            tracing::debug!("planner harness ignoring item/agentMessage/delta without its fields");
            PlannerEventKind::Ignored
        }
    }
}
