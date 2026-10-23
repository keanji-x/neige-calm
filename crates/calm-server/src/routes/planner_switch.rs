//! Human REST adapter for the Track-owned Planner switch.
use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::db::sqlite::track_planner::{PlannerRevision, PlannerSwitchRefusal as TruthRefusal};
use crate::extract::{Json, JsonBody, Path};
use crate::{
    actor::Actor,
    auth::HumanInputSource,
    error::{CalmError, ErrorBody},
    planner_switch::{self, PlannerSwitchError, PlannerSwitchRefusal, SwitchPlannerRequest},
    state::{AppState, RouteState},
};

#[derive(Deserialize, ToSchema)]
pub struct PlannerSwitchRequest {
    pub target_assistant_card_id: String,
    pub expected_planner_card_id: String,
    #[schema(minimum = 0)]
    pub expected_revision: i64,
}
#[derive(Serialize, ToSchema)]
pub struct PlannerSwitchResponse {
    pub track_id: String,
    pub planner_card_id: String,
    pub demoted_card_id: String,
    pub planner_revision: i64,
}
#[derive(Serialize, ToSchema)]
pub struct PlannerSwitchRefusalBody {
    pub error: String,
    pub code: String,
    pub card_id: String,
}

/// Refusals carry card context; extractor, Origin and access failures use ErrorBody.
pub struct PlannerSwitchErrorBody;
impl utoipa::PartialSchema for PlannerSwitchErrorBody {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        utoipa::openapi::schema::AnyOfBuilder::new()
            .item(utoipa::openapi::Ref::from_schema_name("ErrorBody"))
            .item(utoipa::openapi::Ref::from_schema_name(
                "PlannerSwitchRefusalBody",
            ))
            .into()
    }
}
impl ToSchema for PlannerSwitchErrorBody {}

pub fn router() -> Router<AppState> {
    Router::new().route("/api/tracks/{id}/planner/switch", post(switch_planner))
}

#[utoipa::path(
    post, path = "/api/tracks/{id}/planner/switch", tag = "tracks",
    params(("id" = String, Path, description = "Track id")),
    request_body = PlannerSwitchRequest,
    responses(
        (status = 200, description = "Planner switched; reconcile uncertain outcomes by reading Track detail", body = PlannerSwitchResponse),
        (status = 400, description = "Invalid request or planner_target_invalid", body = inline(PlannerSwitchErrorBody)),
        (status = 401, description = "Current human session required", body = ErrorBody),
        (status = 403, description = "Access denied or planner_user_required", body = inline(PlannerSwitchErrorBody)),
        (status = 404, description = "Track or Area not found", body = ErrorBody),
        (status = 409, description = "Planner changed or conversation is unsafe to switch", body = PlannerSwitchRefusalBody),
        (status = 500, description = "Internal failure; refresh detail to reconcile", body = ErrorBody),
    )
)]
pub(crate) async fn switch_planner(
    State(s): State<RouteState>,
    source: HumanInputSource,
    actor: Actor,
    Path(id): Path<String>,
    JsonBody(body): JsonBody<PlannerSwitchRequest>,
) -> Response {
    if body.expected_revision < 0
        || body.target_assistant_card_id.trim().is_empty()
        || body.expected_planner_card_id.trim().is_empty()
    {
        return CalmError::BadRequest(
            "Planner ids must be nonempty and revision nonnegative".into(),
        )
        .into_response();
    }
    let request = SwitchPlannerRequest {
        track_id: id.clone().into(),
        target_assistant_card_id: body.target_assistant_card_id.into(),
        expected_planner_card_id: body.expected_planner_card_id.into(),
        expected_revision: PlannerRevision(body.expected_revision),
    };
    match planner_switch::switch_planner(&s, &actor, &source, request).await {
        Ok(receipt) => Json(PlannerSwitchResponse {
            track_id: id,
            planner_card_id: receipt.promoted.card_id,
            demoted_card_id: receipt.demoted.card_id,
            planner_revision: receipt.revision.0,
        })
        .into_response(),
        Err(error) => refusal_response(error),
    }
}

pub(crate) fn refusal_response(error: PlannerSwitchError) -> Response {
    let (reason, card_id) = match error {
        PlannerSwitchError::Refused { reason, card_id } => (reason, card_id),
        PlannerSwitchError::Failed(error) => return error.into_response(),
    };
    use PlannerSwitchRefusal as R;
    use TruthRefusal as T;
    let (status, message) = match reason {
        R::UserRequired => (
            StatusCode::FORBIDDEN,
            "Only a human user can switch the Planner.",
        ),
        R::Truth(T::TargetInvalid) => (
            StatusCode::BAD_REQUEST,
            "Choose another Assistant card in this Track.",
        ),
        R::Truth(T::Changed) => (
            StatusCode::CONFLICT,
            "The Planner changed. Refresh before trying again.",
        ),
        R::Truth(T::HistoricalStorage) => (
            StatusCode::CONFLICT,
            "This conversation uses historical storage and cannot switch yet.",
        ),
        R::Truth(T::BindingUnsupported) => (
            StatusCode::CONFLICT,
            "This conversation’s role binding or configuration does not support switching.",
        ),
        R::Truth(T::PendingMessages) => {
            (StatusCode::CONFLICT, "Wait for pending messages to finish.")
        }
        R::Truth(T::PendingSubmission) => (
            StatusCode::CONFLICT,
            "Wait for pending submissions to finish.",
        ),
        R::Truth(T::TurnInFlight) => (StatusCode::CONFLICT, "Wait for the current turn to finish."),
        R::Truth(T::SessionUnavailable) => (
            StatusCode::CONFLICT,
            "The session cannot be confirmed safe to switch. Restore it before trying again.",
        ),
        R::PendingObservations => (
            StatusCode::CONFLICT,
            "Wait for the Planner’s pending observations to finish.",
        ),
        R::OpenAsk => (StatusCode::CONFLICT, "Resolve the open questions first."),
    };
    (
        status,
        Json(PlannerSwitchRefusalBody {
            error: message.into(),
            code: reason.code().into(),
            card_id,
        }),
    )
        .into_response()
}
