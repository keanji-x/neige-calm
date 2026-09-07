use super::{publication, *};
use crate::{
    db::{RouteRepo, write_in_tx_typed},
    operation::*,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

pub struct FilePublicationAdapter {
    repo: Arc<dyn RouteRepo>,
}
impl FilePublicationAdapter {
    pub fn new(repo: Arc<dyn RouteRepo>) -> Self {
        Self { repo }
    }
}
#[async_trait]
impl ProviderAdapter for FilePublicationAdapter {
    fn kind(&self) -> &'static str {
        OPERATION_KIND
    }
    fn phases(&self) -> &'static [PhaseTag] {
        &[PhaseTag::Pending, PhaseTag::TxCommitted]
    }
    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: PublicationPayload = serde_json::from_value(input.clone())?;
        if payload.task_id.is_empty()
            || payload.track_id.is_empty()
            || payload.source_operation_id.is_empty()
        {
            return Err(conflict("invalid file publication identity"));
        }
        Ok(())
    }
    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        op: &Operation,
    ) -> Result<TxOutput> {
        self.validate(input).await?;
        let (task, _) = publication::authorize_tx(tx, op).await?;
        Ok(TxOutput::new(
            "track",
            Some(task.track_id),
            json!({"publication_operation_id":op.id}),
        ))
    }
    async fn app_server_interact(
        &self,
        _: &mut TxOutput,
        _: &Operation,
        _: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        Ok(AppServerInteractOutcome::NotApplicable)
    }
    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        _: &TxOutput,
        _: &Operation,
    ) -> Result<CompensationStateVersioned> {
        // Sealed evidence is retained; failed JSON is never recaptured or removed.
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.into(),
            steps: vec![],
        })
    }
    async fn compensate_step(
        &self,
        _: &CompensationStep,
        _: &TxOutput,
        _: &Operation,
        _: &SpawnCtx,
    ) -> Result<()> {
        Err(conflict("publication has no compensation steps"))
    }
    async fn spawn_side_effect(
        &self,
        _: &TxOutput,
        op: &Operation,
        _: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        let owned = op.clone();
        let (task, source) = write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move {
                require_owner_tx(tx, &owned).await?;
                publication::authorize_tx(tx, &owned).await
            })
        })
        .await?;
        let owned = op.clone();
        let receipt =
            tokio::task::spawn_blocking(move || publication::capture(owned, task, source))
                .await
                .map_err(|_| conflict("file publication capture interrupted"))??;
        let owned = op.clone();
        write_in_tx_typed(self.repo.as_ref(), move |tx| Box::pin(async move {
            require_owner_tx(tx, &owned).await?;
            let (task, _) = publication::authorize_tx(tx, &owned).await?;
            if selection(&task)?.as_ref() != Some(&receipt.contract) {
                return Err(conflict("file publication contract changed during capture"));
            }
            let FileDelivery::Producer { slot, .. } = &receipt.contract else { unreachable!() };
            let json = serde_json::to_string(&receipt)?;
            let existing: Option<String> = sqlx::query_scalar("SELECT receipt_json FROM task_file_publications WHERE operation_id=?1")
                .bind(&owned.id).fetch_optional(&mut **tx).await?;
            if let Some(existing) = existing {
                if existing != json { return Err(conflict("file publication replay changed identity")); }
            } else {
                sqlx::query("INSERT INTO task_file_publications(operation_id,track_id,producer_attempt_id,source_operation_id,slot,receipt_json) VALUES(?1,?2,?3,?4,?5,?6)")
                    .bind(&owned.id).bind(&task.track_id).bind(&task.id).bind(&receipt.source.source_operation_id).bind(slot).bind(json)
                    .execute(&mut **tx).await?;
            }
            Ok(())
        })).await?;
        Ok(SpawnOutcome::Ready(SpawnHandle::NoOp))
    }
}

async fn require_owner_tx(tx: &mut Tx<'_>, op: &Operation) -> Result<()> {
    let owner = op
        .lease_owner
        .as_deref()
        .ok_or_else(|| conflict("publication requires owned lease"))?;
    let owned: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM operations WHERE id=?1 AND kind='task-file-publication' AND phase='tx_committed' AND lease_owner=?2)")
        .bind(&op.id).bind(owner).fetch_one(&mut **tx).await?;
    if !owned {
        return Err(conflict("publication operation lease or phase changed"));
    }
    Ok(())
}
