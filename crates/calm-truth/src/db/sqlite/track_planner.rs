//! Track-owned Planner tenure composition. Callers retain lifecycle/owner guards
//! through commit and pair publication; this module does no provider or cache I/O.
use super::conversation::*;
use crate::error::TruthError;
use crate::planner_binding::PlannerBinding;
use PlannerSwitchRefusal as Refusal;
use calm_types::harness::HarnessProfile;
use sqlx::{Sqlite, Transaction};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlannerRevision(pub i64);

#[derive(Clone, Copy)]
pub struct ExpectedPlanner<'a> {
    pub track_id: &'a str,
    pub planner_card_id: &'a str,
    pub target_card_id: &'a str,
    pub revision: PlannerRevision,
    /// Authenticated human identity, established by the calling owner.
    pub user_id: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlannerSwitchRefusal {
    TargetInvalid,
    Changed,
    HistoricalStorage,
    BindingUnsupported,
    PendingMessages,
    PendingSubmission,
    TurnInFlight,
    SessionUnavailable,
}
impl PlannerSwitchRefusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::TargetInvalid => "planner_target_invalid",
            Self::Changed => "planner_changed",
            Self::HistoricalStorage => "planner_historical_storage",
            Self::BindingUnsupported => "planner_binding_unsupported",
            Self::PendingMessages => "planner_pending_messages",
            Self::PendingSubmission => "planner_pending_submission",
            Self::TurnInFlight => "planner_turn_in_flight",
            Self::SessionUnavailable => "planner_session_unavailable",
        }
    }
}
#[derive(Debug, thiserror::Error)]
pub enum PlannerSwitchError {
    #[error("{reason:?} ({card_id})")]
    Refused {
        reason: PlannerSwitchRefusal,
        card_id: String,
    },
    #[error(transparent)]
    Storage(#[from] TruthError),
}
impl PlannerSwitchError {
    pub fn code(&self) -> Option<&'static str> {
        match self {
            Self::Refused { reason, .. } => Some(reason.code()),
            Self::Storage(_) => None,
        }
    }
}
fn refusal(reason: PlannerSwitchRefusal, card: &str) -> PlannerSwitchError {
    PlannerSwitchError::Refused {
        reason,
        card_id: card.into(),
    }
}
fn map(error: TruthError, card: &str) -> PlannerSwitchError {
    use ReplacementRefusal::*;
    match error {
        TruthError::CarrierReplacementRefused(reason) => refusal(
            match reason {
                Changed => Refusal::Changed,
                HistoricalStorage => Refusal::HistoricalStorage,
                BindingUnsupported => Refusal::BindingUnsupported,
                PendingMessages => Refusal::PendingMessages,
                PendingSubmission => Refusal::PendingSubmission,
                TurnInFlight => Refusal::TurnInFlight,
                SessionUnavailable => Refusal::SessionUnavailable,
            },
            card,
        ),
        other => PlannerSwitchError::Storage(other),
    }
}

pub struct PlannerSwitchReceipt {
    pub revision: PlannerRevision,
    pub event_baseline: i64,
    pub demoted: CarrierReplacement,
    pub promoted: CarrierReplacement,
    /// Publish only after commit, using the existing plural transaction owner.
    pub events: Vec<crate::event::Event>,
}

struct Revision<'a>(&'a ExpectedPlanner<'a>);
#[async_trait::async_trait]
impl CarrierReplacementGuard for Revision<'_> {
    async fn recheck_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        _: &CurrentInputCarrier,
    ) -> crate::error::Result<()> {
        let revision: i64 = sqlx::query_scalar("SELECT planner_revision FROM tracks WHERE id=?1")
            .bind(self.0.track_id)
            .fetch_one(&mut **tx)
            .await?;
        if revision != self.0.revision.0 {
            return Err(TruthError::CarrierReplacementRefused(
                ReplacementRefusal::Changed,
            ));
        }
        Ok(())
    }
}

/// Both carriers are full protected observations, not just ids. Owner callbacks
/// include qualified event-tail/open-ask checks; SQL preflight covers both before
/// either revocation. On any error the caller MUST roll back the transaction.
pub async fn switch_planner_tx(
    tx: &mut Transaction<'_, Sqlite>,
    expected: ExpectedPlanner<'_>,
    protected_pair: (ExpectedCarrier<'_>, ExpectedCarrier<'_>),
) -> Result<PlannerSwitchReceipt, PlannerSwitchError> {
    let (old, target) = protected_pair;
    let invalid = || refusal(Refusal::TargetInvalid, expected.target_card_id);
    if expected.planner_card_id == expected.target_card_id
        || old.card_id != expected.planner_card_id
        || target.card_id != expected.target_card_id
        || old.track_id != expected.track_id
        || target.track_id != expected.track_id
    {
        return Err(invalid());
    }
    let planner: Option<String> =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=?1 AND role='planner'")
            .bind(expected.track_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(TruthError::from)?;
    if planner.as_deref() != Some(expected.planner_card_id) {
        return Err(refusal(Refusal::Changed, expected.planner_card_id));
    }
    let valid: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM tracks t JOIN cards p ON p.track_id=t.id JOIN cards a ON a.track_id=t.id
         WHERE t.id=?1 AND t.closed_at IS NULL AND p.id=?2 AND p.role='planner' AND p.kind='codex'
         AND a.id=?3 AND a.role='assistant' AND a.kind='codex'
         AND NOT EXISTS(SELECT 1 FROM assistant_conversation_archives WHERE card_id=a.id)
         AND (SELECT count(*) FROM cards WHERE track_id=t.id AND role='planner')=1)")
        .bind(expected.track_id).bind(expected.planner_card_id).bind(expected.target_card_id)
        .fetch_one(&mut **tx).await.map_err(TruthError::from)?;
    if !valid {
        return Err(invalid());
    }
    let base: i64 = sqlx::query_scalar("SELECT planner_revision FROM tracks WHERE id=?1")
        .bind(expected.track_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(TruthError::from)?;
    if base != expected.revision.0 {
        return Err(refusal(Refusal::Changed, expected.planner_card_id));
    }
    let protected = ExpectedPlanner {
        revision: PlannerRevision(base),
        ..expected
    };
    let revision = Revision(&protected);
    // Preserve caller revision callbacks before installing the shared Track hook.
    check_idle_carrier_tx(tx, &old)
        .await
        .map_err(|e| map(e, &old.card_id))?;
    check_idle_carrier_tx(tx, &target)
        .await
        .map_err(|e| map(e, &target.card_id))?;
    let old = ExpectedCarrier {
        revision: CallerRevision::Compare(&revision),
        ..old
    };
    let target = ExpectedCarrier {
        revision: CallerRevision::Compare(&revision),
        ..target
    };
    let from = PlannerBinding {
        profile: HarnessProfile::Assistant,
        provider: old.binding.provider.clone(),
    };
    let to = PlannerBinding {
        profile: HarnessProfile::Planner,
        provider: target.binding.provider.clone(),
    };
    let old_id = old.card_id.clone();
    let target_id = target.card_id.clone();
    let demoted = replace_idle_carrier_tx(tx, old, from)
        .await
        .map_err(|e| map(e, &old_id))?;
    let promoted = replace_idle_carrier_tx(tx, target, to)
        .await
        .map_err(|e| map(e, &target_id))?;
    promoted
        .repoint_root_tx(tx)
        .await
        .map_err(|e| map(e, &target_id))?;
    // Capture the existing Track tail before the role-change events are appended.
    let event_baseline: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(id),0) FROM events WHERE scope_track=?1")
            .bind(expected.track_id)
            .fetch_one(&mut **tx)
            .await
            .map_err(TruthError::from)?;
    let next = base
        .checked_add(1)
        .filter(|r| *r > 0)
        .ok_or_else(|| refusal(Refusal::Changed, expected.planner_card_id))?;
    let changed = sqlx::query("UPDATE tracks SET planner_revision=?1,planner_event_baseline=?2 WHERE id=?3 AND planner_revision=?4")
        .bind(next).bind(event_baseline).bind(expected.track_id).bind(base)
        .execute(&mut **tx).await.map_err(TruthError::from)?.rows_affected();
    if changed != 1 {
        return Err(refusal(Refusal::Changed, expected.planner_card_id));
    }
    let binding = |b: &PlannerBinding| {
        serde_json::json!({"profile":b.profile,"provider":b.provider}).to_string()
    };
    sqlx::query("INSERT INTO track_planner_changes(track_id,revision,user_id,from_card_id,to_card_id,
        from_old_worker,from_new_worker,to_old_worker,to_new_worker,from_old_binding,from_new_binding,
        to_old_binding,to_new_binding,from_watermark,to_watermark,from_history_boundary,to_history_boundary,event_baseline,created_at_ms)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)")
        .bind(expected.track_id).bind(next).bind(expected.user_id).bind(&old_id).bind(&target_id)
        .bind(demoted.old_worker.id.as_str()).bind(demoted.new_worker.id.as_str())
        .bind(promoted.old_worker.id.as_str()).bind(promoted.new_worker.id.as_str())
        .bind(binding(&demoted.old_binding)).bind(binding(&demoted.new_binding))
        .bind(binding(&promoted.old_binding)).bind(binding(&promoted.new_binding))
        .bind(demoted.watermark).bind(promoted.watermark).bind(demoted.history_boundary).bind(promoted.history_boundary)
        .bind(event_baseline).bind(crate::model::now_ms()).execute(&mut **tx).await.map_err(TruthError::from)?;
    let valid: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM tracks t JOIN cards c ON c.track_id=t.id
        WHERE t.id=?1 AND c.id=?2 AND c.role='planner' AND c.session_id=?3 AND t.root_session_id=c.session_id
        AND (SELECT count(*) FROM cards WHERE track_id=t.id AND role='planner')=1)")
        .bind(expected.track_id).bind(&target_id).bind(promoted.new_worker.id.as_str())
        .fetch_one(&mut **tx).await.map_err(TruthError::from)?;
    if !valid {
        return Err(refusal(Refusal::Changed, &target_id));
    }
    let mut events = Vec::new();
    for card in [&old_id, &target_id] {
        events.push(crate::event::Event::CardUpdated(
            super::card::card_for_update_tx(tx, card).await?,
        ));
    }
    Ok(PlannerSwitchReceipt {
        revision: PlannerRevision(next),
        event_baseline,
        demoted,
        promoted,
        events,
    })
}
