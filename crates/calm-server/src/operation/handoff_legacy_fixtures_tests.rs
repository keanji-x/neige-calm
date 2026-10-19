//! Test-only frozen legacy bind seam from f1f9e5b7a:handoff_activation_sql::bind_resources.
//! Only the removed facts wrapper is adapted to its retained current_facts owner.
//! The pre-0172 pending transition trigger is installed from the frozen migration,
//! and restored afterwards. Native IDs/tokens come from the actual start adapter.
//! This is not an old executable or a successful receipt fixture.
use super::handoff_activation::ActivationResult;
use super::handoff_cutover::conflict;
use super::{Operation, OperationRepo, TxOutput};
use crate::error::Result;
use crate::harness::HarnessPhaseTag;
use serde_json::json;

pub(crate) async fn proven(
    repo: &dyn crate::db::Repo,
    cutover: &str,
    start: &super::planner_harness_start_adapter::PlannerHarnessStartAdapter,
) -> Result<String> {
    let id = super::handoff_activation::record_legacy_intent_for_test(repo, cutover).await?;
    let pool = repo.sqlite_db().ok_or_else(conflict)?;
    sqlx::query("UPDATE operations SET phase='app_server_interact',phase_detail_json='{\"kind\":\"mint_and_await\",\"thread_id\":null}' WHERE id=?1").bind(&id).execute(pool.writer_pool()).await?;
    let operations = super::SqlxOperationRepo::new(pool.clone());
    let op = operations
        .claim_drive_target(&id)
        .await?
        .ok_or_else(conflict)?;
    let mut output = op.tx_output.clone().ok_or_else(conflict)?;
    let plan: super::handoff_cutover::ValidatedCutoverPlan = serde_json::from_str(
        &sqlx::query_scalar::<_, String>("SELECT payload_json FROM operations WHERE id=?1")
            .bind(cutover)
            .fetch_one(pool.read_pool())
            .await?,
    )?;
    let (thread, token) = start
        .mint_harness_resources(
            super::planner_harness_start_adapter::HarnessProfile::Planner,
            &plan.configuration.conversation.provider,
            plan.scope.target_card_id.as_str(),
            plan.scope.track_id.as_str(),
            output.output_string("cwd", "legacy fixture")?,
            true,
        )
        .await?;
    let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
    let trigger: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE name='pending_activation_no_generic_start'",
    )
    .fetch_one(&mut **tx)
    .await?;
    sqlx::raw_sql("DROP TRIGGER pending_activation_no_generic_start")
        .execute(&mut **tx)
        .await?;
    let frozen = include_str!("../../../calm-truth/migrations/0174_planner_handoff_activation.sql");
    let begin = frozen
        .find("CREATE TRIGGER pending_activation_no_generic_start")
        .ok_or_else(conflict)?;
    let end = frozen[begin..].find("END;").ok_or_else(conflict)? + begin + 4;
    sqlx::raw_sql(&frozen[begin..end])
        .execute(&mut **tx)
        .await?;
    bind_resources(&mut tx, &op, &mut output, &thread, token.as_deref(), start).await?;
    sqlx::raw_sql("DROP TRIGGER pending_activation_no_generic_start")
        .execute(&mut **tx)
        .await?;
    sqlx::raw_sql(&trigger).execute(&mut **tx).await?;
    tx.commit().await?;
    sqlx::query("UPDATE operations SET phase='stuck',phase_detail_json='{\"reason\":\"legacy checkpoint seam\",\"since\":0,\"from_phase\":\"app_server_interact\"}',lease_owner=NULL,lease_until_ms=NULL WHERE id=?1").bind(&id).execute(pool.writer_pool()).await?;
    Ok(id)
}

pub(super) async fn bind_resources(
    tx: &mut super::Tx<'_>,
    op: &Operation,
    output: &mut TxOutput,
    thread_id: &str,
    token_hash: Option<&str>,
    start: &super::planner_harness_start_adapter::PlannerHarnessStartAdapter,
) -> Result<()> {
    let cutover = op.payload["cutover_operation_id"]
        .as_str()
        .ok_or_else(conflict)?;
    let plan = serde_json::from_str(
        &sqlx::query_scalar::<_, String>("SELECT payload_json FROM operations WHERE id=?1")
            .bind(cutover)
            .fetch_one(&mut **tx)
            .await?,
    )?;
    let facts =
        super::handoff_activation_sql::current_facts(tx, plan, cutover.into(), Some(&op.id), false)
            .await?;
    facts.validate_output(output)?;
    let scope = &facts.plan.scope;
    let mut snapshot = facts.snapshot.clone();
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(thread_id.into());
    let mut next = output.clone();
    next.result = serde_json::to_value(ActivationResult::ResourcesCheckpointed {
        thread_id: thread_id.into(),
    })?;
    next.data["codex_thread_id"] = json!(thread_id);
    next.data["snapshot"] = serde_json::to_value(&snapshot)?;
    sqlx::query(concat!("UPDATE planner_handoff_activations SET thread_id=?1,token_hash=?2 WHERE activation_operation_id=?3 ","AND thread_id IS NULL"))
        .bind(thread_id).bind(token_hash).bind(&op.id).execute(&mut **tx).await?;
    // Checkpoint is written before the narrow transition trigger consumes its proof, all in
    // one transaction. Mere presence of a claim never authorizes activation.
    super::checkpoint_app_server_interact_tx(
        tx,
        op,
        super::AppServerInteractKind::MintAndAwait {
            thread_id: Some(thread_id.into()),
        },
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

    *output = next;
    Ok(())
}

/// Frozen f1f9e5b7a activation finalization seam: install and exact SQL validation
/// precede the real checkpoint and the old unchecked reservation UPDATE.
pub(crate) async fn finish(
    repo: std::sync::Arc<dyn crate::db::Repo>,
    child: &str,
    start: &super::planner_harness_start_adapter::PlannerHarnessStartAdapter,
) -> Result<()> {
    let pool = repo.sqlite_db().ok_or_else(conflict)?;
    sqlx::query("UPDATE operations SET phase='app_server_interact',phase_detail_json='{\"kind\":\"mint_and_await\",\"thread_id\":null}' WHERE id=?1").bind(child).execute(pool.writer_pool()).await?;
    let operations = std::sync::Arc::new(super::SqlxOperationRepo::new(pool.clone()));
    let op = operations
        .claim_drive_target(child)
        .await?
        .ok_or_else(conflict)?;
    let output = op.tx_output.clone().ok_or_else(conflict)?;
    let ctx = super::SpawnCtx::new(
        repo,
        operations,
        std::sync::Arc::new(crate::state::DaemonClient::new_stub()),
        crate::terminal_renderer::TerminalRendererRegistry::new(),
        crate::event::EventBus::new(),
        super::OperationCompletionBus::new(),
    );
    start
        .install_harness(
            &output,
            &ctx,
            super::planner_harness_start_adapter::HarnessInstallation::ActivationRecovery,
        )
        .await?;
    let cutover = op.payload["cutover_operation_id"]
        .as_str()
        .ok_or_else(conflict)?;
    let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
    let plan = serde_json::from_str(
        &sqlx::query_scalar::<_, String>("SELECT payload_json FROM operations WHERE id=?1")
            .bind(cutover)
            .fetch_one(&mut **tx)
            .await?,
    )?;
    let facts = super::handoff_activation_sql::current_facts(
        &mut tx,
        plan,
        cutover.into(),
        Some(child),
        false,
    )
    .await?;
    facts.validate_output(&output)?;
    let valid: bool = sqlx::query_scalar(concat!(
        "SELECT EXISTS(SELECT 1 FROM planner_handoff_activations a JOIN worker_sessions s ON ",
        "s.id=a.session_id JOIN cards c ON c.id=s.card_id JOIN tracks t ON t.id=c.track_id ",
        "WHERE a.activation_operation_id=?1 AND a.cutover_operation_id=?2 AND ",
        "a.session_id=?3 AND s.state IN ('starting','running','idle','turn_pending') AND ",
        "s.thread_id=a.thread_id AND (a.token_hash IS NULL OR (s.mcp_token_hash=a.token_hash ",
        "AND EXISTS(SELECT 1 FROM card_mcp_tokens token WHERE token.card_id=c.id AND ",
        "token.hashed_token=a.token_hash))) AND c.role='planner' AND c.session_id=s.id AND ",
        "t.root_session_id=s.id AND t.closed_at IS NULL AND t.purpose IS NULL)",
    ))
    .bind(child)
    .bind(cutover)
    .bind(&facts.plan.scope.new_session_id)
    .fetch_one(&mut **tx)
    .await?;
    if !valid {
        return Err(conflict());
    }
    let mut next = output.clone();
    next.result = serde_json::to_value(ActivationResult::Activated {
        session_id: facts.plan.scope.new_session_id,
    })?;
    super::checkpoint_app_server_interact_tx(
        &mut tx,
        &op,
        super::AppServerInteractKind::MintAndAwait {
            thread_id: output.output_optional_string("codex_thread_id", "activation")?,
        },
        &output,
        &next,
    )
    .await?;
    sqlx::query(concat!(
        "UPDATE planner_handoff_coordinators SET active=0 WHERE active=1 AND operation_id IN ",
        "(SELECT id FROM operations WHERE kind='handoff-coordinator' AND ",
        "json_extract(payload_json,'$.plan_id')=?1 AND json_extract(tx_output_json,",
        "'$.result.committed.operation_id')=?2)",
    ))
    .bind(&facts.plan.scope.plan_id)
    .bind(cutover)
    .execute(&mut **tx)
    .await?;
    tx.commit().await?;
    sqlx::query(
        "UPDATE operations SET phase='succeeded',lease_owner=NULL,lease_until_ms=NULL WHERE id=?1",
    )
    .bind(child)
    .execute(pool.writer_pool())
    .await?;
    Ok(())
}
