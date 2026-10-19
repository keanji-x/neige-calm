use super::*;
use crate::operation::*;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

struct BrokenOwner(u8);
#[async_trait]
impl ProviderAdapter for BrokenOwner {
    fn kind(&self) -> &'static str {
        "broken-owner"
    }
    fn phases(&self) -> &'static [PhaseTag] {
        &[PhaseTag::Parked]
    }
    fn owns_parked_resource(&self) -> Option<&'static str> {
        Some("terminal_launch")
    }
    async fn validate(&self, _: &Value) -> Result<()> {
        unreachable!()
    }
    async fn prepare_tx<'tx>(&self, _: &mut Tx<'tx>, _: &Value, _: &Operation) -> Result<TxOutput> {
        unreachable!()
    }
    async fn app_server_interact(
        &self,
        _: &mut TxOutput,
        _: &Operation,
        _: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        unreachable!()
    }
    async fn spawn_side_effect(
        &self,
        _: &TxOutput,
        _: &Operation,
        _: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        unreachable!()
    }
    async fn plan_compensation(
        &self,
        _: PhaseTag,
        _: &str,
        _: &TxOutput,
        _: &Operation,
    ) -> Result<CompensationStateVersioned> {
        unreachable!()
    }
    async fn compensate_step(
        &self,
        _: &CompensationStep,
        _: &TxOutput,
        _: &Operation,
        _: &SpawnCtx,
    ) -> Result<()> {
        unreachable!()
    }
    async fn recover_owned_parked(
        &self,
        op: &Operation,
        _: RecoveryMode,
        ctx: &SpawnCtx,
    ) -> Result<ParkedRecovery> {
        match self.0 {
            0 => Err(CalmError::Conflict("recover failed".into())),
            1 => Ok(ParkedRecovery::Fail {
                reason: "unresolved".into(),
            }),
            2 => Ok(ParkedRecovery::Complete(ParkedOutcome::Succeeded {
                result: json!({}),
            })),
            _ => {
                sqlx::query("UPDATE operations SET lease_owner='replacement',last_error='replacement error' WHERE id=?1")
                    .bind(&op.id).execute(ctx.operation_repo.sqlite_db().writer_pool()).await?;
                Err(CalmError::Conflict("stale recovery".into()))
            }
        }
    }
    async fn complete_owned_parked_tx(
        &self,
        _: &mut Tx<'_>,
        _: &Operation,
    ) -> Result<Vec<crate::event::BroadcastEnvelope>> {
        Err(CalmError::Conflict("completion conflict".into()))
    }
}

#[tokio::test]
async fn error_keeps_lease_and_writes_last_error() {
    for case in 0..4 {
        let db = Arc::new(
            crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
                .await
                .unwrap(),
        );
        let repo = Arc::new(SqlxOperationRepo::new(db.db().clone()));
        let id = repo
            .insert_operation(
                "broken-owner",
                OperationKey {
                    operation_key: new_id(),
                    idempotency_key: None,
                    payload_hash: "hash".into(),
                },
                json!({}),
            )
            .await
            .unwrap();
        sqlx::query("UPDATE operations SET phase='parked',parked_at_ms=1,parked_deadline_ms=?1,tx_output_json=?2 WHERE id=?3")
            .bind(crate::model::now_ms()+60_000).bind(serde_json::to_string(&TxOutput::new("unknown",None,json!({}))).unwrap()).bind(&id).execute(db.pool()).await.unwrap();
        let op = repo.claim_parked(&id).await.unwrap().unwrap();
        let ctx = SpawnCtx::new(
            db.clone(),
            repo.clone(),
            Arc::new(crate::state::DaemonClient::new_stub()),
            crate::terminal_renderer::TerminalRendererRegistry::new_with_repo(db),
            crate::event::EventBus::new(),
            OperationCompletionBus::new(),
        );
        assert!(
            reconcile(&BrokenOwner(case), &op, RecoveryMode::Boot, &ctx)
                .await
                .is_err()
        );
        let after = repo.get_operation(&id).await.unwrap().unwrap();
        assert_eq!(after.phase, Phase::Parked);
        assert_eq!(after.lease_until_ms, op.lease_until_ms);
        assert!(repo.claim_parked(&id).await.unwrap().is_none());
        if case == 3 {
            assert_eq!(after.lease_owner.as_deref(), Some("replacement"));
            assert_eq!(after.last_error.as_deref(), Some("replacement error"));
        } else {
            assert_eq!(after.lease_owner, op.lease_owner);
            assert!(
                after
                    .last_error
                    .unwrap()
                    .starts_with("owned execution retained:")
            );
        }
    }
}
