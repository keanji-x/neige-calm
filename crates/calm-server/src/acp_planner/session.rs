use super::start::{self, StartFailure};
mod admission;
mod cancellation;
mod compact;
mod driver;
mod input;
mod issuance;
mod recovery;
mod setup;
use cancellation::IssueGuard;
use driver::drive;
use input::InputContract;
mod execution_end;
use super::{config::AcpPlannerHost, execution::Process, process::setup_request};
use crate::db::sqlite::{
    AcpSubmission, acp_submission_finish, acp_submission_get, acp_submission_prepare,
    acp_submission_unresolved,
};
use crate::db::{Repo, write_in_tx_typed};
use crate::error::{CalmError, Result};
use crate::harness::backend::TurnStartFailure;
use crate::harness::held_requests::HeldRequestSender;
use crate::planner_permission_mode::PlannerPermissionMode;
use crate::thread_seals::ThreadSeals;
use calm_types::runtime::AgentProvider;
use calm_types::worker::{WorkerContract, WorkerSessionId};
pub(crate) use execution_end::PreparedInterrupt;
use execution_end::{DriverSlot, join_driver};
use provider::InputItem;
use provider::acp::{
    Incoming,
    approvals::Approvals,
    protocol,
    translate::{TurnContext, TurnTranslator},
};
use provider::events::{PlannerEvent, PlannerEventKind};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::{broadcast, watch};

pub struct SessionParams {
    /// The host Engine carries each ACP turn.
    pub engine: Option<crate::execution_boot::ExecutionEngine>,
    pub host: Arc<AcpPlannerHost>,
    pub provider: AgentProvider,
    pub repo: Arc<dyn Repo>,
    pub seals: Arc<ThreadSeals>,
    pub worker_session_id: String,
    pub card_id: String,
    pub track_id: String,
    pub cwd: std::path::PathBuf,
    pub instructions: String,
}
#[derive(Clone, Copy)]
enum CancelRequest {
    Running,
    Requested,
    Written,
}
struct Active {
    thread: String,
    turn: String,
    cancel: watch::Sender<CancelRequest>,
    approvals: std::sync::Weak<Approvals>,
    native_session_id: String,
    execution_id: workspace_runtime::execution::ExecutionId,
}
struct State {
    active: Option<Active>,
    closed: bool,
}
pub(super) struct Shared {
    pub(super) params: SessionParams,
    events: broadcast::Sender<PlannerEvent>,
    state: Mutex<State>,
    issue: tokio::sync::Mutex<()>,
    parked: crate::harness::carrier_park::ParkState,
    driver: Mutex<Option<Arc<tokio::sync::Mutex<DriverSlot>>>>,
    /// Set when the registry installed the harness: the channel the turns' approvals go to.
    installed: OnceLock<HeldRequestSender>,
}
pub struct AcpPlannerSession {
    shared: Arc<Shared>,
}
impl AcpPlannerSession {
    pub fn open(params: SessionParams) -> Self {
        params.host.completion.bind(params.repo.clone());
        let (events, _) = broadcast::channel(1024);
        Self {
            shared: Arc::new(Shared {
                params,
                events,
                state: Mutex::new(State {
                    active: None,
                    closed: false,
                }),
                issue: tokio::sync::Mutex::new(()),
                parked: Default::default(),
                driver: Mutex::new(None),
                installed: OnceLock::new(),
            }),
        }
    }
    pub fn provider(&self) -> AgentProvider {
        self.shared.params.provider.clone()
    }
    pub fn host(&self) -> &AcpPlannerHost {
        &self.shared.params.host
    }
    pub fn subscribe_events(&self) -> broadcast::Receiver<PlannerEvent> {
        self.shared.events.subscribe()
    }
    /// The registry installed the harness holding this session; turns may start from now on, and
    /// their permission requests go to `held` (#2348). A later call is ignored.
    pub fn mark_installed(&self, held: HeldRequestSender) {
        let _ = self.shared.installed.set(held);
    }
    pub fn thread_sealed(&self, thread: &str) -> bool {
        self.shared.params.seals.is_sealed(thread)
    }
    pub(crate) fn carrier_idle_for_detach(
        &self,
    ) -> Option<crate::harness::carrier_park::ParkToken<'_>> {
        let _issue = self.shared.issue.try_lock().ok()?;
        if !self.carrier_idle() {
            return None;
        }
        self.shared.parked.park()
    }
    pub(crate) fn carrier_idle(&self) -> bool {
        {
            let state = self.shared.state.lock().expect("ACP state");
            if state.closed || state.active.is_some() {
                return false;
            }
        }
        execution_end::driver_finished(&self.shared)
    }

    pub fn active_turn_id_for_thread(&self, thread: &str) -> Option<String> {
        self.shared
            .state
            .lock()
            .expect("ACP state")
            .active
            .as_ref()
            .filter(|active| active.thread == thread)
            .map(|active| active.turn.clone())
    }
    pub(crate) async fn start_turn(
        &self,
        thread: &str,
        request: provider::control::TurnRequest,
        claim: &[crate::harness::QueueEntry],
        track_fence: &crate::provider_execution::TrackFenceHandoff,
    ) -> std::result::Result<
        provider::control::TurnStartOutcome<CalmError, crate::harness::backend::StartDiagnostic>,
        TurnStartFailure,
    > {
        // Freeze before waiting on the ACP issue lock, as on the original managed path.
        let claim = crate::harness::submission_claims::freeze(claim);
        start::start_result(
            self.issue(
                thread,
                request,
                InputContract::Historical(claim),
                &mut ReceiptPresence::Unchecked,
                track_fence,
            )
            .await
            .map(|receipt| receipt.turn),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn start_canonical(
        &self,
        thread: &str,
        items: Vec<InputItem>,
        client: &str,
        selection: &provider::TurnModelSelection,
        permission: PlannerPermissionMode,
        execution: crate::harness::backend::ExecutionInput,
        admission: &crate::harness::native_admission::NativePromptAdmission<'_>,
        track_fence: &crate::provider_execution::TrackFenceHandoff,
    ) -> CanonicalStart {
        let mut receipt_turn = ReceiptPresence::Unchecked;
        let result = self
            .issue(
                thread,
                provider::control::TurnRequest {
                    input: provider::control::InputRequest {
                        items,
                        correlation: provider::control::CorrelationKey(client.into()),
                    },
                    selection: selection.clone(),
                    permission,
                    edit: provider::control::TurnEdit::NoEdit,
                },
                InputContract::Canonical {
                    execution,
                    admission,
                },
                &mut receipt_turn,
                track_fence,
            )
            .await;
        match result {
            Ok(receipt) => CanonicalStart::Receipted {
                receipt,
                outcome: Ok(()),
            },
            Err(error) => match receipt_turn {
                ReceiptPresence::Present(receipt) => CanonicalStart::Receipted {
                    receipt,
                    outcome: Err(error.into()),
                },
                ReceiptPresence::Absent => CanonicalStart::BeforeReceipt(error.into_host()),
                ReceiptPresence::Unchecked => CanonicalStart::UncertainReceipt(error.into()),
            },
        }
    }

    pub async fn turn_interrupt(
        &self,
        thread: &str,
        turn: &str,
    ) -> provider::control::InterruptOutcome<tokio::sync::watch::error::SendError<bool>> {
        if let Some(active) = self
            .shared
            .state
            .lock()
            .expect("ACP state")
            .active
            .as_ref()
            .filter(|a| a.thread == thread && a.turn == turn)
        {
            return cancel_outcome(
                active
                    .cancel
                    .send(CancelRequest::Requested)
                    .map_err(|_| watch::error::SendError(true)),
            );
        }
        provider::control::InterruptOutcome::NoTarget
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.shared.state.lock().expect("ACP state").closed = true;
        let _issue = self.shared.issue.lock().await;
        if self.shared.installed.get().is_none() {
            return Ok(());
        }
        {
            let state = self.shared.state.lock().expect("ACP state");
            if let Some(active) = state.active.as_ref() {
                let _ = active.cancel.send(CancelRequest::Requested);
            }
        }
        let params = &self.shared.params;
        let revoked = revoke(params).await;
        let stopped = self
            .stop_execution(crate::provider_execution::StopReason::Shutdown)
            .await;
        // Cleanup errors never detach the writer: recovery waits for all its writes.
        join_driver(&self.shared).await?;
        revoked?;
        stopped?;
        Ok(())
    }
}
pub(crate) struct ReceiptAttempt {
    pub(crate) turn: String,
    pub(crate) native_session_id: String,
}
enum ReceiptPresence {
    Unchecked,
    Absent,
    Present(ReceiptAttempt),
}
pub(crate) enum CanonicalStart {
    UncertainReceipt(CalmError),
    BeforeReceipt(TurnStartFailure),
    Receipted {
        receipt: ReceiptAttempt,
        outcome: Result<()>,
    },
}

fn refused(reason: &str) -> StartFailure {
    StartFailure::Host(TurnStartFailure::Refused {
        error: CalmError::Conflict(reason.into()),
        reader: reason.into(),
    })
}
fn wire_error(error: provider::acp::Error) -> CalmError {
    CalmError::Conflict(error.to_string())
}
pub(super) fn unknown_outcome(turn: &str) -> Value {
    json!({"id":turn,"status":"failed","error":{"message":"ACP execution outcome is unknown. The submission will not be resent; start a fresh session (history is kept)."}})
}

async fn bind_native(params: &SessionParams, thread: &str, native: &str) -> Result<()> {
    let worker = params.worker_session_id.clone();
    let thread = thread.to_owned();
    let native = native.to_owned();
    let provider = params.provider.clone();
    write_in_tx_typed(params.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let row = calm_truth::decision_gate::WriteTx::read_worker_session(
                tx,
                &WorkerSessionId(worker.clone()),
            )
            .await?
            .ok_or_else(|| CalmError::NotFound("ACP owner".into()))?;
            if !row.state.is_active_authority() || row.thread_id.as_deref() != Some(&thread) {
                return Err(CalmError::Conflict("ACP native owner changed".into()));
            }
            crate::db::sqlite::session_bind_attribution_tx(
                tx,
                &worker,
                crate::session_projection_repo::ThreadAttribution {
                    worker_session_id: worker.clone(),
                    provider,
                    thread_id: Some(thread),
                    session_id: Some(native),
                    active_turn_id: row.active_turn_id,
                },
            )
            .await?;
            Ok(())
        })
    })
    .await
}
async fn mint_token(params: &SessionParams) -> Result<String> {
    let card = params.card_id.clone();
    let worker = params.worker_session_id.clone();
    write_in_tx_typed(params.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let row = calm_truth::decision_gate::WriteTx::read_worker_session(
                tx,
                &WorkerSessionId(worker.clone()),
            )
            .await?
            .ok_or_else(|| CalmError::Conflict("ACP credential carrier changed".into()))?;
            if !row.state.is_active_authority()
                || row.card_id.as_ref().map(|id| id.as_str()) != Some(card.as_str())
            {
                return Err(CalmError::Conflict("ACP credential carrier changed".into()));
            }
            crate::mcp_server::wiring::mint_and_persist_managed_planner_token(tx, &card, &worker)
                .await
        })
    })
    .await
}
async fn revoke(params: &SessionParams) -> Result<()> {
    let worker = params.worker_session_id.clone();
    write_in_tx_typed(params.repo.as_ref(), move |tx| {
        Box::pin(async move {
            sqlx::query("UPDATE worker_sessions SET mcp_token_hash=NULL WHERE id=?1")
                .bind(worker)
                .execute(&mut **tx)
                .await?;
            Ok(())
        })
    })
    .await
}
fn cancel_outcome(
    result: std::result::Result<(), tokio::sync::watch::error::SendError<bool>>,
) -> provider::control::InterruptOutcome<tokio::sync::watch::error::SendError<bool>> {
    match result {
        Ok(()) => provider::control::InterruptOutcome::LocalSignalDelivered,
        Err(error) => provider::control::InterruptOutcome::Unconfirmed {
            error,
            evidence: provider::control::SubmissionEvidence::NotWritten,
        },
    }
}

#[cfg(test)]
mod control_tests;

#[cfg(test)]
mod admission_tests;

impl Shared {
    pub(super) fn finish_execution(
        &self,
        id: &str,
        turn_id: &str,
        thread: &str,
        events: Vec<PlannerEvent>,
        turn: Value,
    ) {
        let mut state = self.state.lock().expect("ACP state");
        if !state.active.as_ref().is_some_and(|active| {
            active.execution_id.as_str() == id && active.turn == turn_id && active.thread == thread
        }) {
            return;
        }
        state.active = None;
        if events.is_empty() {
            let _ = self.events.send(PlannerEvent {
                thread_id: Some(thread.into()),
                kind: PlannerEventKind::TurnCompleted { turn },
            });
        } else {
            for event in events {
                let _ = self.events.send(event);
            }
        }
    }
}
impl AcpPlannerSession {
    pub(crate) async fn provider_transition_gate(&self) -> Result<()> {
        let _issue = self.shared.issue.lock().await;
        let params = &self.shared.params;
        let pool = params
            .repo
            .sqlite_db()
            .ok_or(CalmError::ExecutionUnavailable)?;
        params
            .host
            .provider_execution
            .admission_gate(&pool, params.engine.as_ref(), &params.worker_session_id)
            .await
    }
    async fn stop_execution(&self, reason: crate::provider_execution::StopReason) -> Result<()> {
        use crate::provider_execution as execution;
        let params = &self.shared.params;
        let pool = params
            .repo
            .sqlite_db()
            .ok_or(CalmError::ExecutionUnavailable)?;
        if let Some(claim) = calm_truth::db::sqlite::provider_execution::load(
            pool.read_pool(),
            &params.worker_session_id,
        )
        .await?
        {
            let engine = execution::engine(params.engine.as_ref())?;
            params
                .host
                .provider_execution
                .stop(
                    &pool,
                    Some(engine),
                    &params.worker_session_id,
                    &claim,
                    reason,
                )
                .await?;
            execution::wait_settled(
                engine,
                &execution::execution_id(claim.record())?,
                Duration::from_secs(12),
            )
            .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod compaction_owner_tests;

#[cfg(test)]
impl AcpPlannerSession {
    pub(crate) async fn handoff_for_test(&self) -> crate::provider_execution::TrackFenceHandoff {
        let track = &self.shared.params.track_id;
        crate::provider_execution::TrackFenceHandoff::new(
            crate::harness::run_loop::issuance::track_fence_for_test(track).await,
        )
    }
}

#[cfg(test)]
impl AcpPlannerSession {
    pub(crate) fn track_for_test(&self) -> &str {
        &self.shared.params.track_id
    }
}
