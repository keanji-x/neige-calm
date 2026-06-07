use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::aspect::AspectRegistry;
use crate::card_role_cache::CardRoleCache;
use crate::codex_appserver::InputItem;
use crate::db::sqlite::{
    card_codex_thread_upsert_tx, card_update_tx, runtime_get_active_for_card_tx, runtime_start_tx,
    runtime_supersede_tx,
};
use crate::db::{write_in_tx_typed, write_with_event_typed};
use crate::dispatcher::Dispatcher;
use crate::error::{CalmError, Result};
use crate::event::{Event, EventScope};
use crate::ids::ActorId;
use crate::model::{CardPatch, CardRole, Wave, now_ms};
use crate::pending_codex_threads::{PendingEntry, PendingThreadStartRegistry};
use crate::routes::waves::{
    await_shared_spec_initial_turn_lifecycle, install_spec_push_sinks_and_park_parts,
};
use crate::runtime_repo::{AgentProvider, RunStatus, RuntimeInit, RuntimeKind};
use crate::shared_codex_appserver::{SharedCodexAppServer, SharedThreadStartParams};
use crate::spec_card::{
    SeededCardRole, SpecPushDaemonArgs, render_system_prompt, seed_and_spawn_spec_daemon_parts,
};
use crate::spec_push::{self, SharedStatus, SpecPushPhase, SpecPushRegistry, SpecPushStatus};
use crate::state::{CodexClient, WriteContext};
use crate::terminal_sweeper::reap_terminal_artifacts_with_renderer;
use crate::wave_cove_cache::WaveCoveCache;

use super::{
    AppServerInteractKind, AppServerInteractOutcome, CompensationStateVersioned, CompensationStep,
    Operation, Phase, PhaseTag, ProviderAdapter, SpawnCtx, SpawnHandle, Tx, TxOutput,
    checkpoint_app_server_interact_tx,
};

#[cfg(feature = "fixtures")]
use futures::future::BoxFuture;

#[cfg(feature = "fixtures")]
type SpawnHook = Arc<
    dyn Fn(String, String, String, Value) -> BoxFuture<'static, Result<SpawnHandle>> + Send + Sync,
>;

const SHARED_DAEMON_SPAWN_PHASES: &[PhaseTag] = &[
    PhaseTag::Pending,
    PhaseTag::TxCommitted,
    PhaseTag::AppServerInteract,
    PhaseTag::SpawnStarted,
    PhaseTag::SpawnSucceeded,
    PhaseTag::Succeeded,
];

#[derive(Clone)]
pub struct SharedDaemonSpawnAdapter {
    repo: Arc<dyn crate::db::RouteRepo>,
    codex: Arc<CodexClient>,
    shared_codex_appserver: Arc<SharedCodexAppServer>,
    pending_codex_threads: Arc<PendingThreadStartRegistry>,
    pending_codex_threads_spawn_serial: Arc<Mutex<()>>,
    card_role_cache: CardRoleCache,
    wave_cove_cache: WaveCoveCache,
    dispatcher: Arc<Dispatcher>,
    spec_push: SpecPushRegistry,
    aspects: Arc<AspectRegistry>,
    #[cfg(feature = "fixtures")]
    spawn_hook: Option<SpawnHook>,
}

impl SharedDaemonSpawnAdapter {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        repo: Arc<dyn crate::db::RouteRepo>,
        codex: Arc<CodexClient>,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
        pending_codex_threads: Arc<PendingThreadStartRegistry>,
        pending_codex_threads_spawn_serial: Arc<Mutex<()>>,
        card_role_cache: CardRoleCache,
        wave_cove_cache: WaveCoveCache,
        dispatcher: Arc<Dispatcher>,
        spec_push: SpecPushRegistry,
        aspects: Arc<AspectRegistry>,
    ) -> Self {
        Self {
            repo,
            codex,
            shared_codex_appserver,
            pending_codex_threads,
            pending_codex_threads_spawn_serial,
            card_role_cache,
            wave_cove_cache,
            dispatcher,
            spec_push,
            aspects,
            #[cfg(feature = "fixtures")]
            spawn_hook: None,
        }
    }

    #[cfg(feature = "fixtures")]
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_spawn_hook(
        repo: Arc<dyn crate::db::RouteRepo>,
        codex: Arc<CodexClient>,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
        pending_codex_threads: Arc<PendingThreadStartRegistry>,
        pending_codex_threads_spawn_serial: Arc<Mutex<()>>,
        card_role_cache: CardRoleCache,
        wave_cove_cache: WaveCoveCache,
        dispatcher: Arc<Dispatcher>,
        spec_push: SpecPushRegistry,
        aspects: Arc<AspectRegistry>,
        spawn_hook: SpawnHook,
    ) -> Self {
        Self {
            repo,
            codex,
            shared_codex_appserver,
            pending_codex_threads,
            pending_codex_threads_spawn_serial,
            card_role_cache,
            wave_cove_cache,
            dispatcher,
            spec_push,
            aspects,
            spawn_hook: Some(spawn_hook),
        }
    }

    async fn persist_prompt_thread(
        &self,
        ctx: &SpawnCtx,
        op: &Operation,
        output: &TxOutput,
        wave: &Wave,
        thread_id: &str,
    ) -> Result<()> {
        let write = WriteContext::new(self.card_role_cache.clone(), self.wave_cove_cache.clone());
        let op_for_tx = op.clone();
        let output_for_tx = output.clone();
        let card_id = output_string(output, "spec_card_id")?;
        let wave_id = wave.id.to_string();
        let remote_uri = self.shared_codex_appserver.remote_uri();
        let wave_for_tx = wave.clone();
        let thread_id_for_tx = thread_id.to_string();
        let card_id_for_tx = card_id.clone();
        let scope = EventScope::Card {
            card: card_id.clone().into(),
            wave: wave.id.clone(),
            cove: wave.cove_id.clone(),
        };
        let (_updated, _id) = write_with_event_typed(
            ctx.repo.as_ref(),
            ActorId::Kernel,
            scope,
            None,
            &ctx.events,
            &write,
            move |tx| {
                Box::pin(async move {
                    card_codex_thread_upsert_tx(
                        tx,
                        &card_id_for_tx,
                        &thread_id_for_tx,
                        CardRole::Spec,
                        Some(wave_id.as_str()),
                    )
                    .await?;
                    let card = persist_shared_spec_runtime_fields_tx(
                        tx,
                        &card_id_for_tx,
                        &wave_for_tx,
                        &remote_uri,
                        None,
                        Some(&thread_id_for_tx),
                    )
                    .await?;
                    let mut checkpoint_output = output_for_tx.clone();
                    checkpoint_output.result = serde_json::to_value(&card)?;
                    checkpoint_app_server_interact_tx(
                        tx,
                        &op_for_tx,
                        AppServerInteractKind::MintAndAwait {
                            thread_id: Some(thread_id_for_tx),
                        },
                        &checkpoint_output,
                    )
                    .await?;
                    Ok((card.clone(), Event::CardUpdated(card)))
                })
            },
        )
        .await?;
        Ok(())
    }

    async fn checkpoint_prompt_turn_started(
        &self,
        ctx: &SpawnCtx,
        op: &Operation,
        output: &TxOutput,
        thread_id: &str,
    ) -> Result<()> {
        let op_for_tx = op.clone();
        let output_for_tx = output.clone();
        let thread_id_for_tx = thread_id.to_string();
        write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
            Box::pin(async move {
                checkpoint_app_server_interact_tx(
                    tx,
                    &op_for_tx,
                    AppServerInteractKind::MintAndAwait {
                        thread_id: Some(thread_id_for_tx),
                    },
                    &output_for_tx,
                )
                .await
            })
        })
        .await
    }

    async fn persist_pending_thread(
        &self,
        ctx: &SpawnCtx,
        op: &Operation,
        output: &TxOutput,
        wave: &Wave,
    ) -> Result<()> {
        let write = WriteContext::new(self.card_role_cache.clone(), self.wave_cove_cache.clone());
        let op_for_tx = op.clone();
        let output_for_tx = output.clone();
        let card_id = output_string(output, "spec_card_id")?;
        let terminal_id = output_string(output, "terminal_id")?;
        let remote_uri = self.shared_codex_appserver.remote_uri();
        let wave_for_tx = wave.clone();
        let card_id_for_tx = card_id.clone();
        let terminal_id_for_tx = terminal_id.clone();
        let scope = EventScope::Card {
            card: card_id.clone().into(),
            wave: wave.id.clone(),
            cove: wave.cove_id.clone(),
        };
        let (_updated, _id) = write_with_event_typed(
            ctx.repo.as_ref(),
            ActorId::Kernel,
            scope,
            None,
            &ctx.events,
            &write,
            move |tx| {
                Box::pin(async move {
                    let card = persist_shared_spec_runtime_fields_tx(
                        tx,
                        &card_id_for_tx,
                        &wave_for_tx,
                        &remote_uri,
                        Some(&terminal_id_for_tx),
                        None,
                    )
                    .await?;
                    let mut checkpoint_output = output_for_tx.clone();
                    checkpoint_output.result = serde_json::to_value(&card)?;
                    checkpoint_app_server_interact_tx(
                        tx,
                        &op_for_tx,
                        AppServerInteractKind::RegisterPending {
                            entry_id: Some(card_id_for_tx.clone()),
                        },
                        &checkpoint_output,
                    )
                    .await?;
                    Ok((card.clone(), Event::CardUpdated(card)))
                })
            },
        )
        .await?;
        Ok(())
    }

    async fn park_handle(
        &self,
        output: &TxOutput,
        wave: &Wave,
        notifications: tokio::sync::broadcast::Receiver<crate::codex_appserver::Notification>,
        status: SharedStatus,
    ) -> Result<SpecPushDaemonArgs> {
        let spec_card_id = output_string(output, "spec_card_id")?;
        let thread_id = output_optional_string(output, "codex_thread_id")?;
        let needs_initial_prompt = output_bool(output, "needs_initial_prompt")?;
        let handle = spec_push::park_shared_handle(
            self.shared_codex_appserver.clone(),
            thread_id.clone(),
            notifications,
            status,
            needs_initial_prompt.then(|| spec_card_id.clone()),
            spec_push::TurnWatchdogConfig::default(),
        );
        let wave_id = wave.id.clone();
        let _push_guard = self.dispatcher.push_lock(&wave_id).await;
        install_spec_push_sinks_and_park_parts(
            self.dispatcher.as_ref(),
            &self.spec_push,
            self.aspects.as_ref(),
            &spec_card_id,
            wave,
            handle,
        )
        .await;
        Ok(SpecPushDaemonArgs {
            thread_id,
            sock_uri: self.shared_codex_appserver.remote_uri(),
            developer_instructions: needs_initial_prompt.then(|| {
                render_system_prompt(SeededCardRole::Spec.prompt_template(), wave.id.as_str())
            }),
        })
    }

    async fn spawn_spec_tui(
        &self,
        output: &TxOutput,
        wave: &Wave,
        env: Value,
        mcp_token: Option<String>,
        push_args: SpecPushDaemonArgs,
        ctx: &SpawnCtx,
    ) -> Result<SpawnHandle> {
        let spec_card_id = output_string(output, "spec_card_id")?;
        let terminal_id = output_string(output, "terminal_id")?;

        #[cfg(feature = "fixtures")]
        if let Some(hook) = &self.spawn_hook {
            return hook(
                terminal_id.clone(),
                push_args.command_line(),
                wave.cwd.clone(),
                env,
            )
            .await;
        }

        seed_and_spawn_spec_daemon_parts(
            ctx.repo.as_ref(),
            ctx.daemon.as_ref(),
            ctx.terminal_renderer.as_ref(),
            spec_card_id,
            wave.id.to_string(),
            wave.cwd.clone(),
            env,
            mcp_token,
            push_args,
        )
        .await?;
        Ok(SpawnHandle {
            terminal_id: terminal_id.clone(),
            renderer_id: terminal_id,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SharedDaemonSpawnPayload {
    pub wave_id: String,
    pub spec_card_id: String,
    pub terminal_id: String,
    pub wave_title: String,
    pub wave_cwd: String,
    #[serde(default)]
    pub env: Value,
    #[serde(default)]
    pub mcp_token: Option<String>,
    pub needs_initial_prompt: bool,
}

#[async_trait]
impl ProviderAdapter for SharedDaemonSpawnAdapter {
    fn kind(&self) -> &'static str {
        "shared-daemon-spawn"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        SHARED_DAEMON_SPAWN_PHASES
    }

    fn app_server_interact_kind(
        &self,
        output: &TxOutput,
        _op: &Operation,
    ) -> Result<AppServerInteractKind> {
        if output_string(output, "wave_title")?.trim().is_empty() {
            Ok(AppServerInteractKind::RegisterPending { entry_id: None })
        } else {
            Ok(AppServerInteractKind::MintAndAwait { thread_id: None })
        }
    }

    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: SharedDaemonSpawnPayload = serde_json::from_value(input.clone())?;
        let Some(wave) = self.repo.wave_get(&payload.wave_id).await? else {
            return Err(CalmError::NotFound(format!("wave {}", payload.wave_id)));
        };
        let role = self.repo.card_role_get(&payload.spec_card_id).await?;
        if role != Some(CardRole::Spec) {
            return Err(CalmError::BadRequest(format!(
                "card {} is not the spec card for wave {}",
                payload.spec_card_id, payload.wave_id
            )));
        }
        let Some(card) = self.repo.card_get(&payload.spec_card_id).await? else {
            return Err(CalmError::NotFound(format!(
                "card {}",
                payload.spec_card_id
            )));
        };
        if card.wave_id != wave.id {
            return Err(CalmError::BadRequest(format!(
                "spec card {} does not belong to wave {}",
                payload.spec_card_id, payload.wave_id
            )));
        }
        let Some(terminal) = self
            .repo
            .terminal_get_by_card(&payload.spec_card_id)
            .await?
        else {
            return Err(CalmError::Internal(format!(
                "spec terminal row missing for card {}",
                payload.spec_card_id
            )));
        };
        if terminal.id != payload.terminal_id || terminal.card_id != card.id {
            return Err(CalmError::BadRequest(format!(
                "terminal {} does not belong to spec card {}",
                payload.terminal_id, payload.spec_card_id
            )));
        }
        if !self.shared_codex_appserver.is_running() {
            return Err(CalmError::Internal(
                "shared codex app-server is not running".into(),
            ));
        }
        Ok(())
    }

    async fn prepare_tx<'tx>(
        &self,
        _tx: &mut Tx<'tx>,
        input: &Value,
        _op: &Operation,
    ) -> Result<TxOutput> {
        let payload: SharedDaemonSpawnPayload = serde_json::from_value(input.clone())?;
        let mut output = TxOutput::new(
            "wave",
            Some(payload.wave_id.clone()),
            json!({
                "type": "wave",
                "id": payload.wave_id,
                "spec_card_id": payload.spec_card_id,
                "terminal_id": payload.terminal_id,
            }),
        );
        output.data = json!({
            "wave_id": payload.wave_id,
            "spec_card_id": payload.spec_card_id,
            "terminal_id": payload.terminal_id,
            "wave_title": payload.wave_title,
            "wave_cwd": payload.wave_cwd,
            "env": payload.env,
            "mcp_token": payload.mcp_token,
            "needs_initial_prompt": payload.needs_initial_prompt,
        });
        Ok(output)
    }

    async fn app_server_interact(
        &self,
        output: &mut TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        let wave_id = output_string(output, "wave_id")?;
        let spec_card_id = output_string(output, "spec_card_id")?;
        let wave = ctx
            .repo
            .wave_get(&wave_id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("wave {wave_id}")))?;
        let wave_title = output_string(output, "wave_title")?;

        if !wave_title.trim().is_empty() {
            let mut notifications = self.shared_codex_appserver.subscribe_notifications();
            let status = new_spec_status(false);
            let thread_id = match output_optional_string(output, "codex_thread_id")?
                .or_else(|| phase_minted_thread_id(&op.phase))
            {
                Some(thread_id) => thread_id,
                None => {
                    if let Some(row) = ctx
                        .repo
                        .card_codex_thread_get_by_card(&spec_card_id)
                        .await?
                    {
                        row.thread_id
                    } else {
                        self.shared_codex_appserver
                            .thread_start_for_card(
                                &spec_card_id,
                                CardRole::Spec,
                                Some(wave_id.as_str()),
                                SharedThreadStartParams {
                                    cwd: output_string(output, "wave_cwd")?,
                                    approval_policy: "never".into(),
                                    sandbox_mode: "workspace-write".into(),
                                    developer_instructions: Some(render_system_prompt(
                                        SeededCardRole::Spec.prompt_template(),
                                        wave_id.as_str(),
                                    )),
                                },
                            )
                            .await?
                    }
                }
            };
            {
                let mut guard = status.lock().await;
                guard.last_thread_id = Some(thread_id.clone());
            }
            set_output_data(output, "codex_thread_id", json!(thread_id.clone()))?;
            self.persist_prompt_thread(ctx, op, output, &wave, &thread_id)
                .await?;

            if output_optional_i64(output, "turn_started_at_ms")?.is_none() {
                self.shared_codex_appserver
                    .turn_start(&thread_id, vec![InputItem::text(wave_title.trim())])
                    .await?;
                set_output_data(output, "turn_started_at_ms", json!(now_ms()))?;
                self.checkpoint_prompt_turn_started(ctx, op, output, &thread_id)
                    .await?;
            }
            await_shared_spec_initial_turn_lifecycle(&mut notifications, &thread_id, &status)
                .await?;
            Ok(AppServerInteractOutcome::MintedAndAwaited { thread_id })
        } else {
            self.shared_codex_appserver
                .ensure_respawn_for_current_settings()
                .await?;
            set_output_data(output, "pending_registered", json!(true))?;
            self.persist_pending_thread(ctx, op, output, &wave).await?;
            Ok(
                AppServerInteractOutcome::RegisteredPendingForLaterAttribution {
                    entry_id: spec_card_id,
                },
            )
        }
    }

    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        _op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnHandle> {
        let wave_id = output_string(output, "wave_id")?;
        let spec_card_id = output_string(output, "spec_card_id")?;
        let terminal_id = output_string(output, "terminal_id")?;
        let wave = ctx
            .repo
            .wave_get(&wave_id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("wave {wave_id}")))?;
        let env = spec_tui_env(output, self.codex.as_ref())?;
        let mcp_token = output_optional_string(output, "mcp_token")?;
        let needs_initial_prompt = output_bool(output, "needs_initial_prompt")?;

        if needs_initial_prompt {
            let _pending_spawn_serial_guard = self.pending_codex_threads_spawn_serial.lock().await;
            self.pending_codex_threads
                .register(
                    PendingEntry::new(
                        spec_card_id.clone(),
                        Some(wave_id.clone()),
                        terminal_id.clone(),
                    )
                    .with_role(CardRole::Spec),
                )
                .await?;
            let notifications = self.shared_codex_appserver.subscribe_notifications();
            let status = new_spec_status(true);
            let push_args = self
                .park_handle(output, &wave, notifications, status)
                .await?;
            return self
                .spawn_spec_tui(output, &wave, env, mcp_token, push_args, ctx)
                .await;
        }

        let notifications = self.shared_codex_appserver.subscribe_notifications();
        let status = new_spec_status(false);
        if let Some(thread_id) = output_optional_string(output, "codex_thread_id")? {
            let mut guard = status.lock().await;
            guard.last_thread_id = Some(thread_id);
        }
        let push_args = self
            .park_handle(output, &wave, notifications, status)
            .await?;
        self.spawn_spec_tui(output, &wave, env, mcp_token, push_args, ctx)
            .await
    }

    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        output: &TxOutput,
        _op: &Operation,
    ) -> Result<CompensationStateVersioned> {
        let wave_id = output_string(output, "wave_id")?;
        let spec_card_id = output_string(output, "spec_card_id")?;
        let terminal_id = output_string(output, "terminal_id")?;
        let mut steps = vec![step(
            "reap_terminal_pty",
            json!({ "terminal_id": terminal_id }),
        )];
        if output_bool(output, "needs_initial_prompt")? {
            steps.extend([
                step(
                    "remove_pending_codex_thread_entry",
                    json!({ "card_id": spec_card_id.clone() }),
                ),
                step(
                    "clear_shared_spec_runtime_fields",
                    json!({ "card_id": spec_card_id.clone(), "wave_id": wave_id.clone() }),
                ),
                step(
                    "remove_spec_push_handle",
                    json!({ "wave_id": wave_id.clone() }),
                ),
                step(
                    "runtime_set_status_failed_for_card",
                    json!({ "card_id": spec_card_id }),
                ),
            ]);
        } else {
            steps.extend([
                step(
                    "interrupt_shared_codex_turn",
                    json!({
                        "card_id": spec_card_id.clone(),
                        "thread_id": output_optional_string(output, "codex_thread_id")?,
                    }),
                ),
                step(
                    "delete_card_codex_thread_by_card_id",
                    json!({ "card_id": spec_card_id.clone() }),
                ),
                step(
                    "clear_shared_spec_runtime_fields",
                    json!({ "card_id": spec_card_id.clone(), "wave_id": wave_id.clone() }),
                ),
                step(
                    "remove_spec_push_handle",
                    json!({ "wave_id": wave_id.clone() }),
                ),
                step(
                    "runtime_set_status_failed_for_card",
                    json!({ "card_id": spec_card_id }),
                ),
            ]);
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
            "reap_terminal_pty" => {
                let terminal_id = step_arg_string(step, "terminal_id")?;
                if let Some(term) = ctx.repo.terminal_get(&terminal_id).await? {
                    reap_terminal_artifacts_with_renderer(
                        Some(ctx.terminal_renderer.as_ref()),
                        &term,
                    )
                    .await;
                }
                Ok(())
            }
            "interrupt_shared_codex_turn" => {
                let card_id = step_arg_string(step, "card_id")?;
                let thread_id =
                    if let Some(thread_id) = step.args.get("thread_id").and_then(Value::as_str) {
                        Some(thread_id.to_string())
                    } else {
                        ctx.repo
                            .card_codex_thread_get_by_card(&card_id)
                            .await?
                            .map(|row| row.thread_id)
                    };
                if let Some(thread_id) = thread_id
                    && let Err(e) = self
                        .shared_codex_appserver
                        .interrupt_active_turn(&thread_id)
                        .await
                {
                    tracing::warn!(
                        target: "operation::shared_daemon_spawn::compensation",
                        card_id = %card_id,
                        thread_id = %thread_id,
                        error = %e,
                        "could not interrupt active shared spec turn during compensation"
                    );
                }
                Ok(())
            }
            "delete_card_codex_thread_by_card_id" => {
                let card_id = step_arg_string(step, "card_id")?;
                ctx.repo.card_codex_thread_delete_by_card(&card_id).await?;
                Ok(())
            }
            "remove_pending_codex_thread_entry" => {
                let card_id = step_arg_string(step, "card_id")?;
                self.pending_codex_threads.remove_by_card(&card_id).await;
                Ok(())
            }
            "clear_shared_spec_runtime_fields" => {
                let card_id = step_arg_string(step, "card_id")?;
                let wave_id = step_arg_string(step, "wave_id")?;
                clear_shared_spec_runtime_fields(
                    ctx,
                    &self.card_role_cache,
                    &self.wave_cove_cache,
                    &card_id,
                    &wave_id,
                )
                .await
            }
            "remove_spec_push_handle" => {
                let wave_id = step_arg_string(step, "wave_id")?;
                self.spec_push.remove(&wave_id.into());
                Ok(())
            }
            "runtime_set_status_failed_for_card" => {
                let card_id = step_arg_string(step, "card_id")?;
                ctx.repo
                    .runtime_complete_for_card(&card_id, RunStatus::Failed)
                    .await?;
                Ok(())
            }
            other => Err(CalmError::Internal(format!(
                "unknown shared-daemon-spawn compensation op {other}"
            ))),
        }
    }
}

async fn persist_shared_spec_runtime_fields_tx(
    tx: &mut Tx<'_>,
    spec_card_id: &str,
    wave: &Wave,
    remote_uri: &str,
    terminal_run_id: Option<&str>,
    thread_id: Option<&str>,
) -> Result<crate::model::Card> {
    let mut payload = card_payload_get_tx(tx, spec_card_id).await?;
    let Some(map) = payload.as_object_mut() else {
        return Err(CalmError::Internal(format!(
            "spec card {spec_card_id} payload is not a JSON object; cannot persist shared codex runtime fields"
        )));
    };
    if let Some(thread_id) = thread_id {
        map.insert(
            "codex_thread_id".into(),
            Value::String(thread_id.to_string()),
        );
    } else {
        map.remove("codex_thread_id");
    }
    map.insert("codex_source".into(), Value::String("shared".into()));
    map.insert(
        "appserver_sock".into(),
        Value::String(remote_uri.to_string()),
    );
    map.remove("appserver_pgid");
    map.remove("appserver_start_time");
    map.remove("appserver_boot_id");
    map.remove("appserver_needs_initial_prompt");
    map.insert("push_watermark".into(), Value::Number(0i64.into()));

    let card = card_update_tx(
        tx,
        spec_card_id,
        CardPatch {
            kind: None,
            sort: None,
            payload: Some(payload),
            deletable: None,
        },
    )
    .await?;
    let runtime_init = RuntimeInit {
        id: crate::model::new_id(),
        card_id: spec_card_id.to_string(),
        kind: RuntimeKind::SharedSpec,
        agent_provider: Some(AgentProvider::Codex),
        status: if thread_id.is_some() {
            RunStatus::Running
        } else {
            RunStatus::TurnPending
        },
        terminal_run_id: terminal_run_id.map(str::to_string),
        thread_id: thread_id.map(str::to_string),
        session_id: None,
        active_turn_id: None,
        handle_state_json: None,
        lease_owner: None,
        lease_until_ms: None,
        now_ms: now_ms(),
    };
    if let Some(existing) = runtime_get_active_for_card_tx(tx, spec_card_id).await? {
        runtime_supersede_tx(tx, &existing.id, runtime_init).await?;
    } else {
        runtime_start_tx(tx, runtime_init).await?;
    }
    let _ = wave;
    Ok(card)
}

async fn clear_shared_spec_runtime_fields(
    ctx: &SpawnCtx,
    card_role_cache: &CardRoleCache,
    wave_cove_cache: &WaveCoveCache,
    card_id: &str,
    wave_id: &str,
) -> Result<()> {
    let Some(wave) = ctx.repo.wave_get(wave_id).await? else {
        return Ok(());
    };
    let Some(card) = ctx.repo.card_get(card_id).await? else {
        return Ok(());
    };
    let mut payload = card.payload;
    let Some(map) = payload.as_object_mut() else {
        return Ok(());
    };
    map.remove("codex_source");
    map.remove("codex_thread_id");
    map.remove("appserver_sock");
    map.remove("appserver_pgid");
    map.remove("appserver_start_time");
    map.remove("appserver_boot_id");
    map.remove("appserver_needs_initial_prompt");
    map.remove("push_watermark");

    let scope = EventScope::Card {
        card: card_id.to_string().into(),
        wave: wave.id.clone(),
        cove: wave.cove_id.clone(),
    };
    let write = WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone());
    let card_id_for_tx = card_id.to_string();
    let payload_for_tx = payload;
    let (_updated, _id) = write_with_event_typed(
        ctx.repo.as_ref(),
        ActorId::Kernel,
        scope,
        None,
        &ctx.events,
        &write,
        move |tx| {
            Box::pin(async move {
                let card = card_update_tx(
                    tx,
                    &card_id_for_tx,
                    CardPatch {
                        kind: None,
                        sort: None,
                        payload: Some(payload_for_tx),
                        deletable: None,
                    },
                )
                .await?;
                Ok((card.clone(), Event::CardUpdated(card)))
            })
        },
    )
    .await?;
    Ok(())
}

async fn card_payload_get_tx(tx: &mut Tx<'_>, card_id: &str) -> Result<Value> {
    let row: Option<(String,)> = sqlx::query_as("SELECT payload FROM cards WHERE id = ?1")
        .bind(card_id)
        .fetch_optional(&mut **tx)
        .await?;
    let payload_text = row
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?
        .0;
    serde_json::from_str(&payload_text)
        .map_err(|e| CalmError::Internal(format!("card {card_id} payload is not valid JSON: {e}")))
}

fn new_spec_status(needs_initial_prompt: bool) -> SharedStatus {
    if needs_initial_prompt {
        Arc::new(tokio::sync::Mutex::new(SpecPushStatus {
            phase: SpecPushPhase::PendingThreadStart,
            last_thread_id: None,
            last_turn_id: None,
        }))
    } else {
        Arc::new(tokio::sync::Mutex::new(SpecPushStatus::default()))
    }
}

fn spec_tui_env(output: &TxOutput, codex: &CodexClient) -> Result<Value> {
    let mut env = output.data.get("env").cloned().unwrap_or_else(|| json!({}));
    let map = env
        .as_object_mut()
        .ok_or_else(|| CalmError::Internal("shared-daemon-spawn env must be an object".into()))?;
    map.insert(
        "CODEX_HOME".into(),
        Value::String(codex.codex_home_dir().to_string_lossy().to_string()),
    );
    Ok(env)
}

fn output_string(output: &TxOutput, key: &str) -> Result<String> {
    output
        .data
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| CalmError::Internal(format!("shared-daemon-spawn tx_output missing {key}")))
}

fn output_optional_string(output: &TxOutput, key: &str) -> Result<Option<String>> {
    match output.data.get(key) {
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(CalmError::Internal(format!(
            "shared-daemon-spawn tx_output {key} must be string or null"
        ))),
    }
}

fn output_optional_i64(output: &TxOutput, key: &str) -> Result<Option<i64>> {
    match output.data.get(key) {
        Some(Value::Number(value)) => value.as_i64().map(Some).ok_or_else(|| {
            CalmError::Internal(format!(
                "shared-daemon-spawn tx_output {key} must be a signed integer"
            ))
        }),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(CalmError::Internal(format!(
            "shared-daemon-spawn tx_output {key} must be a signed integer or null"
        ))),
    }
}

fn output_bool(output: &TxOutput, key: &str) -> Result<bool> {
    output
        .data
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| CalmError::Internal(format!("shared-daemon-spawn tx_output missing {key}")))
}

fn set_output_data(output: &mut TxOutput, key: &str, value: Value) -> Result<()> {
    let data = output.data.as_object_mut().ok_or_else(|| {
        CalmError::Internal("shared-daemon-spawn tx_output data is not an object".into())
    })?;
    data.insert(key.to_string(), value);
    Ok(())
}

fn phase_minted_thread_id(phase: &Phase) -> Option<String> {
    match phase {
        Phase::AppServerInteract {
            kind:
                AppServerInteractKind::MintAndAwait {
                    thread_id: Some(thread_id),
                },
        } => Some(thread_id.clone()),
        _ => None,
    }
}

fn step(op: &str, args: Value) -> CompensationStep {
    CompensationStep {
        op: op.to_string(),
        args,
        completed: false,
        attempts: 0,
        last_error: None,
    }
}

fn step_arg_string(step: &CompensationStep, key: &str) -> Result<String> {
    step.args
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            CalmError::Internal(format!(
                "shared-daemon-spawn compensation step {} missing {key}",
                step.op
            ))
        })
}
