use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::card_role_cache::CardRoleCache;
use crate::db::sqlite::{
    event_append_for_operation_tx, runtime_complete_tx, runtime_get_active_for_card_tx,
    runtime_set_status_tx, runtime_start_tx, terminal_get_by_card_tx,
};
use crate::db::write_with_events_typed;
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, SYNC_EVENT_VERSION};
use crate::ids::{ActorId, CardId, WaveId};
use crate::model::new_id;
use crate::operation::claude_adapter::{CLAUDE_PHASES, build_claude_env};
use crate::routes::cards::card_scope;
use crate::routes::claude_cards::{build_claude_settings_json, claude_hook_command};
use crate::routes::codex_cards::shell_single_quote;
use crate::runtime_lookup::resolve_claude_session_for_card;
use crate::runtime_repo::{AgentProvider, RunStatus, RuntimeInit, RuntimeKind};
use crate::state::{CodexClient, WriteContext};
use crate::wave_cove_cache::WaveCoveCache;

use super::{
    AppServerInteractOutcome, CompensationStateVersioned, CompensationStep, Operation, PhaseTag,
    ProviderAdapter, SpawnCtx, SpawnHandle, Tx, TxOutput,
};

#[cfg(feature = "fixtures")]
use futures::future::BoxFuture;

#[cfg(feature = "fixtures")]
type SpawnHook = Arc<
    dyn Fn(String, String, String, Value) -> BoxFuture<'static, Result<SpawnHandle>> + Send + Sync,
>;

#[derive(Clone)]
pub struct ClaudeRestartAdapter {
    repo: Arc<dyn crate::db::RouteRepo>,
    codex: Arc<CodexClient>,
    card_role_cache: CardRoleCache,
    wave_cove_cache: WaveCoveCache,
    #[cfg(feature = "fixtures")]
    spawn_hook: Option<SpawnHook>,
}

impl ClaudeRestartAdapter {
    pub fn new(
        repo: Arc<dyn crate::db::RouteRepo>,
        codex: Arc<CodexClient>,
        card_role_cache: CardRoleCache,
        wave_cove_cache: WaveCoveCache,
    ) -> Self {
        Self {
            repo,
            codex,
            card_role_cache,
            wave_cove_cache,
            #[cfg(feature = "fixtures")]
            spawn_hook: None,
        }
    }

    #[cfg(feature = "fixtures")]
    pub fn new_with_spawn_hook(
        repo: Arc<dyn crate::db::RouteRepo>,
        codex: Arc<CodexClient>,
        card_role_cache: CardRoleCache,
        wave_cove_cache: WaveCoveCache,
        spawn_hook: SpawnHook,
    ) -> Self {
        Self {
            repo,
            codex,
            card_role_cache,
            wave_cove_cache,
            spawn_hook: Some(spawn_hook),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClaudeRestartOperationPayload {
    pub actor: ActorId,
    #[serde(default)]
    pub runtime_id: Option<String>,
    pub card_id: String,
}

#[async_trait]
impl ProviderAdapter for ClaudeRestartAdapter {
    fn kind(&self) -> &'static str {
        "claude-restart"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        CLAUDE_PHASES
    }

    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: ClaudeRestartOperationPayload = serde_json::from_value(input.clone())?;
        if payload.card_id.trim().is_empty() {
            return Err(CalmError::BadRequest("card_id is required".into()));
        }
        Ok(())
    }

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        _op: &Operation,
    ) -> Result<TxOutput> {
        let payload: ClaudeRestartOperationPayload = serde_json::from_value(input.clone())?;
        let card_id = payload.card_id.trim().to_string();
        let card = self
            .repo
            .card_get(&card_id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
        if card.kind != "claude" {
            return Err(CalmError::Forbidden(format!(
                "card {card_id} is not a Claude card"
            )));
        }

        let claude_session_id = resolve_claude_session_for_card(self.repo.as_ref(), &card_id)
            .await?
            .ok_or_else(|| {
                CalmError::Forbidden("Claude card has no resumable session id".into())
            })?;
        let settings_path = card
            .payload
            .get("settings_path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| CalmError::Forbidden("Claude card has no settings_path".into()))?;
        let term = terminal_get_by_card_tx(tx, &card_id)
            .await?
            .ok_or_else(|| CalmError::Forbidden("Claude card has no terminal".into()))?;

        if let Some(active) = runtime_get_active_for_card_tx(tx, &card_id).await? {
            // Claude runtimes only reach Starting/Running here today;
            // Idle/TurnPending are not part of the Claude state machine.
            if matches!(active.status, RunStatus::Starting | RunStatus::Running) {
                return Err(CalmError::Conflict(
                    "kill or wait for child exit before restart".into(),
                ));
            }
            runtime_complete_tx(tx, &active.id, RunStatus::Exited).await?;
        }

        let command_line = format!(
            "{} --allow-dangerously-skip-permissions --settings {} --resume {}",
            shell_single_quote(&self.codex.claude_bin),
            shell_single_quote(&settings_path),
            shell_single_quote(&claude_session_id),
        );
        let env = build_claude_env(self.repo.as_ref(), &self.codex, &card_id).await?;
        let runtime_id = payload.runtime_id.clone().unwrap_or_else(new_id);
        runtime_start_tx(
            tx,
            RuntimeInit {
                id: runtime_id.clone(),
                card_id: card_id.clone(),
                kind: RuntimeKind::ClaudeCard,
                agent_provider: Some(AgentProvider::Claude),
                status: RunStatus::Starting,
                terminal_run_id: Some(term.id.clone()),
                thread_id: None,
                session_id: Some(claude_session_id.clone()),
                active_turn_id: None,
                handle_state_json: None,
                lease_owner: None,
                lease_until_ms: None,
                now_ms: crate::model::now_ms(),
            },
        )
        .await?;

        let scope = card_scope(
            self.repo.as_ref(),
            CardId::from(card_id.clone()),
            card.wave_id.clone(),
        )
        .await?;
        let runtime_event = Event::RuntimeStarted {
            runtime_id: runtime_id.clone(),
            card_id: card_id.clone(),
            kind: RuntimeKind::ClaudeCard,
            agent_provider: Some(AgentProvider::Claude),
            status: RunStatus::Starting,
        };
        if let Err(violation) = crate::role_gate::enforce_role(
            &payload.actor,
            &runtime_event,
            &scope,
            &self.card_role_cache,
            &self.wave_cove_cache,
        ) {
            return Err(CalmError::Forbidden(violation.to_string()));
        }
        let runtime_event_id =
            event_append_for_operation_tx(tx, &payload.actor, &scope, None, &runtime_event).await?;

        // Preserve the previous exit row so compensation can restore the
        // Restart affordance if the replacement spawn fails.
        let prev_exit_code = term.exit_code;
        let prev_signal_killed = term.signal_killed;
        let mut output = TxOutput::new(
            "runtime",
            Some(runtime_id.clone()),
            serde_json::to_value(card)?,
        );
        output.data = json!({
            "card_id": card_id,
            "runtime_id": runtime_id,
            "wave_id": scope.wave_id().map(|id| id.as_str().to_string()),
            "terminal_id": term.id,
            "settings_path": settings_path,
            "claude_session_id": claude_session_id,
            "command_line": command_line,
            "cwd": term.cwd,
            "env": env,
            "prev_exit_code": prev_exit_code,
            "prev_signal_killed": prev_signal_killed,
        });
        output.post_commit_events.push(BroadcastEnvelope {
            id: runtime_event_id,
            event_version: SYNC_EVENT_VERSION,
            actor: payload.actor,
            scope,
            event: runtime_event,
        });
        Ok(output)
    }

    async fn app_server_interact(
        &self,
        _output: &mut TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        Ok(AppServerInteractOutcome::NotApplicable)
    }

    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        _op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnHandle> {
        let card_id = output_string(output, "card_id")?;
        let terminal_id = output_string(output, "terminal_id")?;
        let settings_path = PathBuf::from(output_string(output, "settings_path")?);
        let settings_dir = settings_path_parent(&settings_path)?;
        let command_line = output_string(output, "command_line")?;
        let cwd = output_string(output, "cwd")?;
        let env = output.data.get("env").cloned().unwrap_or_else(|| json!({}));

        ctx.repo.terminal_clear_exit_for_spawn(&terminal_id).await?;
        ctx.terminal_renderer.drop_entry(&terminal_id).await;
        let term = ctx
            .repo
            .terminal_get(&terminal_id)
            .await?
            .ok_or_else(|| CalmError::Internal(format!("terminal {terminal_id} vanished")))?;
        std::fs::create_dir_all(&settings_dir).map_err(|e| {
            CalmError::Internal(format!(
                "mkdir claude settings dir {}: {e}",
                settings_dir.display()
            ))
        })?;
        let hook_command = claude_hook_command(
            &self.codex.bridge_bin.to_string_lossy(),
            &card_id,
            &self.codex.ingest_url,
        );
        std::fs::write(&settings_path, build_claude_settings_json(&hook_command))
            .map_err(|e| CalmError::Internal(format!("write claude settings.json: {e}")))?;

        #[cfg(feature = "fixtures")]
        let handle = if let Some(hook) = &self.spawn_hook {
            hook(terminal_id.clone(), command_line, cwd, env).await
        } else {
            ctx.spawn_terminal(&term, &command_line, &cwd, &env).await
        };

        #[cfg(not(feature = "fixtures"))]
        let handle = ctx.spawn_terminal(&term, &command_line, &cwd, &env).await;

        match handle {
            Ok(handle) => {
                let status_result: Result<()> = async {
                    let existing = ctx.repo.runtime_get_active_for_card(&card_id).await?;
                    let needs_status_write = existing
                        .as_ref()
                        .map(|runtime| runtime.status != RunStatus::Running)
                        .unwrap_or(true);
                    if !needs_status_write {
                        return Ok(());
                    }

                    let wave_id =
                        if let Some(wave_id) = output.data.get("wave_id").and_then(Value::as_str) {
                            WaveId::from(wave_id.to_string())
                        } else {
                            ctx.repo
                                .card_get(&card_id)
                                .await?
                                .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?
                                .wave_id
                        };
                    let scope =
                        card_scope(ctx.repo.as_ref(), CardId::from(card_id.clone()), wave_id)
                            .await?;
                    let write = WriteContext::new(
                        self.card_role_cache.clone(),
                        self.wave_cove_cache.clone(),
                    );
                    let card_id_for_tx = card_id.clone();
                    let (_unit, _ids) = write_with_events_typed(
                        ctx.repo.as_ref(),
                        ActorId::Kernel,
                        None,
                        &ctx.events,
                        &write,
                        move |tx| {
                            Box::pin(async move {
                                let runtime =
                                    runtime_get_active_for_card_tx(tx, &card_id_for_tx)
                                        .await?
                                        .ok_or_else(|| {
                                            CalmError::Internal(format!(
                                                "claude card {card_id_for_tx} has no active runtime to mark running"
                                            ))
                                        })?;
                                let old_status = runtime.status.clone();
                                let runtime_id = runtime.id.clone();
                                runtime_set_status_tx(tx, &runtime.id, RunStatus::Running)
                                    .await?;
                                Ok((
                                    (),
                                    vec![(
                                        scope,
                                        Event::RuntimeStatusChanged {
                                            runtime_id,
                                            card_id: card_id_for_tx,
                                            old_status,
                                            new_status: RunStatus::Running,
                                        },
                                    )],
                                ))
                            })
                        },
                    )
                    .await?;
                    Ok(())
                }
                .await;
                if let Err(e) = status_result {
                    tracing::warn!(
                        target: "operation::claude_restart_adapter::runtime_running_mark_failed",
                        card_id = %card_id,
                        terminal_id = %terminal_id,
                        error = %e,
                        "failed to mark claude restart runtime running after spawn; continuing operation"
                    );
                }
                Ok(handle)
            }
            Err(e) => Err(e),
        }
    }

    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        output: &TxOutput,
        _op: &Operation,
    ) -> Result<CompensationStateVersioned> {
        let card_id = output_string(output, "card_id")?;
        let terminal_id = output_string(output, "terminal_id")?;
        let prev_exit_code = output_optional_i32(output, "prev_exit_code");
        let prev_signal_killed = output_bool(output, "prev_signal_killed");
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.to_string(),
            steps: vec![
                CompensationStep {
                    op: "runtime_set_status_failed_for_card".into(),
                    args: json!({ "card_id": card_id }),
                    completed: false,
                    attempts: 0,
                    last_error: None,
                },
                CompensationStep {
                    op: "restore_terminal_exit".into(),
                    args: json!({
                        "terminal_id": terminal_id,
                        "prev_exit_code": prev_exit_code,
                        "prev_signal_killed": prev_signal_killed,
                    }),
                    completed: false,
                    attempts: 0,
                    last_error: None,
                },
            ],
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
            "runtime_set_status_failed_for_card" => {
                let card_id = step_arg_string(step, "card_id")?;
                ctx.repo
                    .runtime_complete_for_card(&card_id, RunStatus::Failed)
                    .await?;
                Ok(())
            }
            "restore_terminal_exit" => {
                let terminal_id = step_arg_string(step, "terminal_id")?;
                let prev_exit_code = step
                    .args
                    .get("prev_exit_code")
                    .and_then(|v| if v.is_null() { None } else { v.as_i64() })
                    .map(|n| n as i32);
                let prev_signal_killed = step
                    .args
                    .get("prev_signal_killed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                ctx.repo
                    .terminal_set_exit(&terminal_id, prev_exit_code, prev_signal_killed)
                    .await?;
                Ok(())
            }
            other => Err(CalmError::Internal(format!(
                "unknown claude restart compensation op {other}"
            ))),
        }
    }
}

fn settings_path_parent(path: &Path) -> Result<PathBuf> {
    path.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| CalmError::Internal("claude settings_path has no parent".into()))
}

fn output_string(output: &TxOutput, key: &str) -> Result<String> {
    output
        .data
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| CalmError::Internal(format!("claude restart tx_output missing {key}")))
}

fn output_optional_i32(output: &TxOutput, key: &str) -> Option<i32> {
    output.data.get(key).and_then(|v| {
        if v.is_null() {
            None
        } else {
            v.as_i64().map(|n| n as i32)
        }
    })
}

fn output_bool(output: &TxOutput, key: &str) -> bool {
    output
        .data
        .get(key)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn step_arg_string(step: &CompensationStep, key: &str) -> Result<String> {
    step.args
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            CalmError::Internal(format!(
                "claude restart compensation step missing {key} arg"
            ))
        })
}
