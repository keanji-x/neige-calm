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
            2 => Ok(ParkedRecovery::Complete(Box::new((
                ParkedOutcome::Succeeded { result: json!({}) },
                None,
            )))),
            4..=6 => Ok(ParkedRecovery::Complete(Box::new((
                ParkedOutcome::Succeeded {
                    result: json!({"owner":"done"}),
                },
                Some(owner_event()),
            )))),
            _ => {
                sqlx::query("UPDATE operations SET lease_owner='replacement',last_error='replacement error' WHERE id=?1")
                    .bind(&op.id).execute(ctx.operation_repo.sqlite_db().writer_pool()).await?;
                Err(CalmError::Conflict("stale recovery".into()))
            }
        }
    }
    async fn complete_owned_parked_tx(
        &self,
        tx: &mut Tx<'_>,
        op: &Operation,
    ) -> Result<Vec<crate::event::BroadcastEnvelope>> {
        if self.0 >= 4 {
            let phase: String = sqlx::query_scalar("SELECT phase FROM operations WHERE id=?1")
                .bind(&op.id)
                .fetch_one(&mut **tx)
                .await?;
            assert_eq!(
                phase, "succeeded",
                "owner completion runs after phase write in this transaction"
            );
            if self.0 == 5 {
                // The event append succeeds, then COMMIT fails on this deferred constraint.
                sqlx::query("INSERT INTO completion_child VALUES (1)")
                    .execute(&mut **tx)
                    .await?;
            }
            return Ok(Vec::new());
        }
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

fn owner_event() -> OwnerEvent {
    OwnerEvent {
        actor: ActorId::User,
        scope: EventScope::Area {
            area: "owner-area".into(),
        },
        event: Event::AreaDeleted {
            id: "owner-area".into(),
        },
    }
}

#[tokio::test]
async fn c3_complete_appends_owner_event_in_completion_tx() {
    use sqlx::Row;
    // Successful commit, failed commit after append, and failed append after completion.
    for case in 4..=6 {
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
            .bind(crate::model::now_ms()+60_000)
            .bind(serde_json::to_string(&TxOutput::new("unknown",None,json!({}))).unwrap())
            .bind(&id).execute(db.pool()).await.unwrap();
        sqlx::query("CREATE TABLE completion_parent (id INTEGER PRIMARY KEY)")
            .execute(db.pool())
            .await
            .unwrap();
        sqlx::query("CREATE TABLE completion_child (id INTEGER REFERENCES completion_parent(id) DEFERRABLE INITIALLY DEFERRED)")
            .execute(db.pool()).await.unwrap();
        if case == 6 {
            sqlx::query("CREATE TRIGGER refuse_owner_event BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'event append refused'); END")
                .execute(db.pool()).await.unwrap();
        }
        let op = repo.claim_parked(&id).await.unwrap().unwrap();
        let bus = crate::event::EventBus::new();
        let mut rx = bus.subscribe();
        let ctx = SpawnCtx::new(
            db.clone(),
            repo.clone(),
            Arc::new(crate::state::DaemonClient::new_stub()),
            crate::terminal_renderer::TerminalRendererRegistry::new_with_repo(db.clone()),
            bus,
            OperationCompletionBus::new(),
        );
        let result = reconcile(&BrokenOwner(case), &op, RecoveryMode::Boot, &ctx).await;
        let after = repo.get_operation(&id).await.unwrap().unwrap();
        let rows = sqlx::query("SELECT id,actor,scope_kind,scope_area,correlation,payload FROM events WHERE kind='area.deleted'")
            .fetch_all(db.pool()).await.unwrap();
        if case == 4 {
            result.unwrap();
            assert_eq!(after.phase, Phase::Succeeded);
            assert_eq!(rows.len(), 1);
            let envelope = rx.try_recv().expect("publish after successful commit");
            assert_eq!(envelope.id, rows[0].get::<i64, _>("id"));
            assert_eq!(envelope.actor, owner_event().actor);
            assert_eq!(envelope.scope, owner_event().scope);
            assert_eq!(
                serde_json::to_value(envelope.event).unwrap(),
                serde_json::to_value(owner_event().event).unwrap()
            );
            assert_eq!(
                rows[0].get::<String, _>("actor"),
                serde_json::to_string(&owner_event().actor).unwrap()
            );
            assert_eq!(rows[0].get::<String, _>("scope_kind"), "area");
            assert_eq!(rows[0].get::<String, _>("scope_area"), "owner-area");
            assert!(rows[0].get::<Option<String>, _>("correlation").is_none());
            assert!(rx.try_recv().is_err(), "exactly one event");
            // Reconciliation under a stale completed claim cannot append a duplicate.
            assert!(
                reconcile(&BrokenOwner(case), &op, RecoveryMode::Boot, &ctx)
                    .await
                    .is_err()
            );
            assert!(rx.try_recv().is_err());
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
                .fetch_one(db.pool())
                .await
                .unwrap();
            assert_eq!(count, 1);
        } else {
            assert!(result.is_err());
            assert_eq!(
                after.phase,
                Phase::Parked,
                "failed event or commit rolls completion back"
            );
            assert!(rows.is_empty(), "failed transaction leaves no event");
            assert!(rx.try_recv().is_err(), "no publication before commit");
        }
    }
}
