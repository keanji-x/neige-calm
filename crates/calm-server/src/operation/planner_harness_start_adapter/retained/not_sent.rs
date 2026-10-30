//! Preserve transport BeforeSend evidence independently of fallible claim cleanup.
use super::*;

impl PlannerHarnessStartAdapter {
    // `prepared` already means no native crossing. Claim plus crossing_unknown
    // commits atomically, so a claimed worker can return to prepared only with
    // actual BeforeSend evidence. Persist that fact before attempting cleanup.
    pub(super) async fn retained_not_sent(
        &self,
        wire: &RetainedPayload,
        op: &Operation,
        output: &mut TxOutput,
    ) -> Result<()> {
        let previous = output.clone();
        let mut prepared = output.clone();
        prepared.data = json!({"activation":"prepared"});
        let checkpoint = prepared.clone();
        let operation = op.clone();
        write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move {
                checkpoint_app_server_interact_tx(
                    tx,
                    &operation,
                    AppServerInteractKind::MintAndAwait { thread_id: None },
                    &previous,
                    &checkpoint,
                )
                .await
            })
        })
        .await?;
        *output = prepared;
        let wire = wire.clone();
        let op = op.clone();
        write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move {
                crate::operation::repo_sqlite::require_live_claim_tx(tx, &op).await?;
                release_tx(tx, &wire, &op.id).await
            })
        })
        .await
    }

    // Called only by the current retained owner under the card start fence.
    // A terminal operation cannot acquire a new drive lease. Read its original
    // payload and last committed checkpoint in the same transaction as cleanup;
    // Failed/Stuck alone never supplies evidence of non-delivery.
    pub(super) async fn retained_recover_not_sent(
        &self,
        wire: &RetainedPayload,
        operation: &str,
    ) -> Result<()> {
        let this = self.clone();
        let wire = wire.clone();
        let operation = operation.to_owned();
        write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move {
                let current = this.retained_admit_tx(tx, &wire).await?;
                if current.activation_operation.as_deref() != Some(operation.as_str())
                    || current.snapshot.header.last_thread_id.is_some()
                {
                    return Err(unresolved());
                }
                let row = sqlx::query(
                    "SELECT * FROM operations WHERE id=?1 AND kind=?2
                    AND phase IN ('failed','stuck') AND completed_at_ms IS NOT NULL
                    AND lease_owner IS NULL AND lease_until_ms IS NULL",
                )
                .bind(&operation)
                .bind(this.kind())
                .fetch_optional(&mut **tx)
                .await?
                .ok_or_else(unresolved)?;
                let original = crate::operation::repo_sqlite::operation_from_row(&row)?;
                let original_wire =
                    RetainedPayload::decode(&original.payload)?.ok_or_else(unresolved)?;
                let output = crate::operation::required_output(&original)?;
                if original_wire.card_id != wire.card_id
                    || original_wire.worker_session_id != wire.worker_session_id
                    || output.data.get("activation").and_then(Value::as_str) != Some("prepared")
                {
                    return Err(unresolved());
                }
                release_tx(tx, &wire, &operation).await
            })
        })
        .await
    }
}

async fn release_tx(tx: &mut Tx<'_>, wire: &RetainedPayload, operation: &str) -> Result<()> {
    if !crate::db::sqlite::session_release_unstarted_operation_tx(
        tx,
        &wire.card_id,
        &wire.worker_session_id,
        operation,
    )
    .await?
    {
        return Err(CalmError::Conflict(
            "retained activation owner changed".into(),
        ));
    }
    Ok(())
}

fn unresolved() -> CalmError {
    CalmError::Conflict("retained native activation is unresolved".into())
}
