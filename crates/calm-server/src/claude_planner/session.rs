//! One Claude Planner session admits one Engine execution per turn or compaction.
//! The session issue lock and durable provider owner claim serialize admission and
//! carrier detachment. Credentials are released over Held stdin before the first
//! user line; protocol decoding remains in provider. The registered owner completes
//! outcomes and session state after settlement, including recovery after driver loss.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::sync::broadcast;
use uuid::Uuid;

use super::approvals::Approvals;
use super::config::{ClaudePlannerHost, needs_an_operator, unavailable_message};
use super::protocol_turn::{ProtocolTurn, drive};
use super::role::PlannerRole;
use super::spawn::{self, InstructionsFile, ResumeTruncation, SessionStart, SpawnPurpose};
use super::turn_sink::PlannerSink;
use super::turn_slot::{TerminalCause, TurnSlot};
use crate::db::Repo;
use crate::error::{CalmError, Result};
use crate::harness::held_requests::HeldRequestSender;
use crate::planner_permission_mode::PlannerPermissionMode;
use crate::provider_execution::StopReason;
use crate::thread_seals::ThreadSeals;
use calm_types::worker::WorkerSessionId;
use provider::InputItem;
use provider::TurnModelSelection;
use provider::claude::protocol::UserLine;
use provider::claude::translate::{ToolNames, TurnContext, TurnTranslator};
use provider::events::PlannerEvent;

pub(super) mod execution_end;
pub(crate) use execution_end::PreparedInterrupt;
mod process;

/// After an interrupt, the turn is stopped if the CLI has not ended it within this bound.
pub const STOP_TIMER: Duration = Duration::from_secs(10);
pub use super::protocol_turn::SETTLE_AFTER_STOP;
/// How long [`ClaudePlannerSession::shutdown`] waits for the running turn to settle after its stop.
const SHUTDOWN_SETTLE_WAIT: Duration = Duration::from_secs(20);

/// Everything one Claude Planner harness is built from; the caller (the start adapter and
/// recovery) supplies every field.
pub struct ClaudePlannerSessionParams {
    /// All provider executions use the registered Engine; there is no native fallback.
    pub engine: Option<crate::execution_boot::ExecutionEngine>,
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
    /// Live observations survive abort or failure of the completion write.
    finished: FinishedTurn,
}

struct State {
    start: SessionStart,
    /// Whether the row's `agent_session_id` is persisted; a turn whose bind failed leaves it
    /// unset, and the next `--resume` spawn retries the bind.
    row_bound: bool,
    /// Set once durable facts leave a recorded launch unconfirmed; it never clears.
    identity_hold: Option<IdentityHold>,
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
    /// Serializes provider admission, carrier detachment and shutdown.
    issue: tokio::sync::Mutex<()>,
    parked: crate::harness::carrier_park::ParkState,
    driver: tokio::sync::Mutex<Option<execution_end::DriverSlot>>,
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

    /// Only the exact active slot can publish facts consumed by either completion path.
    pub(crate) fn observe_turn<R>(
        &self,
        slot: &TurnSlot,
        observe: impl FnOnce(&mut FinishedTurn) -> R,
    ) -> Option<R> {
        self.state()
            .active
            .as_mut()
            .filter(|active| std::ptr::eq(active.slot.as_ref(), slot))
            .map(|active| observe(&mut active.finished))
    }

    /// The driver's last step: the session's facts move forward, the slot is freed and the turn's
    /// last events go out under one state lock, the same lock under which `turn_start`
    /// registers the next turn and sends its `TurnStarted`, so a subscriber never sees the next
    /// turn start before this one completed.
    pub(crate) fn finish_turn(
        &self,
        turn: &str,
        slot: &TurnSlot,
        events: impl IntoIterator<Item = PlannerEvent>,
    ) {
        let mut state = self.state();
        if !state.active.as_ref().is_some_and(|active| {
            active.turn_id == turn && std::ptr::eq(active.slot.as_ref(), slot)
        }) {
            return;
        }
        let ended = state.active.take().expect("checked active slot").finished;
        if ended.named_session {
            state.start = SessionStart::Resume;
        }
        if ended.row_bound {
            state.row_bound = true;
        }
        if let Some(total_tokens) = ended.total_tokens {
            state.total_tokens = total_tokens;
        }
        for event in events {
            let _ = self.events.send(event);
        }
    }
}

/// A recorded `--session-id` launch that no durable fact confirms: this session neither
/// resumes nor relaunches it. The recovery owner separately qualifies a fresh session; ordinary detachment
/// still requires a bound identity.
#[derive(Clone, Copy)]
pub(super) struct IdentityHold {
    pub(super) canonical: bool,
}

impl IdentityHold {
    fn reader(self) -> String {
        if self.canonical {
            "This Claude session could not be confirmed after an earlier launch. Nothing was sent; saved messages are kept."
        } else {
            "This Claude session could not be confirmed, so nothing was sent. Start a fresh session to continue (history is kept)."
        }
        .into()
    }

    pub(super) fn refusal(self) -> crate::harness::backend::TurnStartFailure {
        crate::harness::backend::TurnStartFailure::AwaitingRecovery {
            error: CalmError::Conflict(if self.canonical {
                "Claude session launch is unconfirmed; it is never launched again".into()
            } else {
                "Claude session launch is unconfirmed; only /planner/restart continues".into()
            }),
            reader: self.reader(),
        }
    }
}

/// What a settled turn tells the session.
#[derive(Default)]
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
    pub(crate) instructions_fault: Option<spawn::InstructionsFault>,
    pub(crate) after_spawn: Option<Arc<dyn Fn() + Send + Sync>>,
    pub(crate) before_turn_completed: Option<SettlePause>,
    pub(crate) user_flush_failure: Option<SettlePause>,
    pub(crate) user_writer_wait: Option<SettlePause>,
    pub(crate) dispatch_result: Option<SettlePause>,
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
    Canonical {
        items: Vec<InputItem>,
        client_id: &'a str,
        execution: crate::harness::backend::ExecutionInput,
    },
    Compaction,
    CanonicalCompaction(crate::harness::backend::MaintenanceTarget),
}

/// Historical invocation intentionally has no canonical input policy.
/// The required gate still travels to the real transport; there is no missing-gate fallback.
pub(super) fn historical_admission()
-> crate::harness::native_admission::NativePromptAdmission<'static> {
    crate::harness::native_admission::NativePromptAdmission::new(
        || Ok(()),
        || Box::pin(async { Ok(()) }),
    )
}

#[cfg(feature = "fixtures")]
mod fixtures;
mod invocation;
mod launch;
mod start;
pub(crate) use start::{BeforeWriteFailure, SpawnFailure};
impl ClaudePlannerSession {
    /// Open the session for its worker-session row: a row whose `agent_session_id` is bound
    /// resumes that Claude session (`--resume`). An unbound canonical row first recovers
    /// from verified native replay evidence. A recorded launch without either is held;
    /// only a row with no unconfirmed launch starts new.
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
        use super::identity::Identity;
        let identity = super::identity::open(&params).await?;
        let row_bound = matches!(identity, Identity::Bound | Identity::Recovered);
        let identity_hold = (identity == Identity::Unconfirmed).then_some(IdentityHold {
            canonical: super::identity::canonical(&row),
        });
        let start = if row_bound {
            SessionStart::Resume
        } else {
            SessionStart::New
        };
        let (events, _) = broadcast::channel(1024);
        let state = State {
            start,
            row_bound,
            identity_hold,
            settle_after_stop: SETTLE_AFTER_STOP,
            total_tokens: params.prior_total_tokens,
            mcp_token: None,
            shutting_down: false,
            active: None,
        };
        let session = Self {
            shared: Arc::new(Shared {
                params,
                events,
                state: Mutex::new(state),
                issue: tokio::sync::Mutex::new(()),
                parked: Default::default(),
                driver: tokio::sync::Mutex::new(None),
                installed: OnceLock::new(),
                #[cfg(feature = "fixtures")]
                hooks: Mutex::new(TestHooks::default()),
            }),
        };
        session
            .host()
            .completion
            .bind(session.shared.params.repo.clone());
        Ok(session)
    }

    /// Matching init or a durable repair established identity in this host. This is
    /// a display fact only; it grants no input authority or execution completion.
    pub(crate) fn identity_known_in_host(&self) -> bool {
        let state = self.shared.state();
        state.start == SessionStart::Resume && !state.shutting_down
    }

    /// The provider-owned issuance hold of an unconfirmed launch, in the reader's words.
    pub(crate) fn identity_hold(&self) -> Option<String> {
        self.shared.state().identity_hold.map(IdentityHold::reader)
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

    pub(crate) async fn carrier_idle_for_detach(
        &self,
    ) -> Option<crate::harness::carrier_park::ParkToken<'_>> {
        let _issue = self.shared.issue.try_lock().ok()?;
        self.provider_admission_gate().await.ok()?;
        if !self.carrier_idle() {
            return None;
        }
        self.shared.parked.park()
    }
    pub(crate) async fn carrier_idle_for_recovery(
        &self,
    ) -> Option<crate::harness::carrier_park::ParkToken<'_>> {
        let _issue = self.shared.issue.try_lock().ok()?;
        self.provider_admission_gate().await.ok()?;
        if !self.recovery_idle() {
            return None;
        }
        self.shared.parked.park()
    }
    pub(crate) fn recovery_idle(&self) -> bool {
        let state = self.shared.state();
        !state.shutting_down && state.active.is_none()
    }

    pub(crate) fn carrier_idle(&self) -> bool {
        let state = self.shared.state();
        !state.shutting_down && state.active.is_none() && state.row_bound
    }

    pub fn active_turn_id_for_thread(&self, thread: &str) -> Option<String> {
        self.shared
            .state()
            .active
            .as_ref()
            .filter(|active| active.thread_id == thread)
            .map(|active| active.turn_id.clone())
    }

    /// Fixture access to the production neutral invocation entry.
    #[cfg(feature = "fixtures")]
    pub async fn turn_start(
        &self,
        thread: &str,
        request: provider::control::TurnRequest,
    ) -> Result<
        provider::control::TurnStartOutcome<CalmError, crate::harness::backend::StartDiagnostic>,
        crate::harness::backend::TurnStartFailure,
    > {
        let fence =
            crate::harness::run_loop::issuance::track_fence_for_test(&self.shared.params.track_id)
                .await;
        let handoff = crate::provider_execution::TrackFenceHandoff::new(fence);
        self.start_turn(thread, request, &handoff).await
    }

    #[cfg(any(test, feature = "fixtures"))]
    pub(crate) fn track_for_test(&self) -> &str {
        &self.shared.params.track_id
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
            Some(slot) => match interrupt(&slot, historical_admission().gate()).await {
                Ok(()) => provider::control::InterruptOutcome::LocalSignalDelivered,
                Err((error, evidence)) => {
                    provider::control::InterruptOutcome::Unconfirmed { error, evidence }
                }
            },
            None => provider::control::InterruptOutcome::NoTarget,
        }
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
async fn interrupt(
    slot: &TurnSlot,
    gate: &provider::send_boundary::PreSendGate<'_>,
) -> std::result::Result<(), (CalmError, provider::control::SubmissionEvidence)> {
    arm_interrupt(slot);
    interrupt_after_admission(slot, gate).await
}

fn arm_interrupt(slot: &TurnSlot) {
    slot.record(TerminalCause::Interrupted);
    slot.set_stop_reason(StopReason::Interrupted);
    slot.arm_stop(tokio::time::Instant::now() + STOP_TIMER);
}

/// Canonical controls have no lifecycle effect until the actual writer crosses
/// admission. Arming in its first poll also bounds a pending or failed write.
async fn interrupt_after_admission(
    slot: &TurnSlot,
    gate: &provider::send_boundary::PreSendGate<'_>,
) -> std::result::Result<(), (CalmError, provider::control::SubmissionEvidence)> {
    let request = provider::claude::protocol::ControlRequestOut::new(
        format!("interrupt-{}", Uuid::new_v4().simple()),
        provider::claude::protocol::ControlRequestKind::Interrupt,
    );
    use provider::control::SubmissionEvidence;
    let written = match serde_json::to_string(&request) {
        Ok(line) => provider::claude::line_writer::write_line_on_first_poll(
            &slot.stdin,
            &line,
            gate,
            || arm_interrupt(slot),
        )
        .await
        .map_err(|error| {
            let evidence = match &error {
                provider::claude::line_writer::LineWriteError::BeforeWrite(_) => {
                    SubmissionEvidence::NotWritten
                }
                provider::claude::line_writer::LineWriteError::CrossedWrite(_) => {
                    slot.stdin_failed(&error);
                    SubmissionEvidence::WriteMayHaveReachedPeer
                }
            };
            (super::turn_slot::write_error(error), evidence)
        }),
        Err(error) => Err((error.into(), SubmissionEvidence::NotWritten)),
    };
    if let Err((error, _evidence)) = &written {
        tracing::warn!(%error, stop_armed = slot.stop_armed(), "claude planner: interrupt request not written");
    }
    written
}

fn sealed(thread: &str) -> CalmError {
    CalmError::Conflict(format!("thread {thread} is sealed for deletion"))
}

#[cfg(test)]
mod interrupt_tests;

#[cfg(test)]
impl ClaudePlannerSession {
    pub(crate) async fn handoff_for_test(&self) -> crate::provider_execution::TrackFenceHandoff {
        let track = self.track_for_test();
        crate::provider_execution::TrackFenceHandoff::new(
            crate::harness::run_loop::issuance::track_fence_for_test(track).await,
        )
    }
}
