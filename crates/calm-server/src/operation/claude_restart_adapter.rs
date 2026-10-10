use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::card_role_cache::CardRoleCache;
use crate::db::sqlite::{
    append_decision_event_in_tx, card_is_worker_spawn_target_tx, session_complete_tx,
    session_projection_active_for_card_tx, session_projection_projectable_for_card_tx,
    session_set_status_tx, session_start_runtime_tx, terminal_create_tx, terminal_get_by_card_tx,
    worker_card_declared_head_tx,
};
use crate::db::write_with_events_typed;
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, SYNC_EVENT_VERSION};
use crate::ids::{ActorId, CardId, TrackId};
use crate::mcp_server::McpServer;
use crate::model::new_id;
use crate::operation::claude_adapter::{
    CLAUDE_CARD_PERMISSION_FLAGS, CLAUDE_PHASES, CLAUDE_WORKER_PERMISSION_FLAGS, build_claude_env,
    worker_mcp,
};
use crate::operation::workspace_lease::worker::{
    record_declared_head, verify_declared_head, verify_recorded_head,
};
use crate::routes::cards::{card_scope, card_scope_tx};
use crate::routes::claude_cards::{build_claude_settings_json, claude_hook_command};
use crate::routes::codex_cards::shell_single_quote;
use crate::routes::theme::RequestTheme;
use crate::session_projection_lookup::{claude_session_of_runtime, legacy_claude_session_of_card};
use crate::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use crate::state::{CodexClient, WriteContext};
use crate::track_area_cache::TrackAreaCache;
use calm_truth::model::NewTerminal;

use super::{
    AppServerInteractOutcome, CompensationStateVersioned, CompensationStep, Operation,
    OperationKey, OperationOutcome, OperationRuntime, PhaseTag, ProviderAdapter, SpawnCtx,
    SpawnOutcome, Tx, TxOutput,
};

#[cfg(feature = "fixtures")]
use super::SpawnHandle;
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
    /// A task worker's card restarts with the kernel MCP server, as it first started.
    mcp_server: Option<Arc<McpServer>>,
    card_role_cache: CardRoleCache,
    track_area_cache: TrackAreaCache,
    #[cfg(feature = "fixtures")]
    spawn_hook: Option<SpawnHook>,
}

impl ClaudeRestartAdapter {
    pub fn new(
        repo: Arc<dyn crate::db::RouteRepo>,
        codex: Arc<CodexClient>,
        mcp_server: Option<Arc<McpServer>>,
        card_role_cache: CardRoleCache,
        track_area_cache: TrackAreaCache,
    ) -> Self {
        Self {
            repo,
            codex,
            mcp_server,
            card_role_cache,
            track_area_cache,
            #[cfg(feature = "fixtures")]
            spawn_hook: None,
        }
    }

    #[cfg(feature = "fixtures")]
    pub fn new_with_spawn_hook(
        repo: Arc<dyn crate::db::RouteRepo>,
        codex: Arc<CodexClient>,
        mcp_server: Option<Arc<McpServer>>,
        card_role_cache: CardRoleCache,
        track_area_cache: TrackAreaCache,
        spawn_hook: SpawnHook,
    ) -> Self {
        Self {
            repo,
            codex,
            mcp_server,
            card_role_cache,
            track_area_cache,
            spawn_hook: Some(spawn_hook),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClaudeRestartOperationPayload {
    pub actor: ActorId,
    #[serde(default)]
    /// Wire key frozen as `runtime_id`: stored payloads keep the old key, and without the `rename` a parked row with a real id
    /// would deserialize to `None` and `unwrap_or_else(new_id)` below would mint a FRESH session id for it.
    #[serde(rename = "runtime_id")]
    pub worker_session_id: Option<String>,
    pub card_id: String,
}

/// One unkeyed `claude-restart` of `card_id`, submitted and awaited: the restart of a card whose
/// child is gone, shared by the card's Update route and the boot auto-resume (#2516).
pub async fn run_claude_restart(
    runtime: &OperationRuntime,
    actor: ActorId,
    card_id: String,
) -> Result<OperationOutcome> {
    let payload_hash = crate::routes::idempotency_key::stable_payload_hash(&json!({
        "actor": &actor,
        "card_id": &card_id,
    }))?;
    let payload = serde_json::to_value(ClaudeRestartOperationPayload {
        actor,
        worker_session_id: Some(new_id()),
        card_id,
    })?;
    let op_id = runtime
        .submit(
            "claude-restart",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: None,
                payload_hash,
            },
            payload,
        )
        .await?;
    Ok(runtime.wait(&op_id).await?.outcome)
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

        // #1933: a resumed reader is refused before anything is written while its checkout is not
        // at its declared head; the spawn checks again for a re-drive.
        let declared_head = worker_card_declared_head_tx(tx, &card_id).await?;
        if let Some(head) = declared_head.as_deref() {
            let term = terminal_get_by_card_tx(tx, &card_id)
                .await?
                .ok_or_else(|| {
                    CalmError::Conflict(format!(
                        "refused: worker card {card_id} has no terminal to check its declared head in"
                    ))
                })?;
            verify_declared_head(Path::new(&term.cwd), head)?;
        }
        // Read in this transaction: a SessionStart hook moves the session id under the same write
        // lock (#2516), so the restart resumes exactly what the card last started.
        let projectable = session_projection_projectable_for_card_tx(tx, &card_id).await?;
        let claude_session_id = claude_session_of_runtime(projectable.as_ref())
            .or_else(|| legacy_claude_session_of_card(&card_id, projectable.as_ref(), Some(&card)))
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
        if let Some(active) = session_projection_active_for_card_tx(tx, &card_id).await? {
            // Claude runtimes only reach Starting/Running here; Idle/TurnPending are not part of the Claude state machine.
            if matches!(
                active.status,
                WorkerSessionState::Starting | WorkerSessionState::Running
            ) {
                return Err(CalmError::Conflict(
                    "kill or wait for child exit before restart".into(),
                ));
            }
            session_complete_tx(tx, &active.id, WorkerSessionState::Exited).await?;
        }

        // A task worker's card gets the permission mode (#2521) and the MCP servers (#2470) it
        // first started with. `exec` as at create. `--resume=<id>` is one token: the id comes
        // from a hook payload, so it must stay the option's value, never be read as another
        // option (#2516).
        let is_worker = card_is_worker_spawn_target_tx(tx, &card_id).await?;
        let mut command_line = format!(
            "exec {} {} --settings {} --resume={}",
            shell_single_quote(&self.codex.claude_bin),
            if is_worker {
                CLAUDE_WORKER_PERMISSION_FLAGS
            } else {
                CLAUDE_CARD_PERMISSION_FLAGS
            },
            shell_single_quote(&settings_path),
            shell_single_quote(&claude_session_id),
        );
        let mcp_config = if is_worker {
            let path = worker_mcp::mcp_config_path(Path::new(&settings_path))?;
            command_line.push_str(&worker_mcp::mcp_flags(&path));
            Some(path)
        } else {
            None
        };
        let env = build_claude_env(self.repo.as_ref(), &self.codex, &card_id).await?;
        let term = match terminal_get_by_card_tx(tx, &card_id).await? {
            Some(term) => term,
            None => {
                // A payload cwd still wins; the fallback is the track's workspace, never the kernel process's own directory, and an empty workspace is a hard error.
                let cwd = crate::operation::terminal_adapter::terminal_cwd_or_track_workspace(
                    tx,
                    card.track_id.as_str(),
                    card.payload
                        .get("cwd")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(ToOwned::to_owned),
                )
                .await?;
                terminal_create_tx(
                    tx,
                    NewTerminal {
                        card_id: card.id.clone(),
                        program: command_line.clone(),
                        cwd,
                        env: env.clone(),
                        theme: RequestTheme::default_dark(),
                    },
                )
                .await?
            }
        };
        let runtime_id = payload.worker_session_id.clone().unwrap_or_else(new_id);
        session_start_runtime_tx(
            tx,
            WorkerSessionInit {
                id: runtime_id.clone(),
                card_id: card_id.clone(),
                kind: WorkerSessionKind::ClaudeCard,
                agent_provider: Some(AgentProvider::Claude),
                status: WorkerSessionState::Starting,
                terminal_run_id: Some(term.id.clone()),
                thread_id: None,
                session_id: Some(claude_session_id.clone()),
                active_turn_id: None,
                handle_state_json: None,
                spawn_op_id: None,
                now_ms: crate::model::now_ms(),
            },
        )
        .await?;

        // `card_scope_tx`, NOT `card_scope`: this transaction may hold the write lock on `tracks` (the terminal row above froze the workspace), so resolving through the pool would deadlock the task against itself.
        let scope = card_scope_tx(tx, CardId::from(card_id.clone()), card.track_id.clone()).await?;
        let runtime_event = Event::WorkerSessionStarted {
            worker_session_id: runtime_id.clone(),
            card_id: card_id.clone(),
            kind: WorkerSessionKind::ClaudeCard,
            agent_provider: Some(AgentProvider::Claude),
            status: WorkerSessionState::Starting,
        };
        let runtime_event_id =
            append_decision_event_in_tx(tx, &payload.actor, &scope, None, &runtime_event).await?;

        // Preserve the previous exit row so compensation can restore the Restart affordance if the replacement spawn fails.
        let prev_exit_code = term.exit_code;
        let prev_signal_killed = term.signal_killed;
        let prev_pty_output = term.pty_output.clone();
        let prev_pty_output_truncated = term.pty_output_truncated;
        let mut output = TxOutput::new(
            "runtime",
            Some(runtime_id.clone()),
            serde_json::to_value(card)?,
        );
        output.data = json!({
            "card_id": card_id,
            "runtime_id": runtime_id,
            "track_id": scope.track_id().map(|id| id.as_str().to_string()),
            "terminal_id": term.id,
            "settings_path": settings_path,
            "claude_session_id": claude_session_id,
            "command_line": command_line,
            "mcp_config_path": mcp_config,
            "cwd": term.cwd,
            "env": env,
            "prev_exit_code": prev_exit_code,
            "prev_signal_killed": prev_signal_killed,
            "prev_pty_output": prev_pty_output,
            "prev_pty_output_truncated": prev_pty_output_truncated,
        });
        record_declared_head(&mut output.data, declared_head.as_deref());
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
    ) -> Result<SpawnOutcome> {
        let card_id = output.output_string("card_id", "claude restart")?;
        let worker_session_id = output.output_string("runtime_id", "claude restart")?;
        let terminal_id = output.output_string("terminal_id", "claude restart")?;
        let settings_path = PathBuf::from(output.output_string("settings_path", "claude restart")?);
        let settings_dir = settings_path_parent(&settings_path)?;
        let command_line = output.output_string("command_line", "claude restart")?;
        let cwd = output.output_string("cwd", "claude restart")?;
        let mut env = output.data.get("env").cloned().unwrap_or_else(|| json!({}));
        // #1933: a resumed reader starts only while its checkout is still at its declared head.
        verify_recorded_head(output, "claude restart")?;
        let kernel_mcp = match output.output_optional_string("mcp_config_path", "claude restart")? {
            Some(path) => Some((
                self.mcp_server.as_deref().ok_or_else(|| {
                    CalmError::Internal(
                        "MCP server is not running; claude worker cannot restart".into(),
                    )
                })?,
                PathBuf::from(path),
            )),
            None => None,
        };

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
        if let Some((mcp_server, mcp_config)) = &kernel_mcp {
            worker_mcp::wire(
                ctx,
                mcp_server,
                &card_id,
                &worker_session_id,
                mcp_config,
                &mut env,
            )
            .await?;
        }

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
                    let existing = ctx.repo.session_projection_active_for_card(&card_id).await?;
                    let needs_status_write = existing
                        .as_ref()
                        .map(|runtime| runtime.status != WorkerSessionState::Running)
                        .unwrap_or(true);
                    if !needs_status_write {
                        return Ok(());
                    }

                    let track_id =
                        if let Some(track_id) = output.data.get("track_id").and_then(Value::as_str) {
                            TrackId::from(track_id.to_string())
                        } else {
                            ctx.repo
                                .card_get(&card_id)
                                .await?
                                .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?
                                .track_id
                        };
                    let scope =
                        card_scope(ctx.repo.as_ref(), CardId::from(card_id.clone()), track_id)
                            .await?;
                    let write = WriteContext::new(
                        self.card_role_cache.clone(),
                        self.track_area_cache.clone(),
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
                                    session_projection_active_for_card_tx(tx, &card_id_for_tx)
                                        .await?
                                        .ok_or_else(|| {
                                            CalmError::Internal(format!(
                                                "claude card {card_id_for_tx} has no active runtime to mark running"
                                            ))
                                        })?;
                                let old_status = runtime.status;
                                let runtime_id = runtime.id.clone();
                                session_set_status_tx(tx, &runtime.id, WorkerSessionState::Running)
                                    .await?;
                                Ok((
                                    (),
                                    vec![(
                                        scope,
                                        Event::WorkerSessionStatusChanged {
                                            worker_session_id: runtime_id,
                                            card_id: card_id_for_tx,
                                            old_status,
                                            new_status: WorkerSessionState::Running,
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
                Ok(SpawnOutcome::Ready(handle))
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
        let card_id = output.output_string("card_id", "claude restart")?;
        let terminal_id = output.output_string("terminal_id", "claude restart")?;
        let prev_exit_code = output_optional_i32(output, "prev_exit_code");
        let prev_signal_killed = output_bool(output, "prev_signal_killed");
        let prev_pty_output = output
            .data
            .get("prev_pty_output")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let prev_pty_output_truncated = output_bool(output, "prev_pty_output_truncated");
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.to_string(),
            steps: vec![
                CompensationStep {
                    op: "session_projection_set_status_failed_for_card".into(),
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
                        "prev_pty_output": prev_pty_output,
                        "prev_pty_output_truncated": prev_pty_output_truncated,
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
            // Back-compat: accept the legacy op string during recovery so in-flight compensation states still drain.
            "session_projection_set_status_failed_for_card"
            | "runtime_set_status_failed_for_card" => {
                let card_id = step_arg_string(step, "card_id")?;
                ctx.repo
                    .session_projection_complete_for_card(&card_id, WorkerSessionState::Failed)
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
                let prev_pty_output = step
                    .args
                    .get("prev_pty_output")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let prev_pty_output_truncated = step
                    .args
                    .get("prev_pty_output_truncated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                ctx.repo
                    .terminal_set_exit_with_output(
                        &terminal_id,
                        prev_exit_code,
                        prev_signal_killed,
                        prev_pty_output,
                        prev_pty_output_truncated,
                    )
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
