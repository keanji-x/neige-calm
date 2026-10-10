//! `neige_worker_report`'s write: the watcher's verdict on a quiet worker becomes one
//! kernel-templated `track.wake_requested` for its Track's Planner. The event is kernel-only at the
//! role gate, so the kernel writes it after its own checks (the `neige_mail_send` precedent), never
//! under the caller's identity, and re-checks the caller's authority where the write commits.

use calm_truth::decision_gate::WriteTx;
use calm_types::observation::WORKER_WATCH_WAKE_SOURCE;

use super::{Verdict, report_line};
use crate::db::sqlite::track_get_tx;
use crate::db::write_with_events_typed;
use crate::error::CalmError;
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, CardId, TrackId};
use crate::mcp_server::framing::{RpcError, calm_error};
use crate::mcp_server::registry::{AppContext, ToolCallIdentity};
use crate::model::new_id;
use crate::terminal_interaction::{Target, TerminalInteraction};

/// Where a report has passed its caller check and resolved its attempt, before the transaction
/// that writes the wake; keyed by the caller's session id.
#[cfg(feature = "fixtures")]
pub const WORKER_REPORT_AUTHORIZED: &str = "worker-report-authorized";

/// Wake the caller's Planner with `verdict` on `attempt_id`, after the terminal tools' own caller
/// check and same-Track resolution of the attempt. Returns the wake's key.
///
/// The key is `<attempt_id>:<fresh id>`: every call is its own report. Nothing dedupes reports, so
/// a repeated call costs the Planner one more line, never a lost report.
pub async fn report(
    ctx: &AppContext,
    identity: &ToolCallIdentity,
    attempt_id: &str,
    verdict: &Verdict,
) -> Result<String, RpcError> {
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
    let key = format!("{}:{}", task.attempt_id, new_id());
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point(WORKER_REPORT_AUTHORIZED, &identity.session_id).await;
    let identity = identity.clone();
    let wake_key = key.clone();
    write_with_events_typed(
        ctx.repo.as_ref(),
        ActorId::Kernel,
        None,
        &ctx.events,
        &ctx.write,
        move |tx| {
            Box::pin(async move {
                // The caller's authority, again where the wake commits: a session retired or a
                // card re-roled since the check above writes nothing.
                let role = tx
                    .read_card_role(&CardId::from(identity.card_id.clone()))
                    .await?;
                if role != Some(identity.role) || !identity.session_is_active(tx, &track).await? {
                    return Err(CalmError::Forbidden(
                        "this session is no longer the active authority of its card on its Track; \
                         nothing was reported"
                            .into(),
                    ));
                }
                let row = track_get_tx(tx, &TrackId::from(track)).await?;
                let event = Event::TrackWakeRequested {
                    track_id: row.id.clone(),
                    source: WORKER_WATCH_WAKE_SOURCE.into(),
                    key: wake_key,
                    text,
                };
                let scope = EventScope::Track {
                    track: row.id,
                    area: row.area_id,
                };
                Ok(((), vec![(scope, event)]))
            })
        },
    )
    .await
    .map_err(calm_error)?;
    Ok(key)
}
