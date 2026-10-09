//! `neige_worker_report`'s write: the watcher's verdict on a quiet worker becomes one
//! kernel-templated `track.wake_requested` for its Track's Planner. The event is kernel-only at the
//! role gate, so the kernel writes it after its own checks (the `neige_mail_send` precedent), never
//! under the caller's identity.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use calm_types::observation::WORKER_WATCH_WAKE_SOURCE;

use super::{Verdict, report_line};
use crate::db::sqlite::track_get_tx;
use crate::db::write_with_events_typed;
use crate::error::CalmError;
use crate::event::{Event, EventScope};
use crate::harness::turn_input;
use crate::ids::{ActorId, TrackId};
use crate::mcp_server::framing::{RpcError, calm_error};
use crate::mcp_server::registry::{AppContext, ToolCallIdentity};
use crate::terminal_interaction::{Target, TerminalInteraction};

/// What a report wrote: the wake's key, and whether this call repeated an earlier report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reported {
    pub key: String,
    pub replayed: bool,
}

/// Wake the caller's Planner with `verdict` on `attempt_id`, after the terminal tools' own caller
/// check and same-Track resolution of the attempt.
///
/// The wake's key is `<attempt_id>:<outcome>:<caller turn>`, the turn being the caller session's
/// newest turn input (`turn_input::latest`, the instant it was recorded). A retry of the same
/// report in the same turn finds that key already written and is answered as a replay; a later
/// episode reaches the watcher as a new message and so a new turn, and a different outcome or
/// attempt is a different key, so distinct reports are all delivered.
pub async fn report(
    ctx: &AppContext,
    identity: &ToolCallIdentity,
    attempt_id: &str,
    verdict: &Verdict,
) -> Result<Reported, RpcError> {
    let track = TerminalInteraction::authorize(ctx.repo.as_ref(), identity)
        .await
        .map_err(|error| RpcError::forbidden(error.to_string()))?;
    let resolved = TerminalInteraction::resolve_in_track(
        ctx.repo.as_ref(),
        &track,
        &Target::Attempt(attempt_id.to_string()),
    )
    .await
    .map_err(|error| RpcError::forbidden(error.to_string()))?;
    let task = resolved
        .binding
        .task
        .ok_or_else(|| RpcError::internal("an attempt target resolved without its task"))?;
    let text = report_line(&task.task_key, &task.attempt_id, verdict).map_err(calm_error)?;
    let outcome = verdict.outcome().as_str();
    let (card_id, session_id) = (identity.card_id.clone(), identity.session_id.clone());
    let track = TrackId::from(track);
    let replayed = Arc::new(AtomicBool::new(false));
    let replay = Arc::clone(&replayed);
    let written = write_with_events_typed(
        ctx.repo.as_ref(),
        ActorId::Kernel,
        None,
        &ctx.events,
        &ctx.write,
        move |tx| {
            Box::pin(async move {
                let row = track_get_tx(tx, &track).await?;
                let turn = turn_input::latest(tx, &card_id, &session_id)
                    .await?
                    .ok_or_else(|| {
                        CalmError::Conflict(
                            "this turn's input is not recorded; report again from your next turn"
                                .into(),
                        )
                    })?;
                let key = format!("{}:{outcome}:{}", task.attempt_id, turn.created_at_ms);
                let exists: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM events WHERE scope_track = ?1 \
                       AND kind = 'track.wake_requested' \
                       AND json_extract(payload, '$.source') = ?2 \
                       AND json_extract(payload, '$.key') = ?3)",
                )
                .bind(row.id.as_str())
                .bind(WORKER_WATCH_WAKE_SOURCE)
                .bind(&key)
                .fetch_one(&mut **tx)
                .await?;
                if exists {
                    // Nothing to write; the error only rolls the empty transaction back.
                    replay.store(true, Ordering::SeqCst);
                    return Err(CalmError::Conflict(key));
                }
                let event = Event::TrackWakeRequested {
                    track_id: row.id.clone(),
                    source: WORKER_WATCH_WAKE_SOURCE.into(),
                    key: key.clone(),
                    text,
                };
                let scope = EventScope::Track {
                    track: row.id,
                    area: row.area_id,
                };
                Ok((key, vec![(scope, event)]))
            })
        },
    )
    .await;
    match written {
        Ok((key, _)) => Ok(Reported {
            key,
            replayed: false,
        }),
        Err(CalmError::Conflict(key)) if replayed.load(Ordering::SeqCst) => Ok(Reported {
            key,
            replayed: true,
        }),
        Err(error) => Err(calm_error(error)),
    }
}
