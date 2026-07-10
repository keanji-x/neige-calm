//! Emit tools for dispatching workers and recording worker outcomes.
//!
//! All three lower a JSON `arguments` object to a single eventized
//! write. The kernel translates the per-call [`ToolCallIdentity`]
//! into an [`ActorId`] (Spec → `AiSpec`, Worker → `AiCodex`) and emits
//! through `write_with_event_typed`, which runs the role gate, persists
//! the event row, and broadcasts on the bus.
//!
//! ## Tool surface
//!
//! * `calm.task.dispatch` — retired #644 compatibility shim. Hidden
//!   from tools/list; persisted pre-cutover spec threads can still call
//!   it and receive a structured migration payload. It performs no write.
//!
//! * `calm.task.complete` — Worker reports success with an opaque
//!   result + artifact list. Maps to `Event::TaskCompleted`.
//!
//! * `calm.task.fail` — Worker reports failure with a free-form
//!   reason. Maps to `Event::TaskFailed`.
//!
//! ## Scope construction
//!
//! Every emitted event's `EventScope` is anchored on the *caller's*
//! card — the kernel pulls `wave_id` + `cove_id` by looking up the
//! card row + the wave row, so the spec card's emissions land under
//! `EventScope::Card { card, wave, cove }`. Worker cards emit under
//! their own card scope; the role gate enforces that they can't
//! escape it.

use crate::decision_sink::CardDecisionSink;
use crate::error::CalmError;
use crate::event::{Event, FieldSource, ForgeEventSpec};
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    register_deprecated_alias, require_role, role_gated_write_annotations,
};
use crate::mcp_server::tools::lifecycle_args::{lifecycle_schema, message_schema};
use crate::mcp_server::transport::{PluginForgePayload, submit_forge_action};
use crate::model::CardRole;
use crate::operation::forge_action_adapter::ProbeSpec;
use crate::operation::workspace_lease::{
    git_repo_root_for_wave_cwd, workspace_lease_path_for, workspace_slice_branch_for,
};
use crate::session_projection_repo::AgentProvider;
use calm_types::forge_git::{
    GIT_COMMIT_OUTPUT_PROBE_SCRIPT, GIT_COMMIT_PROBE_SCRIPT, GIT_COMMIT_SCRIPT,
};
use serde_json::Map;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

pub const TOOL_TASK_DISPATCH: &str = "calm.task.dispatch";
pub const TOOL_TASK_COMPLETE: &str = "calm.task.complete";
pub const TOOL_TASK_FAIL: &str = "calm.task.fail";
const GIT_FORGE_PLUGIN_ID: &str = "dev.neige.git-forge";

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(task_dispatch_descriptor(), wrap(task_dispatch));
    registry.register(task_complete_descriptor(), wrap(task_complete));
    registry.register(task_fail_descriptor(), wrap(task_fail));
    register_deprecated_alias(registry, "calm.dispatch_request", TOOL_TASK_DISPATCH);
    register_deprecated_alias(registry, "calm.task_completed", TOOL_TASK_COMPLETE);
    register_deprecated_alias(registry, "calm.task_failed", TOOL_TASK_FAIL);
}

/// Common wrapper that turns a typed async fn into the boxed-future
/// `ToolHandler` the registry expects. Saves three copies of the same
/// `Box::pin` boilerplate.
fn wrap<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(Arc<AppContext>, ToolCallIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    Arc::new(move |ctx, identity, args| -> ToolHandlerFuture { Box::pin(f(ctx, identity, args)) })
}

// ---------------------------------------------------------------------------
// calm.task.dispatch
// ---------------------------------------------------------------------------

fn task_dispatch_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TASK_DISPATCH.into(),
        description: "Deprecated compatibility shim: `calm.task.dispatch` was \
             retired in #644. Use `calm.plan.upsert` to maintain the task plan; \
             the kernel schedules ready tasks and runs gates."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "kind": { "type": "string", "enum": ["codex", "terminal"] },
                "idempotency_key": { "type": "string", "minLength": 1 },
                "goal": { "type": "string" },
                "context": {},
                "acceptance_criteria": { "type": ["string", "null"] },
                "cmd": { "type": "string" },
                "cwd": { "type": ["string", "null"] },
                "message": message_schema(),
                "lifecycle": lifecycle_schema()
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[],
    }
}

async fn task_dispatch(
    _ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    _args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Spec)?;
    Ok(json!({
        "error": "calm.task.dispatch was retired (#644); no task was dispatched",
        "migration": {
            "use": "calm.plan.upsert",
            "shape": "{ tasks: [{ key, kind, goal, depends_on?, priority?, gate? }], message }",
            "notes": "The kernel schedules ready tasks and runs verification gates. Use calm.plan.list to see task status."
        }
    }))
}

// ---------------------------------------------------------------------------
// calm.task.complete
// ---------------------------------------------------------------------------

fn task_complete_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TASK_COMPLETE.into(),
        description: "Report that a worker card has completed its task. \
             `idempotency_key` should echo the kernel-provided task id so \
             the spec card can correlate."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["idempotency_key"],
            "properties": {
                "idempotency_key": { "type": "string", "minLength": 1 },
                "result": {},
                "artifacts": { "type": "array" }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        // #838 Move 2 — visible to workers so a codex worker's `tools/list`
        // advertises the native completion tool (it reports completion via
        // this tool instead of the `neige` CLI). Role-gated to Worker (the
        // handler also `require_role(Worker)`s).
        visible_to_roles: &[CardRole::Worker],
    }
}

async fn task_complete(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Worker)?;

    let idempotency_key = args
        .get("idempotency_key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            RpcError::invalid_params("task_complete: missing `idempotency_key` (non-empty)")
        })?
        .to_string();
    let result = args.get("result").cloned().unwrap_or(Value::Null);
    let artifacts_val = args
        .get("artifacts")
        .cloned()
        .unwrap_or(Value::Array(vec![]));
    let artifacts: Vec<crate::event::ArtifactRef> = serde_json::from_value(artifacts_val)
        .map_err(|e| RpcError::invalid_params(format!("task_complete: invalid artifacts: {e}")))?;

    let event = Event::TaskCompleted {
        idempotency_key,
        result,
        artifacts,
        agent_message: None,
    };
    commit_worker_task_report_for_identity(&ctx, &identity, event).await?;
    if let Err(error) = submit_worker_success_commit(&ctx, &identity).await {
        tracing::warn!(
            card_id = %identity.card_id,
            wave_id = identity.wave_id.as_deref().unwrap_or("<missing>"),
            error = %error,
            "task_complete: worker success persisted but deterministic commit enqueue failed"
        );
    }
    Ok(json!({ "status": "emitted" }))
}

async fn submit_worker_success_commit(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
) -> Result<(), String> {
    let wave_id = identity
        .wave_id
        .clone()
        .ok_or_else(|| "worker success commit requires a wave-scoped caller".to_string())?;
    let wave = ctx
        .repo
        .wave_get(&wave_id)
        .await
        .map_err(|e| format!("worker success commit wave lookup: {e}"))?
        .ok_or_else(|| format!("unknown wave `{wave_id}`"))?;
    if wave.cove_id.as_str() != identity.cove_id.as_str() {
        return Err("worker success commit wave belongs to a different cove".into());
    }

    let card_id = identity.card_id.clone();
    let Some(cwd_lease) = codex_git_worktree_lease_for_completion(identity, &wave_id, &wave.cwd)?
    else {
        return Ok(());
    };
    let branch = workspace_slice_branch_for(&wave_id, &card_id)
        .map_err(|e| format!("worker success commit branch: {e}"))?;
    let message = format!("neige: worker {card_id} @ wave {wave_id}");

    let payload = PluginForgePayload {
        argv: vec![
            "sh".into(),
            "-c".into(),
            GIT_COMMIT_SCRIPT.into(),
            "sh".into(),
            message,
            branch.clone(),
        ],
        idem_key: "git.commit:auto".into(),
        event_spec: Some(worktree_committed_event_spec()),
        subject: None,
        context: Map::new(),
        probe: Some(ProbeSpec {
            probe_argv: vec![
                "sh".into(),
                "-c".into(),
                GIT_COMMIT_PROBE_SCRIPT.into(),
                "sh".into(),
            ],
            output_probe_argv: Some(vec![
                "sh".into(),
                "-c".into(),
                GIT_COMMIT_OUTPUT_PROBE_SCRIPT.into(),
                "sh".into(),
                branch,
            ]),
        }),
        parked: false,
    };

    match submit_forge_action(
        ctx,
        GIT_FORGE_PLUGIN_ID,
        wave_id,
        card_id,
        cwd_lease,
        payload,
    )
    .await
    .map_err(|e| e.to_string())?
    {
        Ok(_submission) => Ok(()),
        Err(error) => Err(error),
    }
}

fn codex_git_worktree_lease_for_completion(
    identity: &ToolCallIdentity,
    wave_id: &str,
    wave_cwd: &str,
) -> Result<Option<PathBuf>, String> {
    if identity.provider != AgentProvider::Codex {
        tracing::debug!(
            card_id = %identity.card_id,
            wave_id,
            provider = ?identity.provider,
            "task_complete: skipping auto commit for non-codex worker"
        );
        return Ok(None);
    }

    let repo_root = git_repo_root_for_wave_cwd(wave_id, wave_cwd)
        .map_err(|e| format!("worker success commit repo root lookup: {e}"))?;
    let path = workspace_lease_path_for(&repo_root, wave_id, &identity.card_id)
        .map_err(|e| format!("worker success commit workspace path: {e}"))?;
    if !is_isolated_git_worktree(&path) {
        tracing::debug!(
            card_id = %identity.card_id,
            wave_id,
            path = %path.display(),
            "task_complete: skipping auto commit because workspace lease is not an isolated git worktree"
        );
        return Ok(None);
    }

    Ok(Some(path))
}

fn is_isolated_git_worktree(path: &Path) -> bool {
    if !path.is_dir() {
        return false;
    }
    let Ok(path_top) = path.canonicalize() else {
        return false;
    };
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--show-toplevel"])
        .output();
    let Ok(output) = output else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let toplevel = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if toplevel.is_empty() {
        return false;
    }
    let Ok(git_top) = Path::new(&toplevel).canonicalize() else {
        return false;
    };
    git_top == path_top
}

fn worktree_committed_event_spec() -> ForgeEventSpec {
    ForgeEventSpec {
        event_kind: "worktree.committed".into(),
        fields: BTreeMap::from([
            (
                "branch".into(),
                FieldSource::JsonField {
                    path: "/branch".into(),
                },
            ),
            (
                "commit_sha".into(),
                FieldSource::JsonField {
                    path: "/commit".into(),
                },
            ),
        ]),
    }
}

// ---------------------------------------------------------------------------
// calm.task.fail
// ---------------------------------------------------------------------------

fn task_fail_descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL_TASK_FAIL.into(),
        description: "Report that a worker card has failed its task. \
             `reason` is free-form and persisted verbatim on the event row."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["idempotency_key", "reason"],
            "properties": {
                "idempotency_key": { "type": "string", "minLength": 1 },
                "reason": { "type": "string" }
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        // #838 Move 2 — visible to workers (see `task_complete_descriptor`).
        visible_to_roles: &[CardRole::Worker],
    }
}

async fn task_fail(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    require_role(&identity, CardRole::Worker)?;

    let idempotency_key = args
        .get("idempotency_key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            RpcError::invalid_params("task_fail: missing `idempotency_key` (non-empty)")
        })?
        .to_string();
    let reason = args
        .get("reason")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("task_fail: missing `reason`"))?
        .to_string();

    let event = Event::TaskFailed {
        idempotency_key,
        reason,
        agent_message: None,
    };
    commit_worker_task_report_for_identity(&ctx, &identity, event).await?;
    Ok(json!({ "status": "emitted" }))
}

// ---------------------------------------------------------------------------
// Shared emit path — derives the session-shaped actor from ToolCallIdentity
// inside CardDecisionSink and delegates the eventized write.
// ---------------------------------------------------------------------------

async fn commit_worker_task_report_for_identity(
    ctx: &Arc<AppContext>,
    identity: &ToolCallIdentity,
    event: Event,
) -> Result<(), RpcError> {
    let kind_tag = event.kind_tag();
    let result = CardDecisionSink::from_app_context(ctx)
        .commit_worker_task_report(identity, event)
        .await;

    match result {
        Ok(_) => Ok(()),
        Err(CalmError::Forbidden(msg)) => {
            // Role gate refusal — surface as a custom error code so a
            // mis-roled card sees a deterministic failure shape rather
            // than a generic internal error.
            Err(RpcError::custom(
                -32403,
                format!("emit {kind_tag}: forbidden: {msg}"),
            ))
        }
        Err(e) => Err(RpcError::internal(format!("emit {kind_tag}: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card_role_cache::CardRoleCache;
    use crate::db::sqlite::{SqlxRepo, begin_immediate_tx, session_start_runtime_tx};
    use crate::db::{RepoSyncDomainRaw, RouteRepo};
    use crate::event::EventBus;
    use crate::ids::WaveId;
    use crate::model::{CardRole, NewCard, NewCove, NewWave, new_id, now_ms};
    use crate::operation::forge_action_adapter::{FORGE_ACTION_KIND, ForgeActionAdapter};
    use crate::operation::workspace_lease::{
        acquire_plain_workspace_lease_tx, release_workspace_lease_for_card_repo,
    };
    use crate::operation::{
        OperationCompletionBus, OperationRuntime, ProviderAdapter, SpawnCtx, SqlxOperationRepo,
    };
    use crate::session_projection_repo::{
        WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
    };
    use crate::state::{DaemonClient, WriteContext};
    use crate::terminal_renderer::TerminalRendererRegistry;
    use crate::wave_cove_cache::WaveCoveCache;
    use std::fs;
    use tempfile::TempDir;
    use tokio::sync::OnceCell;

    struct CommitGuardFixture {
        _tmp: TempDir,
        repo: Arc<SqlxRepo>,
        ctx: Arc<AppContext>,
        wave_id: String,
        cove_id: String,
        repo_root: PathBuf,
    }

    #[tokio::test]
    async fn worker_success_commit_guard_requires_codex_real_worktree() {
        let fx = commit_guard_fixture().await;

        let claude = fx
            .worker_with_runtime(
                WorkerSessionKind::ClaudeCard,
                Some(AgentProvider::Claude),
                AgentProvider::Claude,
            )
            .await;
        let claude_path = fx.worktree_path(&claude.card_id);
        fx.add_git_worktree(&claude_path, &format!("claude-{}", claude.card_id));
        fx.lease(&claude.card_id, &claude_path).await;
        submit_worker_success_commit(&fx.ctx, &claude)
            .await
            .expect("non-codex skip is not an error");
        assert_eq!(forge_op_count(&fx.repo).await, 0);

        let codex_plain = fx
            .worker_with_runtime(
                WorkerSessionKind::CodexCard,
                Some(AgentProvider::Codex),
                AgentProvider::Codex,
            )
            .await;
        let codex_plain_path = fx.worktree_path(&codex_plain.card_id);
        fx.lease(&codex_plain.card_id, &codex_plain_path).await;
        submit_worker_success_commit(&fx.ctx, &codex_plain)
            .await
            .expect("plain directory skip is not an error");
        assert_eq!(forge_op_count(&fx.repo).await, 0);

        let codex_worktree = fx
            .worker_with_runtime(
                WorkerSessionKind::CodexCard,
                Some(AgentProvider::Codex),
                AgentProvider::Codex,
            )
            .await;
        let codex_worktree_path = fx.worktree_path(&codex_worktree.card_id);
        let branch =
            workspace_slice_branch_for(&fx.wave_id, &codex_worktree.card_id).expect("slice branch");
        fx.add_git_worktree(&codex_worktree_path, &branch);
        fx.lease(&codex_worktree.card_id, &codex_worktree_path)
            .await;
        submit_worker_success_commit(&fx.ctx, &codex_worktree)
            .await
            .expect("codex worktree enqueue");
        assert_eq!(forge_op_count(&fx.repo).await, 1);
    }

    #[tokio::test]
    async fn worker_success_commit_payload_uses_shared_git_scripts_as_drift_lock() {
        let fx = commit_guard_fixture().await;
        let codex_worktree = fx
            .worker_with_runtime(
                WorkerSessionKind::CodexCard,
                Some(AgentProvider::Codex),
                AgentProvider::Codex,
            )
            .await;
        let codex_worktree_path = fx.worktree_path(&codex_worktree.card_id);
        let branch =
            workspace_slice_branch_for(&fx.wave_id, &codex_worktree.card_id).expect("slice branch");
        fx.add_git_worktree(&codex_worktree_path, &branch);
        fx.lease(&codex_worktree.card_id, &codex_worktree_path)
            .await;

        submit_worker_success_commit(&fx.ctx, &codex_worktree)
            .await
            .expect("codex worktree enqueue");

        let idem_key = format!(
            "{GIT_FORGE_PLUGIN_ID}:{}:{}:git.commit:auto",
            fx.wave_id, codex_worktree.card_id
        );
        let payload = forge_payload_by_idem(&fx.repo, &idem_key).await;
        let message = format!(
            "neige: worker {} @ wave {}",
            codex_worktree.card_id, fx.wave_id
        );
        assert_eq!(
            payload["argv"],
            json!(["sh", "-c", GIT_COMMIT_SCRIPT, "sh", message, branch.clone()])
        );
        assert_eq!(
            payload["probe"]["probe_argv"],
            json!(["sh", "-c", GIT_COMMIT_PROBE_SCRIPT, "sh"])
        );
        assert_eq!(
            payload["probe"]["output_probe_argv"],
            json!(["sh", "-c", GIT_COMMIT_OUTPUT_PROBE_SCRIPT, "sh", branch])
        );
    }

    #[tokio::test]
    async fn worker_success_commit_emits_after_codex_workspace_lease_released() {
        let fx = commit_guard_fixture().await;
        let codex_worktree = fx
            .worker_with_runtime(
                WorkerSessionKind::CodexCard,
                Some(AgentProvider::Codex),
                AgentProvider::Codex,
            )
            .await;
        let codex_worktree_path = fx.worktree_path(&codex_worktree.card_id);
        let branch =
            workspace_slice_branch_for(&fx.wave_id, &codex_worktree.card_id).expect("slice branch");
        fx.add_git_worktree(&codex_worktree_path, &branch);
        fx.lease(&codex_worktree.card_id, &codex_worktree_path)
            .await;
        fx.release_lease(&codex_worktree.card_id).await;

        fs::write(
            codex_worktree_path.join("released-worker-output.txt"),
            "released worker output\n",
        )
        .expect("write worker output");

        submit_worker_success_commit(&fx.ctx, &codex_worktree)
            .await
            .expect("released codex worktree enqueue");
        assert_eq!(forge_op_count(&fx.repo).await, 1);

        let payloads = wait_for_event_payloads(&fx.repo, "worktree.committed", 1).await;
        let head = git_stdout(&codex_worktree_path, ["rev-parse", "HEAD"]);
        assert_eq!(payloads[0]["wave_id"], fx.wave_id);
        assert_eq!(payloads[0]["card_id"], codex_worktree.card_id);
        assert_eq!(payloads[0]["branch"], branch);
        assert_eq!(payloads[0]["commit_sha"], head);
        assert_eq!(
            git_stdout(
                &codex_worktree_path,
                ["show", "HEAD:released-worker-output.txt"],
            ),
            "released worker output"
        );
    }

    impl CommitGuardFixture {
        async fn worker_with_runtime(
            &self,
            kind: WorkerSessionKind,
            agent_provider: Option<AgentProvider>,
            identity_provider: AgentProvider,
        ) -> ToolCallIdentity {
            let card = self
                .repo
                .card_create(NewCard {
                    wave_id: WaveId::from(self.wave_id.clone()),
                    kind: "codex".into(),
                    sort: None,
                    payload: json!({ "schemaVersion": 1 }),
                })
                .await
                .expect("create worker card");
            let runtime_id = new_id();
            let mut tx = self.repo.pool().begin().await.expect("begin runtime tx");
            session_start_runtime_tx(
                &mut tx,
                WorkerSessionInit {
                    id: runtime_id.clone(),
                    card_id: card.id.to_string(),
                    kind,
                    agent_provider,
                    status: WorkerSessionState::Running,
                    terminal_run_id: None,
                    thread_id: Some(format!("thread-{}", card.id)),
                    session_id: Some(format!("session-{}", card.id)),
                    active_turn_id: None,
                    handle_state_json: None,
                    spawn_op_id: None,
                    now_ms: now_ms(),
                },
            )
            .await
            .expect("start runtime");
            tx.commit().await.expect("commit runtime tx");

            ToolCallIdentity {
                card_id: card.id.to_string(),
                role: CardRole::Worker,
                provider: identity_provider,
                session_id: runtime_id,
                wave_id: Some(self.wave_id.clone()),
                cove_id: self.cove_id.clone(),
                thread_id: format!("thread-{}", card.id),
            }
        }

        fn worktree_path(&self, card_id: &str) -> PathBuf {
            self.repo_root
                .join(".claude")
                .join("worktrees")
                .join(&self.wave_id)
                .join(card_id)
        }

        fn add_git_worktree(&self, path: &Path, branch: &str) {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create worktree parent");
            }
            run_git(
                &self.repo_root,
                [
                    "worktree",
                    "add",
                    "-b",
                    branch,
                    path.to_str().expect("utf8 worktree path"),
                    "HEAD",
                ],
            );
        }

        async fn lease(&self, card_id: &str, path: &Path) {
            let mut tx = begin_immediate_tx(self.repo.pool())
                .await
                .expect("begin lease tx");
            acquire_plain_workspace_lease_tx(&mut tx, card_id, &self.wave_id, "op-test", path)
                .await
                .expect("acquire lease");
            tx.commit().await.expect("commit lease tx");
        }

        async fn release_lease(&self, card_id: &str) {
            release_workspace_lease_for_card_repo(self.repo.as_ref(), &EventBus::new(), card_id)
                .await
                .expect("release lease");
            let state: String = sqlx::query_scalar(
                "SELECT state FROM workspace_leases WHERE card_id = ?1 ORDER BY created_at_ms DESC LIMIT 1",
            )
            .bind(card_id)
            .fetch_one(self.repo.pool())
            .await
            .expect("lease state");
            assert_eq!(state, "released");
        }
    }

    async fn commit_guard_fixture() -> CommitGuardFixture {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo_root = tmp.path().join("repo");
        fs::create_dir_all(&repo_root).expect("create repo root");
        init_git_repo(&repo_root);

        let repo = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory repo"),
        );
        let cove = repo
            .cove_create(NewCove {
                name: "commit guard".into(),
                color: "#123456".into(),
                sort: None,
            })
            .await
            .expect("create cove");
        let wave = repo
            .wave_create(NewWave {
                workflow_input: None,
                cove_id: cove.id.clone(),
                title: "commit guard".into(),
                sort: None,
                cwd: repo_root.display().to_string(),
                workflow_id: None,
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .expect("create wave");
        let events = EventBus::new();
        let completion = OperationCompletionBus::new();
        let operation_repo = Arc::new(SqlxOperationRepo::new(repo.pool().clone()));
        let route_repo: Arc<dyn RouteRepo> = repo.clone();
        let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo.clone());
        let spawn_ctx = SpawnCtx::new(
            route_repo.clone(),
            operation_repo.clone(),
            Arc::new(DaemonClient::new_stub()),
            terminal_renderer,
            events.clone(),
            completion.clone(),
        );
        let runtime = Arc::new(OperationRuntime::new_unchecked(
            operation_repo,
            vec![Arc::new(ForgeActionAdapter::new()) as Arc<dyn ProviderAdapter>],
            events.clone(),
            completion,
            spawn_ctx,
        ));
        let operation_runtime = Arc::new(OnceCell::new());
        assert!(operation_runtime.set(runtime).is_ok());
        let plugin_host = Arc::new(OnceCell::new());
        let ctx = Arc::new(AppContext {
            repo: route_repo,
            wave_vcs: None,
            events,
            write: WriteContext::new(CardRoleCache::new(), WaveCoveCache::new()),
            daemon_token_hash: None,
            gate_logs_dir: tmp.path().join("gate-logs"),
            plugin_host,
            operation_runtime,
        });

        CommitGuardFixture {
            _tmp: tmp,
            repo,
            ctx,
            wave_id: wave.id.to_string(),
            cove_id: cove.id.to_string(),
            repo_root,
        }
    }

    async fn forge_op_count(repo: &SqlxRepo) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM operations WHERE kind = ?1")
            .bind(FORGE_ACTION_KIND)
            .fetch_one(repo.pool())
            .await
            .expect("count forge ops")
    }

    async fn forge_payload_by_idem(repo: &SqlxRepo, idem_key: &str) -> Value {
        let payload: String = sqlx::query_scalar(
            "SELECT payload_json FROM operations WHERE kind = ?1 AND idempotency_key = ?2",
        )
        .bind(FORGE_ACTION_KIND)
        .bind(idem_key)
        .fetch_one(repo.pool())
        .await
        .expect("operation payload lookup");
        serde_json::from_str(&payload).expect("operation payload json")
    }

    async fn wait_for_event_payloads(repo: &SqlxRepo, kind: &str, expected: usize) -> Vec<Value> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let payloads = event_payloads(repo, kind).await;
            if payloads.len() >= expected {
                return payloads;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {expected} {kind} events"
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }

    async fn event_payloads(repo: &SqlxRepo, kind: &str) -> Vec<Value> {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT payload FROM events WHERE kind = ?1 ORDER BY id ASC")
                .bind(kind)
                .fetch_all(repo.pool())
                .await
                .expect("event payload rows");
        rows.into_iter()
            .map(|(payload,)| serde_json::from_str(&payload).expect("event payload json"))
            .collect()
    }

    fn init_git_repo(path: &Path) {
        run_git(path, ["init"]);
        run_git(path, ["config", "user.email", "commit-guard@example.test"]);
        run_git(path, ["config", "user.name", "Commit Guard"]);
        fs::write(path.join("README.md"), "init\n").expect("write readme");
        run_git(path, ["add", "README.md"]);
        run_git(path, ["commit", "-m", "init"]);
    }

    fn run_git<const N: usize>(cwd: &Path, args: [&str; N]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git failed: status={:?} stdout={} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_stdout<const N: usize>(cwd: &Path, args: [&str; N]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git failed: status={:?} stdout={} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }
}
