use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::card_role_cache::CardRoleCache;
use crate::db::sqlite::{card_update_tx, runtime_bind_attribution_tx, runtime_start_tx};
use crate::db::{Repo, write_in_tx_typed, write_with_event_typed};
use crate::error::{CalmError, Result};
use crate::event::Event;
use crate::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessRegistry, HarnessSnapshot, SpecHarness,
    SpecHarnessParams, initial_snapshot_with_goal,
};
use crate::ids::{ActorId, CardId, WaveId};
use crate::model::{Card, CardPatch, CardRole, new_id, now_ms};
use crate::routes::cards::card_scope;
use crate::runtime_repo::{AgentProvider, RunStatus, RuntimeInit, RuntimeKind, ThreadAttribution};
use crate::shared_codex_appserver::{SharedCodexAppServer, SharedThreadStartParams};
use crate::state::WriteContext;
use crate::wave_cove_cache::WaveCoveCache;

use super::{
    AppServerInteractKind, AppServerInteractOutcome, CompensationStateVersioned, CompensationStep,
    Operation, PhaseTag, ProviderAdapter, SpawnCtx, SpawnHandle, Tx, TxOutput,
    checkpoint_app_server_interact_tx,
};

const START_PHASES: &[PhaseTag] = &[
    PhaseTag::Pending,
    PhaseTag::TxCommitted,
    PhaseTag::AppServerInteract,
    PhaseTag::SpawnStarted,
    PhaseTag::SpawnSucceeded,
    PhaseTag::Succeeded,
];

#[derive(Clone)]
pub struct SpecHarnessStartAdapter {
    repo: Arc<dyn Repo>,
    daemon: Arc<SharedCodexAppServer>,
    harness_registry: HarnessRegistry,
    card_role_cache: CardRoleCache,
    wave_cove_cache: WaveCoveCache,
}

impl SpecHarnessStartAdapter {
    pub fn new(
        repo: Arc<dyn Repo>,
        daemon: Arc<SharedCodexAppServer>,
        harness_registry: HarnessRegistry,
        card_role_cache: CardRoleCache,
        wave_cove_cache: WaveCoveCache,
    ) -> Self {
        Self {
            repo,
            daemon,
            harness_registry,
            card_role_cache,
            wave_cove_cache,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpecHarnessStartOperationPayload {
    pub actor: ActorId,
    pub wave_id: String,
    pub spec_card_id: CardId,
    #[serde(default)]
    pub report_card_id: Option<String>,
    #[serde(default)]
    pub sort: Option<f64>,
    pub cwd: String,
    #[serde(default)]
    pub goal: Option<String>,
}

#[async_trait]
impl ProviderAdapter for SpecHarnessStartAdapter {
    fn kind(&self) -> &'static str {
        "spec-harness-start"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        START_PHASES
    }

    fn app_server_interact_kind(
        &self,
        _output: &TxOutput,
        _op: &Operation,
    ) -> Result<AppServerInteractKind> {
        Ok(AppServerInteractKind::MintAndAwait { thread_id: None })
    }

    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: SpecHarnessStartOperationPayload = serde_json::from_value(input.clone())?;
        if self.repo.wave_get(&payload.wave_id).await?.is_none() {
            return Err(CalmError::NotFound(format!("wave {}", payload.wave_id)));
        }
        let Some(card) = self.repo.card_get(payload.spec_card_id.as_str()).await? else {
            return Err(CalmError::NotFound(format!(
                "card {}",
                payload.spec_card_id
            )));
        };
        if card.wave_id.as_str() != payload.wave_id {
            return Err(CalmError::BadRequest(format!(
                "spec card {} belongs to wave {}, not {}",
                card.id, card.wave_id, payload.wave_id
            )));
        }
        if self.card_role_cache.get(&card.id) != Some(CardRole::Spec) {
            return Err(CalmError::BadRequest(format!(
                "card {} is not a spec card",
                card.id
            )));
        }
        if !self.daemon.is_running() {
            return Err(CalmError::Internal(
                "shared codex app-server is not running".into(),
            ));
        }
        Ok(())
    }

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        _op: &Operation,
    ) -> Result<TxOutput> {
        let payload: SpecHarnessStartOperationPayload = serde_json::from_value(input.clone())?;
        let card_id = payload.spec_card_id;
        let wave_id = payload.wave_id;
        let report_card_id = payload.report_card_id;
        let snapshot = initial_snapshot_with_goal(payload.goal.clone());
        let card = sqlx::query_as::<_, Card>(
            r#"SELECT id, wave_id, kind, sort, payload, deletable, created_at, updated_at
                 FROM cards
                WHERE id = ?1
                  AND wave_id = ?2"#,
        )
        .bind(card_id.as_str())
        .bind(&wave_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;

        let runtime_id = new_id();
        let runtime = runtime_start_tx(
            tx,
            RuntimeInit {
                id: runtime_id.clone(),
                card_id: card.id.to_string(),
                kind: RuntimeKind::SharedSpec,
                agent_provider: Some(AgentProvider::Codex),
                status: RunStatus::Starting,
                terminal_run_id: None,
                thread_id: None,
                session_id: None,
                active_turn_id: None,
                handle_state_json: Some(serde_json::to_value(&snapshot)?),
                lease_owner: None,
                lease_until_ms: None,
                now_ms: now_ms(),
            },
        )
        .await?;

        let mut output = TxOutput::new(
            "card",
            Some(card.id.to_string()),
            serde_json::to_value(&card)?,
        );
        output.data = json!({
            "card_id": card.id,
            "wave_id": wave_id,
            "runtime_id": runtime.id,
            "cwd": payload.cwd,
            "goal": payload.goal,
            "report_card_id": report_card_id,
            "snapshot": snapshot,
        });
        Ok(output)
    }

    async fn app_server_interact(
        &self,
        output: &mut TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        let payload: SpecHarnessStartOperationPayload = serde_json::from_value(op.payload.clone())?;
        let card_id = output_string(output, "card_id")?;
        let wave_id = output_string(output, "wave_id")?;
        let runtime_id = output_string(output, "runtime_id")?;
        let cwd = output_string(output, "cwd")?;
        if let Some(existing) = output_existing_thread_id(output)? {
            return Ok(AppServerInteractOutcome::MintedAndAwaited {
                thread_id: existing,
            });
        }
        let developer_instructions = crate::spec_card::render_system_prompt(
            crate::spec_card::SeededCardRole::Spec.prompt_template(),
            &wave_id,
        );
        let thread_id = self
            .daemon
            .thread_start_for_card(
                &card_id,
                CardRole::Spec,
                Some(&wave_id),
                SharedThreadStartParams {
                    cwd,
                    approval_policy: "never".into(),
                    sandbox_mode: "workspace-write".into(),
                    developer_instructions: Some(developer_instructions),
                },
            )
            .await?;
        set_output_data(output, "codex_thread_id", json!(thread_id.clone()))?;
        let mut snapshot = output_snapshot(output)?;
        snapshot.phase = HarnessPhaseTag::Idle;
        snapshot.last_thread_id = Some(thread_id.clone());
        set_output_data(output, "snapshot", serde_json::to_value(&snapshot)?)?;
        let mut card: crate::model::Card = serde_json::from_value(output.result.clone())?;
        let mut card_payload = card.payload.clone();
        let Some(map) = card_payload.as_object_mut() else {
            return Err(CalmError::Internal(format!(
                "spec harness card {card_id} payload is not a JSON object"
            )));
        };
        map.insert("codex_thread_id".into(), Value::String(thread_id.clone()));
        map.insert(
            "appserver_sock".into(),
            Value::String(self.daemon.remote_uri()),
        );
        map.remove("appserver_pgid");
        map.remove("appserver_start_time");
        map.remove("appserver_boot_id");
        map.remove("appserver_needs_initial_prompt");

        let scope = card_scope(ctx.repo.as_ref(), card.id.clone(), card.wave_id.clone()).await?;
        let write = WriteContext::new(self.card_role_cache.clone(), self.wave_cove_cache.clone());
        let op_clone = op.clone();
        let output_clone = output.clone();
        let thread_for_tx = thread_id.clone();
        let (updated_card, _id) = write_with_event_typed(
            ctx.repo.as_ref(),
            payload.actor,
            scope,
            None,
            &ctx.events,
            &write,
            move |tx| {
                Box::pin(async move {
                    runtime_bind_attribution_tx(
                        tx,
                        &runtime_id,
                        ThreadAttribution {
                            runtime_id: runtime_id.clone(),
                            provider: AgentProvider::Codex,
                            thread_id: Some(thread_for_tx.clone()),
                            session_id: None,
                            active_turn_id: None,
                        },
                    )
                    .await?;
                    crate::db::sqlite::runtime_set_handle_state_tx(
                        tx,
                        &runtime_id,
                        Some(serde_json::to_value(&snapshot)?),
                    )
                    .await?;
                    let card = card_update_tx(
                        tx,
                        &card_id,
                        CardPatch {
                            kind: None,
                            sort: None,
                            payload: Some(card_payload),
                            deletable: None,
                        },
                    )
                    .await?;
                    let mut checkpoint_output = output_clone;
                    checkpoint_output.result = serde_json::to_value(&card)?;
                    checkpoint_output.target_id = Some(card.id.to_string());
                    checkpoint_app_server_interact_tx(
                        tx,
                        &op_clone,
                        AppServerInteractKind::MintAndAwait {
                            thread_id: Some(thread_for_tx),
                        },
                        &checkpoint_output,
                    )
                    .await?;
                    Ok((card.clone(), Event::CardUpdated(card)))
                })
            },
        )
        .await?;
        card = updated_card;
        output.result = serde_json::to_value(&card)?;
        output.target_id = Some(card.id.to_string());

        Ok(AppServerInteractOutcome::MintedAndAwaited { thread_id })
    }

    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<SpawnHandle> {
        let runtime_id = output_string(output, "runtime_id")?;
        let card_id = output_string(output, "card_id")?;
        let wave_id = output_string(output, "wave_id")?;
        let thread_id = output_optional_string(output, "codex_thread_id")?;
        let snapshot = output_snapshot(output)?;
        if let Some(existing) = self.harness_registry.remove(&runtime_id) {
            existing.shutdown().await?;
        }
        let handle = SpecHarness::run(SpecHarnessParams {
            runtime_id: runtime_id.clone(),
            wave_id: WaveId::from(wave_id),
            card_id: CardId::from(card_id),
            thread_id,
            repo: self.repo.clone(),
            daemon: self.daemon.clone(),
            config: HarnessConfig::default(),
            snapshot,
        });
        handle.persist_snapshot().await?;
        self.harness_registry
            .insert(runtime_id.clone(), handle.clone());
        Ok(SpawnHandle::Harness { runtime_id })
    }

    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        output: &TxOutput,
        _op: &Operation,
    ) -> Result<CompensationStateVersioned> {
        let card_id = output_string(output, "card_id")?;
        let runtime_id = output_string(output, "runtime_id")?;
        let mut steps = Vec::new();
        if matches!(
            from_phase,
            PhaseTag::SpawnStarted | PhaseTag::SpawnSucceeded
        ) {
            steps.push(step(
                "abort_harness_task",
                json!({ "runtime_id": runtime_id }),
            ));
        }
        if matches!(
            from_phase,
            PhaseTag::AppServerInteract | PhaseTag::SpawnStarted | PhaseTag::SpawnSucceeded
        ) {
            steps.push(step(
                "interrupt_thread",
                json!({
                    "card_id": card_id,
                    "thread_id": output_optional_string(output, "codex_thread_id")?,
                }),
            ));
        }
        steps.push(step("delete_runtime", json!({ "runtime_id": runtime_id })));
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.to_string(),
            steps,
        })
    }

    async fn compensate_step(
        &self,
        step: &CompensationStep,
        _output: &TxOutput,
        _op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<()> {
        if step.completed {
            return Ok(());
        }
        match step.op.as_str() {
            "abort_harness_task" => {
                let runtime_id = step_arg_string(step, "runtime_id")?;
                if let Some(handle) = self.harness_registry.remove(&runtime_id) {
                    handle.shutdown().await?;
                }
                Ok(())
            }
            "interrupt_thread" => {
                if let Some(thread_id) = step.args.get("thread_id").and_then(Value::as_str)
                    && let Err(e) = self.daemon.interrupt_active_turn(thread_id).await
                {
                    tracing::warn!(thread_id, error = %e, "spec harness compensation interrupt failed");
                }
                let card_id = step_arg_string(step, "card_id")?;
                ctx.repo.card_codex_thread_delete_by_card(&card_id).await?;
                clear_card_runtime_fields(ctx, &card_id).await?;
                Ok(())
            }
            "delete_runtime" => {
                let runtime_id = step_arg_string(step, "runtime_id")?;
                write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
                    Box::pin(async move {
                        sqlx::query("DELETE FROM runtimes WHERE id = ?1")
                            .bind(runtime_id)
                            .execute(&mut **tx)
                            .await?;
                        Ok(())
                    })
                })
                .await
            }
            other => Err(CalmError::Internal(format!(
                "unknown spec harness start compensation op {other}"
            ))),
        }
    }
}

fn step(op: &str, args: Value) -> CompensationStep {
    CompensationStep {
        op: op.into(),
        args,
        completed: false,
        attempts: 0,
        last_error: None,
    }
}

fn output_snapshot(output: &TxOutput) -> Result<HarnessSnapshot> {
    let value = output
        .data
        .get("snapshot")
        .cloned()
        .ok_or_else(|| CalmError::Internal("spec harness output missing snapshot".into()))?;
    Ok(serde_json::from_value(value)?)
}

fn output_string(output: &TxOutput, key: &str) -> Result<String> {
    output
        .data
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| CalmError::Internal(format!("spec harness tx_output missing {key}")))
}

fn output_optional_string(output: &TxOutput, key: &str) -> Result<Option<String>> {
    match output.data.get(key) {
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(CalmError::Internal(format!(
            "spec harness tx_output {key} must be string or null"
        ))),
    }
}

fn output_existing_thread_id(output: &TxOutput) -> Result<Option<String>> {
    Ok(output_optional_string(output, "codex_thread_id")?.filter(|id| !id.trim().is_empty()))
}

fn set_output_data(output: &mut TxOutput, key: &str, value: Value) -> Result<()> {
    let obj = output.data.as_object_mut().ok_or_else(|| {
        CalmError::Internal("spec harness tx_output data is not an object".into())
    })?;
    obj.insert(key.to_string(), value);
    Ok(())
}

async fn clear_card_runtime_fields(ctx: &SpawnCtx, card_id: &str) -> Result<()> {
    let card_id = card_id.to_string();
    write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let row: Option<(String,)> = sqlx::query_as("SELECT payload FROM cards WHERE id = ?1")
                .bind(&card_id)
                .fetch_optional(&mut **tx)
                .await?;
            let Some((payload_text,)) = row else {
                return Ok(());
            };
            let mut payload: Value = serde_json::from_str(&payload_text).map_err(|e| {
                CalmError::Internal(format!("card {card_id} payload is not valid JSON: {e}"))
            })?;
            let Some(map) = payload.as_object_mut() else {
                return Ok(());
            };
            map.remove("codex_thread_id");
            map.remove("appserver_sock");
            map.remove("appserver_pgid");
            map.remove("appserver_start_time");
            map.remove("appserver_boot_id");
            let _card = card_update_tx(
                tx,
                &card_id,
                CardPatch {
                    kind: None,
                    sort: None,
                    payload: Some(payload),
                    deletable: None,
                },
            )
            .await?;
            Ok(())
        })
    })
    .await
}

fn step_arg_string(step: &CompensationStep, key: &str) -> Result<String> {
    step.args
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            CalmError::Internal(format!(
                "spec harness compensation step {} missing {key}",
                step.op
            ))
        })
}
