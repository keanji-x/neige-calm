//! Internal Track role composition. No transport exposes this operation.
#![allow(
    dead_code,
    reason = "internal entry point for the next Planner switch slice"
)]
use crate::db::sqlite::{self, conversation::*, track_planner as truth};
use crate::harness::backend::factory::PlannerFactory;
use crate::harness::carrier_replacement::{
    CarrierOwnerError, CarrierReplacementOwner, OwnerProofError, OwnerRefusal,
};
use crate::model::CardRole;
use crate::{actor::Actor, auth::HumanInputSource, error::CalmError, ids::*, state::RouteState};
use calm_truth::planner_binding::PlannerBinding;
pub(crate) use truth::PlannerSwitchReceipt;

pub(crate) struct SwitchPlannerRequest {
    pub track_id: TrackId,
    pub expected_planner_card_id: CardId,
    pub target_assistant_card_id: CardId,
    pub expected_revision: truth::PlannerRevision,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlannerSwitchRefusal {
    Truth(truth::PlannerSwitchRefusal),
    UserRequired,
    PendingObservations,
    OpenAsk,
}
impl PlannerSwitchRefusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::Truth(reason) => reason.code(),
            Self::UserRequired => "planner_user_required",
            Self::PendingObservations => "planner_pending_observations",
            Self::OpenAsk => "planner_open_ask",
        }
    }
}
#[derive(Debug, thiserror::Error)]
pub(crate) enum PlannerSwitchError {
    #[error("{reason:?} ({card_id})")]
    Refused {
        reason: PlannerSwitchRefusal,
        card_id: String,
    },
    #[error(transparent)]
    Failed(#[from] CalmError),
}
impl From<sqlx::Error> for PlannerSwitchError {
    fn from(e: sqlx::Error) -> Self {
        Self::Failed(e.into())
    }
}
impl From<calm_truth::error::TruthError> for PlannerSwitchError {
    fn from(e: calm_truth::error::TruthError) -> Self {
        Self::Failed(e.into())
    }
}
impl From<truth::PlannerSwitchError> for PlannerSwitchError {
    fn from(e: truth::PlannerSwitchError) -> Self {
        match e {
            truth::PlannerSwitchError::Refused { reason, card_id } => {
                refused(PlannerSwitchRefusal::Truth(reason), &card_id)
            }
            truth::PlannerSwitchError::Storage(e) => e.into(),
        }
    }
}
fn refused(reason: PlannerSwitchRefusal, card: &str) -> PlannerSwitchError {
    PlannerSwitchError::Refused {
        reason,
        card_id: card.into(),
    }
}
fn owner_reason(reason: OwnerRefusal) -> PlannerSwitchRefusal {
    use truth::PlannerSwitchRefusal as T;
    match reason {
        OwnerRefusal::OpenAsk => PlannerSwitchRefusal::OpenAsk,
        OwnerRefusal::PendingObservations => PlannerSwitchRefusal::PendingObservations,
        OwnerRefusal::TurnInFlight => PlannerSwitchRefusal::Truth(T::TurnInFlight),
        OwnerRefusal::PendingMessages => PlannerSwitchRefusal::Truth(T::PendingMessages),
        OwnerRefusal::PendingSubmission => PlannerSwitchRefusal::Truth(T::PendingSubmission),
        OwnerRefusal::SessionUnavailable => PlannerSwitchRefusal::Truth(T::SessionUnavailable),
        OwnerRefusal::Changed => PlannerSwitchRefusal::Truth(T::Changed),
    }
}
fn proof_error(e: OwnerProofError, card: &str) -> PlannerSwitchError {
    match e {
        OwnerProofError::Refused { reason, .. } => refused(owner_reason(reason), card),
        OwnerProofError::Failed(e) => e.into(),
    }
}
fn owner_error(e: CarrierOwnerError, card: &str) -> PlannerSwitchError {
    match e {
        CarrierOwnerError::Refused { reason, .. } => refused(owner_reason(reason), card),
        CarrierOwnerError::Failed(e) => e.into(),
        CarrierOwnerError::PredicateFailed(e) => e.into(),
    }
}
// Retain the typed owner verdict across Truth's generic guard contract.
struct Protected<'a> {
    owner: CarrierReplacementOwner<'a>,
    error: std::sync::Mutex<Option<PlannerSwitchError>>,
}
#[async_trait::async_trait]
impl CarrierReplacementGuard for Protected<'_> {
    async fn recheck_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        carrier: &CurrentInputCarrier,
    ) -> calm_truth::error::Result<()> {
        if let Err(e) = self.owner.recheck_typed_tx(tx, carrier).await {
            *self.error.lock().expect("owner verdict") = Some(owner_error(e, carrier.card_id()));
            return Err(calm_truth::error::TruthError::Conflict(
                "Planner owner recheck failed",
            ));
        }
        Ok(())
    }
}

pub(crate) async fn switch_planner(
    s: &RouteState,
    actor: &Actor,
    human: &HumanInputSource,
    request: SwitchPlannerRequest,
) -> Result<PlannerSwitchReceipt, PlannerSwitchError> {
    let (s, actor, human) = (s.clone(), actor.clone(), human.clone());
    tokio::spawn(async move { switch_owned(s, actor, human, request).await })
        .await
        .map_err(|_| CalmError::Internal("retained Planner switch task failed".into()))?
}

async fn switch_owned(
    s: RouteState,
    actor: Actor,
    human: HumanInputSource,
    r: SwitchPlannerRequest,
) -> Result<PlannerSwitchReceipt, PlannerSwitchError> {
    use truth::PlannerSwitchRefusal as T;
    if actor.as_str() != "user" {
        return Err(refused(
            PlannerSwitchRefusal::UserRequired,
            r.expected_planner_card_id.as_str(),
        ));
    }
    let principal = human.current_principal()?;
    crate::track_access::require_detail_access(s.repo.as_ref(), r.track_id.as_str(), &principal)
        .await?;
    let ids = [&r.expected_planner_card_id, &r.target_assistant_card_id];
    let db = s
        .mcp_context
        .sqlite_db
        .clone()
        .ok_or_else(|| CalmError::Internal("Planner switch requires SQLite".into()))?;
    let current: Option<String> =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=?1 AND role='planner'")
            .bind(r.track_id.as_str())
            .fetch_optional(db.read_pool())
            .await?;
    if current.as_deref() != Some(r.expected_planner_card_id.as_str()) {
        return Err(refused(
            PlannerSwitchRefusal::Truth(T::Changed),
            r.expected_planner_card_id.as_str(),
        ));
    }
    let mut workers = Vec::new();
    let mut cards = Vec::new();
    let mut handles = Vec::new();
    let mut slots = Vec::new();
    let factory = PlannerFactory::new(
        std::sync::Arc::downgrade(&s.shared_codex_appserver),
        &s.claude_planner,
        &s.acp_planner,
    );
    for (id, role) in ids
        .into_iter()
        .zip([CardRole::Planner, CardRole::Assistant])
    {
        let card =
            s.repo.card_get(id.as_str()).await?.ok_or_else(|| {
                refused(PlannerSwitchRefusal::Truth(T::TargetInvalid), id.as_str())
            })?;
        let (stored_role, worker): (String, Option<String>) =
            sqlx::query_as("SELECT role,session_id FROM cards WHERE id=?1")
                .bind(id.as_str())
                .fetch_one(db.read_pool())
                .await?;
        if card.track_id != r.track_id || stored_role != role.as_db_str() || ids[0] == ids[1] {
            return Err(refused(
                PlannerSwitchRefusal::Truth(T::TargetInvalid),
                id.as_str(),
            ));
        }
        let mut storage = db.read_pool().acquire().await?;
        if !card_has_canonical_storage(&mut storage, id.as_str()).await? {
            return Err(refused(
                PlannerSwitchRefusal::Truth(T::HistoricalStorage),
                id.as_str(),
            ));
        }
        drop(storage);
        let binding = PlannerBinding::from_card(&card, role).ok_or_else(|| {
            refused(
                PlannerSwitchRefusal::Truth(T::BindingUnsupported),
                id.as_str(),
            )
        })?;
        let changed = crate::harness::profile::PlannerBinding {
            profile: if role == CardRole::Planner {
                calm_types::harness::HarnessProfile::Assistant
            } else {
                calm_types::harness::HarnessProfile::Planner
            },
            provider: binding.provider,
        };
        if !PlannerFactory::canonical_inputs(&changed) {
            return Err(refused(
                PlannerSwitchRefusal::Truth(T::BindingUnsupported),
                id.as_str(),
            ));
        }
        factory
            .ready_retained(&changed.provider)
            .await
            .map_err(|_| {
                refused(
                    PlannerSwitchRefusal::Truth(T::SessionUnavailable),
                    id.as_str(),
                )
            })?;
        // Do not start a missing session to manufacture positive idle proof.
        let worker = worker.as_ref().ok_or_else(|| {
            refused(
                PlannerSwitchRefusal::Truth(T::SessionUnavailable),
                id.as_str(),
            )
        })?;
        let handle = s.harness.get(worker).ok_or_else(|| {
            refused(
                PlannerSwitchRefusal::Truth(T::SessionUnavailable),
                id.as_str(),
            )
        })?;
        slots.push(s.harness.carrier_retirement_slot(worker));
        workers.push(worker.clone());
        handles.push(handle);
        cards.push(card);
    }
    let context = &s.mcp_context;
    let mut parks = Vec::new();
    let mut calls = Vec::new();
    for (i, handle) in handles.iter().enumerate() {
        parks.push(
            handle
                .idle_carrier_owner_typed()
                .await
                .map_err(|e| proof_error(e, ids[i].as_str()))?,
        );
        calls.push(context.tool_calls.fence(&workers[i]).map_err(|_| {
            refused(
                PlannerSwitchRefusal::Truth(T::PendingSubmission),
                ids[i].as_str(),
            )
        })?);
    }
    let _push = s.dispatcher.planner_switch_push_lock(&r.track_id).await;
    let mut sorted = ids;
    sorted.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    let mut fences = Vec::new();
    for id in sorted {
        fences.push(
            crate::operation::planner_start_fence::CardStartFence::lock(
                &s.planner_recovery_locks,
                &s.repo,
                &s.operation_runtime,
                id,
            )
            .await,
        );
    }
    let drive = s.operation_runtime.lock_for_track_delete().await;
    let _track = crate::per_card_lock::lock_key(&s.track_delete_locks, r.track_id.as_str()).await;
    for (i, park) in parks.iter().enumerate() {
        park.recheck_typed()
            .await
            .map_err(|e| proof_error(e, ids[i].as_str()))?;
    }
    // Qualification may read repository facts; keep those awaits outside the writer.
    // Even a read snapshot uses the shared immediate owner to avoid deferred-lock cycles.
    let mut read = sqlite::begin_immediate_tx(&db).await?;
    let mut arrivals: Vec<Vec<calm_truth::db::sqlite::event_inputs::EventInputFact>> = Vec::new();
    for (i, worker) in workers.iter().enumerate() {
        let carrier = current_input_carrier_tx(&mut read, ids[i].as_str(), worker).await?;
        let after = carrier.snapshot()["push_watermark"]
            .as_i64()
            .ok_or_else(|| CalmError::Internal("missing watermark".into()))?;
        arrivals.push(
            s.dispatcher
                .carrier_quiescence_prefix_tx(&mut read, &r.track_id, after)
                .await?,
        );
    }
    let baseline: i64 = sqlx::query_scalar("SELECT planner_event_baseline FROM tracks WHERE id=?1")
        .bind(r.track_id.as_str())
        .fetch_one(&mut **read)
        .await?;
    read.rollback().await?;
    if s.dispatcher
        .planner_switch_pending(&r.track_id, &arrivals[0], baseline)
        .await?
    {
        return Err(refused(
            PlannerSwitchRefusal::PendingObservations,
            ids[0].as_str(),
        ));
    }
    let owners: Vec<_> = (0..2)
        .map(|i| Protected {
            owner: CarrierReplacementOwner {
                idle: &parks[i],
                calls: &calls[i],
                dispatcher: &s.dispatcher,
                track: r.track_id.clone(),
                arrivals: &arrivals[i],
            },
            error: std::sync::Mutex::new(None),
        })
        .collect();
    let mut tx = sqlite::begin_immediate_tx(&db).await?;
    let outcome = async {
        let principal = human.current_principal()?;
        let area =
            crate::track_access::require_detail_access_tx(&mut tx, r.track_id.as_str(), &principal)
                .await?;
        crate::planner_permission_mode::inherit_for_switch_tx(
            &mut tx,
            ids[0].as_str(),
            ids[1].as_str(),
        )
        .await
        .map_err(|error| match error {
            crate::planner_permission_mode::InheritanceError::Malformed(error) => {
                tracing::warn!(%error, "outgoing Planner permission mode unreadable");
                refused(
                    PlannerSwitchRefusal::Truth(T::BindingUnsupported),
                    ids[0].as_str(),
                )
            }
            crate::planner_permission_mode::InheritanceError::Storage(error) => error.into(),
        })?;
        let mut expected = Vec::new();
        for (i, card) in cards.iter().enumerate() {
            let carrier = current_input_carrier_tx(&mut tx, ids[i].as_str(), &workers[i]).await?;
            let binding = PlannerBinding::from_card(
                card,
                if i == 0 {
                    CardRole::Planner
                } else {
                    CardRole::Assistant
                },
            )
            .ok_or_else(|| {
                refused(
                    PlannerSwitchRefusal::Truth(T::BindingUnsupported),
                    ids[i].as_str(),
                )
            })?;
            expected.push(ExpectedCarrier {
                card_id: ids[i].to_string(),
                track_id: r.track_id.to_string(),
                worker_session_id: carrier.worker_session_id().into(),
                original_snapshot: carrier.original_snapshot().into(),
                execution: RuntimeExecutionFence::from_carrier(&carrier),
                accepted_prefix: carrier.snapshot()["push_watermark"]
                    .as_i64()
                    .ok_or_else(|| CalmError::Internal("missing watermark".into()))?,
                binding,
                ingress: carrier_ingress_revision_tx(
                    &mut tx,
                    ids[i].as_str(),
                    r.track_id.as_str(),
                    carrier.worker_session_id(),
                )
                .await?,
                owner: &owners[i],
                revision: CallerRevision::Unversioned,
            });
            // Both open-ask checks precede the first credential revocation.
            owners[i]
                .owner
                .recheck_typed_tx(&mut tx, &carrier)
                .await
                .map_err(|e| owner_error(e, ids[i].as_str()))?;
        }
        let target = expected.pop().expect("target");
        let old = expected.pop().expect("Planner");
        let receipt = truth::switch_planner_tx(
            &mut tx,
            truth::ExpectedPlanner {
                track_id: r.track_id.as_str(),
                planner_card_id: ids[0].as_str(),
                target_card_id: ids[1].as_str(),
                revision: r.expected_revision,
                user_id: &principal.user_id,
            },
            (old, target),
        )
        .await
        .map_err(PlannerSwitchError::from)?;
        let scope = crate::event::EventScope::Track {
            track: r.track_id.clone(),
            area,
        };
        let event_ids = sqlite::append_decision_events_in_tx(
            &mut tx,
            &ActorId::User,
            &scope,
            None,
            &receipt.events,
        )
        .await?;
        Ok::<_, PlannerSwitchError>((receipt, scope, event_ids))
    }
    .await;
    let (receipt, scope, event_ids) = match outcome {
        Ok(value) => value,
        Err(e) => {
            tx.rollback().await?;
            return Err(owners
                .iter()
                .find_map(|o| o.error.lock().expect("owner verdict").take())
                .unwrap_or(e));
        }
    };
    tx.commit().await?;
    // No await between SQL commit and synchronous pair publication.
    #[allow(deprecated)] // The role-change owner publishes the shared cache after commit.
    s.harness.publish_pair(
        [
            (&receipt.demoted, &slots[0]),
            (&receipt.promoted, &slots[1]),
        ],
        s.write.role_cache(),
    )?;
    for (event, id) in receipt.events.iter().zip(event_ids) {
        s.events.emit_envelope(crate::event::BroadcastEnvelope {
            id,
            event_version: crate::event::SYNC_EVENT_VERSION,
            actor: ActorId::User,
            scope: scope.clone(),
            event: event.clone(),
        });
    }
    drop(drive);
    drop(parks);
    for (replacement, slot) in [&receipt.demoted, &receipt.promoted].into_iter().zip(slots) {
        if let Err(error) = crate::harness::HarnessRegistry::cleanup_replacement(
            replacement,
            slot,
            Some(&s.shared_codex_appserver),
        )
        .await
        {
            tracing::warn!(%error, "committed Planner switch cleanup failed");
        }
    }
    Ok(receipt)
}
