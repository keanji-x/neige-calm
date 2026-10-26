//! Fixture for rows prepared by the pre-Pipe kernel. Never used in production registration.
use super::*;
use crate::db::sqlite::card_update_tx;

#[derive(Clone)]
pub struct LegacyPreparedWorker(pub TerminalWorkerAdapter);
impl std::ops::Deref for LegacyPreparedWorker {
    type Target = TerminalWorkerAdapter;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
#[async_trait]
impl ProviderAdapter for LegacyPreparedWorker {
    fn kind(&self) -> &'static str {
        self.0.kind()
    }
    fn phases(&self) -> &'static [PhaseTag] {
        self.0.phases()
    }
    async fn validate(&self, input: &Value) -> Result<()> {
        self.0.validate(input).await
    }
    async fn preparation_track(&self, input: &Value, ctx: &SpawnCtx) -> Result<Option<String>> {
        self.0.preparation_track(input, ctx).await
    }
    async fn prepare_external(
        &self,
        input: &Value,
        _op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<Option<Box<dyn crate::operation::PreparedTx>>> {
        let payload: TerminalWorkerOperationPayload = serde_json::from_value(input.clone())?;
        target::prepare(ctx, &payload.track_id, explicit_terminal_cwd(payload.cwd)).await?;
        Ok(None)
    }
    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        op: &Operation,
    ) -> Result<TxOutput> {
        let payload: TerminalWorkerOperationPayload = serde_json::from_value(input.clone())?;
        crate::workspace_service::reclaim::operation::terminal_admission_tx(tx, &payload.track_id)
            .await?;
        crate::operation::refuse_if_context_stale(tx, Some(&payload.idempotency_key)).await?;
        // Derived from the `tasks` row inside this tx (never carried on the payload, which would move `stable_payload_hash`); `None` leaves the card untitled.
        let card_title =
            crate::operation::task_key_for_card_title(tx, &payload.idempotency_key).await;
        let card_id = new_id();
        let runtime_id = new_id();
        let track_id = TrackId::from(payload.track_id.clone());
        let cwd = terminal_cwd_or_track_workspace(
            tx,
            &payload.track_id,
            explicit_terminal_cwd(payload.cwd.clone()),
        )
        .await?;
        let mut env = terminal_worker_env(self.repo.as_ref()).await?;
        // A Planner-dispatched task may run `claude`: auto-memory off, as for every kernel-launched Claude (#1814).
        let (key, value) = crate::claude_code_env::DISABLE_AUTO_MEMORY;
        env[key] = Value::String(value.into());
        let scope = card_scope(
            self.repo.as_ref(),
            CardId::from(card_id.clone()),
            track_id.clone(),
        )
        .await?;
        let (mut card, term) = card_with_terminal_create_tx(
            tx,
            card_id,
            &runtime_id,
            Some(op.id.as_str()),
            track_id,
            None,
            None,
            payload.cmd.clone(),
            cwd.clone(),
            env.clone(),
            CardRole::Worker,
            true,
            &self.card_role_cache,
            RequestTheme::default_dark(),
            false,
            calm_types::card_presentation::CardCreationSource::Task,
        )
        .await?;

        if let Some(existing_map) = card.payload.as_object() {
            let mut merged = existing_map.clone();
            merged.insert(
                "idempotency_key".into(),
                Value::String(payload.idempotency_key.clone()),
            );
            merged.insert("role_request".into(), Value::String("terminal".into()));
            merged.insert("cmd".into(), Value::String(payload.cmd.clone()));
            merged.insert("cwd".into(), Value::String(cwd.clone()));
            card = card_update_tx(
                tx,
                card.id.as_ref(),
                crate::model::CardPatch {
                    title: card_title,
                    kind: None,
                    sort: None,
                    payload: Some(Value::Object(merged)),
                    deletable: None,
                },
            )
            .await?;
        }

        let mut output = TxOutput::new(
            "card",
            Some(card.id.to_string()),
            serde_json::to_value(&card)?,
        );
        output.data = json!({
            "card_id": card.id,
            "runtime_id": runtime_id,
            "track_id": card.track_id,
            "terminal_id": term.id,
            "cmd": payload.cmd,
            "cwd": cwd,
            "env": env,
            "scope": scope,
            "terminal_launch": crate::operation::terminal_launch::fresh_state(),
        });
        Ok(output)
    }

    async fn app_server_interact(
        &self,
        output: &mut TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        self.0.app_server_interact(output, op, ctx).await
    }
    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        self.0.spawn_side_effect(output, op, ctx).await
    }
    async fn plan_compensation(
        &self,
        phase: PhaseTag,
        reason: &str,
        output: &TxOutput,
        op: &Operation,
    ) -> Result<CompensationStateVersioned> {
        self.0.plan_compensation(phase, reason, output, op).await
    }
    async fn compensate_step(
        &self,
        step: &CompensationStep,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<()> {
        self.0.compensate_step(step, output, op, ctx).await
    }
}
