//! Native prompt evidence, before the conversation owner attaches its durable attempt.
use super::*;
use futures_util::future::BoxFuture;
use provider::codex::{RequestCrossing, RpcAttemptFailure};

/// Retain the caller's error in its owning layer; the provider sees only refusal.
pub(crate) struct NativePromptAdmission<'a> {
    gate: provider::codex::PreSendGate<'a>,
    #[cfg(feature = "fixtures")]
    fixture: Arc<dyn Fn() -> BoxFuture<'a, Result<()>> + Send + Sync + 'a>,
    refusal: Arc<std::sync::Mutex<Option<CalmError>>>,
}

impl<'a> NativePromptAdmission<'a> {
    pub(crate) fn new(
        live_issuer: impl Fn() -> Result<()> + Send + Sync + 'a,
        revalidate: impl Fn() -> BoxFuture<'a, Result<()>> + Send + Sync + 'a,
    ) -> Self {
        // The async owner must finish all reads and its commit before the live
        // issuer is checked. No caller can accidentally reverse this ordering.
        let live_issuer = Arc::new(live_issuer);
        let admission = Arc::new(move || {
            let future = revalidate();
            let live_issuer = live_issuer.clone();
            Box::pin(async move {
                future.await?;
                live_issuer()
            }) as BoxFuture<'a, Result<()>>
        });
        #[cfg(feature = "fixtures")]
        let fixture = admission.clone();
        let refusal = Arc::new(std::sync::Mutex::new(None));
        let captured = refusal.clone();
        let gate = provider::codex::PreSendGate::new(move || {
            let future = admission();
            let captured = captured.clone();
            Box::pin(async move {
                future.await.map_err(|error| {
                    *captured.lock().unwrap() = Some(error);
                    provider::codex::error::Error::Refused(
                        "caller admission refused before send".into(),
                    )
                })
            })
        });
        Self {
            gate,
            refusal,
            #[cfg(feature = "fixtures")]
            fixture,
        }
    }

    // The fixture branch performs only local simulated effects. Native code
    // has no callable admission API; its only consumer is a provider gate.
    #[cfg(feature = "fixtures")]
    pub(super) async fn check_fixture(&self) -> Result<()> {
        (self.fixture)().await
    }

    pub(super) fn gate(&self) -> &provider::codex::PreSendGate<'a> {
        &self.gate
    }

    pub(super) fn take_refusal(&self) -> Option<CalmError> {
        self.refusal.lock().unwrap().take()
    }

    pub(super) fn finish<T>(&self, result: provider::codex::error::Result<T>) -> Result<T> {
        result.map_err(|error| self.take_refusal().unwrap_or_else(|| error.into()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CodexPromptDelivery {
    NotDispatched,
    Unknown,
    Accepted { turn_id: String },
}

/// Constructed only at this native boundary; diagnostics are deliberately not serializable.
pub(crate) struct CodexPromptReport {
    thread_id: String,
    client_id: Option<String>,
    delivery: CodexPromptDelivery,
    outcome: std::result::Result<TurnId, CodexPromptFailure>,
    response_turn: Option<String>,
}

pub(crate) enum CodexPromptFailure {
    Local(CalmError),
    Rpc(RpcAttemptFailure),
    Admission(CalmError, RpcAttemptFailure),
    Start(control::StartError),
    Control(ControlError),
}

impl CodexPromptFailure {
    pub(crate) fn into_error(self) -> CalmError {
        match self {
            Self::Local(error) | Self::Admission(error, _) => error,
            Self::Rpc(error) => error.into_legacy_error().into(),
            Self::Start(error) => error.into(),
            Self::Control(error) => error.into(),
        }
    }
}

impl CodexPromptReport {
    fn failure(thread: &str, client: Option<&str>, error: CalmError, sent: bool) -> Self {
        Self::source_failure(thread, client, CodexPromptFailure::Local(error), sent)
    }

    fn source_failure(
        thread: &str,
        client: Option<&str>,
        failure: CodexPromptFailure,
        sent: bool,
    ) -> Self {
        Self {
            response_turn: None,
            thread_id: thread.into(),
            client_id: client.map(str::to_owned),
            delivery: if sent {
                CodexPromptDelivery::Unknown
            } else {
                CodexPromptDelivery::NotDispatched
            },
            outcome: Err(failure),
        }
    }

    fn rpc_failure(
        thread: &str,
        client: Option<&str>,
        error: RpcAttemptFailure,
        refusal: Option<CalmError>,
    ) -> Self {
        Self {
            response_turn: None,
            thread_id: thread.into(),
            client_id: client.map(str::to_owned),
            // A correlated after-send error does not guarantee atomic non-acceptance.
            delivery: match error.crossing() {
                RequestCrossing::BeforeSend => CodexPromptDelivery::NotDispatched,
                RequestCrossing::AfterSend => CodexPromptDelivery::Unknown,
            },
            outcome: Err(match refusal {
                Some(refusal) => CodexPromptFailure::Admission(refusal, error),
                None => CodexPromptFailure::Rpc(error),
            }),
        }
    }

    fn accepted(
        thread: &str,
        client: Option<&str>,
        turn: String,
        outcome: std::result::Result<TurnId, CodexPromptFailure>,
    ) -> Self {
        Self {
            response_turn: None,
            thread_id: thread.into(),
            client_id: client.map(str::to_owned),
            delivery: CodexPromptDelivery::Accepted { turn_id: turn },
            outcome,
        }
    }

    pub(crate) fn thread_id(&self) -> &str {
        &self.thread_id
    }
    pub(crate) fn client_id(&self) -> Option<&str> {
        self.client_id.as_deref()
    }
    pub(crate) fn delivery(&self) -> &CodexPromptDelivery {
        &self.delivery
    }
    pub(crate) fn rpc_failure_ref(&self) -> Option<&RpcAttemptFailure> {
        match &self.outcome {
            Err(CodexPromptFailure::Rpc(error) | CodexPromptFailure::Admission(_, error)) => {
                Some(error)
            }
            Ok(_)
            | Err(
                CodexPromptFailure::Local(_)
                | CodexPromptFailure::Start(_)
                | CodexPromptFailure::Control(_),
            ) => None,
        }
    }
    pub(crate) fn into_start_outcome(self) -> std::result::Result<TurnId, control::StartError> {
        self.outcome.map_err(|failure| match failure {
            CodexPromptFailure::Local(error) | CodexPromptFailure::Admission(error, _) => {
                control::StartError::Host(error)
            }
            CodexPromptFailure::Start(error) => error,
            CodexPromptFailure::Control(error) => error.into(),
            CodexPromptFailure::Rpc(error) => control::StartError::from(error),
        })
    }
    pub(crate) fn into_start_delivery(self) -> StartDelivery<control::StartError> {
        let response_turn = self.response_turn.clone();
        match self.delivery.clone() {
            CodexPromptDelivery::Accepted { turn_id } => StartDelivery::NativeAck {
                turn: turn_id,
                cleanup: self.into_start_outcome().err(),
            },
            CodexPromptDelivery::NotDispatched => StartDelivery::NotWritten {
                error: self.into_start_outcome().expect_err("not dispatched"),
            },
            CodexPromptDelivery::Unknown => StartDelivery::Unknown {
                error: self.into_start_outcome().expect_err("unacknowledged start"),
                response_turn,
            },
        }
    }
    pub(crate) fn into_steer_delivery(self) -> SteerDelivery<ControlError> {
        let outcome = self.outcome.map_err(|failure| match failure {
            CodexPromptFailure::Local(error) | CodexPromptFailure::Admission(error, _) => {
                ControlError::Unanswered(error)
            }
            CodexPromptFailure::Control(error) => error,
            CodexPromptFailure::Start(error) => ControlError::Unanswered(error.into()),
            CodexPromptFailure::Rpc(error) => ControlError::from(error),
        });
        match self.delivery {
            CodexPromptDelivery::Accepted { turn_id } => SteerDelivery::NativeAck { turn: turn_id },
            CodexPromptDelivery::NotDispatched => SteerDelivery::NotWritten {
                error: outcome.expect_err("not dispatched"),
            },
            CodexPromptDelivery::Unknown => SteerDelivery::Unknown {
                error: outcome.expect_err("unacknowledged steer"),
            },
        }
    }
    pub(crate) fn into_outcome(self) -> Result<TurnId> {
        self.outcome.map_err(CodexPromptFailure::into_error)
    }
}

enum PromptSource<'a> {
    Native(&'a Arc<CodexAppServer>),
    #[cfg(feature = "fixtures")]
    Fixture,
}

impl SharedCodexAppServer {
    // Keep the native turn/start protocol fields explicit beside its execution
    // association and late admission callback; this boundary does not build a second plan.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn turn_start_report<'a>(
        &self,
        thread_id: &str,
        items: Vec<InputItem>,
        selection: &TurnModelSelection,
        approvals: TurnApprovals,
        client_id: Option<&str>,
        input: NativeInput,
        admission: &NativePromptAdmission<'a>,
    ) -> CodexPromptReport {
        if let Err(error) = self.require_start_authentication() {
            return CodexPromptReport::source_failure(
                thread_id,
                client_id,
                CodexPromptFailure::Start(error),
                false,
            );
        }
        if self.thread_seals.is_sealed(thread_id) {
            return CodexPromptReport::failure(
                thread_id,
                client_id,
                CalmError::Conflict(format!(
                    "thread {thread_id} is sealed because its track is being deleted"
                )),
                false,
            );
        }
        if !self.thread_cache.contains_key(thread_id) {
            tracing::warn!(target: "shared_codex_daemon::mapping_miss", %thread_id,
                method = "turn/start", "turn/start for thread missing shared daemon card mapping");
        }
        #[cfg(feature = "fixtures")]
        if let Some(fake) = self.fake.as_ref() {
            if let Err(error) = admission.check_fixture().await {
                return CodexPromptReport::failure(thread_id, client_id, error, false);
            }
            if fake.reject_turn_start.load(Ordering::SeqCst) {
                return CodexPromptReport::source_failure(
                    thread_id,
                    client_id,
                    CodexPromptFailure::Start(control::StartError::Refused(
                        CalmError::CodexRefused(
                            "turn/start failed: unknown model (code -32602)".into(),
                        ),
                    )),
                    true,
                );
            }
            if fake.fail_turn_start.load(Ordering::SeqCst) {
                return CodexPromptReport::source_failure(
                    thread_id,
                    client_id,
                    CodexPromptFailure::Start(control::StartError::Unanswered(
                        CalmError::CodexAppServer("forced turn/start failure for test".into()),
                    )),
                    true,
                );
            }
            fake.started_turns
                .lock()
                .expect("fake shared codex started turns mutex poisoned")
                .push((thread_id.to_owned(), items.clone()));
            fake.started_turn_selections
                .lock()
                .expect("fake shared codex turn selections mutex poisoned")
                .push((thread_id.to_owned(), selection.clone()));
            fake.started_turn_client_ids
                .lock()
                .expect("fake shared codex turn client ids mutex poisoned")
                .push(client_id.map(str::to_owned));
            fake.started_turn_approvals
                .lock()
                .expect("fake shared codex turn approvals mutex poisoned")
                .push(approvals);
            let n = fake.next_turn.fetch_add(1, Ordering::SeqCst);
            let turn_id = format!("fake-turn-{n:04}");
            let hook = fake
                .turn_start_return_hook
                .lock()
                .expect("fake shared codex turn-start hook mutex poisoned")
                .take();
            if let Some(hook) = hook {
                hook.entered.notify_one();
                hook.release.notified().await;
            }
            self.native_turns
                .insert(thread_id.to_owned(), turn_id.clone());
            let _ = self.notifications.send(Notification::TurnStarted {
                thread_id: thread_id.to_owned(),
                turn: serde_json::json!({"id":turn_id,"input_len":items.len()}),
            });
            let outcome = self
                .finish_accepted_start(PromptSource::Fixture, thread_id, &turn_id)
                .await;
            return CodexPromptReport::accepted(
                thread_id,
                client_id,
                turn_id,
                outcome.map_err(CodexPromptFailure::Start),
            );
        }
        let client = match self.connected_client().await {
            Ok(client) => client,
            Err(error) => {
                return CodexPromptReport::source_failure(
                    thread_id,
                    client_id,
                    CodexPromptFailure::Control(error),
                    false,
                );
            }
        };
        if let Err(error) = self.require_start_authentication() {
            return CodexPromptReport::source_failure(
                thread_id,
                client_id,
                CodexPromptFailure::Start(error),
                false,
            );
        }
        let ticket = match self.native_turns.prepare_start(&client, thread_id, input) {
            Ok(ticket) => ticket,
            Err(error) => return CodexPromptReport::failure(thread_id, client_id, error, false),
        };
        let gate = admission;
        let delivery = client
            .turn_start_delivery_gated(
                thread_id,
                items,
                selection,
                approvals,
                client_id,
                gate.gate(),
            )
            .await;
        let response_turn = match &delivery {
            StartDelivery::Unknown { response_turn, .. } => response_turn.clone(),
            StartDelivery::NativeAck { .. } | StartDelivery::NotWritten { .. } => None,
        };
        let turn_id = match delivery {
            StartDelivery::NativeAck { turn, .. } => turn,
            StartDelivery::NotWritten { error } | StartDelivery::Unknown { error, .. } => {
                if error.crossing() == RequestCrossing::BeforeSend || error.remote_error().is_some()
                {
                    // Native slot cleanup is independent of application input ownership.
                    // A conflicting turn notification keeps its candidate interruptible.
                    self.native_turns.not_started(&client, thread_id, &ticket);
                }
                let mut report = CodexPromptReport::rpc_failure(
                    thread_id,
                    client_id,
                    error,
                    gate.take_refusal(),
                );
                report.response_turn = response_turn;
                return report;
            }
        };
        self.native_turns
            .accepted(&client, thread_id, &ticket, &turn_id);
        let outcome = self
            .finish_accepted_start(PromptSource::Native(&client), thread_id, &turn_id)
            .await;
        CodexPromptReport::accepted(
            thread_id,
            client_id,
            turn_id,
            outcome.map_err(CodexPromptFailure::Start),
        )
    }

    /// Acceptance is already known. Failure here cannot return ownership of its input.
    async fn finish_accepted_start(
        &self,
        source: PromptSource<'_>,
        thread_id: &str,
        turn_id: &str,
    ) -> std::result::Result<TurnId, control::StartError> {
        if self.thread_seals.is_sealed(thread_id) {
            // Preserve the interrupt producer's neutral failure before widening diagnostics.
            let outcome = match source {
                PromptSource::Native(client) => self
                    .interrupt_native_source_outcome(Some(client), thread_id, turn_id)
                    .await
                    .map_error(control::StartError::Unanswered),
                #[cfg(feature = "fixtures")]
                PromptSource::Fixture => self.interrupt_control(thread_id, turn_id).await,
            };
            if let provider::control::NativeInterruptOutcome::Unconfirmed { error } = outcome {
                return Err(error);
            }
            return Err(control::StartError::Host(CalmError::Conflict(format!(
                "thread {thread_id} was sealed while turn/start was in flight"
            ))));
        }
        Ok(turn_id.to_owned())
    }

    pub(crate) async fn turn_steer_report<'a>(
        &self,
        thread_id: &str,
        expected_turn_id: &str,
        items: Vec<InputItem>,
        client_id: Option<&str>,
        input: NativeSteer,
        admission: &NativePromptAdmission<'a>,
    ) -> CodexPromptReport {
        if let Err(error) = self.require_authentication() {
            return CodexPromptReport::source_failure(
                thread_id,
                client_id,
                CodexPromptFailure::Control(error),
                false,
            );
        }
        #[cfg(feature = "fixtures")]
        if let Some(fake) = self.fake.as_ref() {
            if let Err(error) = admission.check_fixture().await {
                return CodexPromptReport::failure(thread_id, client_id, error, false);
            }
            fake.steered_turns
                .lock()
                .expect("fake shared codex steered turns mutex poisoned")
                .push((
                    thread_id.into(),
                    expected_turn_id.into(),
                    items.clone(),
                    client_id.map(str::to_owned),
                ));
            let hook = fake
                .turn_steer_return_hook
                .lock()
                .expect("fake shared codex turn-steer hook mutex poisoned")
                .take();
            if let Some(hook) = hook {
                hook.entered.notify_one();
                hook.release.notified().await;
            }
            if let Some(message) = fake
                .reject_turn_steer
                .lock()
                .expect("fake shared codex reject-steer mutex poisoned")
                .clone()
            {
                return CodexPromptReport::source_failure(
                    thread_id,
                    client_id,
                    CodexPromptFailure::Control(ControlError::Refused(CalmError::CodexRefused(
                        message,
                    ))),
                    true,
                );
            }
            if fake.fail_turn_steer.load(Ordering::SeqCst) {
                return CodexPromptReport::failure(
                    thread_id,
                    client_id,
                    CalmError::CodexAppServer("request turn/steer timed out".into()),
                    true,
                );
            }
            return match self.active_turn_id_for_thread(thread_id) {
                None => CodexPromptReport::source_failure(
                    thread_id,
                    client_id,
                    CodexPromptFailure::Control(ControlError::Refused(CalmError::CodexRefused(
                        "turn/steer failed: no active turn to steer (code -32600)".into(),
                    ))),
                    true,
                ),
                Some(active) if active != expected_turn_id => CodexPromptReport::source_failure(
                    thread_id,
                    client_id,
                    CodexPromptFailure::Control(ControlError::Refused(CalmError::CodexRefused(
                        format!(
                            "turn/steer failed: expected active turn id `{expected_turn_id}` but found `{active}` (code -32600)"
                        ),
                    ))),
                    true,
                ),
                Some(active) => {
                    CodexPromptReport::accepted(thread_id, client_id, active.clone(), Ok(active))
                }
            };
        }
        let client = match self.authenticated_client().await {
            Ok(client) => client,
            Err(error) => {
                return CodexPromptReport::source_failure(
                    thread_id,
                    client_id,
                    CodexPromptFailure::Control(error),
                    false,
                );
            }
        };
        let ticket =
            match self
                .native_turns
                .begin_steer(&client, thread_id, expected_turn_id, &input)
            {
                Ok(ticket) => ticket,
                Err(error) => {
                    return CodexPromptReport::failure(thread_id, client_id, error, false);
                }
            };
        let gate = admission;
        let result = client
            .turn_steer_delivery_gated(thread_id, expected_turn_id, items, client_id, gate.gate())
            .await;
        self.native_turns.finish_steer(&client, thread_id, &ticket);
        match result {
            SteerDelivery::NotWritten { error } | SteerDelivery::Unknown { error } => {
                CodexPromptReport::rpc_failure(thread_id, client_id, error, gate.take_refusal())
            }
            SteerDelivery::NativeAck { turn } => {
                CodexPromptReport::accepted(thread_id, client_id, turn.clone(), Ok(turn))
            }
        }
    }
}
