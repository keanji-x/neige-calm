use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::card_role_cache::CardRoleCache;
use crate::db::sqlite::{
    card_mcp_token_set_tx, card_update_tx, harness_items_delete_by_card_tx,
    runtime_bind_attribution_tx, runtime_get_active_for_card_tx, runtime_start_tx,
    runtime_supersede_tx,
};
use crate::db::{Repo, write_in_tx_typed, write_with_event_typed};
use crate::error::{CalmError, Result};
use crate::event::Event;
use crate::harness::{
    HARNESS_MODE, HarnessConfig, HarnessPhaseTag, HarnessRegistry, HarnessSnapshot, SpecHarness,
    SpecHarnessParams, initial_snapshot_with_goal,
};
use crate::ids::{ActorId, CardId, WaveId};
use crate::model::{Card, CardPatch, CardRole, new_id, now_ms};
// Issue #649 i2 lifted the per-card lock-map machinery that used to live in
// this module into `crate::per_card_lock` so the `/spec/input` lazy-recovery
// path can share it. Same semantics: guards self-clean their entry on drop.
use crate::per_card_lock::{PerCardLockGuard, PerCardLocks, lock_card, new_per_card_locks};
use crate::routes::cards::card_scope;
use crate::runtime_repo::{AgentProvider, RunStatus, RuntimeInit, RuntimeKind, ThreadAttribution};
use crate::shared_codex_appserver::{SharedCodexAppServer, SharedThreadStartParams};
use crate::state::WriteContext;
use crate::wave_cove_cache::WaveCoveCache;

use super::{
    AppServerInteractKind, AppServerInteractOutcome, CompensationStateVersioned, CompensationStep,
    Operation, PhaseTag, ProviderAdapter, SpawnCtx, SpawnHandle, SpawnOutcome, Tx, TxOutput,
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

#[cfg(feature = "fixtures")]
pub const FIXTURE_SOCKET_PREFIX: &str = "neige-mcp-fixture-";

#[cfg(feature = "fixtures")]
pub(crate) fn fixture_socket_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "{FIXTURE_SOCKET_PREFIX}{}.sock",
        std::process::id()
    ))
}

#[derive(Clone)]
pub struct SpecHarnessStartAdapter {
    repo: Arc<dyn Repo>,
    daemon: Arc<SharedCodexAppServer>,
    harness_registry: HarnessRegistry,
    card_role_cache: CardRoleCache,
    wave_cove_cache: WaveCoveCache,
    mcp_socket_path: Option<PathBuf>,
    per_card_mint_locks: PerCardLocks,
}

impl SpecHarnessStartAdapter {
    pub fn new(
        repo: Arc<dyn Repo>,
        daemon: Arc<SharedCodexAppServer>,
        harness_registry: HarnessRegistry,
        card_role_cache: CardRoleCache,
        wave_cove_cache: WaveCoveCache,
        mcp_socket_path: Option<PathBuf>,
    ) -> Self {
        Self {
            repo,
            daemon,
            harness_registry,
            card_role_cache,
            wave_cove_cache,
            mcp_socket_path,
            per_card_mint_locks: new_per_card_locks(),
        }
    }

    /// Defense-in-depth: today OperationRuntime drives serially under
    /// drive_mutex, but if drive ever shifts to per-card-lease parallelism,
    /// this lock keeps card_mcp_token rotation atomic with the thread/start
    /// RPC that ships the matching raw token.
    async fn lock_card_mint(&self, card_id: &str) -> PerCardLockGuard {
        lock_card(&self.per_card_mint_locks, card_id).await
    }

    fn mcp_socket_path_for_thread(&self) -> Result<String> {
        if let Some(path) = self.mcp_socket_path.as_ref() {
            return Ok(path.to_string_lossy().to_string());
        }

        #[cfg(feature = "fixtures")]
        {
            let path = fixture_socket_path();
            Ok(path.to_string_lossy().to_string())
        }
        #[cfg(not(feature = "fixtures"))]
        {
            Err(CalmError::Internal(
                "spec harness MCP socket path missing".into(),
            ))
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
    #[serde(default)]
    pub reset_harness_items: bool,
    #[serde(default)]
    pub force_new_thread: bool,
}

#[derive(Serialize)]
struct SpecThreadEnvSet<'a> {
    #[serde(rename = "NEIGE_MCP_SOCKET")]
    neige_mcp_socket: &'a str,
    #[serde(rename = "NEIGE_MCP_TOKEN")]
    neige_mcp_token: &'a str,
}

#[derive(Serialize)]
struct SpecThreadEnvPolicy<'a> {
    #[serde(rename = "set")]
    set: SpecThreadEnvSet<'a>,
}

#[derive(Serialize)]
struct SpecThreadStartConfig<'a> {
    #[serde(rename = "shell_environment_policy")]
    shell_environment_policy: SpecThreadEnvPolicy<'a>,
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
        let defer_runtime_start = payload.force_new_thread;
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

        let inherited_snapshot = if defer_runtime_start {
            runtime_get_active_for_card_tx(tx, card.id.as_str())
                .await?
                .and_then(|runtime| {
                    let state = runtime.handle_state_json?;
                    (state.get("mode").and_then(Value::as_str) == Some(HARNESS_MODE))
                        .then(|| HarnessSnapshot::from_value_strict(state))
                })
        } else {
            None
        };
        let mut snapshot = initial_snapshot_with_goal(payload.goal.clone());
        if let Some(inherited) = inherited_snapshot {
            snapshot.push_watermark = inherited.push_watermark;
            snapshot.pending_queue = inherited.pending_queue;
            snapshot.pending_envelope_ids = inherited.pending_envelope_ids;
            snapshot.align_pending_envelope_ids();
        }

        let runtime_id = new_id();
        if !defer_runtime_start {
            runtime_start_tx(
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
        }

        let mut output = TxOutput::new(
            "card",
            Some(card.id.to_string()),
            serde_json::to_value(&card)?,
        );
        output.data = json!({
            "card_id": card.id,
            "wave_id": wave_id,
            "runtime_id": runtime_id,
            "runtime_deferred": defer_runtime_start,
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
        let reset_harness_items = payload.reset_harness_items;
        let force_new_thread = payload.force_new_thread;
        let card_id = output_string(output, "card_id")?;
        let wave_id = output_string(output, "wave_id")?;
        let runtime_id = output_string(output, "runtime_id")?;
        let runtime_deferred = output_bool(output, "runtime_deferred")?;
        let cwd = output_string(output, "cwd")?;
        if let Some(existing) = output_existing_thread_id(output)? {
            return Ok(AppServerInteractOutcome::MintedAndAwaited {
                thread_id: existing,
            });
        }
        let mint_lock_guard = self.lock_card_mint(&card_id).await;
        // Reuse requires the existing thread to have been minted under
        // PR #567's per-card token contract: the card owns a
        // `card_mcp_tokens` row. Migration 0035 forces a fresh mint for
        // any earlier thread.
        let reusable_thread_id = if force_new_thread {
            None
        } else if let Some(runtime) = self.repo.runtime_get_active_for_card(&card_id).await?
            && let Some(thread_id) = non_empty_string(runtime.thread_id.as_deref())
        {
            Some(thread_id)
        } else {
            None
        };
        let mut new_mcp_token_hash = None;
        let thread_id = if let Some(thread_id) = reusable_thread_id {
            if !self.repo.card_mcp_token_exists_for_card(&card_id).await? {
                tracing::warn!(
                    target: "spec_harness::reusable_thread_invariant",
                    %card_id,
                    thread_id = %thread_id,
                    "spec card reuses thread without per-card MCP token row - AI shell `neige` will fail -32401; migration 0035 should have nulled this thread_id"
                );
            }
            thread_id
        } else {
            let developer_instructions = crate::spec_card::render_system_prompt(
                crate::spec_card::SeededCardRole::Spec.prompt_template(),
                &wave_id,
            );
            let raw = crate::mcp_server::auth::CardMcpToken::generate();
            let hashed = crate::mcp_server::auth::hash_token(raw.as_str());
            new_mcp_token_hash = Some(hashed);
            let socket_path = self.mcp_socket_path_for_thread()?;
            let cfg = serde_json::to_value(SpecThreadStartConfig {
                shell_environment_policy: SpecThreadEnvPolicy {
                    set: SpecThreadEnvSet {
                        neige_mcp_socket: socket_path.as_str(),
                        neige_mcp_token: raw.as_str(),
                    },
                },
            })?;
            let params = SharedThreadStartParams {
                cwd,
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: Some(developer_instructions),
                config: Some(cfg),
            };
            if runtime_deferred {
                self.daemon
                    .thread_start_mint_for_card(&card_id, params)
                    .await?
            } else {
                self.daemon
                    .thread_start_for_card(&card_id, CardRole::Spec, Some(&wave_id), params)
                    .await?
            }
        };
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
        let transcript_scope = scope.clone();
        let transcript_runtime_id = runtime_id.clone();
        let transcript_card_id = CardId::from(card_id.clone());
        let transcript_wave_id = WaveId::from(wave_id.clone());
        let write = WriteContext::new(self.card_role_cache.clone(), self.wave_cove_cache.clone());
        let op_clone = op.clone();
        let output_clone = output.clone();
        let thread_for_tx = thread_id.clone();
        let ((updated_card, old_runtime_id, old_runtime_status), _id) = write_with_event_typed(
            ctx.repo.as_ref(),
            payload.actor,
            scope,
            None,
            &ctx.events,
            &write,
            move |tx| {
                Box::pin(async move {
                    let mut checkpoint_output = output_clone;
                    let mut old_runtime_id = None;
                    let mut old_runtime_status = None;
                    if let Some(hashed) = new_mcp_token_hash.as_ref() {
                        card_mcp_token_set_tx(tx, &card_id, hashed).await?;
                    }
                    if runtime_deferred {
                        let runtime_init = RuntimeInit {
                            id: runtime_id.clone(),
                            card_id: card_id.clone(),
                            kind: RuntimeKind::SharedSpec,
                            agent_provider: Some(AgentProvider::Codex),
                            status: RunStatus::Starting,
                            terminal_run_id: None,
                            thread_id: Some(thread_for_tx.clone()),
                            session_id: None,
                            active_turn_id: None,
                            handle_state_json: Some(serde_json::to_value(&snapshot)?),
                            lease_owner: None,
                            lease_until_ms: None,
                            now_ms: now_ms(),
                        };
                        if let Some(existing) = runtime_get_active_for_card_tx(tx, &card_id).await?
                        {
                            let existing_id = existing.id.clone();
                            let existing_status = existing.status.clone();
                            old_runtime_id = Some(existing_id.clone());
                            old_runtime_status = Some(existing_status.clone());
                            set_output_data(
                                &mut checkpoint_output,
                                "old_runtime_id",
                                json!(existing_id),
                            )?;
                            set_output_data(
                                &mut checkpoint_output,
                                "old_runtime_status",
                                serde_json::to_value(&existing_status)?,
                            )?;
                            runtime_supersede_tx(tx, &existing.id, runtime_init).await?;
                        } else {
                            runtime_start_tx(tx, runtime_init).await?;
                        }
                    } else {
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
                    }
                    if reset_harness_items {
                        harness_items_delete_by_card_tx(tx, &card_id).await?;
                    }
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
                    Ok((
                        (card.clone(), old_runtime_id, old_runtime_status),
                        Event::CardUpdated(card),
                    ))
                })
            },
        )
        .await?;
        drop(mint_lock_guard);
        card = updated_card;
        if let Some(old_runtime_id) = old_runtime_id {
            set_output_data(output, "old_runtime_id", json!(old_runtime_id))?;
        }
        if let Some(old_runtime_status) = old_runtime_status {
            set_output_data(
                output,
                "old_runtime_status",
                serde_json::to_value(&old_runtime_status)?,
            )?;
        }
        if reset_harness_items {
            ctx.repo
                .log_pure_event(
                    ActorId::Kernel,
                    transcript_scope,
                    None,
                    &ctx.events,
                    &self.card_role_cache,
                    &self.wave_cove_cache,
                    Event::HarnessTranscriptCleared {
                        runtime_id: transcript_runtime_id,
                        card_id: transcript_card_id,
                        wave_id: transcript_wave_id,
                    },
                )
                .await?;
        }
        output.result = serde_json::to_value(&card)?;
        output.target_id = Some(card.id.to_string());

        Ok(AppServerInteractOutcome::MintedAndAwaited { thread_id })
    }

    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        _op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
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
            events: ctx.events.clone(),
            card_role_cache: self.card_role_cache.clone(),
            wave_cove_cache: self.wave_cove_cache.clone(),
            daemon: self.daemon.clone(),
            config: HarnessConfig::default(),
            snapshot,
        });
        self.harness_registry
            .insert(runtime_id.clone(), handle.clone());
        handle.persist_snapshot().await?;
        Ok(SpawnOutcome::Ready(SpawnHandle::Harness { runtime_id }))
    }

    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        output: &TxOutput,
        op: &Operation,
    ) -> Result<CompensationStateVersioned> {
        let payload: SpecHarnessStartOperationPayload = serde_json::from_value(op.payload.clone())?;
        let card_id = output_string(output, "card_id")?;
        let runtime_id = output_string(output, "runtime_id")?;
        let thread_id = output_optional_string(output, "codex_thread_id")?;
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
        ) && (!payload.force_new_thread || thread_id.is_some())
        {
            steps.push(step(
                "interrupt_thread",
                json!({
                    "card_id": card_id,
                    "thread_id": thread_id,
                }),
            ));
        }
        steps.push(step("fail_runtime", json!({ "runtime_id": runtime_id })));
        if let Some(old_runtime_id) = output_optional_string(output, "old_runtime_id")? {
            let old_runtime_status =
                output
                    .data
                    .get("old_runtime_status")
                    .cloned()
                    .ok_or_else(|| {
                        CalmError::Internal(
                            "spec harness tx_output missing old_runtime_status".into(),
                        )
                    })?;
            steps.push(step(
                "restore_old_runtime",
                json!({
                    "runtime_id": old_runtime_id,
                    "status": old_runtime_status,
                }),
            ));
        }
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
                clear_card_runtime_fields(ctx, &card_id).await?;
                Ok(())
            }
            "fail_runtime" => {
                let runtime_id = step_arg_string(step, "runtime_id")?;
                write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
                    Box::pin(async move {
                        let now = now_ms();
                        sqlx::query(
                            r#"UPDATE runtimes
                                  SET status = 'failed',
                                      updated_at_ms = ?1,
                                      completed_at_ms = ?1
                                WHERE id = ?2
                                  AND status IN ('starting', 'running', 'idle', 'turn_pending')"#,
                        )
                        .bind(now)
                        .bind(&runtime_id)
                        .execute(&mut **tx)
                        .await?;
                        Ok(())
                    })
                })
                .await
            }
            "restore_old_runtime" => {
                let runtime_id = step_arg_string(step, "runtime_id")?;
                let status = step_arg_run_status(step, "status")?;
                restore_old_runtime_after_spawn_failure(ctx.repo.as_ref(), runtime_id, status).await
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

fn non_empty_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
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

fn output_bool(output: &TxOutput, key: &str) -> Result<bool> {
    match output.data.get(key) {
        Some(Value::Bool(value)) => Ok(*value),
        None => Ok(false),
        Some(_) => Err(CalmError::Internal(format!(
            "spec harness tx_output {key} must be bool"
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

async fn restore_old_runtime_after_spawn_failure(
    repo: &dyn crate::db::RouteRepo,
    old_runtime_id: String,
    status: RunStatus,
) -> Result<()> {
    let status_db = active_run_status_to_db(&status)?;
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            let now = now_ms();
            let res = sqlx::query(
                r#"UPDATE runtimes
                      SET status = ?1,
                          updated_at_ms = ?2,
                          completed_at_ms = NULL
                    WHERE id = ?3
                      AND status = 'superseded'"#,
            )
            .bind(status_db)
            .bind(now)
            .bind(&old_runtime_id)
            .execute(&mut **tx)
            .await?;
            if res.rows_affected() > 0 {
                return Ok(());
            }

            let current: Option<(String,)> =
                sqlx::query_as("SELECT status FROM runtimes WHERE id = ?1")
                    .bind(&old_runtime_id)
                    .fetch_optional(&mut **tx)
                    .await?;
            match current {
                Some((current,)) if current == status_db => Ok(()),
                Some((current,)) => Err(CalmError::Internal(format!(
                    "runtime {old_runtime_id} has status {current}; cannot restore old spec harness runtime to {status_db}"
                ))),
                None => Err(CalmError::Internal(format!(
                    "runtime {old_runtime_id} missing while restoring old spec harness runtime"
                ))),
            }
        })
    })
    .await
}

fn active_run_status_to_db(status: &RunStatus) -> Result<&'static str> {
    match status {
        RunStatus::Starting => Ok("starting"),
        RunStatus::Running => Ok("running"),
        RunStatus::Idle => Ok("idle"),
        RunStatus::TurnPending => Ok("turn_pending"),
        RunStatus::Failed | RunStatus::Exited | RunStatus::Superseded => Err(CalmError::Internal(
            format!("cannot restore old spec harness runtime to terminal status {status:?}"),
        )),
    }
}

fn step_arg_run_status(step: &CompensationStep, key: &str) -> Result<RunStatus> {
    let value = step.args.get(key).cloned().ok_or_else(|| {
        CalmError::Internal(format!(
            "spec harness compensation step {} missing {key}",
            step.op
        ))
    })?;
    Ok(serde_json::from_value(value)?)
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

// The per-card lock behavior test moved to `crate::per_card_lock::tests`
// alongside the lifted implementation (issue #649 i2).
