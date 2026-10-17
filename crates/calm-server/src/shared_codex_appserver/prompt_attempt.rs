//! Native prompt evidence, before the conversation owner attaches its durable attempt.
use super::*;
use provider::codex::{RequestCrossing, RpcAttemptFailure};

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
}

pub(crate) enum CodexPromptFailure {
    Local(CalmError),
    Rpc(RpcAttemptFailure),
}

impl CodexPromptFailure {
    pub(crate) fn into_error(self) -> CalmError {
        match self {
            Self::Local(error) => error,
            Self::Rpc(error) => error.into_legacy_error().into(),
        }
    }
}

impl CodexPromptReport {
    fn failure(thread: &str, client: Option<&str>, error: CalmError, sent: bool) -> Self {
        Self {
            thread_id: thread.into(),
            client_id: client.map(str::to_owned),
            delivery: if sent {
                CodexPromptDelivery::Unknown
            } else {
                CodexPromptDelivery::NotDispatched
            },
            outcome: Err(CodexPromptFailure::Local(error)),
        }
    }

    fn rpc_failure(thread: &str, client: Option<&str>, error: RpcAttemptFailure) -> Self {
        Self {
            thread_id: thread.into(),
            client_id: client.map(str::to_owned),
            delivery: match error.crossing() {
                RequestCrossing::BeforeSend => CodexPromptDelivery::NotDispatched,
                RequestCrossing::AfterSend => CodexPromptDelivery::Unknown,
            },
            outcome: Err(CodexPromptFailure::Rpc(error)),
        }
    }

    fn accepted(thread: &str, client: Option<&str>, turn: String, outcome: Result<TurnId>) -> Self {
        Self {
            thread_id: thread.into(),
            client_id: client.map(str::to_owned),
            delivery: CodexPromptDelivery::Accepted { turn_id: turn },
            outcome: outcome.map_err(CodexPromptFailure::Local),
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
            Err(CodexPromptFailure::Rpc(error)) => Some(error),
            _ => None,
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
    pub(crate) async fn turn_start_report(
        &self,
        thread_id: &str,
        items: Vec<InputItem>,
        selection: &TurnModelSelection,
        approvals: TurnApprovals,
        client_id: Option<&str>,
        input: NativeInput,
    ) -> CodexPromptReport {
        if let Err(error) = self.require_authentication() {
            return CodexPromptReport::failure(thread_id, client_id, error, false);
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
            if fake.reject_turn_start.load(Ordering::SeqCst) {
                return CodexPromptReport::failure(
                    thread_id,
                    client_id,
                    CalmError::CodexRefused(
                        "turn/start failed: unknown model (code -32602)".into(),
                    ),
                    true,
                );
            }
            if fake.fail_turn_start.load(Ordering::SeqCst) {
                return CodexPromptReport::failure(
                    thread_id,
                    client_id,
                    CalmError::CodexAppServer("forced turn/start failure for test".into()),
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
            return CodexPromptReport::accepted(thread_id, client_id, turn_id, outcome);
        }
        let client = match self.authenticated_client().await {
            Ok(client) => client,
            Err(error) => return CodexPromptReport::failure(thread_id, client_id, error, false),
        };
        let ticket = match self.native_turns.begin(&client, thread_id, input) {
            Ok(ticket) => ticket,
            Err(error) => return CodexPromptReport::failure(thread_id, client_id, error, false),
        };
        let result = match client
            .turn_start_attempt(thread_id, items, selection, approvals, client_id)
            .await
        {
            Ok(result) => result,
            Err(error) => {
                if error.crossing() == RequestCrossing::BeforeSend {
                    self.native_turns.not_sent(&client, thread_id, &ticket);
                }
                return CodexPromptReport::rpc_failure(thread_id, client_id, error);
            }
        };
        let Some(turn_id) = result
            .turn_id()
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
        else {
            return CodexPromptReport::failure(
                thread_id,
                client_id,
                CalmError::CodexAppServer("turn/start returned no turn.id".into()),
                true,
            );
        };
        self.native_turns
            .accepted(&client, thread_id, &ticket, &turn_id);
        let outcome = self
            .finish_accepted_start(PromptSource::Native(&client), thread_id, &turn_id)
            .await;
        CodexPromptReport::accepted(thread_id, client_id, turn_id, outcome)
    }

    /// Acceptance is already known. Failure here cannot return ownership of its input.
    async fn finish_accepted_start(
        &self,
        source: PromptSource<'_>,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<TurnId> {
        if self.thread_seals.is_sealed(thread_id) {
            match source {
                PromptSource::Native(client) => {
                    self.interrupt_native_source(Some(client), thread_id, turn_id)
                        .await?
                }
                #[cfg(feature = "fixtures")]
                PromptSource::Fixture => self.turn_interrupt(thread_id, turn_id).await?,
            }
            return Err(CalmError::Conflict(format!(
                "thread {thread_id} was sealed while turn/start was in flight"
            )));
        }
        Ok(turn_id.to_owned())
    }

    pub(crate) async fn turn_steer_report(
        &self,
        thread_id: &str,
        expected_turn_id: &str,
        items: Vec<InputItem>,
        client_id: Option<&str>,
        input: NativeSteer,
    ) -> CodexPromptReport {
        if let Err(error) = self.require_authentication() {
            return CodexPromptReport::failure(thread_id, client_id, error, false);
        }
        #[cfg(feature = "fixtures")]
        if let Some(fake) = self.fake.as_ref() {
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
                return CodexPromptReport::failure(
                    thread_id,
                    client_id,
                    CalmError::CodexRefused(message),
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
                None => CodexPromptReport::failure(
                    thread_id,
                    client_id,
                    CalmError::CodexRefused(
                        "turn/steer failed: no active turn to steer (code -32600)".into(),
                    ),
                    true,
                ),
                Some(active) if active != expected_turn_id => CodexPromptReport::failure(
                    thread_id,
                    client_id,
                    CalmError::CodexRefused(format!(
                        "turn/steer failed: expected active turn id `{expected_turn_id}` but found `{active}` (code -32600)"
                    )),
                    true,
                ),
                Some(active) => {
                    CodexPromptReport::accepted(thread_id, client_id, active.clone(), Ok(active))
                }
            };
        }
        let client = match self.authenticated_client().await {
            Ok(client) => client,
            Err(error) => return CodexPromptReport::failure(thread_id, client_id, error, false),
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
        let result = client
            .turn_steer_attempt(thread_id, expected_turn_id, items, client_id)
            .await;
        self.native_turns.finish_steer(&client, thread_id, &ticket);
        match result {
            Err(error) => CodexPromptReport::rpc_failure(thread_id, client_id, error),
            Ok(result) if !result.turn_id.is_empty() && result.turn_id == expected_turn_id => {
                CodexPromptReport::accepted(
                    thread_id,
                    client_id,
                    result.turn_id.clone(),
                    Ok(result.turn_id),
                )
            }
            Ok(_) => CodexPromptReport::failure(
                thread_id,
                client_id,
                CalmError::CodexAppServer("turn/steer returned an unexpected turnId".into()),
                true,
            ),
        }
    }
}
