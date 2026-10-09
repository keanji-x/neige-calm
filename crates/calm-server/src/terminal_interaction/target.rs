//! Resolve the real execution binding; never follow a replacement implicitly.
use super::*;
use crate::model::{Task, TaskKind, TaskStatus};
use serde::Serialize;

#[derive(Clone, Debug)]
pub enum Target {
    Terminal(String),
    Attempt(String),
}
impl Target {
    pub fn from_ids(terminal_id: Option<String>, attempt_id: Option<String>) -> Result<Self> {
        let target = match (terminal_id, attempt_id) {
            (Some(id), None) => Self::Terminal(id),
            (None, Some(id)) => Self::Attempt(id),
            _ => anyhow::bail!("supply exactly one of terminal_id or attempt_id"),
        };
        let id = match &target {
            Self::Terminal(id) | Self::Attempt(id) => id,
        };
        ensure!(
            !id.is_empty() && id.len() <= 512,
            "invalid terminal_id or attempt_id"
        );
        Ok(target)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TaskBinding {
    pub attempt_id: String,
    pub task_key: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Binding {
    pub terminal_id: String,
    pub card_id: String,
    pub worker_session_id: String,
    pub task: Option<TaskBinding>,
}
impl Binding {
    pub fn key(&self, identity: &ToolCallIdentity) -> String {
        // IDs are data, not delimited path components.
        serde_json::to_string(&(identity.session_id.as_str(), self))
            .expect("serializable terminal binding")
    }
}
/// Typing into a codex task Worker's remote TUI interrupts its turn and starts no replacement
/// turn (#1782), so the Planner's input and claim on that card are refused (#1784).
const CODEX_TASK_WORKER_INPUT_REFUSED: &str =
    include_str!("../../prompts/terminal/codex-task-worker-input-refused.md");

/// The typed refusal behind [`CODEX_TASK_WORKER_INPUT_REFUSED`].
#[derive(Debug)]
pub struct CodexTaskWorkerInputRefused;
impl std::fmt::Display for CodexTaskWorkerInputRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(CODEX_TASK_WORKER_INPUT_REFUSED)
    }
}
impl std::error::Error for CodexTaskWorkerInputRefused {}

pub(crate) struct Resolved {
    pub binding: Binding,
    pub controllable: bool,
    /// The card's current worker session still holds live authority (not exited or failed).
    pub session_active: bool,
    pub task_status: Option<TaskStatus>,
    pub card_kind: String,
    /// A codex card bound to a task execution: observable, never writable.
    pub codex_task_worker: bool,
}
impl Resolved {
    /// Checked before any connection or claim, so a refused input writes no byte.
    pub(super) fn ensure_accepts_input(&self) -> Result<()> {
        if self.codex_task_worker {
            return Err(CodexTaskWorkerInputRefused.into());
        }
        Ok(())
    }
}

impl TerminalInteraction {
    async fn current_task(repo: &dyn RouteRepo, track: &str, id: &str) -> Result<Task> {
        let task = repo
            .task_get(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("task execution unavailable"))?;
        ensure!(task.track_id == track, "task outside Planner Track");
        let current = repo
            .task_current_get(track, &task.key)
            .await?
            .ok_or_else(|| anyhow::anyhow!("task has no current execution"))?;
        ensure!(
            current.id == task.id,
            "task execution superseded; select the current attempt_id explicitly"
        );
        Ok(task)
    }
    pub(super) async fn resolve_target(
        repo: &dyn RouteRepo,
        identity: &ToolCallIdentity,
        target: &Target,
    ) -> Result<Resolved> {
        let track = Self::authorize(repo, identity).await?;
        Self::resolve_in_track(repo, &track, target).await
    }
    /// What a Planner of `track` resolves for `target`, minus the caller's identity check; the
    /// quiet-worker detector resolves through it so it wakes only about what the tools can reach.
    pub(crate) async fn resolve_in_track(
        repo: &dyn RouteRepo,
        track: &str,
        target: &Target,
    ) -> Result<Resolved> {
        let requested_task = match target {
            Target::Attempt(id) => Some(Self::current_task(repo, track, id).await?),
            Target::Terminal(_) => None,
        };
        let terminal = match target {
            Target::Terminal(id) => repo.terminal_get(id).await?,
            Target::Attempt(_) => {
                let card = requested_task
                    .as_ref()
                    .and_then(|task| task.worker_card_id.as_ref())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "task has no worker terminal yet (or runs in a child Track)"
                        )
                    })?;
                repo.terminal_get_by_card(card).await?
            }
        }
        // An existing row is gone only with its card or because the sweeper reaped residue; an
        // exited Terminal-card terminal stays.
        .ok_or_else(|| {
            anyhow::anyhow!(
                "terminal not found: unknown id, deleted with its card, or reaped as residue"
            )
        })?;
        let card = repo
            .card_get(terminal.card_id.as_str())
            .await?
            .ok_or_else(|| anyhow::anyhow!("terminal card unavailable"))?;
        ensure!(
            card.track_id.as_str() == track,
            "terminal outside Planner Track"
        );
        ensure!(
            matches!(card.kind.as_str(), "terminal" | "codex" | "claude"),
            "card does not provide a supported Terminal view"
        );
        ensure!(
            repo.card_role_get(card.id.as_str()).await? == Some(CardRole::Worker),
            "only Worker terminal cards may be controlled; Planner/Assistant cards are excluded"
        );
        let current = repo
            .session_projection_projectable_for_card(&card.id.to_string())
            .await?
            .ok_or_else(|| anyhow::anyhow!("terminal has no current worker session"))?;
        let expected_kind = match card.kind.as_str() {
            "terminal" => crate::session_projection_repo::WorkerSessionKind::Terminal,
            "codex" => crate::session_projection_repo::WorkerSessionKind::CodexCard,
            "claude" => crate::session_projection_repo::WorkerSessionKind::ClaudeCard,
            _ => unreachable!("validated terminal card kind"),
        };
        ensure!(
            current.kind == expected_kind,
            "card and worker session kinds disagree"
        );
        let session = repo
            .session_get_by_id(&current.id.clone().into())
            .await?
            .ok_or_else(|| anyhow::anyhow!("worker session unavailable"))?;
        ensure!(
            current.terminal_run_id.as_deref() == Some(terminal.id.as_str())
                && session.terminal_run_id.as_deref() == Some(terminal.id.as_str())
                && session.card_id.as_ref() == Some(&card.id)
                && session.track_id == card.track_id,
            "terminal does not belong to the card's current worker session"
        );
        let operation_task = match session.spawn_op_id.as_deref() {
            Some(op) => Some(
                repo.operation_idempotency_key_by_id(op)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("worker operation binding unavailable"))?,
            ),
            None => None,
        };
        let task = if let Some(task) = requested_task {
            Some(task)
        } else if let Some(id) = operation_task.as_deref() {
            Some(Self::current_task(repo, track, id).await?)
        } else {
            repo.task_for_worker_card(card.id.as_str()).await?
        };
        let task = match task {
            Some(task) => {
                let latest = Self::current_task(repo, track, &task.id).await?;
                ensure!(
                    latest.worker_card_id.as_deref() == Some(card.id.as_str())
                        && operation_task.as_deref() == Some(latest.id.as_str()),
                    "worker card/session is not owned by this task execution"
                );
                let expected = match latest.kind {
                    TaskKind::Terminal => "terminal",
                    TaskKind::Codex => "codex",
                    TaskKind::Claude => "claude",
                };
                ensure!(
                    card.kind == expected,
                    "task kind and worker terminal kind disagree"
                );
                Some(latest)
            }
            None => None,
        };
        let codex_task_worker = card.kind == "codex" && task.is_some();
        let session_active = session.state.is_active_authority();
        let controllable = session_active
            && !codex_task_worker
            && task
                .as_ref()
                .is_none_or(|task| task.status == TaskStatus::Running);
        Ok(Resolved {
            binding: Binding {
                terminal_id: terminal.id,
                card_id: card.id.to_string(),
                worker_session_id: session.id.to_string(),
                task: task.as_ref().map(|task| TaskBinding {
                    attempt_id: task.id.clone(),
                    task_key: task.key.clone(),
                }),
            },
            controllable,
            session_active,
            task_status: task.map(|task| task.status),
            card_kind: card.kind,
            codex_task_worker,
        })
    }
    /// Re-resolve `expected.terminal_id` and return the current resolution
    /// after proving its execution binding is still `expected`; callers that
    /// emit task status or controllability read them from this result.
    pub(super) async fn check_binding(
        repo: &dyn RouteRepo,
        identity: &ToolCallIdentity,
        expected: &Binding,
        write: bool,
    ) -> Result<Resolved> {
        let current = Self::resolve_target(
            repo,
            identity,
            &Target::Terminal(expected.terminal_id.clone()),
        )
        .await?;
        ensure!(
            &current.binding == expected,
            "terminal task/session binding changed; show and read again"
        );
        if write {
            current.ensure_accepts_input()?;
        }
        ensure!(
            !write || current.controllable,
            "task or worker session is not running; terminal control refused"
        );
        Ok(current)
    }
    pub async fn resolve(&self, identity: &ToolCallIdentity, target: &Target) -> Result<Value> {
        let resolved = Self::resolve_target(self.repo.as_ref(), identity, target).await?;
        let mut result = serde_json::to_value(&resolved.binding)?;
        let entry = self.renderer.get(&resolved.binding.terminal_id);
        let available = entry.as_ref().is_some_and(|entry| {
            entry
                .handle
                .model_view
                .lock()
                .is_ok_and(|view| view.capture(0).is_ok())
        });
        result["available"] = json!(available);
        result["controllable"] = json!(available && resolved.controllable);
        result["card_kind"] = json!(resolved.card_kind);
        result["task_status"] = json!(resolved.task_status);
        if !available {
            result["reason"] = json!("no live observable terminal view; no session was started");
        }
        if resolved.codex_task_worker {
            result["input_refused"] = json!(CODEX_TASK_WORKER_INPUT_REFUSED);
        }
        Ok(result)
    }
}
