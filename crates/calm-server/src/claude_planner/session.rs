//! One Claude Planner session: one `claude -p` process per turn, at most one at a time (design
//! #1791 §5.1, §5.2, §5.6, §6.2).
//!
//! Submission contract ([`ClaudePlannerSession::turn_start`]): refuse until the harness is
//! installed, mint the turn id, check the seal, run `--version` (before any user input), on the
//! harness's first turn mint its MCP credential (§5.1 item 2), `stop` whatever still carries this
//! session's marker, write the instructions file under a per-spawn guard, spawn, re-check the seal,
//! write the one `user` line (bounded), and return `Ok(turn_id)`. Every exit before `Ok` stops the
//! marker, removes the file and returns `Err`; no outcome is recorded, because no turn id was handed
//! out.
//!
//! Settlement (`driver`): one [`TurnSlot`] per turn holds the first recorded [`TerminalCause`]; the
//! outcome is recorded durably, stdin is closed, the direct child gets a bounded wait, `stop` runs,
//! the instructions file goes, and only then is `TurnCompleted` emitted.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use calm_types::planner_attachment::AttachmentId;
use tokio::io::AsyncWriteExt as _;
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{broadcast, watch};
use uuid::Uuid;

use super::config::ClaudePlannerHost;
use super::driver::{TurnRun, drive};
use super::protocol::{Base64Image, UserLine, UserLineContent, client_line_uuid};
use super::spawn::{self, EnvInputs, InstructionsFile, SessionStart};
use super::stop::stop;
use super::translate::{CalmToolNames, TurnContext, TurnTranslator};
use crate::codex_appserver::{InputItem, Notification};
use crate::db::Repo;
use crate::error::{CalmError, Result};
use crate::shared_codex_appserver::SharedCodexAppServer;
use calm_types::worker::WorkerSessionId;

/// After an interrupt, the turn is stopped if the CLI has not ended it within this bound.
pub const STOP_TIMER: Duration = Duration::from_secs(10);
/// Once a stop is armed, the turn's `TurnCompleted` is emitted by `settle_by` = the stop deadline +
/// this margin, whatever the CLI does: every await after the stop is armed is capped by it. An
/// interrupt therefore settles by interrupt + [`STOP_TIMER`] + this = 22 s, inside the harness's
/// 30 s `interrupt_completion_budget` with 8 s left for the run loop's own work around the call.
pub const SETTLE_AFTER_STOP: Duration = Duration::from_secs(12);
/// A stdin line that the CLI does not take within this bound fails the write.
pub(crate) const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
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
    /// The calm tools visible to the card, to restore their dotted names.
    pub calm_tools: CalmToolNames,
    /// `(UPPER, lower, value)` proxy pairs from the server's resolver.
    pub proxy: Vec<(String, String, String)>,
    /// The thread's lifetime token total from the harness snapshot.
    pub prior_total_tokens: i64,
    pub repo: Arc<dyn Repo>,
    /// The shared daemon, held only for its thread-keyed deletion seals.
    pub seals: Arc<SharedCodexAppServer>,
}

/// Why a turn is ending, recorded before the path that ends it acts; the first one recorded wins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalCause {
    /// A user or watchdog interrupt, or a harness shutdown.
    Interrupted,
    /// An init check or an undecodable line.
    Failed(String),
}

/// The live turn's shared state.
pub(crate) struct TurnSlot {
    cause: Mutex<Option<TerminalCause>>,
    pub(crate) stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    /// When the turn is stopped whatever the CLI does: armed by an interrupt (now + the stop
    /// timer) or a shutdown (now); the earliest arming wins.
    pub(crate) stop_at: watch::Sender<Option<tokio::time::Instant>>,
    /// [`SETTLE_AFTER_STOP`], or a fixtures override.
    settle_after_stop: Duration,
    pub(crate) settled: watch::Sender<bool>,
}

impl TurnSlot {
    fn new(stdin: ChildStdin, settle_after_stop: Duration) -> Self {
        Self {
            cause: Mutex::new(None),
            stdin: tokio::sync::Mutex::new(Some(stdin)),
            stop_at: watch::Sender::new(None),
            settle_after_stop,
            settled: watch::Sender::new(false),
        }
    }

    pub(crate) fn record(&self, cause: TerminalCause) {
        let mut slot = self.cause.lock().expect("turn slot poisoned");
        if slot.is_none() {
            *slot = Some(cause);
        }
    }

    /// Arm the stop at `at` unless an earlier one is armed.
    pub(crate) fn arm_stop(&self, at: tokio::time::Instant) {
        self.stop_at.send_if_modified(|current| match current {
            Some(armed) if *armed <= at => false,
            _ => {
                *current = Some(at);
                true
            }
        });
    }

    /// Whether a stop (interrupt timer or shutdown) is armed.
    pub(crate) fn stop_armed(&self) -> bool {
        self.stop_at.borrow().is_some()
    }

    /// The hard settlement deadline, once a stop is armed.
    pub(crate) fn settle_by(&self) -> Option<tokio::time::Instant> {
        self.stop_at.borrow().map(|at| at + self.settle_after_stop)
    }

    /// `fut` bounded by its own timeout and, once a stop is armed (even while `fut` runs), by
    /// [`Self::settle_by`]; `None` when either bound passed first.
    pub(crate) async fn bounded<F: std::future::Future>(
        &self,
        own: Duration,
        fut: F,
    ) -> Option<F::Output> {
        let mut stop_rx = self.stop_at.subscribe();
        tokio::select! {
            biased;
            out = fut => Some(out),
            _ = tokio::time::sleep(own) => None,
            _ = deadline_reached(&mut stop_rx, self.settle_after_stop) => None,
        }
    }

    pub(crate) fn cause(&self) -> Option<TerminalCause> {
        self.cause.lock().expect("turn slot poisoned").clone()
    }

    /// Write one line to the CLI's stdin within [`WRITE_TIMEOUT`].
    pub(crate) async fn write_line(&self, line: &str) -> Result<()> {
        write_line(&self.stdin, line).await
    }
}

async fn write_line(stdin: &tokio::sync::Mutex<Option<ChildStdin>>, line: &str) -> Result<()> {
    let write = async {
        let mut guard = stdin.lock().await;
        let stdin = guard
            .as_mut()
            .ok_or_else(|| CalmError::Conflict("claude stdin is already closed".into()))?;
        // One buffer, so a timed-out write can never leave a line without its newline.
        let mut framed = String::with_capacity(line.len() + 1);
        framed.push_str(line);
        framed.push('\n');
        stdin.write_all(framed.as_bytes()).await?;
        stdin.flush().await?;
        Ok::<_, CalmError>(())
    };
    match tokio::time::timeout(WRITE_TIMEOUT, write).await {
        Ok(written) => written,
        Err(_) => {
            // A partly written line may sit in the pipe: close stdin so nothing is appended to it.
            if let Ok(mut guard) = stdin.try_lock() {
                guard.take();
            }
            Err(CalmError::Conflict(
                "claude did not take its stdin line in time".into(),
            ))
        }
    }
}

/// Resolves once a stop is armed and `extra` has passed since its deadline; `extra` zero is the
/// stop itself, [`SETTLE_AFTER_STOP`] is `settle_by`.
pub(crate) async fn deadline_reached(
    rx: &mut watch::Receiver<Option<tokio::time::Instant>>,
    extra: Duration,
) {
    loop {
        let armed = *rx.borrow_and_update();
        match armed {
            Some(at) => tokio::select! {
                _ = tokio::time::sleep_until(at + extra) => return,
                changed = rx.changed() => if changed.is_err() {
                    tokio::time::sleep_until(at + extra).await;
                    return;
                },
            },
            None => {
                if rx.changed().await.is_err() {
                    std::future::pending::<()>().await;
                }
            }
        }
    }
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
    pub(crate) notifications: broadcast::Sender<Notification>,
    state: Mutex<State>,
    /// Serializes `turn_start` and `shutdown`, so no spawn outlives a shutdown.
    issue: tokio::sync::Mutex<()>,
    /// Set once the harness holding this session is installed in the registry (§5.1 item 2);
    /// before that `turn_start` refuses, so an install-race loser never mints or spawns.
    installed: AtomicBool,
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
    /// last notifications go out under one state lock, the same lock under which `turn_start`
    /// registers the next turn and sends its `TurnStarted`, so a subscriber never sees the next
    /// turn start before this one completed.
    pub(crate) fn finish_turn(
        &self,
        ended: FinishedTurn,
        notifications: impl IntoIterator<Item = Notification>,
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
        for notification in notifications {
            let _ = self.notifications.send(notification);
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
        let (notifications, _) = broadcast::channel(1024);
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
                notifications,
                state: Mutex::new(state),
                issue: tokio::sync::Mutex::new(()),
                installed: AtomicBool::new(false),
                #[cfg(feature = "fixtures")]
                hooks: Mutex::new(TestHooks::default()),
            }),
        })
    }

    pub fn subscribe_notifications(&self) -> broadcast::Receiver<Notification> {
        self.shared.notifications.subscribe()
    }

    /// The registry installed the harness holding this session; turns may start from now on.
    pub fn mark_installed(&self) {
        self.shared.installed.store(true, Ordering::SeqCst);
    }

    /// The shared Claude Planner facility (config, marker instance).
    pub fn host(&self) -> &Arc<ClaudePlannerHost> {
        &self.shared.params.host
    }

    /// The shared daemon this session consults for thread seals.
    pub fn codex(&self) -> &Arc<SharedCodexAppServer> {
        &self.shared.params.seals
    }

    pub fn active_turn_id_for_thread(&self, thread: &str) -> Option<String> {
        self.shared
            .state()
            .active
            .as_ref()
            .filter(|active| active.thread_id == thread)
            .map(|active| active.turn_id.clone())
    }

    /// See the module docs for the submission contract.
    pub async fn turn_start(
        &self,
        thread: &str,
        items: Vec<InputItem>,
        client_id: &str,
    ) -> Result<String> {
        let shared = &self.shared;
        let params = &shared.params;
        let _issue = shared.issue.lock().await;
        if !shared.installed.load(Ordering::SeqCst) {
            return Err(CalmError::Conflict(
                "claude planner harness is not installed yet".into(),
            ));
        }
        let (start, row_bound, token, prior_total_tokens, settle_after_stop) = {
            let state = shared.state();
            if state.shutting_down {
                return Err(CalmError::Conflict(
                    "claude planner session is shutting down".into(),
                ));
            }
            if state.active.is_some() {
                return Err(CalmError::Conflict(
                    "a claude planner turn is still running".into(),
                ));
            }
            (
                state.start,
                state.row_bound,
                state.mcp_token.clone(),
                state.total_tokens,
                state.settle_after_stop,
            )
        };
        let thread_uuid = Uuid::try_parse(thread).map_err(|_| {
            CalmError::BadRequest(format!("claude planner thread {thread} is not a UUID"))
        })?;
        let turn_id = Uuid::new_v4().to_string();
        if params.seals.turn_thread_is_sealed(thread) {
            return Err(sealed(thread));
        }
        let host = &params.host;
        let config = host.configured()?;
        let kernel_path = crate::kernel_bin_path::kernel_led_path()?;
        let env = spawn::base_env(&EnvInputs {
            path: kernel_path.path,
            config_dir: &config.config_dir,
            mcp_socket: &host.mcp_socket,
            marker: host.instance.marker(&params.worker_session_id),
            proxy: &params.proxy,
        });
        config.verify_version(&env).await?;
        let token = match token {
            Some(token) => token,
            None => self.mint_mcp_token().await?,
        };
        stop(&host.instance, &params.worker_session_id).await?;

        let line = serde_json::to_string(&UserLine::new(
            thread_uuid,
            client_line_uuid(client_id).map_err(|e| CalmError::BadRequest(e.to_string()))?,
            user_line_content(&items).await?,
        ))?;
        let translator = TurnTranslator::new(
            TurnContext {
                thread_id: thread.to_string(),
                turn_id: turn_id.clone(),
                client_id: client_id.to_string(),
                input: items,
                cwd: params.cwd.to_string_lossy().into_owned(),
                prior_total_tokens,
            },
            params.calm_tools.clone(),
        )
        .map_err(|e| CalmError::BadRequest(e.to_string()))?;
        let instructions = InstructionsFile::write(
            &host.instructions_dir,
            &params.worker_session_id,
            &turn_id,
            &params.instructions,
        )?;
        let argv = spawn::argv(
            thread_uuid,
            start,
            &params.cwd,
            &host.mcp_shim,
            instructions.path(),
        );
        let argv = match argv {
            Ok(argv) => argv,
            Err(error) => return Err(self.abort_before_ok(None, instructions, error).await),
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
                return Err(self.abort_before_ok(None, instructions, error).await);
            }
        };
        #[cfg(feature = "fixtures")]
        {
            let hook = shared.hooks.lock().expect("hooks").after_spawn.clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        if params.seals.turn_thread_is_sealed(thread) {
            return Err(self
                .abort_before_ok(Some(child), instructions, sealed(thread))
                .await);
        }
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let error = CalmError::Internal("claude planner child has no piped stdio".into());
            return Err(self.abort_before_ok(Some(child), instructions, error).await);
        };
        let slot = Arc::new(TurnSlot::new(stdin, settle_after_stop));
        if let Err(error) = slot.write_line(&line).await {
            return Err(self.abort_before_ok(Some(child), instructions, error).await);
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
            let _ = shared.notifications.send(translator.turn_started());
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
                bind: !row_bound,
                version: config.claude_version.clone(),
            },
        ));
        Ok(turn_id)
    }

    /// The harness's first-turn credential (§5.1 item 2): card row and session hash in one
    /// transaction, the session write guarded on the row still being active. Runs under the
    /// issuance lock, after the install and shutdown checks, so it happens at most once per harness
    /// and never after a shutdown; the plaintext stays in this session.
    async fn mint_mcp_token(&self) -> Result<String> {
        let params = &self.shared.params;
        let card_id = params.card_id.clone();
        let worker_session_id = params.worker_session_id.clone();
        let token = crate::db::write_in_tx_typed(params.repo.as_ref(), move |tx| {
            Box::pin(async move {
                crate::mcp_server::wiring::mint_and_persist_claude_planner_token(
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
    pub async fn turn_interrupt(&self, thread: &str, turn: &str) -> Result<()> {
        let slot = self
            .shared
            .state()
            .active
            .as_ref()
            .filter(|active| active.thread_id == thread && active.turn_id == turn)
            .map(|active| Arc::clone(&active.slot));
        match slot {
            Some(slot) => {
                interrupt(&slot).await;
                Ok(())
            }
            None => Ok(()),
        }
    }

    pub async fn interrupt_active_turn(&self, thread: &str) -> Result<()> {
        match self.active_turn_id_for_thread(thread) {
            Some(turn) => self.turn_interrupt(thread, &turn).await,
            None => Ok(()),
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
        if !shared.installed.load(Ordering::SeqCst) {
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
async fn interrupt(slot: &TurnSlot) {
    slot.record(TerminalCause::Interrupted);
    slot.arm_stop(tokio::time::Instant::now() + STOP_TIMER);
    let request = super::protocol::ControlRequestOut::new(
        format!("interrupt-{}", Uuid::new_v4().simple()),
        super::protocol::ControlRequestKind::Interrupt,
    );
    let written = match serde_json::to_string(&request) {
        Ok(line) => slot.write_line(&line).await,
        Err(error) => Err(error.into()),
    };
    if let Err(error) = written {
        tracing::warn!(%error, "claude planner: interrupt request not written; the stop timer ends the turn");
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
mod tests {
    use std::time::Duration;

    use super::TurnSlot;

    /// `bounded` gives up at `settle_by` when the stop is armed WHILE its future is pending, far
    /// ahead of the future's own bound (the settle_by arm, #1791 PR2b follow-up).
    #[tokio::test(start_paused = true)]
    async fn bounded_gives_up_at_settle_by_armed_while_pending() {
        let mut child = tokio::process::Command::new("cat")
            .stdin(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn cat");
        let slot = TurnSlot::new(child.stdin.take().expect("stdin"), Duration::from_secs(2));
        let started = tokio::time::Instant::now();
        let arm = async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            slot.arm_stop(tokio::time::Instant::now());
        };
        let (bounded, ()) = tokio::join!(
            slot.bounded(Duration::from_secs(60), std::future::pending::<()>()),
            arm
        );
        assert_eq!(bounded, None);
        assert_eq!(
            started.elapsed(),
            Duration::from_secs(3),
            "armed at 1 s + 2 s margin"
        );
    }
}
