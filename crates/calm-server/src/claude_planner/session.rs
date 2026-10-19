//! One Claude Planner session: one `claude -p` process per turn, at most one at a time (design
//! #1791 §5.1, §5.2, §5.6, §6.2).
//!
//! Submission contract (`ClaudePlannerSession::start_turn`): refuse until the harness is
//! installed, mint the turn id, check the seal, run `--version` (before any user input), on the
//! harness's first turn mint its MCP credential (§5.1 item 2), `stop` whatever still carries this session's marker, write the instructions file under a
//! per-spawn guard, spawn, re-check the seal, write the one `user` line (bounded), and return
//! `Ok(turn_id)`. Every exit before `Ok` stops the marker, removes the file and returns `Err`; no
//! outcome is recorded, because no turn id was handed out. A manual compaction
//! ([`ClaudePlannerSession::compact_start`], `compact`) is a spawn through the same path.
//!
//! Settlement (`driver`): one [`TurnSlot`] (`turn_slot`) per turn holds the first recorded
//! [`TerminalCause`]; the outcome is recorded durably, stdin is closed, the direct child gets a
//! bounded wait, `stop` runs, the instructions file goes, and only then is `TurnCompleted` emitted.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use calm_types::planner_attachment::AttachmentId;
use tokio::process::{Child, Command};
use tokio::sync::broadcast;
use uuid::Uuid;

use super::approvals::Approvals;
use super::config::{ClaudePlannerHost, needs_an_operator, unavailable_message};
use super::driver::{TurnRun, drive};
use super::protocol::{Base64Image, UserLine, UserLineContent, client_line_uuid};
use super::spawn::{self, InstructionsFile, ResumeTruncation, SessionStart, SpawnPurpose};
use super::stop::stop;
use super::translate::{ToolNames, TurnContext, TurnTranslator};
use super::turn_slot::{TerminalCause, TurnSlot};
use crate::db::Repo;
use crate::error::{CalmError, Result};
use crate::harness::held_requests::HeldRequestSender;
use crate::harness::planner_event::PlannerEvent;
use crate::planner_model::TurnModelSelection;
use crate::planner_permission_mode::PlannerPermissionMode;
use crate::thread_seals::ThreadSeals;
use calm_types::worker::WorkerSessionId;
use provider::InputItem;

/// After an interrupt, the turn is stopped if the CLI has not ended it within this bound.
pub const STOP_TIMER: Duration = Duration::from_secs(10);
/// Once a stop is armed, the turn's `TurnCompleted` is emitted by `settle_by` = the stop deadline +
/// this margin, whatever the CLI does: every await after the stop is armed is capped by it. An
/// interrupt therefore settles by interrupt + [`STOP_TIMER`] + this = 22 s, inside the harness's
/// 30 s `interrupt_completion_budget` with 8 s left for the run loop's own work around the call.
pub const SETTLE_AFTER_STOP: Duration = Duration::from_secs(12);
/// How long [`ClaudePlannerSession::shutdown`] waits for the running turn to settle after its stop.
const SHUTDOWN_SETTLE_WAIT: Duration = Duration::from_secs(20);

/// Everything one Claude Planner harness is built from; the caller (the start adapter and
/// recovery) supplies every field.
pub struct ClaudePlannerSessionParams {
    pub host: Arc<ClaudePlannerHost>,
    pub worker_session_id: String,
    pub card_id: String,
    pub track_id: String,
    /// The track workspace, the CLI's `cwd`.
    pub cwd: PathBuf,
    /// The rendered Planner instructions, written to a private file at every spawn.
    pub instructions: String,
    /// The calm tools visible to the card, to restore their registry names.
    pub calm_tools: ToolNames,
    /// `(UPPER, lower, value)` proxy pairs from the server's resolver.
    pub proxy: Vec<(String, String, String)>,
    /// The thread's lifetime token total from the harness snapshot.
    pub prior_total_tokens: i64,
    pub repo: Arc<dyn Repo>,
    /// The server's deletion-seal registry: a sealed thread starts no turn.
    pub seals: Arc<ThreadSeals>,
}

struct ActiveTurn {
    thread_id: String,
    turn_id: String,
    slot: Arc<TurnSlot>,
}

struct State {
    start: SessionStart,
    /// Whether the row's `agent_session_id` is persisted; a turn whose bind failed leaves it
    /// unset, and the next `--resume` spawn retries the bind.
    row_bound: bool,
    settle_after_stop: Duration,
    total_tokens: i64,
    mcp_token: Option<String>,
    shutting_down: bool,
    active: Option<ActiveTurn>,
}

pub(crate) struct Shared {
    pub(crate) params: ClaudePlannerSessionParams,
    pub(crate) events: broadcast::Sender<PlannerEvent>,
    state: Mutex<State>,
    /// Serializes `turn_start` and `shutdown`, so no spawn outlives a shutdown.
    issue: tokio::sync::Mutex<()>,
    /// Set once the harness holding this session is installed in the registry (§5.1 item 2), to
    /// the channel that harness reads its paused requests from (#2348); before that `turn_start`
    /// refuses, so an install-race loser never mints or spawns.
    installed: OnceLock<HeldRequestSender>,
    #[cfg(feature = "fixtures")]
    pub(crate) hooks: Mutex<TestHooks>,
}

impl Shared {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .expect("claude planner session state poisoned")
    }

    /// The driver's last step: the session's facts move forward, the slot is freed and the turn's
    /// last events go out under one state lock, the same lock under which `turn_start`
    /// registers the next turn and sends its `TurnStarted`, so a subscriber never sees the next
    /// turn start before this one completed.
    pub(crate) fn finish_turn(
        &self,
        ended: FinishedTurn,
        events: impl IntoIterator<Item = PlannerEvent>,
    ) {
        let mut state = self.state();
        if ended.named_session {
            state.start = SessionStart::Resume;
        }
        if ended.row_bound {
            state.row_bound = true;
        }
        if let Some(total_tokens) = ended.total_tokens {
            state.total_tokens = total_tokens;
        }
        state.active = None;
        for event in events {
            let _ = self.events.send(event);
        }
    }
}

/// What a settled turn tells the session.
pub(crate) struct FinishedTurn {
    /// A `system/init` named this thread's session: later spawns resume it.
    pub(crate) named_session: bool,
    /// The row's `agent_session_id` is persisted.
    pub(crate) row_bound: bool,
    pub(crate) total_tokens: Option<i64>,
}

/// Fixtures-only interleaving points.
#[cfg(feature = "fixtures")]
#[derive(Default)]
pub(crate) struct TestHooks {
    pub(crate) after_spawn: Option<Arc<dyn Fn() + Send + Sync>>,
    pub(crate) before_turn_completed: Option<SettlePause>,
    /// How many user lines `turn_start` has tried to write to a spawned CLI.
    pub(crate) user_line_writes: usize,
}

/// An awaitable pause: the driver signals `entered` and waits for `release`.
#[cfg(feature = "fixtures")]
#[derive(Clone)]
pub struct SettlePause {
    pub entered: Arc<tokio::sync::Notify>,
    pub release: Arc<tokio::sync::Notify>,
}

pub struct ClaudePlannerSession {
    shared: Arc<Shared>,
}

/// What one spawn's user line carries: the person's turn, or a compaction's `/compact`.
pub(super) enum SpawnInput<'a> {
    Turn {
        items: Vec<InputItem>,
        client_id: &'a str,
        /// A rewind's pending cut, applied by this spawn.
        truncation: Option<&'a ResumeTruncation>,
    },
    Compaction,
}

mod invocation;
mod start;
pub(crate) use start::SpawnFailure;
impl ClaudePlannerSession {
    /// Open the session for its worker-session row: a row whose `agent_session_id` is bound
    /// resumes that Claude session (`--resume`); otherwise the first spawn creates it.
    pub async fn open(params: ClaudePlannerSessionParams) -> Result<Self> {
        let row = params
            .repo
            .session_get(&WorkerSessionId(params.worker_session_id.clone()))
            .await?
            .ok_or_else(|| {
                CalmError::NotFound(format!(
                    "worker session {} for a claude planner",
                    params.worker_session_id
                ))
            })?;
        let row_bound = row.agent_session_id.is_some();
        let start = if row_bound {
            SessionStart::Resume
        } else {
            SessionStart::New
        };
        let (events, _) = broadcast::channel(1024);
        let state = State {
            start,
            row_bound,
            settle_after_stop: SETTLE_AFTER_STOP,
            total_tokens: params.prior_total_tokens,
            mcp_token: None,
            shutting_down: false,
            active: None,
        };
        Ok(Self {
            shared: Arc::new(Shared {
                params,
                events,
                state: Mutex::new(state),
                issue: tokio::sync::Mutex::new(()),
                installed: OnceLock::new(),
                #[cfg(feature = "fixtures")]
                hooks: Mutex::new(TestHooks::default()),
            }),
        })
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<PlannerEvent> {
        self.shared.events.subscribe()
    }

    /// The registry installed the harness holding this session; turns may start from now on, and
    /// their tool approvals go to `held`. A session is installed once: a later call is ignored.
    pub fn mark_installed(&self, held: HeldRequestSender) {
        let _ = self.shared.installed.set(held);
    }

    /// The shared Claude Planner facility (config, marker instance).
    pub fn host(&self) -> &Arc<ClaudePlannerHost> {
        &self.shared.params.host
    }

    /// Whether the deletion fence has sealed `thread`.
    pub fn thread_sealed(&self, thread: &str) -> bool {
        self.shared.params.seals.is_sealed(thread)
    }

    pub fn active_turn_id_for_thread(&self, thread: &str) -> Option<String> {
        self.shared
            .state()
            .active
            .as_ref()
            .filter(|active| active.thread_id == thread)
            .map(|active| active.turn_id.clone())
    }

    /// Fixture projection of the production neutral invocation entry.
    #[cfg(feature = "fixtures")]
    pub async fn turn_start(
        &self,
        thread: &str,
        request: provider::control::TurnRequest,
    ) -> Result<String, crate::harness::backend::TurnStartFailure> {
        crate::harness::backend::ordinary_start_result(self.start_turn(thread, request).await)
    }

    /// The submission contract for either kind of spawn; see the module docs.
    pub(super) async fn start_spawn(
        &self,
        thread: &str,
        input: SpawnInput<'_>,
        selection: &TurnModelSelection,
        mode: PlannerPermissionMode,
    ) -> Result<String, SpawnFailure> {
        let purpose = match &input {
            SpawnInput::Turn { .. } => SpawnPurpose::Turn,
            SpawnInput::Compaction => SpawnPurpose::Compaction,
        };
        let shared = &self.shared;
        let params = &shared.params;
        let _issue = shared.issue.lock().await;
        let Some(held) = shared.installed.get() else {
            return Err(
                CalmError::Conflict("claude planner harness is not installed yet".into()).into(),
            );
        };
        let (start, row_bound, token, prior_total_tokens, settle_after_stop) = {
            let state = shared.state();
            if state.shutting_down {
                return Err(
                    CalmError::Conflict("claude planner session is shutting down".into()).into(),
                );
            }
            if state.active.is_some() {
                return Err(
                    CalmError::Conflict("a claude planner turn is still running".into()).into(),
                );
            }
            (
                state.start,
                state.row_bound,
                state.mcp_token.clone(),
                state.total_tokens,
                state.settle_after_stop,
            )
        };
        if purpose == SpawnPurpose::Compaction && start == SessionStart::New {
            return Err(CalmError::BadRequest(
                "this Claude conversation has no session to compact yet".into(),
            )
            .into());
        }
        let thread_uuid = Uuid::try_parse(thread).map_err(|_| {
            CalmError::BadRequest(format!("claude planner thread {thread} is not a UUID"))
        })?;
        let turn_id = Uuid::new_v4().to_string();
        if params.seals.is_sealed(thread) {
            return Err(sealed(thread).into());
        }
        let live = params
            .repo
            .session_get(&WorkerSessionId(params.worker_session_id.clone()))
            .await
            .map_err(CalmError::from)?
            .ok_or_else(|| CalmError::Conflict("Claude owner is missing".into()))?;
        let binding = crate::harness::profile::live_binding(params.repo.as_ref(), &live)
            .await?
            .ok_or_else(|| CalmError::Conflict("Claude live owner identity changed".into()))?;
        if live.provider != calm_types::worker::WorkerProviderKind::Claude
            || live.card_id.as_ref().map(|id| id.as_str()) != Some(params.card_id.as_str())
            || live.track_id.as_str() != params.track_id
            || live.thread_id.as_deref() != Some(thread)
            || (binding.profile == crate::harness::profile::HarnessProfile::Assistant
                && mode != PlannerPermissionMode::Never)
        {
            return Err(CalmError::Conflict("Claude live owner identity changed".into()).into());
        }
        let host = &params.host;
        let config = host
            .configured()
            .map_err(|_| needs_an_operator(unavailable_message()))?;
        let env = spawn::session_env(params, &config.config_dir)?;
        match config.version_problem(&env).await {
            None => {}
            Some(super::config::VersionProblem::NotPinned(problem)) => {
                return Err(needs_an_operator(problem).into());
            }
            Some(super::config::VersionProblem::NoAnswer(problem)) => {
                return Err(SpawnFailure::Failed {
                    error: CalmError::Conflict(problem),
                    line_sent: false,
                });
            }
        }
        // A shutdown or a deletion seal may have landed while `--version` ran; nothing is minted
        // for a session that can no longer start a turn.
        if shared.state().shutting_down {
            return Err(
                CalmError::Conflict("claude planner session is shutting down".into()).into(),
            );
        }
        if params.seals.is_sealed(thread) {
            return Err(sealed(thread).into());
        }
        // A revocation under a live harness (e.g. one an aborted deletion's recovery could not
        // replace, §5.1 item 4) nulls the row's hash; the next spawn must not carry a credential that
        // no longer authenticates.
        let token = match token {
            Some(token) if self.row_hash_present().await? => token,
            _ => self.mint_mcp_token().await?,
        };
        stop(&host.instance, &params.worker_session_id).await?;

        let (line, client_id, items, truncation) = match input {
            SpawnInput::Turn {
                items,
                client_id,
                truncation,
            } => {
                let line = UserLine::new(
                    thread_uuid,
                    client_line_uuid(client_id).map_err(|e| SpawnFailure::Failed {
                        error: CalmError::BadRequest(e.to_string()),
                        line_sent: false,
                    })?,
                    user_line_content(&items)
                        .await
                        .map_err(|error| SpawnFailure::Failed {
                            error,
                            line_sent: false,
                        })?,
                );
                (line, client_id.to_string(), items, truncation)
            }
            // No client sent this line: its uuid is fresh, and nothing of it is stored.
            SpawnInput::Compaction => {
                let uuid = Uuid::new_v4();
                let line = UserLine::compact(thread_uuid, uuid);
                (line, uuid.simple().to_string(), Vec::new(), None)
            }
        };
        let line = serde_json::to_string(&line).map_err(|e| SpawnFailure::Failed {
            error: e.into(),
            line_sent: false,
        })?;
        let mut translator = TurnTranslator::new(
            TurnContext {
                thread_id: thread.to_string(),
                turn_id: turn_id.clone(),
                client_id,
                input: items,
                cwd: params.cwd.to_string_lossy().into_owned(),
                prior_total_tokens,
            },
            params.calm_tools.clone(),
        )
        .map_err(|e| SpawnFailure::Failed {
            error: CalmError::BadRequest(e.to_string()),
            line_sent: false,
        })?;
        let instructions = InstructionsFile::write(
            &host.instructions_dir,
            &params.worker_session_id,
            &turn_id,
            &params.instructions,
        )?;
        let argv = spawn::argv(
            thread_uuid,
            purpose,
            start,
            truncation,
            selection,
            mode,
            &params.cwd,
            &host.mcp_shim,
            instructions.path(),
        );
        let argv = match argv {
            Ok(argv) => argv,
            Err(error) => {
                let failure = self.abort_before_ok(None, instructions, error).await;
                return Err(SpawnFailure::Failed {
                    error: failure,
                    line_sent: false,
                });
            }
        };
        let spawned = Command::new(&config.claude_binary)
            .args(argv)
            .env_clear()
            .envs(env)
            .env("NEIGE_MCP_TOKEN", token)
            .current_dir(&params.cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                let error = CalmError::Conflict(format!(
                    "claude planner spawn of {} failed: {error}",
                    config.claude_binary.display()
                ));
                let failure = self.abort_before_ok(None, instructions, error).await;
                return Err(SpawnFailure::Failed {
                    error: failure,
                    line_sent: false,
                });
            }
        };
        #[cfg(feature = "fixtures")]
        {
            let hook = shared.hooks.lock().expect("hooks").after_spawn.clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        if params.seals.is_sealed(thread) {
            return Err(self
                .abort_before_ok(Some(child), instructions, sealed(thread))
                .await
                .into());
        }
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let error = CalmError::Internal("claude planner child has no piped stdio".into());
            return Err(self
                .abort_before_ok(Some(child), instructions, error)
                .await
                .into());
        };
        let slot = Arc::new(TurnSlot::new(stdin, settle_after_stop));
        #[cfg(feature = "fixtures")]
        {
            shared.hooks.lock().expect("hooks").user_line_writes += 1;
        }
        if let Err(error) = slot.write_line(&line).await {
            let failure = self.abort_before_ok(Some(child), instructions, error).await;
            return Err(SpawnFailure::Failed {
                error: failure,
                line_sent: true,
            });
        }

        {
            // One lock with `finish_turn`'s emit, so this `TurnStarted` never overtakes the
            // previous turn's `TurnCompleted`.
            let mut state = shared.state();
            state.active = Some(ActiveTurn {
                thread_id: thread.to_string(),
                turn_id: turn_id.clone(),
                slot: Arc::clone(&slot),
            });
            let _ = shared
                .events
                .send(translator.turn_started(crate::model::now_ms()));
        }
        tokio::spawn(drive(
            Arc::clone(shared),
            TurnRun {
                child,
                stdout,
                stderr,
                slot,
                translator,
                instructions,
                thread: thread_uuid,
                purpose,
                bind: !row_bound,
                version: config.claude_version.clone(),
                approvals: Approvals::for_spawn(mode, held, &turn_id),
            },
        ));
        Ok(turn_id)
    }

    /// Whether the worker-session row still carries an MCP hash.
    async fn row_hash_present(&self) -> Result<bool> {
        let params = &self.shared.params;
        let row = params
            .repo
            .session_get(&WorkerSessionId(params.worker_session_id.clone()))
            .await?
            .ok_or_else(|| {
                CalmError::NotFound(format!("worker session {}", params.worker_session_id))
            })?;
        Ok(row.mcp_token_hash.is_some())
    }

    /// The harness's first-turn credential (§5.1 item 2): card row and session hash in one
    /// transaction, the session write guarded on the row still being active. Runs under the
    /// issuance lock, after the install and shutdown checks, so it happens at most once per harness
    /// and never after a shutdown (again only if a revocation nulled the row's hash); the plaintext stays
    /// in this session.
    async fn mint_mcp_token(&self) -> Result<String> {
        let params = &self.shared.params;
        let card_id = params.card_id.clone();
        let worker_session_id = params.worker_session_id.clone();
        let token = crate::db::write_in_tx_typed(params.repo.as_ref(), move |tx| {
            Box::pin(async move {
                crate::mcp_server::wiring::mint_and_persist_managed_planner_token(
                    tx,
                    &card_id,
                    &worker_session_id,
                )
                .await
            })
        })
        .await?;
        self.shared.state().mcp_token = Some(token.clone());
        Ok(token)
    }

    /// Stop the marker (which ends a spawned child and anything it started), reap the child, and
    /// drop the instructions guard; the caller's error is what `turn_start` returns.
    async fn abort_before_ok(
        &self,
        child: Option<Child>,
        instructions: InstructionsFile,
        error: CalmError,
    ) -> CalmError {
        let params = &self.shared.params;
        if let Err(stop_error) = stop(&params.host.instance, &params.worker_session_id).await {
            tracing::warn!(
                worker_session_id = %params.worker_session_id,
                error = %stop_error,
                "claude planner: stop after a failed turn start did not confirm"
            );
        }
        if let Some(mut child) = child {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        }
        drop(instructions);
        error
    }

    /// Record `Interrupted`, ask the CLI to interrupt, and arm the stop timer; a turn that is not
    /// this session's live one has nothing to interrupt.
    pub async fn turn_interrupt(
        &self,
        thread: &str,
        turn: &str,
    ) -> provider::control::InterruptOutcome<CalmError> {
        let slot = self
            .shared
            .state()
            .active
            .as_ref()
            .filter(|active| active.thread_id == thread && active.turn_id == turn)
            .map(|active| Arc::clone(&active.slot));
        match slot {
            Some(slot) => interrupt(&slot).await,
            None => provider::control::InterruptOutcome::NoTarget,
        }
    }

    /// Harness shutdown: refuse new turns, end the live one as `Interrupted`, and stop the marker
    /// whether or not a turn runs. The `stop` result is returned for strict callers. A session that
    /// was never installed (an install-race loser) minted and spawned nothing, so it signals nothing:
    /// its id may belong to the winner's live turn.
    pub async fn shutdown(&self) -> Result<()> {
        let shared = &self.shared;
        shared.state().shutting_down = true;
        let _issue = shared.issue.lock().await;
        if shared.installed.get().is_none() {
            return Ok(());
        }
        let slot = shared
            .state()
            .active
            .as_ref()
            .map(|active| Arc::clone(&active.slot));
        if let Some(slot) = &slot {
            slot.record(TerminalCause::Interrupted);
            slot.arm_stop(tokio::time::Instant::now());
        }
        let params = &shared.params;
        let stopped = stop(&params.host.instance, &params.worker_session_id).await;
        if let Some(slot) = slot {
            let mut settled = slot.settled.subscribe();
            if tokio::time::timeout(SHUTDOWN_SETTLE_WAIT, settled.wait_for(|done| *done))
                .await
                .is_err()
            {
                tracing::warn!(
                    worker_session_id = %params.worker_session_id,
                    "claude planner: the running turn did not settle during shutdown"
                );
            }
        }
        stopped
    }

    /// Fixtures only: how many user lines this session has tried to write, observed on the
    /// session's side so a test does not depend on the CLI recording a line before it is stopped.
    #[cfg(feature = "fixtures")]
    pub fn user_line_writes_for_test(&self) -> usize {
        self.shared.hooks.lock().expect("hooks").user_line_writes
    }

    #[cfg(feature = "fixtures")]
    pub fn set_after_spawn_hook_for_test(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        self.shared.hooks.lock().expect("hooks").after_spawn = Some(hook);
    }

    /// Fixtures only: a shorter [`SETTLE_AFTER_STOP`] for turns started after this call, so a test
    /// can make the settlement deadline bind within seconds.
    #[cfg(feature = "fixtures")]
    pub fn set_settle_after_stop_for_test(&self, margin: Duration) {
        self.shared.state().settle_after_stop = margin;
    }

    #[cfg(feature = "fixtures")]
    pub fn set_before_turn_completed_pause_for_test(&self, pause: SettlePause) {
        self.shared
            .hooks
            .lock()
            .expect("hooks")
            .before_turn_completed = Some(pause);
    }
}

/// The stop timer is armed before the interrupt line is written, so a CLI that stopped reading
/// stdin cannot delay it.
async fn interrupt(slot: &TurnSlot) -> provider::control::InterruptOutcome<CalmError> {
    slot.record(TerminalCause::Interrupted);
    slot.arm_stop(tokio::time::Instant::now() + STOP_TIMER);
    let request = super::protocol::ControlRequestOut::new(
        format!("interrupt-{}", Uuid::new_v4().simple()),
        super::protocol::ControlRequestKind::Interrupt,
    );
    use provider::control::SubmissionEvidence;
    let written = match serde_json::to_string(&request) {
        Ok(line) => slot
            .write_line(&line)
            .await
            .map_err(|error| (error, SubmissionEvidence::WriteMayHaveReachedPeer)),
        Err(error) => Err((error.into(), SubmissionEvidence::NotWritten)),
    };
    match written {
        Ok(()) => provider::control::InterruptOutcome::LocalSignalDelivered,
        Err((error, evidence)) => {
            tracing::warn!(%error, "claude planner: interrupt request not written; the stop timer ends the turn");
            provider::control::InterruptOutcome::Unconfirmed { error, evidence }
        }
    }
}

fn sealed(thread: &str) -> CalmError {
    CalmError::Conflict(format!("thread {thread} is sealed for deletion"))
}

/// Text blocks, and one base64 image block per bound attachment (§5.6); the attachment's file name
/// is its id, which names its format.
async fn user_line_content(items: &[InputItem]) -> Result<Vec<UserLineContent>> {
    let mut content = Vec::with_capacity(items.len());
    for item in items {
        match item {
            InputItem::Text { text } => content.push(UserLineContent::Text { text: text.clone() }),
            InputItem::LocalImage { path } => {
                let id = std::path::Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| AttachmentId::parse(name).ok())
                    .ok_or_else(|| {
                        CalmError::BadRequest(format!("{path} is not a bound attachment"))
                    })?;
                let bytes = tokio::fs::read(path).await?;
                content.push(UserLineContent::Image {
                    source: Base64Image::new(
                        id.format().mime(),
                        base64::engine::general_purpose::STANDARD.encode(bytes),
                    ),
                });
            }
        }
    }
    Ok(content)
}

#[cfg(test)]
mod interrupt_tests;
