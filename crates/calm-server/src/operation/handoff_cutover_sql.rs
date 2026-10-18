//! Transaction-bound cutover facts and writes. The operation owns BEGIN/commit/rollback.
use super::Tx;
use super::handoff_cutover::{
    CutoverConfiguration, CutoverResult, CutoverScope, ValidatedCutoverPlan, conflict,
};
use crate::db::sqlite;
use crate::error::Result;
use crate::harness::HarnessSnapshot;
use crate::harness::handoff_pause::PauseState;
use crate::model::Card;
use serde_json::json;

type StoredTrack = (Option<i64>, Option<String>, Option<String>, Option<String>);

pub(super) struct Facts {
    pub configuration: CutoverConfiguration,
    source: Card,
    source_snapshot: HarnessSnapshot,
    target_snapshot: HarnessSnapshot,
}

async fn card(tx: &mut Tx<'_>, id: &str) -> Result<Card> {
    sqlx::query_as::<_, crate::db::rows::CardRow>(
        "SELECT id,track_id,kind,sort,payload,title,deletable,created_at,updated_at FROM cards WHERE id=?1",
    ).bind(id).fetch_optional(&mut **tx).await?.map(Card::from).ok_or_else(conflict)
}

async fn paused(tx: &mut Tx<'_>, scope: &CutoverScope, source: bool) -> Result<HarnessSnapshot> {
    let (card, session, receipt, contract) = if source {
        (
            &scope.source_card_id,
            &scope.source_session_id,
            &scope.source_pause,
            calm_types::worker::WorkerContract::Planner,
        )
    } else {
        (
            &scope.target_card_id,
            &scope.target_session_id,
            &scope.target_pause,
            calm_types::worker::WorkerContract::Assistant,
        )
    };
    let snapshot = idle_snapshot(tx, card, session, &scope.track_id, contract).await?;
    if snapshot.handoff_pause
        != (PauseState::Paused {
            receipt: receipt.clone(),
        })
    {
        return Err(conflict());
    }
    // Assistant has no system observation ownership contract. Refuse, never silently discard.
    if !source && (!snapshot.pending_entries().is_empty() || snapshot.push_watermark != 0) {
        return Err(conflict());
    }
    Ok(snapshot)
}

/// Durable idle facts shared by initial coordinator admission and exact paused precheck.
pub(super) async fn idle_snapshot(
    tx: &mut Tx<'_>,
    card: &crate::ids::CardId,
    session: &str,
    track_id: &crate::ids::TrackId,
    contract: calm_types::worker::WorkerContract,
) -> Result<HarnessSnapshot> {
    if !crate::harness::profile::current_managed_binding_tx(tx, session, card, track_id).await? {
        return Err(conflict());
    }
    let row = sqlite::session_get_tx(tx, &session.into())
        .await?
        .ok_or_else(conflict)?;
    if row.state != crate::session_projection_repo::WorkerSessionState::Idle
        || row.contract != contract
        || row.active_turn_id.is_some()
        || sqlite::acp_submission_unresolved(&mut **tx, session).await?
    {
        return Err(conflict());
    }
    if !crate::ask::open_hold_asks(&mut **tx, track_id, &session.into())
        .await?
        .is_empty()
    {
        return Err(conflict());
    }
    let raw = row.handle_state_json.ok_or_else(conflict)?;
    let snapshot = HarnessSnapshot::parse_known(raw.clone()).ok_or_else(conflict)?;
    snapshot.handoff_entries_checked().ok_or_else(conflict)?;
    let canonical = serde_json::to_value(&snapshot)?;
    for key in [
        "pending_queue",
        "pending_envelope_ids",
        "pending_entry_meta",
        "pending_message_ids",
    ] {
        if raw.get(key).is_none() || raw.get(key) != canonical.get(key) {
            return Err(conflict());
        }
    }
    if !PauseState::quiet_checkpoint(&snapshot) {
        return Err(conflict());
    }
    Ok(snapshot)
}

pub(super) async fn facts(tx: &mut Tx<'_>, scope: &CutoverScope) -> Result<Facts> {
    let (source, configuration) = binding_facts(
        tx,
        &scope.track_id,
        &scope.source_card_id,
        &scope.source_session_id,
        &scope.target_card_id,
        &scope.target_session_id,
    )
    .await?;
    let source_snapshot = paused(tx, scope, true).await?;
    let target_snapshot = paused(tx, scope, false).await?;
    Ok(Facts {
        configuration,
        source,
        source_snapshot,
        target_snapshot,
    })
}

pub(super) async fn cutover(
    tx: &mut Tx<'_>,
    plan: &ValidatedCutoverPlan,
    operation_id: &str,
) -> Result<CutoverResult> {
    let scope = &plan.scope;
    super::handoff_coordinator_sql::validate_cutover_intent(tx, plan).await?;
    let mut facts = facts(tx, scope).await?;
    if facts.configuration != plan.configuration {
        return Err(conflict());
    }
    let now = crate::model::now_ms();
    let mut fresh = HarnessSnapshot::initial(
        facts.source_snapshot.push_watermark,
        facts.source_snapshot.pending_entries(),
    );
    fresh.last_report_body_sha256 = facts.source_snapshot.last_report_body_sha256.clone();
    for (session, snapshot) in [
        (&scope.source_session_id, &mut facts.source_snapshot),
        (&scope.target_session_id, &mut facts.target_snapshot),
    ] {
        let receipt = snapshot
            .handoff_pause
            .receipt()
            .ok_or_else(conflict)?
            .clone();
        snapshot.set_pending_entries(vec![]);
        snapshot.handoff_pause = PauseState::ConsumedByHandoff {
            receipt,
            plan_id: scope.plan_id.clone(),
            operation_id: operation_id.into(),
            new_card_id: scope.target_card_id.clone(),
            new_session_id: scope.new_session_id.clone(),
        };
        sqlite::session_set_handle_state_tx(tx, session, Some(serde_json::to_value(snapshot)?))
            .await?;
        sqlite::session_supersede_active_tx(tx, session, now).await?;
        sqlx::query(
            "UPDATE worker_sessions SET mcp_token_hash=NULL,queue_harvested_at_ms=?1 WHERE id=?2",
        )
        .bind(now)
        .bind(session)
        .execute(&mut **tx)
        .await?;
    }
    sqlx::query("DELETE FROM card_mcp_tokens WHERE card_id IN (?1,?2)")
        .bind(scope.source_card_id.as_str())
        .bind(scope.target_card_id.as_str())
        .execute(&mut **tx)
        .await?;
    let source_map = facts.source.payload.as_object_mut().ok_or_else(conflict)?;
    source_map.remove("planner_harness");
    source_map.insert("planner_history".into(), json!({"plan_id":scope.plan_id,"operation_id":operation_id,"new_card_id":scope.target_card_id,"new_session_id":scope.new_session_id}));
    sqlx::query("UPDATE cards SET role='planner_history',payload=?1,updated_at=?2 WHERE id=?3")
        .bind(serde_json::to_string(&facts.source.payload)?)
        .bind(now)
        .bind(scope.source_card_id.as_str())
        .execute(&mut **tx)
        .await?;
    let mut payload = facts.configuration.conversation.payload.clone();
    payload
        .as_object_mut()
        .ok_or_else(conflict)?
        .insert("handoff_pending_activation".into(), json!(operation_id));
    sqlx::query("UPDATE cards SET role='planner',payload=?1,updated_at=?2 WHERE id=?3")
        .bind(serde_json::to_string(&payload)?)
        .bind(now)
        .bind(scope.target_card_id.as_str())
        .execute(&mut **tx)
        .await?;
    let init = crate::session_projection_repo::WorkerSessionInit::shared_planner(
        scope.new_session_id.clone(),
        scope.target_card_id.to_string(),
        facts.configuration.conversation.provider.clone(),
        crate::session_projection_repo::WorkerSessionState::PendingActivation,
        None,
        serde_json::to_value(&fresh)?,
        now,
    );
    sqlite::session_prepare_pending_activation_tx(tx, &init).await?;
    Ok(CutoverResult::CutoverCommittedPendingActivation {
        operation_id: operation_id.into(),
        plan_id: scope.plan_id.clone(),
        track_id: scope.track_id.clone(),
        history_card_id: scope.source_card_id.clone(),
        retired_source_session_id: scope.source_session_id.clone(),
        retired_target_session_id: scope.target_session_id.clone(),
        planner_card_id: scope.target_card_id.clone(),
        planner_session_id: scope.new_session_id.clone(),
        configuration: facts.configuration,
        snapshot_session_id: scope.new_session_id.clone(),
        push_watermark: fresh.push_watermark,
    })
}

/// Shared identity/configuration admission. Pause and provider policy stay with their owners.
pub(super) async fn binding_facts(
    tx: &mut Tx<'_>,
    track_id: &crate::ids::TrackId,
    source_card_id: &crate::ids::CardId,
    source_session_id: &str,
    target_card_id: &crate::ids::CardId,
    target_session_id: &str,
) -> Result<(Card, CutoverConfiguration)> {
    let track: Option<StoredTrack> = sqlx::query_as(
        "SELECT closed_at,purpose,template_id,root_session_id FROM tracks WHERE id=?1",
    )
    .bind(track_id.as_str())
    .fetch_optional(&mut **tx)
    .await?;
    let (closed, purpose, template_id, root) = track.ok_or_else(conflict)?;
    if closed.is_some() || purpose.is_some() || root.as_deref() != Some(source_session_id) {
        return Err(conflict());
    }
    let planners: Vec<String> =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=?1 AND role='planner'")
            .bind(track_id.as_str())
            .fetch_all(&mut **tx)
            .await?;
    if planners != vec![source_card_id.to_string()] {
        return Err(conflict());
    }
    let source = card(tx, source_card_id.as_str()).await?;
    let target = card(tx, target_card_id.as_str()).await?;
    let roles: (String, String) =
        sqlx::query_as("SELECT s.role,t.role FROM cards s,cards t WHERE s.id=?1 AND t.id=?2")
            .bind(source_card_id.as_str())
            .bind(target_card_id.as_str())
            .fetch_one(&mut **tx)
            .await?;
    if roles != ("planner".into(), "assistant".into()) {
        return Err(conflict());
    }
    let conversation = crate::harness::profile::plan_planner_handoff(&source, &target)?;
    let hash = crate::routes::idempotency_key::stable_payload_hash;
    let configuration = CutoverConfiguration {
        conversation,
        source_payload_hash: hash(&source.payload)?,
        target_payload_hash: hash(&target.payload)?,
        template_id,
    };
    for (card, session) in [
        (source_card_id, source_session_id),
        (target_card_id, target_session_id),
    ] {
        if !crate::harness::profile::current_managed_binding_tx(tx, session, card, track_id).await?
        {
            return Err(conflict());
        }
    }
    Ok((source, configuration))
}
