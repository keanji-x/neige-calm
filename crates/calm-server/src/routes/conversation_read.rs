//! Finite conversation reads: resolve real containment, consume current organization
//! policy, and recheck after assembly. Reading never starts or authorizes execution.
use crate::auth::Principal;
use crate::db::RouteRepo;
use crate::error::{CalmError, Result};
use crate::ids::AreaId;
use calm_types::harness::snapshot::CanonicalHarnessSnapshot;

pub(crate) struct ReadScope {
    card: String,
    pub track: String,
    pub area: AreaId,
    fence: crate::read_access::ReadFence,
}
impl ReadScope {
    pub async fn check(repo: &dyn RouteRepo, card: &str, principal: &Principal) -> Result<Self> {
        Self::check_audience(repo, card, crate::read_access::Audience::Owner(principal)).await
    }
    pub async fn publish(&self, repo: &dyn RouteRepo, principal: &Principal) -> Result<()> {
        self.publish_audience(repo, crate::read_access::Audience::Owner(principal))
            .await
    }
    /// No principal is synthesized from a delegation. Share authorization stays
    /// with share_access, while this owner pins exact card containment.
    pub async fn check_audience(
        repo: &dyn RouteRepo,
        card: &str,
        audience: crate::read_access::Audience<'_>,
    ) -> Result<Self> {
        if let crate::read_access::Audience::Owner(principal) = audience {
            repo.application_user_require_registered(&principal.user_id)
                .await?;
        }
        let track = repo
            .card_track_id(card)
            .await?
            .ok_or_else(|| CalmError::NotFound("conversation".into()))?;
        let fence = audience.check(repo, &track).await?;
        let area = match &fence {
            crate::read_access::ReadFence::Owner(area) => area.clone(),
            crate::read_access::ReadFence::ShareLink(access) => access.area.clone(),
        };
        Ok(Self {
            card: card.into(),
            track,
            area,
            fence,
        })
    }
    pub async fn publish_audience(
        &self,
        repo: &dyn RouteRepo,
        audience: crate::read_access::Audience<'_>,
    ) -> Result<()> {
        let track = repo
            .card_track_id(&self.card)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("card {}", self.card)))?;
        if track != self.track {
            return Err(CalmError::Forbidden("conversation moved".into()));
        }
        audience.publish(repo, &track, &self.fence).await
    }
}

pub(crate) struct CurrentReadFrame {
    pub pending_humans: Vec<calm_truth::db::PendingHumanMessage>,
    pub terminal_outcome: Option<calm_truth::db::terminal_outcome::HarnessTerminalOutcome>,
    pub runtime: Option<calm_truth::db::CurrentConversationRuntime>,
    pub canonical: Option<CanonicalHarnessSnapshot>,
    pub active_input: Option<calm_types::conversation_display::ConversationActiveInput>,
}
/// Declared persisted version selects the reader. Missing or malformed V2 never falls back.
pub(crate) async fn current_frame(repo: &dyn RouteRepo, card: &str) -> Result<CurrentReadFrame> {
    let facts = repo.conversation_read_frame(card).await?;
    let canonical = match facts.snapshot {
        Some(value) => match value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
        {
            Some(version) if version >= 2 => Some(serde_json::from_value(value)?),
            Some(1) if !facts.has_presentations => {
                crate::harness::HarnessSnapshot::parse_known(value).ok_or_else(|| {
                    CalmError::Conflict("unreadable historical conversation frame".into())
                })?;
                None
            }
            Some(_) | None => {
                return Err(CalmError::Conflict(
                    "conversation requires a supported read contract".into(),
                ));
            }
        },
        None if facts.has_presentations => {
            return Err(CalmError::Conflict(
                "canonical content has no current carrier".into(),
            ));
        }
        None => None,
    };
    if canonical.is_some() && facts.runtime.is_none() {
        return Err(CalmError::Conflict(
            "canonical frame has no current runtime fact".into(),
        ));
    }
    Ok(CurrentReadFrame {
        pending_humans: facts.pending_humans,
        terminal_outcome: facts.terminal_outcome,
        runtime: facts.runtime,
        canonical,
        active_input: facts.active_input,
    })
}
