//! Versioned projection on the existing transcript GET. The old array is historical-only.
use super::conversation_read::{ReadScope, current_frame};
use crate::auth::Principal;
use crate::error::{CalmError, ErrorBody, Result};
use crate::extract::{Json, Path, Query};
use crate::model::HarnessItem as TranscriptRow;
use crate::state::RouteState;
use axum::{
    extract::State,
    response::{IntoResponse, Response},
};
use calm_types::conversation_display::*;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

#[derive(Debug, Clone, Copy, Default, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HarnessItemsDirection {
    #[default]
    Asc,
    Desc,
}

#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct HarnessItemsQuery {
    pub contract: Option<ConversationReadContract>,
    /// URL-encoded strict ConversationCursor JSON; exclusive with after_id.
    pub cursor: Option<String>,
    /// Legacy physical item cursor. Accepted only without contract on V1 history.
    pub after_id: Option<i64>,
    pub limit: Option<i64>,
    #[serde(default)]
    pub direction: HarnessItemsDirection,
}

#[derive(Serialize, ToSchema)]
#[serde(untagged)]
pub enum ConversationItemsReadResponse {
    Historical(Vec<TranscriptRow>),
    Conversation(ConversationPage),
}

#[utoipa::path(get, path="/api/cards/{id}/harness/items", tag="cards",
    params(("id"=String,Path,description="Planner card id"),HarnessItemsQuery),
    responses(
        (status=200,description="Negotiated conversation page, or historical-only original raw array",body=ConversationItemsReadResponse),
        (status=400,description="Invalid or mixed cursor",body=ErrorBody),
        (status=401,description="Verified session required",body=ErrorBody),
        (status=403,description="Current Area access refused",body=ErrorBody),
        (status=404,description="Card not found",body=ErrorBody),
        (status=409,description="Read contract or canonical source unsupported",body=ErrorBody),
        (status=500,description="Invalid persisted data",body=ErrorBody)))]
pub(crate) async fn get_harness_items(
    State(s): State<RouteState>,
    principal: Principal,
    Path(id): Path<String>,
    Query(q): Query<HarnessItemsQuery>,
) -> Result<Response> {
    let scope = ReadScope::check(s.repo.as_ref(), &id, &principal).await?;
    let card = super::cards::harness_card(&s, &id).await?;
    if card.track_id.as_str() != scope.track {
        return Err(CalmError::Forbidden("conversation moved".into()));
    }
    let frame = current_frame(s.repo.as_ref(), &id).await?;
    let mode = if frame.canonical.is_some() {
        ConversationCursorSource::Presentation
    } else {
        ConversationCursorSource::HistoricalRaw
    };
    let limit = q.limit.unwrap_or(100).clamp(0, 500);
    let descending = q.direction == HarnessItemsDirection::Desc;
    let after = match (&q.contract, &q.cursor, q.after_id) {
        (None, None, after) if mode == ConversationCursorSource::HistoricalRaw => {
            after.unwrap_or(0).max(0)
        }
        (None, _, _) => {
            return Err(CalmError::Conflict(
                "canonical conversation requires contract=conversation_v1".into(),
            ));
        }
        (Some(_), None, None) => 0,
        (Some(_), Some(raw), None) => {
            let cursor: ConversationCursor = serde_json::from_str(raw)
                .map_err(|_| CalmError::BadRequest("invalid conversation cursor".into()))?;
            if cursor.card_id != id || cursor.position <= 0 {
                return Err(CalmError::BadRequest(
                    "cursor belongs to another conversation or invalid position".into(),
                ));
            }
            if cursor.source != mode {
                return Err(CalmError::Conflict(
                    "conversation cursor contract changed; reread newest page".into(),
                ));
            }
            cursor.position
        }
        _ => {
            return Err(CalmError::BadRequest(
                "conversation cursor cannot be mixed with after_id".into(),
            ));
        }
    };
    let response = if mode == ConversationCursorSource::Presentation {
        let mut page = s
            .repo
            .conversation_presentation_page(&id, after, limit, descending)
            .await?;
        for entry in &mut page.items {
            redact(&mut entry.source);
        }
        Json(ConversationItemsReadResponse::Conversation(page)).into_response()
    } else {
        let mut rows = s
            .repo
            .harness_item_list_transcript_by_card(&id, after, limit, descending)
            .await?;
        for item in &mut rows {
            item.params = crate::planner_attachments::redact_local_image_paths(&item.params);
        }
        if q.contract.is_none() {
            Json(ConversationItemsReadResponse::Historical(rows)).into_response()
        } else {
            let boundary = if descending {
                rows.first()
            } else {
                rows.last()
            };
            let next_cursor = boundary
                .filter(|_| rows.len() == limit as usize)
                .map(|item| ConversationCursor::new(&id, mode, item.id));
            let items = rows
                .into_iter()
                .map(|item| ConversationDisplayItem {
                    cursor: ConversationCursor::new(&id, mode, item.id),
                    source: ConversationDisplaySource::HistoricalRaw { item },
                })
                .collect();
            Json(ConversationItemsReadResponse::Conversation(
                ConversationPage {
                    contract: ConversationReadContract::ConversationV1,
                    source: mode,
                    items,
                    next_cursor,
                },
            ))
            .into_response()
        }
    };
    scope.publish(s.repo.as_ref(), &principal).await?;
    Ok(response)
}

fn redact(source: &mut ConversationDisplaySource) {
    if let ConversationDisplaySource::Native { item }
    | ConversationDisplaySource::HistoricalRaw { item } = source
    {
        item.params = crate::planner_attachments::redact_local_image_paths(&item.params);
    }
}
