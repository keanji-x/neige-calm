//! Human-only first-input acceptance. This private DB composition never starts
//! or wakes a provider; later execution needs its own owning authorization.
use super::super::CreateTrackRequest;
use super::creation_plan::HumanTrackCreationPlan;
use super::*;
use crate::auth::HumanInputSource;
use crate::db::sqlite::conversation::{
    HumanTrackCreationAcceptance, HumanTrackCreationAck, current_input_carrier_tx,
    human_track_creation_get_tx, human_track_creation_insert_tx,
};
use crate::db::sqlite::track_create_idempotency_get_tx;
use crate::harness::profile::{HarnessProfile, PlannerBinding};
use crate::harness::snapshot::CanonicalHarnessSnapshot;
use crate::session_projection_repo::{WorkerSessionInit, WorkerSessionState};
use calm_types::conversation::{AcceptedInputRef, HumanPayload};
use calm_types::organization::AreaAccess;
use sqlx::{Sqlite, Transaction};
use std::sync::{Arc, OnceLock};

#[cfg(all(test, feature = "fixtures"))]
mod tests;

#[derive(Debug)]
pub(in crate::routes::tracks) struct HumanTrackCreationResult {
    pub ack: HumanTrackCreationAck,
    /// The finite response view read under the same Area/resource decision.
    /// It is transient output, never another persisted acceptance authority.
    pub track: crate::model::Track,
}

/// All preparation values are data. This entry obtains authority again and
/// owns fresh allocation rather than accepting caller-supplied allocated IDs.
pub(in crate::routes::tracks) async fn accept_dormant_human_track(
    s: RouteState,
    source: HumanInputSource,
    headers: HeaderMap,
    request: CreateTrackRequest,
) -> Result<HumanTrackCreationResult> {
    let plan = HumanTrackCreationPlan::prepare(request, &headers, s.workspace_root.clone())?;
    let raw_key = plan.key().to_owned();
    let create_sha = plan.create_sha().to_owned();
    let first_sha = plan.first_sha().to_owned();
    let area_id = plan.area_id().to_owned();
    let write = s.write.clone();
    let templates = s.templates;
    // Like keyed Area creation, the event owner rolls a read-only replay back.
    // Only a fully checked receipt populates this local result; unrelated
    // failures cannot turn into success or manufacture a creation event.
    let (replay_tx, mut replay_rx) = tokio::sync::oneshot::channel();
    let result = crate::db::write_with_actor_events_typed(
        s.repo.as_ref(),
        None,
        &s.events,
        &s.write,
        move |tx| {
            Box::pin(async move {
                let principal = source.current_principal()?;
                let area = crate::organization_access::require_area_access_tx(
                    tx,
                    &area_id,
                    &principal,
                    AreaAccess::Collaborate,
                )
                .await?;
                if area.kind != crate::model::AreaKind::User {
                    return Err(CalmError::Forbidden(
                        "Human Track creation does not accept System targets".into(),
                    ));
                }
                if let Some(stored) = human_track_creation_get_tx(tx, &area_id, &raw_key).await? {
                    if stored.source_user_id != principal.user_id
                        || stored.create_request_sha256 != create_sha
                        || stored.first_message_sha256 != first_sha
                    {
                        return Err(CalmError::IdempotencyKeyReused(
                            "this key names another original Human creation".into(),
                        ));
                    }
                    require_surviving_allocation_tx(tx, &area_id, &stored.acknowledgement).await?;
                    let track = response_track_tx(tx, &stored.acknowledgement).await?;
                    let _ = replay_tx.send((stored.acknowledgement, track));
                    return Err(CalmError::Conflict(
                        "Human Track acceptance already committed".into(),
                    ));
                }
                if track_create_idempotency_get_tx(tx, &area_id, &raw_key)
                    .await?
                    .is_some()
                    || prior_start_attempt_tx(tx, &area_id, &raw_key).await?
                {
                    return Err(CalmError::IdempotencyKeyReused(
                        "a historical creation/start cannot be adopted as a fresh Human acceptance"
                            .into(),
                    ));
                }
                let (p, options, text) = plan.into_allocation()?;
                let provider = options.planner_provider.clone();
                let planner = crate::model::new_id();
                let report = crate::model::new_id();
                // Audit attribution is not permission. The actual live source and
                // current organization decision above authorize content acceptance.
                let ((_, created), mut events) = super::super::allocation::allocate_track_structure_tx(
                    tx,
                    ActorId::User,
                    p,
                    options,
                    (planner.clone(), report.clone()),
                    super::super::allocation::TrackAllocationOwners { write, templates },
                    Arc::new(OnceLock::new()),
                )
                .await?;
                if !created {
                    return Err(CalmError::Conflict(
                        "Human acceptance requires its fresh allocation".into(),
                    ));
                }
                let shape = crate::db::sqlite::card_execution_shape_tx(tx, &planner).await?;
                let actual = PlannerBinding::from_shape(&shape.kind, shape.role, &shape.payload)
                    .ok_or_else(|| {
                        CalmError::Conflict("fresh Planner binding is invalid".into())
                    })?;
                if actual.profile != HarnessProfile::Planner || actual.provider != provider {
                    return Err(CalmError::Conflict(
                        "fresh Planner differs from its requested binding".into(),
                    ));
                }
                let already_owned: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM cards WHERE id=?1 AND session_id IS NOT NULL)",
                )
                .bind(&planner)
                .fetch_one(&mut **tx)
                .await?;
                if already_owned {
                    return Err(CalmError::Conflict(
                        "fresh Planner already has an execution owner".into(),
                    ));
                }
                let watermark: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(id),0) FROM events")
                    .fetch_one(&mut **tx)
                    .await?;
                let runtime = crate::model::new_id();
                crate::db::sqlite::session_start_runtime_tx(
                    tx,
                    WorkerSessionInit::shared_planner(
                        runtime.clone(),
                        planner.clone(),
                        provider,
                        WorkerSessionState::Idle,
                        None,
                        serde_json::to_value(CanonicalHarnessSnapshot::dormant(watermark))?,
                        crate::model::now_ms(),
                    ),
                )
                .await?;
                let mut carrier = current_input_carrier_tx(tx, &planner, &runtime).await?;
                let current = source.current_principal()?;
                let accepted = crate::conversation_inputs::accept_human_tx(
                    tx,
                    &current,
                    &mut carrier,
                    HumanPayload {
                        text,
                        bound_attachments: vec![],
                    },
                )
                .await?;
                let AcceptedInputRef::Human { message } = accepted.input_ref else {
                    return Err(CalmError::Internal(
                        "Human creation returned another input class".into(),
                    ));
                };
                let stored = human_track_creation_insert_tx(
                    tx,
                    &area_id,
                    &raw_key,
                    &HumanTrackCreationAcceptance {
                        source_user_id: current.user_id,
                        origin_runtime_id: runtime,
                        first_message: message,
                        presentation_sequence: accepted.presentation_sequence,
                    },
                )
                .await?;
                let track = response_track_tx(tx, &stored.acknowledgement).await?;
                events.push((ActorId::User, crate::event::EventScope::Card {
                    card:planner.clone().into(),track:track.id.clone(),area:track.area_id.clone(),
                }, crate::event::Event::HarnessPresentationChanged {
                    card_id:planner.clone().into(),track_id:track.id.clone(),
                    cursor:calm_types::conversation_display::ConversationCursor::new(&planner,
                        calm_types::conversation_display::ConversationCursorSource::Presentation,accepted.presentation_sequence),
                }));
                Ok(((stored.acknowledgement, track), events))
            })
        },
    )
    .await;
    // Acceptance is already committed. Native availability/wake/report is not
    // part of this acknowledgement and cannot turn it back into "unaccepted".
    match result {
        Ok(((ack, track), _)) => Ok(HumanTrackCreationResult { ack, track }),
        Err(error @ CalmError::Conflict(_)) => replay_rx
            .try_recv()
            .map(|(ack, track)| HumanTrackCreationResult { ack, track })
            .map_err(|_| error),
        Err(error) => Err(error),
    }
}

async fn response_track_tx(
    tx: &mut Transaction<'_, Sqlite>,
    ack: &HumanTrackCreationAck,
) -> Result<crate::model::Track> {
    let HumanTrackCreationAck::V1 { track_id, .. } = ack;
    crate::db::sqlite::track_get_tx(tx, &crate::ids::TrackId::from(track_id.clone())).await
}

async fn require_surviving_allocation_tx(
    tx: &mut Transaction<'_, Sqlite>,
    area_id: &str,
    ack: &HumanTrackCreationAck,
) -> Result<()> {
    let HumanTrackCreationAck::V1 {
        track_id,
        planner_card_id,
        report_card_id,
        ..
    } = ack;
    // Roles, the initial revision and the origin runtime were proved when the
    // receipt was minted. Replay authorizes the surviving resources; it does
    // not re-prove their initial graph or turn the old runtime into a ticket.
    let alive: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM tracks t JOIN cards c ON c.track_id=t.id AND c.id=?2
         JOIN cards r ON r.track_id=t.id AND r.id=?3 WHERE t.id=?1 AND t.area_id=?4)",
    )
    .bind(track_id)
    .bind(planner_card_id)
    .bind(report_card_id)
    .bind(area_id)
    .fetch_one(&mut **tx)
    .await?;
    if !alive {
        return Err(CalmError::IdempotencyKeyExhausted(
            "the original accepted allocation no longer exists in its scope".into(),
        ));
    }
    Ok(())
}

pub(super) async fn prior_start_attempt_tx(
    tx: &mut Transaction<'_, Sqlite>,
    area_id: &str,
    raw_key: &str,
) -> Result<bool> {
    let base = derive_track_create_operation_key(area_id, raw_key);
    let mut query =
        sqlx::QueryBuilder::<Sqlite>::new("SELECT EXISTS(SELECT 1 FROM operations WHERE kind=");
    query.push_bind(crate::routes::conversations_shared::PLANNER_HARNESS_START);
    query.push(" AND idempotency_key IN (");
    let mut keys = query.separated(",");
    for key in crate::routes::conversations_shared::operation_key_attempts(&base) {
        keys.push_bind(key);
    }
    keys.push_unseparated("))");
    Ok(query
        .build_query_scalar::<bool>()
        .fetch_one(&mut **tx)
        .await?)
}
