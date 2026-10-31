//! Server-counted Today summaries on the original System Assistant carrier.
//! An HTTP key freezes its first acceptance; replay returns only that ACK.

use crate::extract::Json;
use axum::{
    Router,
    extract::{FromRef, State},
    http::HeaderMap,
    routing::post,
};
use serde::Serialize;
use utoipa::ToSchema;

use crate::activity_window::{WorkspaceActivityWindow, activity_counts_block};
use crate::error::{CalmError, ErrorBody, Result};
use crate::prompts::render_named;
use crate::state::{AppState, RouteState};

pub fn router() -> Router<AppState> {
    Router::new().route("/api/today/summary", post(write_today_summary))
}

/// The standing instruction accepted exactly once with the original carrier's
/// first body. Receipts, rather than current transcript/runtime rows, own replay.
pub const TODAY_SUMMARY_BOOTSTRAP_TEXT: &str =
    include_str!("../../prompts/today-summary/bootstrap.md");

/// The summary prompt's prose, with `{counts}` where the server-counted activity block goes.
const TODAY_SUMMARY_WRITE_TEXT: &str = include_str!("../../prompts/today-summary/write.md");

/// What the caller gets back on success.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TodaySummaryStarted {
    /// The launchpad track, whose report the agent is being asked to rewrite.
    pub track_id: String,
    /// The summary conversation's card. Stable for the launchpad's lifetime.
    pub card_id: String,
}

/// Render the prompt: template text plus five integers, so its maximum length can be
/// computed by reading it and stays under `MAX_PLANNER_INPUT_CHARS`.
fn summary_prompt(activity: &WorkspaceActivityWindow) -> Result<String> {
    Ok(render_named(
        TODAY_SUMMARY_WRITE_TEXT,
        &[("counts", &activity_counts_block(activity))],
    )?)
}

#[utoipa::path(
    post,
    path = "/api/today/summary",
    tag = "tracks",
    params(("Idempotency-Key" = Option<String>, Header, description = "Identity of one summary operation. Its first acceptance freezes the server-counted body. Reuse returns the original track/card, including on an empty day; omission creates a fresh operation.")),
    responses(
        (status = 200, description = "Original accepted track/card. Bootstrap is accepted once; a fresh operation accepts today's activity body. Native execution and report edits arrive asynchronously, including when the provider is unavailable.", body = TodaySummaryStarted),
        (status = 400, description = "Invalid Idempotency-Key", body = ErrorBody),
        (status = 403, description = "Track access or new-acceptance authority refused", body = ErrorBody),
        (status = 409, description = "A fresh operation found no activity (`today_summary_no_activity`), Today proof failed (`today_adoption_*`), or the original Assistant namespace/current carrier cannot accept. No new acceptance is made. An accepted key returns its original ACK before these checks.", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
/// Ask the summary conversation to write today's progress.
pub(crate) async fn write_today_summary(
    State(app): State<AppState>,
    principal: crate::auth::Principal,
    headers: HeaderMap,
) -> Result<Json<TodaySummaryStarted>> {
    use crate::activity_window::{local_day_window, workspace_activity_window};
    use crate::dispatcher::system_assistant::{PreparedSubmission, PreparedSystemInput};
    let subject = match crate::routes::idempotency_key::parse_idempotency_key_header(&headers)? {
        Some(key) => format!("http:{key}"),
        None => format!("fresh:{}", crate::model::new_id()),
    };
    let route = RouteState::from_ref(&app);
    let issuer = app
        .today_summary_issuer
        .as_ref()
        .ok_or(CalmError::Unauthorized)?;
    if let Some(receipt) = issuer.replay(&route, &principal, &subject).await? {
        return Ok(Json(TodaySummaryStarted {
            track_id: receipt.track_id,
            card_id: receipt.card_id,
        }));
    }
    crate::organization_access::require_deployment_owner(&principal)?;
    let mut target = crate::db::write_in_tx_typed(route.repo.as_ref(), move |tx| {
        Box::pin(async move {
            crate::routes::today::summary_target_tx(tx, crate::model::new_id().into()).await
        })
    })
    .await?;
    loop {
        let reserved = target.clone();
        let prepared_target = target.clone();
        let prepared_route = route.clone();
        let root = app.workspace_root().to_path_buf();
        let result = issuer.submit_prepared(&route, principal.clone(), target, subject.clone(),
            move |tx| Box::pin(async move { crate::routes::today::summary_target_tx(tx, reserved).await }),
            move |tx| Box::pin(async move {
                let (start, end) = local_day_window(crate::model::now_ms());
                let activity = workspace_activity_window(&mut **tx, start, end, Some(prepared_target.as_str())).await?;
                if activity.is_empty() {
                    return Err(CalmError::TodaySummaryNoActivity("nothing happened in this workspace today; no conversation was created and no message was sent".into()));
                }
                let text = summary_prompt(&activity)?;
                let events = crate::routes::today::prepare_summary_today_tx(tx, &prepared_route, &root, &prepared_target).await?;
                Ok(PreparedSystemInput { bootstrap: TODAY_SUMMARY_BOOTSTRAP_TEXT.into(), text, events })
            }),
        ).await?;
        match result {
            PreparedSubmission::Retry(actual) => target = actual,
            PreparedSubmission::Accepted { receipt, fresh } => {
                if fresh && let Some(track) = app.repo.track_get(&receipt.track_id).await? {
                    app.write().remember_track(track.id, track.area_id);
                }
                return Ok(Json(TodaySummaryStarted {
                    track_id: receipt.track_id,
                    card_id: receipt.card_id,
                }));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The actual issuer namespace must retain the original conversation identity.
    #[test]
    fn the_summary_conversation_key_is_bare() {
        let namespace =
            calm_types::system_assistant::SystemAssistantSource::TodaySummary.namespace();
        assert_eq!(namespace, "today-summary");
        let derived =
            crate::conversation_keys::derive_track_conversation_keys("track-1", namespace);
        assert_eq!(derived.card_id, "conv-afe76dc78204daed3ab52a9007298eb0");
        assert_eq!(
            derived.operation_key,
            "wave-conversation-afe76dc78204daed3ab52a9007298eb07f6e17761e2ec9da3718288dad41baff"
        );
    }

    /// `i64::MIN` rather than a plausible count: the widest rendering is the longest
    /// negative integer.
    #[test]
    fn the_prompt_is_bounded_far_below_the_planner_input_ceiling() {
        let widest = summary_prompt(&WorkspaceActivityWindow {
            track_report_edited: i64::MIN,
            task_completed: i64::MIN,
            task_failed: i64::MIN,
            tracks_touched: i64::MIN,
        })
        .unwrap();
        assert!(
            widest.chars().count() < crate::routes::planner_cards::MAX_PLANNER_INPUT_CHARS,
            "the prompt must fit `planner/input` for every possible count; it is \
             {} chars",
            widest.chars().count()
        );
        // The bootstrap travels the same channel under the identical ceiling.
        assert!(
            TODAY_SUMMARY_BOOTSTRAP_TEXT.chars().count()
                < crate::routes::planner_cards::MAX_PLANNER_INPUT_CHARS
        );
        assert!(!TODAY_SUMMARY_BOOTSTRAP_TEXT.trim().is_empty());
    }
}
