//! Native backend: launch consumes authority; stop/recovery only produce observations.
use super::*;
#[cfg(feature = "fixtures")]
use crate::codex_appserver::ThreadStatus;
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
    // Notifications borrow a live connection without retaining the supervisor or storage.
    pub(super) fn for_notification(
        client: Arc<CodexAppServer>,
        active_turns: Arc<DashMap<String, String>>,
        #[cfg(feature = "fixtures")] notifications: NotificationFanout,
    ) -> Self {
        Self {
            connection: Connection::Live(client),
            active_turns,
            #[cfg(feature = "fixtures")]
            fake: None,
            #[cfg(feature = "fixtures")]
            notifications,
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
pub(in crate::operation::execution_manager) struct ScopeObservation {
    pub thread: super::workspace::NativeThread,
    pub background_stopped: bool,
}
impl CodexBackend {
    pub(in crate::operation::execution_manager) async fn discover(
        &self,
        thread: &str,
    ) -> Result<ScopeObservation> {
        #[cfg(feature = "fixtures")]
        if let Some(fake) = self.fake.as_ref() {
            if let Some(facts) = fake
                .native_scope_snapshots
                .lock()
                .expect("fixture scope snapshots")
                .get(thread)
                .cloned()
            {
                return Ok(ScopeObservation {
                    thread: facts,
                    background_stopped: true,
                });
            }
            let history = fake.native_turns.lock().expect("fake provider history");
            let known = history
                .iter()
                .filter(|turn| turn.thread == thread)
                .collect::<Vec<_>>();
            let first = known.first().ok_or_else(|| {
                CalmError::Conflict("fixture provider has no scope history".into())
            })?;
            let stopped = known.iter().all(|turn| {
                matches!(
                    turn.status,
                    TurnStatus::Completed | TurnStatus::Interrupted | TurnStatus::Failed
                )
            });
            return Ok(ScopeObservation {
                thread: super::workspace::NativeThread {
                    id: thread.to_owned(),
                    cwd: first.cwd.clone(),
                    status: if stopped {
                        ThreadStatus::Idle
                    } else {
                        ThreadStatus::Active {
                            active_flags: vec![],
                        }
                    },
                    turns: known
                        .iter()
                        .map(|turn| super::workspace::NativeTurn {
                            id: turn.turn.clone(),
                            status: turn.status,
                            items: vec![
                                serde_json::json!({"type":"userMessage","clientId":turn.nonce}),
                            ],
                        })
                        .collect(),
                },
                background_stopped: true,
            });
        }
        let client = self.client()?;
        Ok(ScopeObservation {
            thread: client.thread_workspace_history(thread).await?.thread,
            background_stopped: client.background_terminals_stopped(thread).await?,
        })
    }

    async fn observe(&self, record: &Record, stop: bool) -> Result<Observation> {
        let mut facts = self.discover(&record.holder).await?;
        if facts.thread.id != record.holder
            || std::fs::canonicalize(&facts.thread.cwd)? != std::fs::canonicalize(&record.cwd)?
        {
            return Err(CalmError::Conflict(
                "native evidence differs from reserved execution scope".into(),
            ));
        }
        let turn = match record.nonce.as_deref() {
            Some(nonce) => facts.thread.turn_for_nonce(nonce)?,
            None => match record.observed.as_deref() {
                Some(id) => facts.thread.turns.iter().find(|turn| turn.id == id),
                None => facts
                    .thread
                    .turns
                    .iter()
                    .rev()
                    .find(|turn| matches!(turn.status, TurnStatus::InProgress))
                    .or_else(|| facts.thread.turns.last()),
            },
        }
        .map(|turn| turn.id.clone());
        if let Some(acknowledged) = record.observed.as_deref()
            && turn.as_deref() != Some(acknowledged)
        {
            return Err(CalmError::Conflict(
                "provider history contradicts acknowledged execution generation".into(),
            ));
        }
        if turn.is_none() && (record.nonce.is_some() || record.observed.is_some()) {
            return Err(CalmError::Conflict(
                "native issuance remains unconfirmed".into(),
            ));
        }
        if stop && let Some(turn) = turn.as_deref() {
            self.interrupt_provider(&record.holder, turn).await?;
            facts = self.discover(&record.holder).await?;
        }
        if facts.thread.id != record.holder
            || std::fs::canonicalize(&facts.thread.cwd)? != std::fs::canonicalize(&record.cwd)?
        {
            return Err(CalmError::Conflict("native stopped scope changed".into()));
        }
        let target_stopped = match turn.as_deref() {
            Some(target) => {
                if let Some(nonce) = record.nonce.as_deref()
                    && facts
                        .thread
                        .turn_for_nonce(nonce)?
                        .map(|turn| turn.id.as_str())
                        != Some(target)
                {
                    return Err(CalmError::Conflict(
                        "stopped provider history lost the exact request generation".into(),
                    ));
                }
                facts
                    .thread
                    .turns
                    .iter()
                    .find(|candidate| candidate.id == target)
                    .is_some_and(|candidate| {
                        matches!(
                            candidate.status,
                            TurnStatus::Completed | TurnStatus::Interrupted | TurnStatus::Failed
                        )
                    })
            }
            None => false,
        };
        let stopped = target_stopped && facts.thread.stopped() && facts.background_stopped;
        Ok(Observation {
            execution: record.id.clone(),
            identity: turn,
            stopped,
        })
    }

    async fn interrupt_provider(&self, thread: &str, turn: &str) -> Result<()> {
        #[cfg(feature = "fixtures")]
        if let Some(fake) = self.fake.as_ref() {
            fake.interrupted_turns
                .lock()
                .expect("fixture interrupts")
                .push((thread.to_owned(), turn.to_owned()));
            if fake.fail_turn_interrupt.load(Ordering::SeqCst) {
                return Err(CalmError::CodexAppServer(
                    "fixture: turn/interrupt failed".into(),
                ));
            }
            for known in fake
                .native_turns
                .lock()
                .expect("fake provider history")
                .iter_mut()
            {
                if known.thread == thread && known.turn == turn {
                    known.status = TurnStatus::Interrupted;
                }
            }
            if let Some(facts) = fake
                .native_scope_snapshots
                .lock()
                .expect("fixture scope snapshots")
                .get_mut(thread)
            {
                for known in &mut facts.turns {
                    if known.id == turn {
                        known.status = TurnStatus::Interrupted;
                    }
                }
                if facts.turns.iter().all(|turn| {
                    matches!(
                        turn.status,
                        TurnStatus::Completed | TurnStatus::Interrupted | TurnStatus::Failed
                    )
                }) {
                    facts.status = ThreadStatus::Idle;
                }
            }
            return Ok(());
        }
        let client = self.client()?;
        client.turn_interrupt(thread, turn).await?;
        client.clean_background_terminals(thread).await
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
