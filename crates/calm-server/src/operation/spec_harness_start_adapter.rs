use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::card_role_cache::CardRoleCache;
use crate::db::sqlite::{
    card_update_tx, harness_items_delete_by_card_tx, session_bind_attribution_tx,
    session_delete_tx, session_fail_if_active_runtime_tx, session_prepare_deferred_spec_tx,
    session_projection_active_for_card_tx, session_restore_from_superseded_runtime_tx,
    session_set_handle_state_tx, session_start_runtime_tx, session_supersede_and_start_tx,
};
use crate::db::{Repo, write_in_tx_typed, write_with_event_typed};
use crate::error::{CalmError, Result};
use crate::event::Event;
use crate::forge_trust::trusted_forge_plugin;
use crate::harness::{
    HARNESS_MODE, HarnessConfig, HarnessPhaseTag, HarnessRegistry, HarnessSnapshot, SpecHarness,
    SpecHarnessParams, initial_snapshot_with_goal, is_harness_snapshot_value,
};
use crate::ids::{ActorId, CardId, WaveId};
use crate::mcp_server::wiring::{
    mint_card_mcp_token_pair, mirror_session_mcp_token, persist_card_mcp_token_hash,
};
use crate::model::{Card, CardPatch, CardRole, new_id, now_ms};
// Issue #649 i2 lifted the per-card lock-map machinery that used to live in
// this module into `crate::per_card_lock` so the `/spec/input` lazy-recovery
// path can share it. Same semantics: guards self-clean their entry on drop.
use crate::per_card_lock::{PerCardLockGuard, PerCardLocks, lock_card, new_per_card_locks};
use crate::plugin_host::{PluginHost, manifest::WorkflowDescriptor};
use crate::routes::cards::card_scope;
use crate::session_projection_repo::{
    AgentProvider, ThreadAttribution, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use crate::shared_codex_appserver::{SharedCodexAppServer, SharedThreadStartParams, ThreadConfig};
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

const REUSABLE_THREAD_MISSING_CARD_MCP_TOKEN_ERROR: &str =
    "no per-card MCP token row; refusing to start an unauthenticated shell";

#[cfg(feature = "fixtures")]
pub const FIXTURE_SOCKET_PREFIX: &str = "neige-mcp-fixture-";

#[cfg(feature = "fixtures")]
pub fn fixture_socket_path() -> std::path::PathBuf {
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
    plugin: Arc<PluginHost>,
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
        plugin: Arc<PluginHost>,
        card_role_cache: CardRoleCache,
        wave_cove_cache: WaveCoveCache,
        mcp_socket_path: Option<PathBuf>,
    ) -> Self {
        Self {
            repo,
            daemon,
            harness_registry,
            plugin,
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

    async fn bound_workflow(&self, wave_id: &str) -> Result<Option<BoundWorkflow>> {
        let wave = match self.repo.wave_get(wave_id).await {
            Ok(wave) => wave,
            Err(error) => {
                tracing::error!(
                    target: "spec_harness::workflow_binding",
                    wave_id,
                    error = %error,
                    "workflow binding lookup failed; using vanilla spec prompt"
                );
                return Ok(None);
            }
        };
        let Some(wave) = wave else {
            tracing::error!(
                target: "spec_harness::workflow_binding",
                wave_id,
                "bound workflow wave was not found while resolving descriptor; using vanilla spec prompt"
            );
            return Ok(None);
        };
        let Some(workflow_id) = wave.workflow_id.as_deref() else {
            return Ok(None);
        };
        let running_plugin_ids = self.plugin.running_plugin_ids().await;
        for manifest in self.plugin.registry().list() {
            if !running_plugin_ids.contains(&manifest.id) || !trusted_forge_plugin(&manifest.id) {
                continue;
            }
            if let Some(workflow) = manifest
                .workflows
                .into_iter()
                .find(|workflow| workflow.id == workflow_id)
            {
                return Ok(Some(BoundWorkflow {
                    descriptor: workflow,
                    input: wave.workflow_input.clone(),
                }));
            }
        }
        // Descriptor unresolved (plugin stopped / trust revoked): fail-safe
        // to the vanilla prompt — the persisted workflow_input is dropped
        // along with the descriptor rather than injected without context.
        tracing::error!(
            target: "spec_harness::workflow_binding",
            wave_id,
            workflow_id,
            "bound workflow descriptor was not resolved from a running trusted forge plugin; using vanilla spec prompt"
        );
        Ok(None)
    }
}

/// #891 — a resolved workflow binding: the descriptor from the running
/// trusted plugin plus the wave row's persisted `workflow_input` (already
/// schema-validated at create time).
struct BoundWorkflow {
    descriptor: WorkflowDescriptor,
    input: Option<serde_json::Value>,
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

fn render_spec_developer_instructions(
    wave_id: &str,
    workflow_descriptor: Option<&WorkflowDescriptor>,
    workflow_input: Option<&serde_json::Value>,
) -> String {
    let mut instructions = crate::spec_card::render_system_prompt(
        crate::spec_card::SeededCardRole::Spec.prompt_template(),
        wave_id,
    );
    let Some(workflow_descriptor) = workflow_descriptor else {
        return instructions;
    };

    if !workflow_descriptor.spec_instructions.is_empty() {
        instructions.push_str("\n\n## Bound Workflow Instructions\n");
        instructions.push_str(&crate::spec_card::render_system_prompt(
            &workflow_descriptor.spec_instructions,
            wave_id,
        ));
    }
    if !workflow_descriptor.plan_template.is_empty() {
        instructions.push_str("\n\n## Bound Workflow Plan Template\n");
        instructions.push_str("```json\n");
        let plan_template_json = serde_json::to_string_pretty(&workflow_descriptor.plan_template)
            .expect("PlanTaskInput serializes");
        instructions.push_str(&plan_template_json);
        instructions.push_str("\n```");
    }
    if !workflow_descriptor.gates.is_empty() {
        instructions.push_str("\n\n## Bound Workflow Gates\n");
        instructions.push_str("```json\n");
        let gates_json =
            serde_json::to_string_pretty(&workflow_descriptor.gates).expect("GateInput serializes");
        instructions.push_str(&gates_json);
        instructions.push_str("\n```");
    }
    // #891 — the wave's validated workflow_input, verbatim. Deliberately
    // NOT passed through `render_system_prompt`: user-controlled JSON must
    // not have literal `{wave_id}` substituted (same raw-JSON precedent as
    // the plan_template / gates sections above).
    if let Some(input) = workflow_input {
        instructions.push_str("\n\n## Bound Workflow Input\n");
        instructions.push_str("```json\n");
        instructions
            .push_str(&serde_json::to_string_pretty(input).expect("workflow_input serializes"));
        instructions.push_str("\n```");
    }
    instructions
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
        let card = sqlx::query_as::<_, crate::db::rows::CardRow>(
            r#"SELECT id, wave_id, kind, sort, payload, title, deletable, created_at, updated_at
                 FROM cards
                WHERE id = ?1
                  AND wave_id = ?2"#,
        )
        .bind(card_id.as_str())
        .bind(wave_id.as_str())
        .fetch_optional(&mut **tx)
        .await?
        .map(Card::from)
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;

        let existing_active_runtime = if defer_runtime_start {
            session_projection_active_for_card_tx(tx, card.id.as_str()).await?
        } else {
            None
        };
        let inherited_snapshot = existing_active_runtime.as_ref().and_then(|runtime| {
            let state = runtime.handle_state_json.as_ref()?;
            if state.get("mode").and_then(Value::as_str) != Some(HARNESS_MODE) {
                return None;
            }
            if !is_harness_snapshot_value(state) {
                tracing::warn!(
                    card_id = %card_id,
                    "reset: dormant runtime snapshot has corrupt/unknown shape; \
                     discarding inherited queue and starting a fresh session"
                );
                return None;
            }
            Some(HarnessSnapshot::from_value_strict(state.clone()))
        });
        let mut snapshot = initial_snapshot_with_goal(payload.goal.clone());
        if let Some(inherited) = inherited_snapshot {
            snapshot.push_watermark = inherited.push_watermark;
            snapshot.pending_queue = inherited.pending_queue;
            snapshot.pending_envelope_ids = inherited.pending_envelope_ids;
            snapshot.align_pending_envelope_ids();
        }

        let runtime_id = new_id();
        let mut old_runtime_id = None;
        let mut old_runtime_status = None;
        let runtime_init = WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedSpec,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Starting,
            terminal_run_id: None,
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&snapshot)?),
            spawn_op_id: None,
            now_ms: now_ms(),
        };
        if defer_runtime_start {
            if let Some(existing) = existing_active_runtime.as_ref() {
                old_runtime_id = Some(existing.id.clone());
                old_runtime_status = Some(existing.status);
            }
            session_prepare_deferred_spec_tx(tx, &runtime_init).await?;
        } else {
            if let Some(existing) =
                session_projection_active_for_card_tx(tx, card.id.as_str()).await?
            {
                old_runtime_id = Some(existing.id.clone());
                old_runtime_status = Some(existing.status);
                session_supersede_and_start_tx(tx, &existing.id, runtime_init).await?;
            } else {
                session_start_runtime_tx(tx, runtime_init).await?;
            }
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
        if let Some(old_runtime_id) = old_runtime_id {
            output.set_output_data("old_runtime_id", json!(old_runtime_id), "spec harness")?;
        }
        if let Some(old_runtime_status) = old_runtime_status {
            output.set_output_data(
                "old_runtime_status",
                serde_json::to_value(old_runtime_status)?,
                "spec harness",
            )?;
        }
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
        let card_id = output.output_string("card_id", "spec harness")?;
        let wave_id = output.output_string("wave_id", "spec harness")?;
        let runtime_id = output.output_string("runtime_id", "spec harness")?;
        let runtime_deferred = output_bool(output, "runtime_deferred")?;
        let cwd = output.output_string("cwd", "spec harness")?;
        // OLD PTY shutdown at Phase-2 entry, immediately after the Phase-1
        // tx commit. Per RATIFY-8 section 5 / 1.4, force_new_thread is a
        // hard reset: the DB-side supersede lives in calm-truth, and the
        // handle kill stays here as the first app-server-side action after
        // commit.
        if let Some(old_runtime_id) =
            output.output_optional_string("old_runtime_id", "spec harness")?
            && old_runtime_id != runtime_id
            && let Some(old_handle) = self.harness_registry.remove(&old_runtime_id)
        {
            old_handle.shutdown().await?;
        }
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
        } else if let Some(runtime) = self
            .repo
            .session_projection_active_for_card(&card_id)
            .await?
            && let Some(thread_id) = TxOutput::non_empty_string(runtime.thread_id.as_deref())
        {
            Some(thread_id)
        } else {
            None
        };
        let mut new_mcp_token_hash = None;
        let thread_id = if let Some(thread_id) = reusable_thread_id {
            if !self.repo.card_mcp_token_exists_for_card(&card_id).await? {
                let message = format!(
                    "spec card {card_id} reuses thread {thread_id} with \
                     {REUSABLE_THREAD_MISSING_CARD_MCP_TOKEN_ERROR} \
                     (re-run to mint a fresh thread)"
                );
                tracing::warn!(
                    target: "spec_harness::reusable_thread_invariant",
                    %card_id,
                    thread_id = %thread_id,
                    error = %message,
                    "refusing to reuse spec thread without per-card MCP token row; migration 0035 should have nulled this thread_id"
                );
                return Err(CalmError::Conflict(message));
            }
            thread_id
        } else {
            let bound_workflow = self.bound_workflow(&wave_id).await?;
            let developer_instructions = render_spec_developer_instructions(
                &wave_id,
                bound_workflow.as_ref().map(|bound| &bound.descriptor),
                bound_workflow
                    .as_ref()
                    .and_then(|bound| bound.input.as_ref()),
            );
            let (raw, hashed) = mint_card_mcp_token_pair();
            new_mcp_token_hash = Some(hashed);
            let socket_path = self.mcp_socket_path_for_thread()?;
            // #838 (lean Move 1): build the channel-3 `thread/start` config
            // (`shell_environment_policy.set.{NEIGE_MCP_SOCKET,NEIGE_MCP_TOKEN}`)
            // through the single shared producer so the worker, cold-respawn,
            // and spec spawn paths all emit the byte-identical shape from one
            // place. Previously this path wrapped `card_mcp_env` in its own
            // parallel `SpecThread*` structs.
            let params = SharedThreadStartParams {
                cwd,
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: Some(developer_instructions),
                config: ThreadConfig::McpShell {
                    socket_path: PathBuf::from(&socket_path),
                    raw_token: raw,
                },
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
        output.set_output_data("codex_thread_id", json!(thread_id.clone()), "spec harness")?;
        let mut snapshot = output_snapshot(output)?;
        snapshot.phase = HarnessPhaseTag::Idle;
        snapshot.last_thread_id = Some(thread_id.clone());
        output.set_output_data("snapshot", serde_json::to_value(&snapshot)?, "spec harness")?;
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
                        persist_card_mcp_token_hash(tx, &card_id, hashed).await?;
                    }
                    if runtime_deferred {
                        let runtime_init = WorkerSessionInit {
                            id: runtime_id.clone(),
                            card_id: card_id.clone(),
                            kind: WorkerSessionKind::SharedSpec,
                            agent_provider: Some(AgentProvider::Codex),
                            status: WorkerSessionState::Starting,
                            terminal_run_id: None,
                            thread_id: Some(thread_for_tx.clone()),
                            session_id: None,
                            active_turn_id: None,
                            handle_state_json: Some(serde_json::to_value(&snapshot)?),
                            spawn_op_id: None,
                            now_ms: now_ms(),
                        };
                        if let Some(existing) =
                            session_projection_active_for_card_tx(tx, &card_id).await?
                        {
                            let existing_id = existing.id.clone();
                            let existing_status = existing.status;
                            if existing_id != runtime_id {
                                old_runtime_id = Some(existing_id.clone());
                                old_runtime_status = Some(existing_status);
                                checkpoint_output.set_output_data(
                                    "old_runtime_id",
                                    json!(existing_id),
                                    "spec harness",
                                )?;
                                checkpoint_output.set_output_data(
                                    "old_runtime_status",
                                    serde_json::to_value(existing_status)?,
                                    "spec harness",
                                )?;
                            }
                            session_supersede_and_start_tx(tx, &existing.id, runtime_init).await?;
                        } else {
                            session_start_runtime_tx(tx, runtime_init).await?;
                        }
                    } else {
                        session_bind_attribution_tx(
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
                        session_set_handle_state_tx(
                            tx,
                            &runtime_id,
                            Some(serde_json::to_value(&snapshot)?),
                        )
                        .await?;
                    }
                    if let Some(hashed) = new_mcp_token_hash.as_ref() {
                        mirror_session_mcp_token(tx, &runtime_id, hashed).await?;
                    }
                    if reset_harness_items {
                        harness_items_delete_by_card_tx(tx, &card_id).await?;
                    }
                    let card = card_update_tx(
                        tx,
                        &card_id,
                        CardPatch {
                            title: None,
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
            output.set_output_data("old_runtime_id", json!(old_runtime_id), "spec harness")?;
        }
        if let Some(old_runtime_status) = old_runtime_status {
            output.set_output_data(
                "old_runtime_status",
                serde_json::to_value(old_runtime_status)?,
                "spec harness",
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
        let runtime_id = output.output_string("runtime_id", "spec harness")?;
        let card_id = output.output_string("card_id", "spec harness")?;
        let wave_id = output.output_string("wave_id", "spec harness")?;
        let thread_id = output.output_optional_string("codex_thread_id", "spec harness")?;
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
        let card_id = output.output_string("card_id", "spec harness")?;
        let runtime_id = output.output_string("runtime_id", "spec harness")?;
        let thread_id = output.output_optional_string("codex_thread_id", "spec harness")?;
        let mut steps = Vec::new();
        if from_phase == PhaseTag::AppServerInteract
            && is_reusable_thread_missing_card_mcp_token_failure(reason)
        {
            return Ok(CompensationStateVersioned {
                version: 1,
                from_phase,
                reason: reason.to_string(),
                steps,
            });
        }
        if matches!(
            from_phase,
            PhaseTag::SpawnStarted | PhaseTag::SpawnSucceeded
        ) {
            steps.push(CompensationStep::new(
                "abort_harness_task",
                json!({ "runtime_id": runtime_id }),
            ));
        }
        if matches!(
            from_phase,
            PhaseTag::AppServerInteract | PhaseTag::SpawnStarted | PhaseTag::SpawnSucceeded
        ) && (!payload.force_new_thread || thread_id.is_some())
        {
            steps.push(CompensationStep::new(
                "interrupt_thread",
                json!({
                    "card_id": card_id,
                    "thread_id": thread_id,
                }),
            ));
        }
        steps.push(CompensationStep::new(
            "fail_runtime",
            json!({ "runtime_id": runtime_id }),
        ));
        if let Some(old_runtime_id) =
            output.output_optional_string("old_runtime_id", "spec harness")?
        {
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
            steps.push(CompensationStep::new(
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
                let runtime_id = step.arg_string("runtime_id", "spec harness")?;
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
                let card_id = step.arg_string("card_id", "spec harness")?;
                clear_card_runtime_fields(ctx, &card_id).await?;
                Ok(())
            }
            "fail_runtime" => {
                let runtime_id = step.arg_string("runtime_id", "spec harness")?;
                write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
                    Box::pin(async move {
                        session_fail_if_active_runtime_tx(tx, &runtime_id)
                            .await
                            .map_err(CalmError::from)
                    })
                })
                .await
            }
            "restore_old_runtime" => {
                let runtime_id = step.arg_string("runtime_id", "spec harness")?;
                let status = step_arg_run_status(step, "status")?;
                restore_old_runtime_after_spawn_failure(ctx.repo.as_ref(), runtime_id, status).await
            }
            "delete_runtime" => {
                let runtime_id = step.arg_string("runtime_id", "spec harness")?;
                write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
                    Box::pin(async move {
                        session_delete_tx(tx, &runtime_id)
                            .await
                            .map_err(CalmError::from)?;
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

fn is_reusable_thread_missing_card_mcp_token_failure(reason: &str) -> bool {
    reason.contains(REUSABLE_THREAD_MISSING_CARD_MCP_TOKEN_ERROR)
}

fn output_snapshot(output: &TxOutput) -> Result<HarnessSnapshot> {
    let value = output
        .data
        .get("snapshot")
        .cloned()
        .ok_or_else(|| CalmError::Internal("spec harness output missing snapshot".into()))?;
    Ok(serde_json::from_value(value)?)
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
    Ok(output
        .output_optional_string("codex_thread_id", "spec harness")?
        .filter(|id| !id.trim().is_empty()))
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
                    title: None,
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
    status: WorkerSessionState,
) -> Result<()> {
    active_run_status_to_db(&status)?;
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            session_restore_from_superseded_runtime_tx(tx, &old_runtime_id, status)
                .await
                .map_err(CalmError::from)
        })
    })
    .await
}

fn active_run_status_to_db(status: &WorkerSessionState) -> Result<&'static str> {
    if status.is_terminal() {
        Err(CalmError::Internal(format!(
            "cannot restore old spec harness runtime to terminal status {status:?}"
        )))
    } else {
        Ok(status.as_db_str())
    }
}

fn step_arg_run_status(step: &CompensationStep, key: &str) -> Result<WorkerSessionState> {
    let value = step.args.get(key).cloned().ok_or_else(|| {
        CalmError::Internal(format!(
            "spec harness compensation step {} missing {key}",
            step.op
        ))
    })?;
    Ok(serde_json::from_value(value)?)
}

// The per-card lock behavior test moved to `crate::per_card_lock::tests`
// alongside the lifted implementation (issue #649 i2).

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    use crate::db::prelude::{ServerRepoOutOfDomainExt, ServerRepoSyncDomainRawExt};
    use crate::db::sqlite::SqlxRepo;
    use crate::event::EventBus;
    use crate::mcp_server::tools::plan::{GateInput, GateStepInput, PlanTaskInput};
    use crate::model::{NewCove, NewPlugin, NewWave};
    use crate::plugin_host::{Manifest, PluginRegistry, PluginRuntimeStatus};
    use crate::routes::theme::RequestTheme;
    use tokio::time::{Instant, sleep};

    const WORKFLOW_ID: &str = "issue-development";

    fn plan_task(key: &str, kind: &str, depends_on: &[&str]) -> PlanTaskInput {
        PlanTaskInput {
            key: key.into(),
            kind: kind.into(),
            goal: "do the thing".into(),
            context: Some(json!({ "issue": 760, "slice": "5a" })),
            acceptance_criteria: Some("passes the requested gates".into()),
            cwd: Some("/workspace/repo".into()),
            depends_on: depends_on.iter().map(|dep| (*dep).to_string()).collect(),
            priority: Some(10),
            gate: Some(GateInput {
                cwd: Some("/workspace/repo".into()),
                timeout_secs: Some(120),
                steps: vec![GateStepInput {
                    name: "test".into(),
                    cmd: "cargo test -p calm-server".into(),
                }],
            }),
            no_gate_reason: None,
        }
    }

    #[test]
    fn spec_developer_instructions_append_workflow_descriptor_when_bound() {
        let workflow = WorkflowDescriptor {
            id: "issue-development".into(),
            spec_instructions: "Follow workflow instructions for wave {wave_id}.".into(),
            plan_template: vec![
                plan_task("review-a", "codex", &[]),
                plan_task("review-b", "claude", &["review-a"]),
                plan_task("merge", "terminal", &["review-a", "review-b"]),
            ],
            gates: vec![GateInput {
                cwd: Some("/workspace/repo".into()),
                timeout_secs: Some(300),
                steps: vec![GateStepInput {
                    name: "fmt".into(),
                    cmd: "cargo fmt --all --check".into(),
                }],
            }],
            card_kinds: vec![],
            input_schema: None,
        };

        let out = render_spec_developer_instructions("wave-abc", Some(&workflow), None);

        assert!(out.contains("Follow workflow instructions for wave wave-abc."));
        assert!(!out.contains("{wave_id}"));
        assert!(out.contains("## Bound Workflow Plan Template"));
        assert!(out.contains("```json"));
        assert!(out.contains(r#""key": "review-a""#));
        assert!(out.contains(r#""goal": "do the thing""#));
        assert!(out.contains(r#""context": {"#));
        assert!(out.contains(r#""acceptance_criteria": "passes the requested gates""#));
        assert!(out.contains(r#""cwd": "/workspace/repo""#));
        assert!(out.contains(r#""priority": 10"#));
        assert!(out.contains(r#""depends_on": ["#));
        assert!(out.contains(r#""review-a""#));
        assert!(out.contains(r#""gate": {"#));
        assert!(out.contains(r#""timeout_secs": 120"#));
        assert!(out.contains(r#""no_gate_reason": null"#));
        assert!(out.contains("## Bound Workflow Gates"));
        assert!(out.contains(r#""name": "fmt""#));
        assert!(out.contains(r#""cmd": "cargo fmt --all --check""#));
        assert!(out.contains(r#""timeout_secs": 300"#));
        assert!(!out.contains(r#""id": "issue-development""#));
        assert!(!out.contains(r#""card_kinds""#));
    }

    #[test]
    fn spec_developer_instructions_skip_empty_workflow_instructions_section() {
        let workflow = WorkflowDescriptor {
            id: "issue-development".into(),
            spec_instructions: String::new(),
            plan_template: vec![plan_task("review-a", "codex", &[])],
            gates: vec![],
            card_kinds: vec![],
            input_schema: None,
        };

        let out = render_spec_developer_instructions("wave-abc", Some(&workflow), None);

        assert!(!out.contains("## Bound Workflow Instructions"));
        assert!(out.contains("## Bound Workflow Plan Template"));
        assert!(out.contains(r#""key": "review-a""#));
    }

    #[test]
    fn spec_developer_instructions_without_workflow_match_static_template() {
        let expected = crate::spec_card::render_system_prompt(
            crate::spec_card::SeededCardRole::Spec.prompt_template(),
            "wave-abc",
        );

        let out = render_spec_developer_instructions("wave-abc", None, None);

        assert_eq!(out, expected);
    }

    #[test]
    fn spec_developer_instructions_append_workflow_input_when_present() {
        let workflow = WorkflowDescriptor {
            id: "issue-development".into(),
            spec_instructions: "Follow workflow instructions for wave {wave_id}.".into(),
            plan_template: vec![plan_task("review-a", "codex", &[])],
            gates: vec![GateInput {
                cwd: Some("/workspace/repo".into()),
                timeout_secs: Some(300),
                steps: vec![GateStepInput {
                    name: "fmt".into(),
                    cmd: "cargo fmt --all --check".into(),
                }],
            }],
            card_kinds: vec![],
            input_schema: None,
        };
        let input = json!({
            "issue_url": "https://github.com/o/r/issues/1",
            "notes": "literal {wave_id} must survive"
        });

        let out = render_spec_developer_instructions("wave-abc", Some(&workflow), Some(&input));

        // Section renders after the Gates section, fenced as JSON.
        let input_at = out
            .find("## Bound Workflow Input")
            .expect("workflow input section");
        let gates_at = out.find("## Bound Workflow Gates").expect("gates section");
        assert!(gates_at < input_at, "input section must follow gates");
        assert!(out[input_at..].contains("```json"));
        assert!(out[input_at..].contains(r#""issue_url": "https://github.com/o/r/issues/1""#));
        // User JSON is injected verbatim — no `{wave_id}` template substitution
        // (the spec_instructions section above it IS substituted).
        assert!(out[input_at..].contains("literal {wave_id} must survive"));
        assert!(out.contains("Follow workflow instructions for wave wave-abc."));

        // input = None renders no section at all.
        let without = render_spec_developer_instructions("wave-abc", Some(&workflow), None);
        assert!(!without.contains("## Bound Workflow Input"));
    }

    #[tokio::test]
    async fn bound_workflow_descriptor_filters_running_trusted_workflow_binding() {
        let trusted_plugin_id = configured_trusted_plugin_id();
        let untrusted_plugin_id = untrusted_plugin_id(&trusted_plugin_id);
        let repo = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory sqlite repo"),
        );
        let bound_input = json!({ "issue_url": "https://github.com/o/r/issues/1" });
        let bound_wave =
            make_wave(repo.as_ref(), Some(WORKFLOW_ID), Some(bound_input.clone())).await;
        let unbound_wave = make_wave(repo.as_ref(), None, None).await;

        let (trusted_running_host, trusted_running_tmp) =
            plugin_host_with_workflow(repo.clone(), &trusted_plugin_id, true).await;
        trusted_running_host
            .spawn(&trusted_plugin_id)
            .await
            .expect("spawn trusted plugin");
        wait_for_running(&trusted_running_host, &trusted_plugin_id).await;
        let trusted_running_adapter = adapter_for(repo.clone(), trusted_running_host.clone());
        let bound = trusted_running_adapter
            .bound_workflow(bound_wave.id.as_str())
            .await
            .expect("resolve trusted running descriptor")
            .expect("bound workflow");
        assert_eq!(bound.descriptor.id, WORKFLOW_ID);
        // The wave row's persisted workflow_input rides along with the descriptor.
        assert_eq!(bound.input.as_ref(), Some(&bound_input));

        let (trusted_stopped_host, _trusted_stopped_tmp) =
            plugin_host_with_workflow(repo.clone(), &trusted_plugin_id, false).await;
        let trusted_stopped_adapter = adapter_for(repo.clone(), trusted_stopped_host);
        assert!(
            trusted_stopped_adapter
                .bound_workflow(bound_wave.id.as_str())
                .await
                .expect("trusted stopped lookup")
                .is_none()
        );

        let (untrusted_running_host, untrusted_running_tmp) =
            plugin_host_with_workflow(repo.clone(), &untrusted_plugin_id, true).await;
        untrusted_running_host
            .spawn(&untrusted_plugin_id)
            .await
            .expect("spawn untrusted plugin");
        wait_for_running(&untrusted_running_host, &untrusted_plugin_id).await;
        let untrusted_running_adapter = adapter_for(repo.clone(), untrusted_running_host.clone());
        assert!(
            untrusted_running_adapter
                .bound_workflow(bound_wave.id.as_str())
                .await
                .expect("untrusted running lookup")
                .is_none()
        );

        assert!(
            trusted_running_adapter
                .bound_workflow(unbound_wave.id.as_str())
                .await
                .expect("unbound lookup")
                .is_none()
        );

        untrusted_running_host
            .stop(&untrusted_plugin_id)
            .await
            .expect("stop untrusted plugin");
        trusted_running_host
            .stop(&trusted_plugin_id)
            .await
            .expect("stop trusted plugin");
        drop(untrusted_running_tmp);
        drop(trusted_running_tmp);
    }

    fn configured_trusted_plugin_id() -> String {
        std::env::var("NEIGE_TRUSTED_FORGE_PLUGINS")
            .ok()
            .and_then(|configured| {
                configured
                    .split(',')
                    .map(str::trim)
                    .find(|id| !id.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "dev.neige.git-forge".to_string())
    }

    fn untrusted_plugin_id(trusted_plugin_id: &str) -> String {
        let mut candidate = "dev.neige.untrusted-workflow-test".to_string();
        let mut suffix = 0;
        while candidate == trusted_plugin_id || trusted_forge_plugin(&candidate) {
            suffix += 1;
            candidate = format!("dev.neige.untrusted-workflow-test-{suffix}");
        }
        candidate
    }

    async fn make_wave(
        repo: &SqlxRepo,
        workflow_id: Option<&str>,
        workflow_input: Option<serde_json::Value>,
    ) -> crate::model::Wave {
        let cove = repo
            .cove_create(NewCove {
                name: format!("cove-{workflow_id:?}"),
                color: "#101010".into(),
                sort: None,
            })
            .await
            .expect("create cove");
        repo.wave_create(NewWave {
            workflow_input,
            cove_id: cove.id,
            title: "workflow resolver".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: workflow_id.map(str::to_string),
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .expect("create wave")
    }

    async fn plugin_host_with_workflow(
        repo: Arc<SqlxRepo>,
        plugin_id: &str,
        seed_plugin_row: bool,
    ) -> (Arc<PluginHost>, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let plugins_dir = tmp.path().join("plugins");
        let plugins_data_dir = tmp.path().join("plugins-data");
        let install_dir = plugins_dir.join(plugin_id);
        let bin_dir = install_dir.join("bin");
        std::fs::create_dir_all(&bin_dir).expect("create plugin bin dir");
        std::fs::create_dir_all(&plugins_data_dir).expect("create plugins data dir");
        std::os::unix::fs::symlink(stub_echo_bin(), bin_dir.join("stub"))
            .expect("symlink echo stub");

        let manifest_json = json!({
            "manifest_version": 1,
            "id": plugin_id,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Workflow Resolver Stub",
            "entrypoint": { "command": "bin/stub" },
            "workflows": [
                {
                    "id": WORKFLOW_ID,
                    "plan_template": [
                        {
                            "key": "inspect",
                            "kind": "codex",
                            "goal": "Inspect the issue.",
                            "depends_on": []
                        }
                    ],
                    "gates": [],
                    "spec_instructions": "Use workflow {wave_id}.",
                    "card_kinds": []
                }
            ],
            "permissions": {}
        });
        let manifest = Manifest::parse(&manifest_json.to_string()).expect("manifest parses");
        let registry = PluginRegistry::empty();
        registry.insert(manifest, Some(install_dir.clone()));
        if seed_plugin_row {
            repo.plugin_install(NewPlugin {
                id: plugin_id.to_string(),
                version: "0.1.0".into(),
                install_path: install_dir.display().to_string(),
                manifest: manifest_json,
                enabled: true,
                user_config: json!({}),
            })
            .await
            .expect("seed plugin row");
        }
        let repo_dyn: Arc<dyn Repo> = repo;
        let host = Arc::new(PluginHost::new_full(
            Arc::new(registry),
            repo_dyn,
            plugins_dir,
            plugins_data_dir,
            Vec::new(),
            EventBus::new(),
            WriteContext::new(CardRoleCache::new(), WaveCoveCache::new()),
        ));
        (host, tmp)
    }

    fn adapter_for(repo: Arc<SqlxRepo>, plugin: Arc<PluginHost>) -> SpecHarnessStartAdapter {
        let repo_dyn: Arc<dyn Repo> = repo;
        SpecHarnessStartAdapter::new(
            repo_dyn.clone(),
            SharedCodexAppServer::new_stub(repo_dyn),
            HarnessRegistry::new(),
            plugin,
            CardRoleCache::new(),
            WaveCoveCache::new(),
            None,
        )
    }

    async fn wait_for_running(host: &Arc<PluginHost>, plugin_id: &str) {
        let start = Instant::now();
        loop {
            if let Some(status) = host.status(plugin_id).await
                && matches!(status.status, PluginRuntimeStatus::Running)
            {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(2),
                "timed out waiting for plugin {plugin_id} to run"
            );
            sleep(Duration::from_millis(25)).await;
        }
    }

    fn stub_echo_bin() -> PathBuf {
        if let Some(path) = std::env::var_os("CARGO_BIN_EXE_plugin-host-stub-echo") {
            return path.into();
        }
        if let Some(path) = option_env!("CARGO_BIN_EXE_plugin-host-stub-echo") {
            return path.into();
        }
        let current = std::env::current_exe().expect("current test executable");
        let deps_dir = current.parent().expect("test executable parent");
        let debug_dir = deps_dir.parent().expect("target debug dir");
        let candidate = debug_dir.join("plugin-host-stub-echo");
        assert!(
            candidate.exists(),
            "missing plugin-host-stub-echo at {}",
            candidate.display()
        );
        candidate
    }
}
