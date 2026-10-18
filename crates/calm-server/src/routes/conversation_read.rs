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
}
impl ReadScope {
    pub async fn check(repo: &dyn RouteRepo, card: &str, principal: &Principal) -> Result<Self> {
        repo.application_user_require_registered(&principal.user_id)
            .await?;
        let track = repo
            .card_track_id(card)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("card {card}")))?;
        let area = crate::track_access::require_detail_access(repo, &track, principal).await?;
        Ok(Self {
            card: card.into(),
            track,
            area,
        })
    }
    pub async fn publish(&self, repo: &dyn RouteRepo, principal: &Principal) -> Result<()> {
        let track = repo
            .card_track_id(&self.card)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("card {}", self.card)))?;
        if track != self.track {
            return Err(CalmError::Forbidden(
                "conversation Track changed while reading".into(),
            ));
        }
        crate::track_access::require_detail_publication(repo, &track, &self.area, principal).await
    }
}

pub(crate) struct CurrentReadFrame {
    pub runtime: Option<calm_truth::db::CurrentConversationRuntime>,
    pub canonical: Option<CanonicalHarnessSnapshot>,
}
/// Declared persisted version selects the reader. Missing or malformed V2 never falls back.
pub(crate) async fn current_frame(repo: &dyn RouteRepo, card: &str) -> Result<CurrentReadFrame> {
    let facts = repo.conversation_read_frame(card).await?;
    let canonical = match facts.snapshot {
        Some(value) => match value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
        {
            Some(2) => Some(serde_json::from_value(value)?),
            Some(1) if !facts.has_presentations => {
                crate::harness::HarnessSnapshot::parse_known(value).ok_or_else(|| {
                    CalmError::Conflict("unreadable historical conversation frame".into())
                })?;
                None
            }
            _ => {
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
        runtime: facts.runtime,
        canonical,
    })
}
