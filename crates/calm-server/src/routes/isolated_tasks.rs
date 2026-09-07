//! One User-authored independent task and its exact accepted execution report.
use crate::actor::Actor;
use crate::auth::Principal;
use crate::db::{sqlite::task_attempt_get_tx, write_in_tx_typed};
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::Event;
use crate::state::{AppState, RouteState};
use crate::track_report::{self, ReportEditTarget, TrackReportPayload};
use axum::{
    Json, Router,
    extract::{Path, State},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/tracks/{id}/isolated-tasks", post(start))
        .route(
            "/api/tracks/{id}/tasks/{key}/attempts/{attempt_id}/report",
            get(report),
        )
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartIsolatedTaskBody {
    pub key: String,
    pub goal: String,
    pub if_doc_rev: u64,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StartIsolatedTaskResponse {
    pub task_key: String,
    pub block_id: String,
    pub doc_rev: u64,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskAttemptReportResponse {
    pub attempt_id: String,
    #[schema(required = true)]
    pub report: Option<AcceptedTaskReport>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum AcceptedTaskReport {
    Completed {
        result: Value,
        artifacts: Vec<String>,
    },
    Failed {
        reason: String,
    },
}

#[utoipa::path(post, path = "/api/tracks/{id}/isolated-tasks", tag = "tracks",
    params(("id" = String, Path)), request_body = StartIsolatedTaskBody,
    responses((status = 200, body = StartIsolatedTaskResponse),
        (status = 400, body = ErrorBody), (status = 401, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub async fn start(
    State(app): State<AppState>,
    State(state): State<RouteState>,
    _principal: Principal,
    actor: Actor,
    Path(track_id): Path<String>,
    Json(body): Json<StartIsolatedTaskBody>,
) -> Result<Json<StartIsolatedTaskResponse>> {
    super::track_report_blocks::require_rest_user_actor_for(
        &actor,
        "Start independent task",
        "Only the person can start an independent task.",
    )?;
    if !app.isolated_tasks_available() {
        return Err(CalmError::ServiceUnavailable(
            "Independent tasks are unavailable on this server. No task was created.".into(),
        ));
    }
    let target = ReportEditTarget::resolve(state.repo.as_ref(), &track_id).await?;
    let (card, block) = track_report::write::rest_user_start(
        state.repo.as_ref(),
        &state.events,
        &state.write,
        target,
        body.key.clone(),
        body.goal,
        body.if_doc_rev,
    )
    .await?;
    let payload: TrackReportPayload = serde_json::from_value(card.payload)?;
    let block =
        block.ok_or_else(|| CalmError::Internal("Task start has no block receipt".into()))?;
    Ok(Json(StartIsolatedTaskResponse {
        task_key: body.key,
        block_id: block.id,
        doc_rev: payload.doc_rev,
    }))
}

#[utoipa::path(get, path = "/api/tracks/{id}/tasks/{key}/attempts/{attempt_id}/report", tag = "tracks",
    params(("id" = String, Path), ("key" = String, Path), ("attempt_id" = String, Path)),
    responses((status = 200, body = TaskAttemptReportResponse),
        (status = 401, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn report(
    State(state): State<RouteState>,
    _principal: Principal,
    actor: Actor,
    Path((track_id, key, attempt_id)): Path<(String, String, String)>,
) -> Result<Json<TaskAttemptReportResponse>> {
    super::track_report_blocks::require_rest_user_actor_for(
        &actor,
        "Read task report",
        "Task reports are available to the person.",
    )?;
    let response = write_in_tx_typed(state.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let report = accepted_report_tx(tx, &track_id, &key, &attempt_id).await?;
            Ok(TaskAttemptReportResponse { attempt_id, report })
        })
    })
    .await?;
    Ok(Json(response))
}

/// Canonical accepted Event/Operation provenance shared by report and file reads.
pub(crate) async fn accepted_report_tx(
    tx: &mut crate::operation::Tx<'_>,
    track_id: &str,
    key: &str,
    attempt_id: &str,
) -> Result<Option<AcceptedTaskReport>> {
    let track = crate::track_lifecycle::track_get_tx(tx, &track_id.into()).await?;
    task_attempt_get_tx(tx, &attempt_id)
        .await?
        .filter(|allocation| allocation.track_id == track_id && allocation.key == key)
        .ok_or_else(|| CalmError::NotFound("Task attempt".into()))?;
    // Card deletion removes worker_sessions but retains keyed Operations,
    // allocation history and Events. Use only the original Operation's
    // immutable identity as provenance; report content comes from Events.
    // Never deserialize or return private paths, tokens or provider state.
    let row: Option<(String, String)> = sqlx::query_as(
                "SELECT e.kind,e.payload FROM operations o JOIN events e ON e.scope_card=o.target_id \
                 WHERE o.kind='codex-isolated-worker' AND o.idempotency_key=?1 \
                 AND o.target_type='card' \
                 AND json_extract(o.payload_json,'$.version')='isolated-worker-v1' \
                 AND json_extract(o.payload_json,'$.actor.kind')='KernelDispatcher' \
                 AND json_extract(o.payload_json,'$.track_id')=?2 \
                 AND json_extract(o.payload_json,'$.task_id')=?1 \
                 AND json_extract(o.payload_json,'$.idempotency_key')=?1 \
                 AND json_extract(o.tx_output_json,'$.target_type')='card' \
                 AND json_extract(o.tx_output_json,'$.target_id')=o.target_id \
                 AND json_extract(o.tx_output_json,'$.data.isolated_execution.version')='isolated-run-v1' \
                 AND json_extract(o.tx_output_json,'$.data.isolated_execution.track_id')=?2 \
                 AND json_extract(o.tx_output_json,'$.data.isolated_execution.request.identity.run_id')=o.id \
                 AND json_extract(o.tx_output_json,'$.data.isolated_execution.request.identity.attempt_id')=?1 \
                 AND json_extract(o.tx_output_json,'$.data.isolated_execution.request.identity.card_id')=o.target_id \
                 AND json_extract(o.tx_output_json,'$.data.isolated_execution.request.identity.session_id')=json_extract(e.actor,'$.id') \
                 AND e.kind IN ('task.completed','task.failed') \
                 AND json_extract(e.payload,'$.idempotency_key')=?1 \
                 AND e.scope_kind='card' AND e.scope_track=?2 AND e.scope_area=?3 \
                 AND json_extract(e.actor,'$.kind')='AiCodexSession' \
                 ORDER BY e.id ASC LIMIT 1",
            )
            .bind(attempt_id).bind(track_id).bind(track.area_id.as_str())
            .fetch_optional(&mut **tx).await?;
    let report = row
        .map(|(kind, payload)| {
            let event = Event::from_kind_and_payload(&kind, serde_json::from_str(&payload)?)?;
            Ok::<_, CalmError>(match event {
                Event::TaskCompleted {
                    result, artifacts, ..
                } => AcceptedTaskReport::Completed {
                    result,
                    artifacts: artifacts.into_iter().map(|artifact| artifact.0).collect(),
                },
                Event::TaskFailed { reason, .. } => AcceptedTaskReport::Failed { reason },
                _ => {
                    return Err(CalmError::Internal("Unexpected task report event".into()));
                }
            })
        })
        .transpose()?;
    Ok(report)
}
