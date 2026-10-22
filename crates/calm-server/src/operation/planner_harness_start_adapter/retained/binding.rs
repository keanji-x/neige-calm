//! The local identity has no external effect: claim, runtime fence, attribution
//! and the existing operation checkpoint commit together, or none of them do.
use super::*;
use crate::db::sqlite::session_bind_attribution_tx;
use crate::harness::snapshot::canonical_runtime_header_value;
use crate::session_projection_repo::ThreadAttribution;

impl PlannerHarnessStartAdapter {
    pub(super) async fn retained_bind_local(
        &self,
        wire: &RetainedPayload,
        output: &mut TxOutput,
        op: &Operation,
        binding: &crate::harness::profile::PlannerBinding,
        thread: uuid::Uuid,
    ) -> Result<AppServerInteractOutcome> {
        let thread = thread.to_string();
        let this = self.clone();
        let wire_tx = wire.clone();
        let op = op.clone();
        let binding = binding.clone();
        let previous = output.clone();
        let mut bound = output.clone();
        bound.data = json!({"activation":"bound", "thread_id":thread});
        let checkpoint = bound.clone();
        let thread_tx = thread.clone();
        write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move {
                let target = this.retained_target_tx(tx, &wire_tx).await?;
                if target.binding != binding || target.snapshot.header.last_thread_id.is_some() {
                    return Err(CalmError::Conflict("retained activation changed".into()));
                }
                if !crate::db::sqlite::session_claim_start_operation_tx(
                    tx,
                    &wire_tx.card_id,
                    &wire_tx.worker_session_id,
                    &op.id,
                )
                .await?
                {
                    return Err(CalmError::Conflict(
                        "retained activation already crossed".into(),
                    ));
                }
                bind_identity_tx(
                    tx,
                    &wire_tx,
                    &op,
                    &target.fence,
                    binding.provider,
                    &thread_tx,
                )
                .await?;
                checkpoint_app_server_interact_tx(
                    tx,
                    &op,
                    AppServerInteractKind::MintAndAwait {
                        thread_id: Some(thread_tx),
                    },
                    &previous,
                    &checkpoint,
                )
                .await?;
                // Last synchronous source check before the transaction returns to commit.
                this.retained_source()?.0.check_current()?;
                Ok(())
            })
        })
        .await?;
        *output = bound;
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point("retained-local-bound", &wire.card_id).await;
        Ok(AppServerInteractOutcome::MintedAndAwaited { thread_id: thread })
    }
}

/// Both preparation mechanisms bind through this one runtime/attribution owner.
/// The operation checkpoint is committed by the caller in this same transaction.
pub(super) async fn bind_identity_tx(
    tx: &mut Tx<'_>,
    wire: &RetainedPayload,
    op: &Operation,
    expected: &RuntimeExecutionFence,
    provider: AgentProvider,
    thread: &str,
) -> Result<()> {
    let current =
        conversation::current_input_carrier_tx(tx, &wire.card_id, &wire.worker_session_id).await?;
    let mut frame: CanonicalHarnessSnapshot = serde_json::from_value(current.snapshot().clone())?;
    frame.header.last_thread_id = Some(thread.to_owned());
    let header = canonical_runtime_header_value(&frame)?;
    conversation::persist_runtime_frame_tx(
        tx,
        &wire.card_id,
        &wire.worker_session_id,
        expected,
        header,
    )
    .await?;
    let owner: Option<String> =
        sqlx::query_scalar("SELECT spawn_op_id FROM worker_sessions WHERE id=?1")
            .bind(&wire.worker_session_id)
            .fetch_one(&mut **tx)
            .await?;
    if owner.as_deref() != Some(op.id.as_str()) {
        return Err(CalmError::Conflict(
            "retained activation owner changed".into(),
        ));
    }
    session_bind_attribution_tx(
        tx,
        &wire.worker_session_id,
        ThreadAttribution {
            worker_session_id: wire.worker_session_id.clone(),
            provider,
            thread_id: Some(thread.to_owned()),
            session_id: None,
            active_turn_id: None,
        },
    )
    .await?;
    Ok(())
}
