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
        if card.track_id.as_str() != track_id
            || !matches!(role, CardRole::Planner | CardRole::Assistant)
            || PlannerBinding::from_card(&card, role).is_none()
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
