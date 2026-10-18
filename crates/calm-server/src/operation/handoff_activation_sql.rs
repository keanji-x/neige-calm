//! Final SQL authority and resource checkpoint for explicit handoff activation.
use serde_json::{Value, json};

use super::handoff_activation::ActivationResult;
use super::handoff_cutover::{ValidatedCutoverPlan, conflict};
use super::{Operation, Tx, TxOutput};
use crate::error::Result;
use crate::harness::{HarnessPhaseTag, HarnessSnapshot, Observation, QueueEntry};

type StoredTrack = (Option<i64>, Option<String>, Option<String>, Option<String>);

pub(super) struct Facts {
    pub plan: ValidatedCutoverPlan,
    pub snapshot: HarnessSnapshot,
    source: Value,
    pub cwd: String,
}

impl Facts {
    pub(super) fn validate_output(&self, output: &TxOutput) -> Result<()> {
        if output.output_string("cwd", "activation")? != self.cwd
            || output.output_string("runtime_id", "activation")? != self.plan.scope.new_session_id
            || output.output_string("card_id", "activation")?
                != self.plan.scope.target_card_id.as_str()
            || output.output_string("track_id", "activation")? != self.plan.scope.track_id.as_str()
        {
            return Err(conflict());
        }
        Ok(())
    }
}

pub(super) async fn handoff_facts(
    tx: &mut Tx<'_>,
    input: &super::handoff_coordinator::CoordinatorInput,
    identity: &super::handoff_coordinator::CommittedIdentity,
    operation_id: &str,
    prepared: bool,
) -> Result<Facts> {
    current_facts(
        tx,
        identity.plan(input),
        identity.evidence_owner(operation_id).into(),
        prepared.then_some(operation_id),
        true,
    )
    .await
}
pub(super) async fn current_facts(
    tx: &mut Tx<'_>,
    plan: ValidatedCutoverPlan,
    cutover_operation_id: String,
    activation: Option<&str>,
    workflow: bool,
) -> Result<Facts> {
    let scope = &plan.scope;
    let track: Option<StoredTrack> = sqlx::query_as(
        "SELECT closed_at,purpose,root_session_id,template_id FROM tracks WHERE id=?1",
    )
    .bind(scope.track_id.as_str())
    .fetch_optional(&mut **tx)
    .await?;
    let Some((None, None, Some(root), template_id)) = track else {
        return Err(conflict());
    };
    if root != scope.new_session_id || template_id != plan.configuration.template_id {
        return Err(conflict());
    }
    let card: crate::db::rows::CardRow = sqlx::query_as(
        concat!("SELECT id,track_id,kind,sort,payload,title,deletable,created_at,updated_at FROM cards WHERE id=?1 ","AND role='planner' AND session_id=?2 AND track_id=?3"))
        .bind(scope.target_card_id.as_str()).bind(&scope.new_session_id).bind(scope.track_id.as_str())
        .fetch_optional(&mut **tx).await?.ok_or_else(conflict)?;
    let card = crate::model::Card::from(card);
    let mut expected = plan.configuration.conversation.payload.clone();
    let row = crate::db::sqlite::session_get_tx(tx, &scope.new_session_id.as_str().into())
        .await?
        .ok_or_else(conflict)?;
    let claim: Option<String> = if workflow {
        sqlx::query_scalar("SELECT operation_id FROM planner_handoff_resources WHERE operation_id=?1 AND session_id=?2")
            .bind(activation.unwrap_or(&cutover_operation_id)).bind(&scope.new_session_id).fetch_optional(&mut **tx).await?
    } else {
        sqlx::query_scalar("SELECT activation_operation_id FROM planner_handoff_activations WHERE cutover_operation_id=?1 AND session_id=?2")
            .bind(&cutover_operation_id).bind(&scope.new_session_id).fetch_optional(&mut **tx).await?
    };
    match activation {
        Some(op) if claim.as_deref() == Some(op) => {}
        None if claim.is_none() => {}
        _ => return Err(conflict()),
    }
    if row.state == crate::session_projection_repo::WorkerSessionState::PendingActivation {
        expected.as_object_mut().ok_or_else(conflict)?.insert(
            "handoff_pending_activation".into(),
            json!(cutover_operation_id),
        );
    } else if activation.is_none() || !row.state.is_active_authority() {
        return Err(conflict());
    }
    // Installation may add transport metadata, never alter the cutover's configuration.
    let actual = super::planner_harness_start_adapter::creation_configuration(card.payload)?;
    let expected = super::planner_harness_start_adapter::creation_configuration(expected)?;
    if actual != expected
        || row.provider
            != calm_types::worker::WorkerProviderKind::from(
                &plan.configuration.conversation.provider,
            )
        || row.contract != calm_types::worker::WorkerContract::Planner
        || row.card_id.as_ref().map(|id| id.as_str()) != Some(scope.target_card_id.as_str())
        || row.track_id.as_str() != scope.track_id.as_str()
    {
        return Err(conflict());
    }
    for (session, card, receipt) in [
        (
            &scope.source_session_id,
            &scope.source_card_id,
            &scope.source_pause,
        ),
        (
            &scope.target_session_id,
            &scope.target_card_id,
            &scope.target_pause,
        ),
    ] {
        let old = crate::db::sqlite::session_get_tx(tx, &session.as_str().into())
            .await?
            .ok_or_else(conflict)?;
        let snapshot = old
            .handle_state_json
            .and_then(HarnessSnapshot::parse_known)
            .ok_or_else(conflict)?;
        if old.state != crate::session_projection_repo::WorkerSessionState::Superseded
            || old.mcp_token_hash.is_some()
            || snapshot.handoff_pause
                != (crate::harness::handoff_pause::PauseState::ConsumedByHandoff {
                    receipt: receipt.clone(),
                    plan_id: scope.plan_id.clone(),
                    operation_id: cutover_operation_id.clone(),
                    new_card_id: scope.target_card_id.clone(),
                    new_session_id: scope.new_session_id.clone(),
                })
            || !receipt.matches_binding(card, session, &scope.track_id)
        {
            return Err(conflict());
        }
    }
    let source = row.handle_state_json.ok_or_else(conflict)?;
    let snapshot = HarnessSnapshot::parse_known(source.clone()).ok_or_else(conflict)?;
    snapshot.handoff_entries_checked().ok_or_else(conflict)?;
    let track: crate::db::rows::TrackRow = sqlx::query_as(&format!(
        "SELECT {} FROM tracks WHERE id=?1",
        crate::db::rows::TRACK_SELECT_COLUMNS
    ))
    .bind(scope.track_id.as_str())
    .fetch_one(&mut **tx)
    .await?;
    let cwd = crate::model::Track::from(track)
        .workspace
        .agent_cwd()
        .to_string();
    Ok(Facts {
        plan,
        snapshot,
        source,
        cwd,
    })
}

pub(super) async fn prepare_snapshot(
    tx: &mut Tx<'_>,
    facts: &Facts,
    briefing: String,
) -> Result<TxOutput> {
    let scope = &facts.plan.scope;
    let mut snapshot = facts.snapshot.clone();
    let mut entries = snapshot.pending_entries();
    entries.insert(
        0,
        QueueEntry::system(Observation::SystemContext { text: briefing }, None)?,
    );
    snapshot.set_pending_entries(entries);
    // The caller owns BEGIN IMMEDIATE. Compare the complete source under that
    // writer lock, then use the existing snapshot writer instead of a second
    // pending-only implementation. Read-back proves durability even if a trigger
    // ignored the write; neither a stale queue nor an unpersisted briefing may issue.
    let current = crate::db::sqlite::session_get_tx(tx, &scope.new_session_id.as_str().into())
        .await?
        .ok_or_else(conflict)?;
    if current.state != crate::session_projection_repo::WorkerSessionState::PendingActivation
        || current.handle_state_json.as_ref() != Some(&facts.source)
    {
        return Err(conflict());
    }
    let prepared = serde_json::to_value(&snapshot)?;
    crate::db::sqlite::session_set_handle_state_of_any_runtime_tx(
        tx,
        &scope.new_session_id,
        Some(prepared.clone()),
        crate::model::now_ms(),
    )
    .await?;
    if crate::db::sqlite::session_handle_state_by_id_tx(tx, &scope.new_session_id)
        .await?
        .as_ref()
        != Some(&prepared)
    {
        return Err(conflict());
    }
    let mut output = TxOutput::new(
        "card",
        Some(scope.target_card_id.to_string()),
        serde_json::to_value(ActivationResult::Preparing)?,
    );
    output.data = json!({"card_id":scope.target_card_id,"track_id":scope.track_id,
        "runtime_id":scope.new_session_id,"cwd":facts.cwd,"snapshot":snapshot,"codex_thread_id":null});
    Ok(output)
}

#[expect(
    clippy::too_many_arguments,
    reason = "shared transaction owns proof, snapshot and credential binding"
)]
pub(super) async fn bind_resources_tx(
    tx: &mut Tx<'_>,
    facts: &Facts,
    op: &Operation,
    output: &mut TxOutput,
    thread_id: &str,
    token_hash: Option<&str>,
    start: &super::planner_harness_start_adapter::PlannerHarnessStartAdapter,
    result: Value,
) -> Result<()> {
    let scope = &facts.plan.scope;
    let mut snapshot = facts.snapshot.clone();
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(thread_id.into());
    let mut next = output.clone();
    next.result = result;
    next.data["codex_thread_id"] = json!(thread_id);
    next.data["snapshot"] = serde_json::to_value(&snapshot)?;
    let statement = "UPDATE planner_handoff_resources SET thread_id=?1,token_hash=?2 WHERE operation_id=?3 AND thread_id IS NULL";
    if sqlx::query(statement)
        .bind(thread_id)
        .bind(token_hash)
        .bind(&op.id)
        .execute(&mut **tx)
        .await?
        .rows_affected()
        != 1
    {
        return Err(conflict());
    }
    // Checkpoint is written before the narrow transition trigger consumes its proof, all in
    // one transaction. Mere presence of a claim never authorizes activation.
    super::checkpoint_app_server_interact_tx(
        tx,
        op,
        super::handoff_coordinator_adapter::continuation(),
        output,
        &next,
    )
    .await?;
    sqlx::query(concat!("UPDATE worker_sessions SET state='starting',thread_id=?1,mcp_token_hash=?2,handle_state_json=?3 ","WHERE id=?4 AND state='pending_activation'"))
        .bind(thread_id).bind(token_hash).bind(serde_json::to_value(&snapshot)?.to_string()).bind(&scope.new_session_id).execute(&mut **tx).await?;
    if let Some(hash) = token_hash {
        crate::mcp_server::wiring::persist_card_mcp_token_hash(
            tx,
            scope.target_card_id.as_str(),
            hash,
        )
        .await?;
    }
    sqlx::query("UPDATE cards SET payload=json_remove(payload,'$.handoff_pending_activation') WHERE id=?1 AND session_id=?2")
        .bind(scope.target_card_id.as_str()).bind(&scope.new_session_id).execute(&mut **tx).await?;
    start
        .apply_activation_transport(tx, scope.target_card_id.as_str(), thread_id)
        .await?;
    validate_resources(tx, facts, &op.id).await?;

    *output = next;
    Ok(())
}

pub(super) async fn validate_resources(
    tx: &mut Tx<'_>,
    facts: &Facts,
    operation_id: &str,
) -> Result<()> {
    let valid: bool = sqlx::query_scalar(r#"
        SELECT EXISTS(SELECT 1 FROM planner_handoff_resources a JOIN worker_sessions s ON
        s.id=a.session_id JOIN cards c ON c.id=s.card_id JOIN tracks t ON t.id=c.track_id WHERE
        a.operation_id=?1 AND a.session_id=?2 AND s.state IN
        ('starting','running','idle','turn_pending') AND s.thread_id=a.thread_id AND (a.token_hash IS
        NULL OR (s.mcp_token_hash=a.token_hash AND EXISTS(SELECT 1 FROM card_mcp_tokens token WHERE
        token.card_id=c.id AND token.hashed_token=a.token_hash))) AND c.role='planner' AND
        c.session_id=s.id AND t.root_session_id=s.id AND t.closed_at IS NULL AND t.purpose IS NULL)
        "#)
        .bind(operation_id).bind(&facts.plan.scope.new_session_id).fetch_one(&mut **tx).await?;
    if !valid {
        return Err(conflict());
    }
    Ok(())
}
