//! Native backend: launch consumes authority; stop/recovery only produce observations.
use super::*;
use crate::codex_appserver::TurnStatus;
use crate::operation::execution_manager::backend::{Backend, LaunchOutcome, Observation};
use crate::operation::execution_manager::{LaunchPermit, Record};

enum Connection {
    Live(Arc<CodexAppServer>),
    Unavailable(String),
    #[cfg(feature = "fixtures")]
    Fixture,
}
pub(in crate::operation::execution_manager) struct CodexBackend {
    connection: Connection,
    active_turns: Arc<DashMap<String, String>>,
    #[cfg(feature = "fixtures")]
    fake: Option<Arc<FakeSharedCodexAppServer>>,
    #[cfg(feature = "fixtures")]
    notifications: NotificationFanout,
}
impl CodexBackend {
    pub(in crate::operation::execution_manager) async fn for_service(
        service: &SharedCodexAppServer,
    ) -> Self {
        #[cfg(feature = "fixtures")]
        let connection = if service.fake.is_some() {
            Connection::Fixture
        } else {
            match service.connected_client().await {
                Ok(client) => Connection::Live(client),
                Err(error) => Connection::Unavailable(error.to_string()),
            }
        };
        #[cfg(not(feature = "fixtures"))]
        let connection = match service.connected_client().await {
            Ok(client) => Connection::Live(client),
            Err(error) => Connection::Unavailable(error.to_string()),
        };
        Self {
            connection,
            active_turns: service.active_turns.clone(),
            #[cfg(feature = "fixtures")]
            fake: service.fake.clone(),
            #[cfg(feature = "fixtures")]
            notifications: service.notifications.clone(),
        }
    }
    fn client(&self) -> Result<&CodexAppServer> {
        match &self.connection {
            Connection::Live(client) => Ok(client),
            Connection::Unavailable(error) => Err(CalmError::CodexAppServer(error.clone())),
            #[cfg(feature = "fixtures")]
            Connection::Fixture => Err(CalmError::CodexAppServer(
                "fixture has no provider transport".into(),
            )),
        }
    }
}

pub(in crate::operation::execution_manager) struct TurnRequest {
    pub thread: String,
    pub items: Vec<InputItem>,
    pub selection: TurnModelSelection,
}
#[async_trait::async_trait]
impl Backend for CodexBackend {
    type Request = TurnRequest;
    fn kind(&self) -> crate::operation::execution_manager::BackendKind {
        crate::operation::execution_manager::BackendKind::NativeTurn
    }
    async fn launch(&self, permit: LaunchPermit, request: TurnRequest) -> LaunchOutcome {
        if permit.record().holder != request.thread {
            return LaunchOutcome::NotIssued(CalmError::Conflict(
                "launch permit belongs to another native owner".into(),
            ));
        }
        let nonce = permit.nonce().to_owned();
        #[cfg(feature = "fixtures")]
        if let Some(fake) = self.fake.as_ref() {
            if fake.reject_turn_start.load(Ordering::SeqCst) {
                return LaunchOutcome::NotIssued(CalmError::CodexRefused(
                    "turn/start failed: unknown model (code -32602)".into(),
                ));
            }
            if fake.fail_turn_start.load(Ordering::SeqCst) {
                return LaunchOutcome::Uncertain(CalmError::CodexAppServer(
                    "forced turn/start failure for test".into(),
                ));
            }
            let n = fake.next_turn.fetch_add(1, Ordering::SeqCst);
            let turn = format!("fake-turn-{n:04}");
            fake.native_turns
                .lock()
                .expect("fake provider history")
                .push(FakeNativeTurn {
                    thread: request.thread.clone(),
                    turn: turn.clone(),
                    cwd: permit.record().cwd.clone(),
                    nonce: nonce.clone(),
                    status: TurnStatus::InProgress,
                });
            fake.started_turns
                .lock()
                .expect("started turns")
                .push((request.thread.clone(), request.items));
            fake.started_turn_selections
                .lock()
                .expect("turn selections")
                .push((request.thread.clone(), request.selection));
            fake.started_turn_client_ids
                .lock()
                .expect("client IDs")
                .push(Some(nonce));
            let hook = fake
                .turn_start_return_hook
                .lock()
                .expect("return hook")
                .take();
            if let Some(hook) = hook {
                hook.entered.notify_one();
                hook.release.notified().await;
            }
            let _ = self.notifications.send(Notification::TurnStarted {
                thread_id: request.thread,
                turn: serde_json::json!({"id":turn}),
            });
            return LaunchOutcome::Started(turn);
        }
        let client = match self.client() {
            Ok(client) => client,
            Err(error) => return LaunchOutcome::NotIssued(error),
        };
        let permissions = permit.policy();
        if permit.record().access == calm_types::workspace_access::WorkspaceAccess::ReadOnly
            && permissions.is_none()
        {
            return LaunchOutcome::NotIssued(CalmError::Conflict(
                "read execution permit lacks its enforced policy".into(),
            ));
        }
        let result = match permissions {
            Some(policy) => {
                client
                    .turn_start_with_permissions(
                        &request.thread,
                        request.items,
                        &request.selection,
                        Some(&nonce),
                        policy,
                    )
                    .await
            }
            None => {
                client
                    .turn_start_with_client_id(
                        &request.thread,
                        request.items,
                        &request.selection,
                        Some(&nonce),
                    )
                    .await
            }
        };
        match result {
            Ok(receipt) => match receipt.turn_id().filter(|id| !id.is_empty()) {
                Some(id) => LaunchOutcome::Started(id.to_owned()),
                None => LaunchOutcome::Uncertain(CalmError::CodexAppServer(
                    "turn/start returned no turn.id".into(),
                )),
            },
            Err(error @ CalmError::CodexRefused(_)) => LaunchOutcome::NotIssued(error),
            Err(error) => LaunchOutcome::Uncertain(error),
        }
    }
    async fn recover(&self, record: &Record) -> Result<Observation> {
        self.observe(record, record.phase == "stopping").await
    }
    async fn stop(&self, record: &Record) -> Result<Observation> {
        self.observe(record, true).await
    }
}
impl CodexBackend {
    async fn observe(&self, record: &Record, stop: bool) -> Result<Observation> {
        #[cfg(feature = "fixtures")]
        if let Some(fake) = self.fake.as_ref() {
            return self.observe_fixture(fake, record, stop);
        }
        let client = self.client()?;
        let mut facts = client
            .thread_workspace_history(&record.holder)
            .await?
            .thread;
        if facts.id != record.holder
            || std::fs::canonicalize(&facts.cwd)? != std::fs::canonicalize(&record.cwd)?
        {
            return Err(CalmError::Conflict(
                "native evidence differs from reserved execution scope".into(),
            ));
        }
        let turn = match record.nonce.as_deref() {
            Some(nonce) => facts.turn_for_nonce(nonce)?,
            None => record
                .observed
                .as_deref()
                .and_then(|id| facts.turns.iter().find(|turn| turn.id == id)),
        }
        .ok_or_else(|| CalmError::Conflict("native issuance remains unconfirmed".into()))?
        .id
        .clone();
        if stop {
            client.turn_interrupt(&record.holder, &turn).await?;
            client.clean_background_terminals(&record.holder).await?;
            facts = client
                .thread_workspace_history(&record.holder)
                .await?
                .thread;
        }
        if facts.id != record.holder
            || std::fs::canonicalize(&facts.cwd)? != std::fs::canonicalize(&record.cwd)?
        {
            return Err(CalmError::Conflict("native stopped scope changed".into()));
        }
        let stopped = facts
            .turns
            .iter()
            .find(|candidate| candidate.id == turn)
            .is_some_and(|candidate| {
                matches!(
                    candidate.status,
                    TurnStatus::Completed | TurnStatus::Interrupted | TurnStatus::Failed
                )
            })
            && facts.stopped()
            && client.background_terminals_stopped(&record.holder).await?;
        if stopped {
            self.active_turns
                .remove_if(&record.holder, |_, active| active == &turn);
        }
        Ok(Observation {
            execution: record.id.clone(),
            identity: Some(turn),
            stopped,
        })
    }
}

impl CodexBackend {
    pub(in crate::operation::execution_manager) async fn steer(
        &self,
        permit: super::control::TurnControlPermit,
        items: Vec<InputItem>,
        client_user_message_id: Option<&str>,
    ) -> Result<TurnId> {
        let thread_id = permit.thread();
        let expected_turn_id = permit.turn();
        #[cfg(feature = "fixtures")]
        if let Some(fake) = self.fake.as_ref() {
            fake.steered_turns
                .lock()
                .expect("fake shared codex steered turns mutex poisoned")
                .push((
                    thread_id.to_string(),
                    expected_turn_id.to_string(),
                    items.clone(),
                    client_user_message_id.map(ToOwned::to_owned),
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
            let scripted = fake
                .reject_turn_steer
                .lock()
                .expect("fake shared codex reject-steer mutex poisoned")
                .clone();
            if let Some(message) = scripted {
                return Err(CalmError::CodexRefused(message));
            }
            if fake.fail_turn_steer.load(Ordering::SeqCst) {
                return Err(CalmError::CodexAppServer(
                    "request turn/steer timed out".into(),
                ));
            }
            return match self
                .active_turns
                .get(thread_id)
                .map(|entry| entry.value().clone())
            {
                None => Err(CalmError::CodexRefused(
                    "turn/steer failed: no active turn to steer (code -32600)".into(),
                )),
                Some(active) if active != expected_turn_id => {
                    Err(CalmError::CodexRefused(format!(
                        "turn/steer failed: expected active turn id `{expected_turn_id}` but \
                         found `{active}` (code -32600)"
                    )))
                }
                Some(active) => Ok(active),
            };
        }
        let client = self.client()?;
        let steered = client
            .turn_steer(thread_id, expected_turn_id, items, client_user_message_id)
            .await?;
        Ok(steered.turn_id)
    }
}

#[cfg(feature = "fixtures")]
pub(super) struct FakeNativeTurn {
    pub thread: String,
    pub turn: String,
    pub cwd: String,
    pub nonce: String,
    pub status: TurnStatus,
}
#[cfg(feature = "fixtures")]
impl CodexBackend {
    fn observe_fixture(
        &self,
        fake: &FakeSharedCodexAppServer,
        record: &Record,
        stop: bool,
    ) -> Result<Observation> {
        let mut history = fake.native_turns.lock().expect("fake provider history");
        let turn = history
            .iter_mut()
            .find(|turn| {
                turn.thread == record.holder
                    && record.nonce.as_deref() == Some(turn.nonce.as_str())
                    && turn.cwd == record.cwd
            })
            .ok_or_else(|| {
                CalmError::Conflict("fixture provider has no matching issuance".into())
            })?;
        if stop {
            fake.interrupted_turns
                .lock()
                .expect("fixture interrupts")
                .push((turn.thread.clone(), turn.turn.clone()));
            if fake.fail_turn_interrupt.load(Ordering::SeqCst) {
                return Err(CalmError::CodexAppServer(
                    "fixture: turn/interrupt failed".into(),
                ));
            }
            turn.status = TurnStatus::Interrupted;
        }
        let identity = turn.turn.clone();
        let stopped = history
            .iter()
            .filter(|turn| turn.thread == record.holder)
            .all(|turn| {
                matches!(
                    turn.status,
                    TurnStatus::Completed | TurnStatus::Interrupted | TurnStatus::Failed
                )
            });
        Ok(Observation {
            execution: record.id.clone(),
            identity: Some(identity),
            stopped,
        })
    }
}
