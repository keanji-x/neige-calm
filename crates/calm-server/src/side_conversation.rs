//! A discussion branch owns its own assistant identity; its source is context only.
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::db::prelude::Repo;
use crate::error::{CalmError, Result};
use crate::harness::profile::PlannerBinding;
use crate::model::CardRole;

pub const MAX_SIDE_CONTEXT_CHARS: usize = 12_000;

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SideConversation {
    /// A Planner or assistant card on this same track; never a provider session id.
    pub source_card_id: String,
    /// A frozen excerpt of loaded user/assistant text, not a complete history fork.
    pub context: String,
}

impl SideConversation {
    pub(crate) async fn validate(&self, repo: &dyn Repo, track_id: &str) -> Result<()> {
        if self.context.chars().count() > MAX_SIDE_CONTEXT_CHARS {
            return Err(CalmError::BadRequest(format!(
                "side context must be at most {MAX_SIDE_CONTEXT_CHARS} characters"
            )));
        }
        let card = repo
            .card_get(&self.source_card_id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("card {}", self.source_card_id)))?;
        let role = repo
            .card_role_get(&self.source_card_id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("card {}", self.source_card_id)))?;
        Self::require_source(
            track_id,
            card.track_id.as_str(),
            &card.kind,
            role,
            &card.payload,
        )
    }

    pub(crate) async fn validate_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        track_id: &str,
    ) -> Result<()> {
        let shape = crate::db::sqlite::card_execution_shape_tx(tx, &self.source_card_id).await?;
        let source_track: String = sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?1")
            .bind(&self.source_card_id)
            .fetch_one(&mut **tx)
            .await?;
        Self::require_source(
            track_id,
            &source_track,
            &shape.kind,
            shape.role,
            &shape.payload,
        )
    }

    fn require_source(
        track_id: &str,
        source_track: &str,
        kind: &str,
        role: CardRole,
        payload: &serde_json::Value,
    ) -> Result<()> {
        if source_track != track_id
            || !matches!(role, CardRole::Planner | CardRole::Assistant)
            || PlannerBinding::from_shape(kind, role, payload).is_none()
        {
            return Err(CalmError::BadRequest(
                "side source must be a Planner or assistant conversation on this track".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn opening_context(&self) -> String {
        format!(
            "{}\nSource: {}\n\n<context_snapshot>\n{}\n</context_snapshot>",
            include_str!("../prompts/side-conversation-context.md"),
            self.source_card_id,
            self.context,
        )
    }
}

/// Shared opening-material owner for historical mint and canonical acceptance.
/// Render before any write when the briefing owner reads through its repository pool.
pub(crate) async fn opening_material(
    repo: &dyn Repo,
    track_id: &str,
    side: Option<&SideConversation>,
    briefing: Option<crate::operation::planner_harness_start_adapter::OpeningBriefing>,
) -> Result<Option<String>> {
    use crate::operation::planner_harness_start_adapter::OpeningBriefing;
    let text = if let Some(side) = side {
        side.validate(repo, track_id).await?;
        Some(side.opening_context())
    } else {
        match briefing {
            Some(OpeningBriefing::TodaysActivityOnTheLaunchpad) => {
                crate::activity_window::launchpad_opening_briefing(repo, track_id).await?
            }
            Some(OpeningBriefing::CallerSuppliesItsOwn) | None => None,
        }
    };
    if text.as_ref().is_some_and(|text| {
        text.chars().count() > crate::routes::planner_cards::MAX_PLANNER_INPUT_CHARS
    }) {
        return Err(CalmError::Internal(
            "opening briefing exceeds input limit".into(),
        ));
    }
    Ok(text)
}
