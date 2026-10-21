use super::start::{self, StartFailure};
mod admission;
mod cancellation;
mod driver;
mod input;
use cancellation::IssueGuard;
use driver::drive;
use input::InputContract;
mod execution_end;
use super::{
    config::AcpPlannerHost,
    process::{Process, setup_request},
};
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
    /// Reserved for C4.1+; provider launches remain unchanged in C4.0b.
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
}
struct State {
    active: Option<Active>,
    closed: bool,
}
struct Shared {
    params: SessionParams,
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
                ReceiptPresence::Absent => CanonicalStart::BeforeReceipt(error.into()),
                ReceiptPresence::Unchecked => CanonicalStart::UncertainReceipt(error.into()),
            },
        }
    }

    async fn issue(
        &self,
        thread: &str,
        request: provider::control::TurnRequest,
        input: InputContract<'_, '_>,
        receipt_turn: &mut ReceiptPresence,
    ) -> std::result::Result<ReceiptAttempt, StartFailure> {
        let provider::control::TurnRequest {
            input: provider::control::InputRequest { items, correlation },
            selection,
            permission,
            edit: _, // The owner-bound managed entry already checked the stored rewind.
        } = request;
        let client = &correlation.0;
        let shared = &self.shared;
        let params = &shared.params;
        let _issue = shared.issue.lock().await;
        if shared.parked.is_parked() {
            return Err(refused("ACP carrier is parked"));
        }
        let pool = params
            .repo
            .sqlite_db()
            .ok_or_else(|| refused("ACP requires durable submission storage"))?;
        let existing = acp_submission_get(pool.read_pool(), &params.worker_session_id, client)
            .await
            .map_err(CalmError::from)?;
        *receipt_turn = match &existing {
            Some(receipt) => ReceiptPresence::Present(ReceiptAttempt {
                turn: receipt.turn_id.clone(),
                native_session_id: receipt.native_session_id.clone(),
            }),
            None => ReceiptPresence::Absent,
        };
        let Some(held) = shared.installed.get().cloned() else {
            return Err(refused("ACP harness has not been installed"));
        };
        if shared.state.lock().expect("ACP state").closed || params.seals.is_sealed(thread) {
            return Err(refused("ACP conversation is closed or sealed"));
        }
        let row = shared.current_owner(thread, Some(permission)).await?;
        match &input {
            InputContract::Historical(_) => {
                crate::conversation_inputs::require_historical_storage(
                    params.repo.as_ref(),
                    &params.card_id,
                )
                .await?
            }
            InputContract::Canonical { .. } => {}
        }
        if let InputContract::Canonical { .. } = &input
            && (row.contract != WorkerContract::Planner
                || !super::profile::AcpAgentProfile::of(&params.provider)
                    .is_some_and(|profile| profile.canonical_planner_inputs))
        {
            return Err(refused("ACP canonical input requires an eligible Planner"));
        }
        if let Some(receipt) = existing {
            let issued = crate::db::sqlite::AcpSubmissionInput::decode(&receipt.input_json)
                .map_err(CalmError::from)?;
            if receipt.thread_id != thread
                || row.agent_session_id.as_deref() != Some(&receipt.native_session_id)
                || !input.matches(&issued)
            {
                return Err(refused("ACP receipt ownership or original input changed"));
            }
            // Exact-key recovery retires the original queue batch without any native write.
            let turn = receipt.turn_id.clone();
            match &input {
                InputContract::Historical(_) => {
                    let outcome = receipt
                        .outcome_json
                        .as_deref()
                        .map(serde_json::from_str)
                        .transpose()
                        .map_err(CalmError::from)?
                        .unwrap_or_else(|| unknown_outcome(&turn));
                    let _ = shared.events.send(PlannerEvent {
                        thread_id: Some(thread.into()),
                        kind: PlannerEventKind::TurnStarted {
                            turn_id: turn.clone(),
                        },
                    });
                    let _ = shared.events.send(PlannerEvent {
                        thread_id: Some(thread.into()),
                        kind: PlannerEventKind::TurnCompleted { turn: outcome },
                    });
                }
                InputContract::Canonical { .. } => {}
            }
            return Ok(ReceiptAttempt {
                turn,
                native_session_id: receipt.native_session_id,
            });
        }
        input.check_source().await?;
        if shared.state.lock().expect("ACP state").active.is_some()
            || acp_submission_unresolved(pool.read_pool(), &params.worker_session_id)
                .await
                .map_err(CalmError::from)?
        {
            return Err(refused(
                "Previous ACP outcome is unknown. Start a fresh session; nothing will be resent.",
            ));
        }
        join_driver(shared).await?;
        let mut blocks = Vec::new();
        let has_receipts: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM acp_submissions WHERE worker_session_id=?1)",
        )
        .bind(&params.worker_session_id)
        .fetch_one(pool.read_pool())
        .await
        .map_err(CalmError::from)?;
        if !has_receipts {
            blocks.push(protocol::ContentBlock::text(format!(
                "Neige conversation instructions:\n{}",
                params.instructions
            )));
        }
        for item in &items {
            match item {
                InputItem::Text { text } => blocks.push(protocol::ContentBlock::text(text.clone())),
                InputItem::LocalImage { .. } => {
                    return Err(refused(
                        "ACP image input is not enabled for this managed Planner",
                    ));
                }
            }
        }
        let registration = params
            .host
            .registration(&params.provider)
            .map_err(|error| refused(&error.to_string()))?;
        let digest = registration.config.native_session_digest(&params.cwd)?;
        // Neige's server under this launch's own key and, for an agent that asks before its
        // tools, the tools it lists the session now.
        let server = registration.profile.mcp_server_key();
        let tools = match registration.profile.mcp_tool_request {
            Some(_) => params.host.listed_tools(&params.worker_session_id).await?,
            None => Vec::new(),
        };
        let asks = registration.profile.asks(&server, tools);
        crate::db::sqlite::acp_registration_claim(
            &pool,
            &params.worker_session_id,
            &digest,
            row.agent_session_id.is_some(),
        )
        .await
        .map_err(CalmError::from)?;
        let mut guard = IssueGuard::new(shared);
        #[cfg(all(test, feature = "fixtures"))]
        super::test_seams::wait_issue(
            &params.worker_session_id,
            super::test_seams::IssueStage::BeforeMint,
        )
        .await;
        let token = match mint_token(params).await {
            Ok(token) => token,
            Err(error) => {
                drop(guard);
                join_driver(shared).await?;
                return Err(error.into());
            }
        };
        let spawned = Process::spawn_control(
            &params.host,
            registration,
            &params.worker_session_id,
            &params.cwd,
            super::process::LaunchContext::Planner {
                mcp_token: &token,
                permission,
            },
        )
        .await;
        guard.process = Some(match spawned {
            Ok(process) => process,
            Err(error) => {
                guard.finish().await?;
                return Err(error);
            }
        });
        #[cfg(all(test, feature = "fixtures"))]
        super::test_seams::wait_issue(
            &params.worker_session_id,
            super::test_seams::IssueStage::AfterSpawn,
        )
        .await;
        let process = guard.process.as_mut().expect("spawned process");
        let original_native = row.agent_session_id.clone();
        let owner_admission =
            shared.admission(thread, Some(permission), original_native.as_deref(), None);
        let admission = input.admission(&owner_admission);
        let prepared = async {
            let mcp = [protocol::McpServer::Stdio {
                name: server.clone(),
                command: params.host.mcp_shim.to_string_lossy().into_owned(),
                // Native MCP consumers may build model output from content alone.
                args: vec!["--structured-content-as-text".into()],
                env: crate::mcp_server::wiring::card_mcp_env(&params.host.mcp_socket, &token)
                    .into_iter()
                    .map(|(name, value)| protocol::EnvVariable {
                        name: name.into(),
                        value,
                    })
                    .collect(),
            }];
            let (native, mut configuration) = match row.agent_session_id {
                Some(native) => {
                    if !process.capabilities.load_session {
                        return Err(refused("ACP agent cannot load its persisted conversation"));
                    }
                    let result = setup_request(
                        process,
                        "session/load",
                        protocol::encode_load_session(&native, &params.cwd, &mcp),
                        admission.gate(),
                    )
                    .await?;
                    (
                        native,
                        provider::acp::configuration::Configuration::from_response(&result)
                            .map_err(|error| StartFailure::Failed(wire_error(error)))?,
                    )
                }
                None => {
                    let result = setup_request(
                        process,
                        "session/new",
                        protocol::encode_new_session(&params.cwd, &mcp),
                        admission.gate(),
                    )
                    .await?;
                    let configuration =
                        provider::acp::configuration::Configuration::from_response(&result)
                            .map_err(|error| StartFailure::Failed(wire_error(error)))?;
                    let native: protocol::NewSessionResponse = protocol::decode(result)
                        .map_err(|error| StartFailure::Failed(wire_error(error)))?;
                    if native.session_id.is_empty() {
                        return Err(refused("ACP agent returned an empty native identity"));
                    }
                    bind_native(params, thread, &native.session_id).await?;
                    (native.session_id, configuration)
                }
            };
            params
                .host
                .record_configuration(&params.card_id, configuration.clone());
            for (category, value) in [
                ("model", selection.model.as_deref()),
                ("thought_level", selection.effort.as_deref()),
            ] {
                let Some(value) = value else {
                    continue;
                };
                let option = configuration
                    .category(category)
                    .filter(|option| option.accepts(value))
                    .ok_or_else(|| {
                        refused("The ACP agent did not declare that model or effort choice")
                    })?;
                let owner_admission =
                    shared.admission(thread, Some(permission), Some(&native), None);
                let admission = input.admission(&owner_admission);
                let result = setup_request(
                    process,
                    "session/set_config_option",
                    protocol::encode_set_config_option(&native, &option.id, value),
                    admission.gate(),
                )
                .await?;
                configuration = provider::acp::configuration::Configuration::from_response(&result)
                    .map_err(|error| StartFailure::Failed(wire_error(error)))?;
                if configuration
                    .category(category)
                    .is_none_or(|option| option.current_value != value)
                {
                    return Err(refused("ACP did not confirm the selected model or effort"));
                }
                params
                    .host
                    .record_configuration(&params.card_id, configuration.clone());
            }
            if params.seals.is_sealed(thread) || shared.state.lock().expect("ACP state").closed {
                return Err(refused("ACP conversation was sealed before dispatch"));
            }
            let turn = uuid::Uuid::new_v4().to_string();
            let receipt = AcpSubmission {
                worker_session_id: params.worker_session_id.clone(),
                client_id: client.into(),
                turn_id: turn.clone(),
                thread_id: thread.into(),
                native_session_id: native.clone(),
                input_json: input.receipt(&items, &blocks)?,
                state: crate::db::sqlite::AcpSubmissionState::Sending,
                outcome_json: None,
            };
            guard.receipt = Some(receipt.clone());
            #[cfg(all(test, feature = "fixtures"))]
            super::test_seams::wait_issue(
                &params.worker_session_id,
                super::test_seams::IssueStage::BeforeReceipt,
            )
            .await;
            match acp_submission_prepare(&pool, &receipt, crate::model::now_ms()).await {
                Ok(()) => {
                    *receipt_turn = ReceiptPresence::Present(ReceiptAttempt {
                        turn: turn.clone(),
                        native_session_id: native.clone(),
                    })
                }
                Err(crate::db::sqlite::AcpPrepareError::BeforeReceipt(error)) => {
                    return Err(CalmError::from(error).into());
                }
                Err(crate::db::sqlite::AcpPrepareError::CommitUncertain(error)) => {
                    *receipt_turn = ReceiptPresence::Present(ReceiptAttempt {
                        turn: turn.clone(),
                        native_session_id: native.clone(),
                    });
                    return Err(CalmError::from(error).into());
                }
            }
            Ok::<_, StartFailure>((native, turn))
        }
        .await;
        let (native, turn) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                guard.finish().await?;
                return Err(error);
            }
        };
        #[cfg(all(test, feature = "fixtures"))]
        super::test_seams::wait_issue(
            &params.worker_session_id,
            super::test_seams::IssueStage::AfterReceipt,
        )
        .await;
        #[cfg(all(test, feature = "fixtures"))]
        if let Some(connection) = super::test_transport::connection(&params.worker_session_id) {
            guard.process.as_mut().expect("prepared process").connection = connection;
        }
        let (process, receipt) = guard.handoff();
        #[cfg(feature = "fixtures")]
        let held = super::test_seams::tap_held(&params.worker_session_id, held);
        let approvals = Arc::new(Approvals::for_turn(
            permission,
            &held,
            &turn,
            &process.connection.client,
            asks,
        ));
        let (cancel, cancelled) = watch::channel(CancelRequest::Running);
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point(
            "carrier-quiescence-acp-issuance",
            &params.worker_session_id,
        )
        .await;
        shared.state.lock().expect("ACP state").active = Some(Active {
            thread: thread.into(),
            turn: turn.clone(),
            cancel,
            approvals: Arc::downgrade(&approvals),
            native_session_id: native.clone(),
        });
        let translator = TurnTranslator::new(TurnContext {
            thread_id: thread.into(),
            turn_id: turn.clone(),
            native_session_id: native.clone(),
        });
        let _ = shared.events.send(translator.started());
        let client = process.connection.client.clone();
        let capabilities = process.capabilities.clone();
        let (send_pending, pending) = tokio::sync::oneshot::channel();
        let retained_receipt = receipt.clone();
        let association = input.association();
        let task_shared = Arc::clone(shared);
        let driver = tokio::spawn(async move {
            drive(
                task_shared,
                process,
                receipt,
                translator,
                pending,
                cancelled,
                approvals,
            )
            .await
        });
        execution_end::install(
            shared,
            DriverSlot::new(driver, retained_receipt, association),
        );
        #[cfg(all(test, feature = "fixtures"))]
        super::test_seams::wait_issue(
            &params.worker_session_id,
            super::test_seams::IssueStage::DriverInstalled,
        )
        .await;
        let pending = {
            let owner_admission =
                shared.admission(thread, Some(permission), Some(&native), Some(&turn));
            let admission = input.admission(&owner_admission);
            protocol::prompt(&client, &capabilities, &native, &blocks, admission.gate()).await
        };
        let _ = send_pending.send(pending);
        // A durable dispatch attempt always hands ownership to the Harness, even if its pipe failed.
        Ok(ReceiptAttempt {
            turn,
            native_session_id: native,
        })
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
        let stopped =
            crate::planner_process::stop(&params.host.instance, &params.worker_session_id).await;
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
    BeforeReceipt(CalmError),
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
