//! Resolve the real execution binding; never follow a replacement implicitly.
use super::*;
use crate::model::{Task, TaskKind, TaskStatus};
use calm_exec::{BoundKeys, TuiInput};
use calm_types::task_recovery::TASK_CHILD_TRACK_ROUTE;
use calm_types::worker::{WorkerProviderKind, WorkerSessionState};
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
/// Typed keys into a task worker whose provider refuses them while bound (a codex remote TUI
/// interrupts its turn and starts none, #1782); `{provider}` is the worker's provider.
const WORKER_KEYS_REFUSED: &str = include_str!("../../prompts/terminal/worker-keys-refused.md");

/// A refusal decided before any connection, claim or byte: `data.refusal` names it (#2493), and a
/// starting worker's adds what it waits for and how long the call waited (#2571).
#[derive(Debug)]
pub struct InputRefused {
    pub refusal: &'static str,
    pub message: String,
    pub starting: Option<Starting>,
}
impl std::fmt::Display for InputRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for InputRefused {}
pub(super) fn refused(refusal: &'static str, message: String) -> InputRefused {
    InputRefused {
        refusal,
        message,
        starting: None,
    }
}

/// What a starting worker has yet to do before it takes input (#2571).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitingFor {
    /// Its task is dispatched, or its worker card is not stamped yet: no worker runs.
    TaskRunning,
    /// Its agent has not submitted its task prompt (a provider whose message waits for it, #2532).
    PromptSubmitted,
}

/// `worker_starting` (#2554): the worker the attempt is waiting for, and how long the refused call
/// waited for it (`None` when it did not wait).
#[derive(Debug)]
pub struct Starting {
    pub waiting_for: WaitingFor,
    pub waited_ms: Option<u64>,
    attempt_id: String,
    status: &'static str,
}
impl Starting {
    /// What the kernel did and which event ends the state: a later wake once the task runs, the
    /// worker watcher's report once a startup screen holds the agent.
    fn text(&self) -> String {
        let (attempt, status) = (&self.attempt_id, self.status);
        let waited = |what: &str| {
            self.waited_ms.map_or(String::new(), |waited| {
                format!(
                    " after this call waited {:.1} s for {what}; nothing was written",
                    waited as f64 / 1000.0
                )
            })
        };
        match self.waiting_for {
            WaitingFor::TaskRunning if self.waited_ms.is_none() => format!(
                "attempt {attempt} is {status}; its worker is starting and takes no input yet. \
                 Send again on a later wake"
            ),
            WaitingFor::TaskRunning => format!(
                "attempt {attempt} is still {status}{}. Send again on a later wake",
                waited("its worker to start")
            ),
            WaitingFor::PromptSubmitted => format!(
                "attempt {attempt} is running, but its worker has not started its task{}. Its agent \
                 is likely held on a startup screen: the worker watcher looks at a worker after at \
                 least {} s of quiet and reports to you. Send again after its report",
                waited("it to submit its task prompt"),
                crate::worker_quiet::QUIET_AFTER.as_secs()
            ),
        }
    }
}
/// The refusal a starting worker answers; a message waits while it holds (#2554).
pub(super) fn starting(
    attempt_id: &str,
    status: TaskStatus,
    waiting_for: WaitingFor,
) -> InputRefused {
    let starting = Starting {
        waiting_for,
        waited_ms: None,
        attempt_id: attempt_id.to_owned(),
        status: status.wire_label(),
    };
    InputRefused {
        refusal: "worker_starting",
        message: starting.text(),
        starting: Some(starting),
    }
}
impl InputRefused {
    /// This refusal answered after the call waited `waited` for its worker to start.
    pub(super) fn waited(mut self, waited: Duration) -> Self {
        if let Some(starting) = &mut self.starting {
            starting.waited_ms = Some(waited.as_millis() as u64);
            self.message = starting.text();
        }
        self
    }
}

/// A request's terminal now serves another task, worker session or terminal than the one it was
/// resolved to: only a fresh show and read reach the new binding. It claims nothing about input:
/// a readback after a write answers it too.
pub(super) fn binding_changed(was: &Binding, now: &Binding) -> InputRefused {
    let attempt = |binding: &Binding| {
        binding
            .task
            .as_ref()
            .map_or("none".to_owned(), |task| task.attempt_id.clone())
    };
    let changes = [
        ("terminal", was.terminal_id.clone(), now.terminal_id.clone()),
        ("card", was.card_id.clone(), now.card_id.clone()),
        ("attempt", attempt(was), attempt(now)),
        (
            "worker session",
            was.worker_session_id.clone(),
            now.worker_session_id.clone(),
        ),
    ]
    .into_iter()
    .filter(|(_, was, now)| was != now)
    .map(|(field, was, now)| format!("{field} {was} is now {now}"))
    .collect::<Vec<_>>()
    .join(", ");
    refused(
        "binding_changed",
        format!("the terminal binding this call resolved changed ({changes}); show and read again"),
    )
}

/// What a task's lifecycle answers a terminal tool whose task has no worker taking input (#2493,
/// #2554): one table whether or not a worker terminal resolved, decided from the task row alone.
pub(super) fn lifecycle_refusal(task: &TaskBinding, status: Option<TaskStatus>) -> InputRefused {
    let (attempt, key) = (&task.attempt_id, &task.task_key);
    let label = status.map_or("unknown", TaskStatus::wire_label);
    let parked = |next: &str| {
        refused(
            "worker_parked",
            format!("attempt {attempt} (task {key}) is {label}; its worker takes no input. {next}"),
        )
    };
    match status {
        // A pending row still takes declaration edits; one that races its dispatch is reported
        // `declaration_changed_in_flight`.
        Some(TaskStatus::Pending) => refused(
            "task_pending",
            format!(
                "attempt {attempt} (task {key}) has not started, so it has no worker yet; \
                 neige_task_ls shows why it waits. To change what it will do, edit its task \
                 block: its worker starts with it"
            ),
        ),
        Some(status @ (TaskStatus::Dispatched | TaskStatus::Running)) => {
            starting(attempt, status, WaitingFor::TaskRunning)
        }
        Some(TaskStatus::Verifying) => parked("Wait for its gate."),
        Some(TaskStatus::Canceled) => parked("Declare a new task."),
        Some(TaskStatus::Done | TaskStatus::Failed) | None => {
            parked("Declare a new task for a fresh worker.")
        }
    }
}

pub(crate) struct Resolved {
    pub binding: Binding,
    /// The write rule: the card's current worker session holds live authority and its task (if
    /// any) is running. Per-action rules (typed keys, `message`) come on top of it.
    pub write_allowed: bool,
    /// The card's current worker session still holds live authority (not exited or failed).
    pub session_active: bool,
    pub session_state: WorkerSessionState,
    pub provider: WorkerProviderKind,
    pub task_status: Option<TaskStatus>,
    pub card_kind: String,
}
impl Resolved {
    /// The write rule's refusal on a task worker (#2493): every agent write path (`text`,
    /// `submit`, `key`, `sequence`, a claim, `message`) refuses a worker whose task is not running,
    /// or whose session ended, with one typed refusal. `None` while writes are allowed and for a
    /// task-less terminal (its ended session keeps the untyped refusal).
    pub(super) fn write_refusal(&self) -> Option<InputRefused> {
        if self.write_allowed {
            return None;
        }
        let task = self.binding.task.as_ref()?;
        Some(match self.task_status {
            Some(TaskStatus::Running) => refused(
                "worker_ended",
                format!(
                    "the worker of attempt {} has ended ({}); this call wrote nothing. \
                     Declare a new task.",
                    task.attempt_id,
                    self.session_state.as_db_str()
                ),
            ),
            status => lifecycle_refusal(task, status),
        })
    }
    /// The write rule as an error: the typed refusal on a task worker, the untyped one on a
    /// task-less terminal whose session ended.
    pub(super) fn ensure_write_allowed(&self) -> Result<()> {
        if let Some(refusal) = self.write_refusal() {
            return Err(refusal.into());
        }
        ensure!(
            self.write_allowed,
            "task or worker session is not running; terminal control refused"
        );
        Ok(())
    }
    /// Whether typed keys and a control claim are accepted: a task worker whose provider refuses
    /// keys while bound takes only `message`.
    pub(super) fn keys_refused(&self, tui: TuiInput) -> Option<InputRefused> {
        (self.binding.task.is_some() && tui.keys_while_bound() == BoundKeys::Refused).then(|| {
            refused(
                "worker_keys_refused",
                WORKER_KEYS_REFUSED.replace("{provider}", self.provider.as_db_str()),
            )
        })
    }
}

impl TerminalInteraction {
    async fn current_task(repo: &dyn RouteRepo, track: &str, id: &str) -> Result<Task> {
        let task = repo
            .task_get(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("task execution unavailable"))?;
        ensure!(task.track_id == track, "task outside the caller's Track");
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
    /// The worker card of an attempt, or the refusal its task row gives before any terminal lookup
    /// (#2554): a pending task, a task that runs as a child Track, or one whose worker card is not
    /// stamped yet (the scheduler stamps it when the task runs). It reads nothing more: the quiet
    /// detector resolves every non-terminal task through here.
    fn attempt_worker_card(task: &Task) -> Result<&str> {
        let binding = TaskBinding {
            attempt_id: task.id.clone(),
            task_key: task.key.clone(),
        };
        let refusal = if task.status == TaskStatus::Pending {
            lifecycle_refusal(&binding, Some(task.status))
        } else if task.spawn == TASK_CHILD_TRACK_ROUTE {
            refused(
                "task_in_child_track",
                format!(
                    "attempt {} (task {}) runs as a child Track; its workers belong to that \
                     Track, not to yours",
                    task.id, task.key
                ),
            )
        } else if let Some(card) = task.worker_card_id.as_deref() {
            return Ok(card);
        } else {
            lifecycle_refusal(&binding, Some(task.status))
        };
        Err(refusal.into())
    }
    /// The one resolution every caller-facing path takes: show, read, control, input and the
    /// queued write's re-check (`check_binding`). An Assistant reaches only a terminal bound to a
    /// current task attempt (#2492): a manual Terminal card or a task-less agent card runs outside
    /// its sandbox, and the watcher needs task workers only.
    pub(super) async fn resolve_target(
        repo: &dyn RouteRepo,
        identity: &ToolCallIdentity,
        target: &Target,
    ) -> Result<Resolved> {
        let track = Self::authorize(repo, identity).await?;
        let resolved = Self::resolve_in_track(repo, &track, target).await?;
        ensure!(
            identity.role != CardRole::Assistant || resolved.binding.task.is_some(),
            "an Assistant reaches only task workers; this terminal is bound to no current task"
        );
        Ok(resolved)
    }
    /// What a caller of `track` resolves for `target`, minus the caller's identity check: the one
    /// same-Track rule of the terminal tools, the quiet-worker detector (it hands the watcher only
    /// what the tools can reach) and `neige_worker_report` (#2492).
    pub(crate) async fn resolve_in_track(
        repo: &dyn RouteRepo,
        track: &str,
        target: &Target,
    ) -> Result<Resolved> {
        let (requested_task, terminal) = match target {
            Target::Attempt(id) => {
                let task = Self::current_task(repo, track, id).await?;
                let card = Self::attempt_worker_card(&task)?;
                let terminal = repo.terminal_get_by_card(card).await?;
                (Some(task), terminal)
            }
            Target::Terminal(id) => (None, repo.terminal_get(id).await?),
        };
        let terminal = terminal
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
            "terminal outside the caller's Track"
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
        let session_active = session.state.is_active_authority();
        let write_allowed = session_active
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
            write_allowed,
            session_active,
            session_state: session.state,
            provider: session.provider,
            task_status: task.map(|task| task.status),
            card_kind: card.kind,
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
        if current.binding != *expected {
            return Err(binding_changed(expected, &current.binding).into());
        }
        if write {
            current.ensure_write_allowed()?;
        }
        Ok(current)
    }
    /// The provider's input declaration for a resolved terminal.
    pub(super) fn tui_input(&self, resolved: &Resolved) -> Result<TuiInput> {
        self.providers.tui_input(resolved.provider).ok_or_else(|| {
            anyhow::anyhow!(
                "worker provider {} is not registered",
                resolved.provider.as_db_str()
            )
        })
    }
    /// The new-write check of every typed-keys path (input, claim), before any claim or byte: the
    /// write rule first, then the provider's keys refusal.
    pub(super) fn ensure_keys_accepted(&self, resolved: &Resolved) -> Result<()> {
        resolved.ensure_write_allowed()?;
        match resolved.keys_refused(self.tui_input(resolved)?) {
            Some(refused) => Err(refused.into()),
            None => Ok(()),
        }
    }
    /// What `show` and `read` report as `controllable`: the write rule and typed keys accepted.
    pub(super) fn keys_controllable(&self, resolved: &Resolved) -> Result<bool> {
        Ok(resolved.write_allowed && resolved.keys_refused(self.tui_input(resolved)?).is_none())
    }
    pub async fn resolve(&self, identity: &ToolCallIdentity, target: &Target) -> Result<Value> {
        let resolved = Self::resolve_target(self.repo.as_ref(), identity, target).await?;
        let mut result = serde_json::to_value(&resolved.binding)?;
        let entry = self.renderer.get(&resolved.binding.terminal_id);
        let available = entry.as_ref().is_some_and(|entry| entry.observable());
        result["available"] = json!(available);
        result["controllable"] = json!(available && self.keys_controllable(&resolved)?);
        result["card_kind"] = json!(resolved.card_kind);
        result["task_status"] = json!(resolved.task_status);
        if !available {
            result["reason"] = json!("no live observable terminal view; no session was started");
        }
        if let Some(refused) = resolved.keys_refused(self.tui_input(&resolved)?) {
            result["input_refused"] = json!(refused.message);
        }
        Ok(result)
    }
}
