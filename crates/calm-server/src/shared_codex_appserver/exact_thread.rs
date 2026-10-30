//! Native exact-thread operations under the daemon's existing transition/replay boundary.
use super::*;
use crate::harness::backend::factory::RecoveryTarget;
use serde_json::Value;
#[cfg(feature = "fixtures")]
use serde_json::json;

/// Only the native identity and status are needed to confirm resume.
pub(crate) struct RecoveryState {
    pub(crate) id: Option<String>,
    pub(crate) status: Option<String>,
}
impl RecoveryState {
    fn from_thread(thread: &Value) -> Self {
        Self {
            id: thread.get("id").and_then(Value::as_str).map(str::to_owned),
            status: thread
                .pointer("/status/type")
                .and_then(Value::as_str)
                .map(str::to_owned),
        }
    }
}

/// The host consumes only the matching terminal turn, never the rest of native history.
pub(crate) struct RecoveryRead {
    pub(crate) state: RecoveryState,
    pub(crate) terminal: Option<Value>,
}

enum RecoveryClient {
    Native(Arc<CodexAppServer>),
    #[cfg(feature = "fixtures")]
    Fixture,
}
impl RecoveryClient {
    fn native(&self) -> Result<&Arc<CodexAppServer>> {
        match self {
            Self::Native(client) => Ok(client),
            #[cfg(feature = "fixtures")]
            Self::Fixture => Err(CalmError::Internal(
                "fixture recovery has no native client".into(),
            )),
        }
    }
}

pub(crate) struct RecoveryThread<'a> {
    client: RecoveryClient,
    daemon: &'a SharedCodexAppServer,
    thread_id: &'a str,
    card_id: &'a str,
}

#[cfg(all(test, feature = "fixtures"))]
pub(super) struct ExactLookupWaiter {
    card_id: String,
    thread_id: String,
    entered: tokio::sync::oneshot::Sender<()>,
}

impl SharedCodexAppServer {
    #[cfg(all(test, feature = "fixtures"))]
    pub(crate) fn expect_exact_lookup_waiter_for_test(
        &self,
        card_id: &str,
        thread_id: &str,
    ) -> tokio::sync::oneshot::Receiver<()> {
        let (entered, receiver) = tokio::sync::oneshot::channel();
        let mut waiter = self.exact_lookup_waiter.lock().unwrap();
        assert!(waiter.is_none(), "exact lookup observer already armed");
        *waiter = Some(ExactLookupWaiter {
            card_id: card_id.to_owned(),
            thread_id: thread_id.to_owned(),
            entered,
        });
        receiver
    }

    #[cfg(all(test, feature = "fixtures"))]
    async fn acquire_exact_client(
        &self,
        target: &RecoveryTarget,
    ) -> std::result::Result<Arc<CodexAppServer>, ControlError> {
        let connection = self.connected_client();
        tokio::pin!(connection);
        std::future::poll_fn(|cx| {
            let result = std::future::Future::poll(connection.as_mut(), cx);
            if result.is_pending() {
                let mut waiter = self.exact_lookup_waiter.lock().unwrap();
                if waiter.as_ref().is_some_and(|waiter| {
                    waiter.card_id == target.card_id() && waiter.thread_id == target.thread_id()
                }) {
                    let _ = waiter.take().unwrap().entered.send(());
                }
            }
            result
        })
        .await
    }

    #[cfg(not(all(test, feature = "fixtures")))]
    async fn acquire_exact_client(
        &self,
        _target: &RecoveryTarget,
    ) -> std::result::Result<Arc<CodexAppServer>, ControlError> {
        self.connected_client().await
    }

    pub(crate) async fn with_exact_thread<T>(
        &self,
        target: &RecoveryTarget,
        run: impl for<'scope> AsyncFnOnce(RecoveryThread<'scope>) -> Result<T>,
    ) -> Result<T> {
        // Recover before entering the exact-thread boundary. No operation inside
        // this scope may acquire the recovery coordinator or change native source.
        #[cfg(feature = "fixtures")]
        let client = if self.fake.is_some() {
            RecoveryClient::Fixture
        } else {
            RecoveryClient::Native(self.acquire_exact_client(target).await?)
        };
        #[cfg(not(feature = "fixtures"))]
        let client = RecoveryClient::Native(self.acquire_exact_client(target).await?);
        let _transition = self.transition_serial.lock().await;
        let _replay = self.resume_replay_serial.lock().await;
        match &client {
            RecoveryClient::Native(expected) => {
                let current = self.ready_client().await;
                if !current
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, expected))
                {
                    return Err(CalmError::Conflict(
                        "Codex source changed before exact-thread recovery; retry".into(),
                    ));
                }
            }
            #[cfg(feature = "fixtures")]
            RecoveryClient::Fixture => {}
        }
        let thread_id = target.thread_id();
        let card_id = target.card_id();
        if self.thread_seals.is_sealed(thread_id) {
            return Err(CalmError::Conflict("conversation is being deleted".into()));
        }
        run(RecoveryThread {
            client,
            daemon: self,
            thread_id,
            card_id,
        })
        .await
    }
    async fn read_recovery_thread(
        &self,
        client: &RecoveryClient,
        thread_id: &str,
        admit: &NativePromptAdmission<'_>,
    ) -> Result<Value> {
        #[cfg(feature = "fixtures")]
        if self.fake.is_some() {
            admit.check_local().await?;
            let status = if self.active_turn_id_for_thread(thread_id).is_some() {
                "active"
            } else {
                "systemError"
            };
            return Ok(json!({"id":thread_id,"status":{"type":status}}));
        }
        let client = client.native()?;
        let gate = admit;
        Ok(gate
            .finish(client.thread_read_full_gated(thread_id, gate.gate()).await)?
            .thread)
    }

    async fn resume_recovery_thread(
        &self,
        client: &RecoveryClient,
        thread_id: &str,
        config: ThreadConfig,
        admit: &NativePromptAdmission<'_>,
    ) -> Result<Value> {
        #[cfg(feature = "fixtures")]
        if let Some(fake) = self.fake.as_ref() {
            admit.check_local().await?;
            if fake.fail_thread_resume.load(Ordering::SeqCst) {
                return Err(CalmError::CodexAppServer(
                    "forced thread/resume failure".into(),
                ));
            }
            fake.resumed_threads
                .lock()
                .expect("fake resumed threads mutex")
                .push((
                    thread_id.to_owned(),
                    matches!(config, ThreadConfig::McpShell { .. }),
                ));
            return Ok(json!({"id":thread_id,"status":{"type":"idle"}}));
        }
        let config = config.to_wire_config()?;
        let client = client.native()?;
        let gate = admit;
        Ok(gate
            .finish(
                client
                    .thread_resume_with_config_gated(thread_id, config, gate.gate())
                    .await,
            )?
            .thread)
    }

    #[cfg(feature = "fixtures")]
    pub fn resumed_threads_for_test(&self) -> Vec<(String, bool)> {
        self.fake
            .as_ref()
            .expect("fake daemon")
            .resumed_threads
            .lock()
            .unwrap()
            .clone()
    }
    #[cfg(feature = "fixtures")]
    pub fn fail_thread_resume_for_test(&self) {
        self.fake
            .as_ref()
            .expect("fake daemon")
            .fail_thread_resume
            .store(true, Ordering::SeqCst);
    }
}

impl RecoveryThread<'_> {
    pub(crate) fn thread_id(&self) -> &str {
        self.thread_id
    }
    pub(crate) fn socket_path(&self) -> &std::path::Path {
        &self.daemon.paths.kernel_mcp_socket
    }
    /// An idle read retires only the unchanged unaddressed start slot. Native
    /// source/ticket/candidate changes during the read continue to hold.
    pub(crate) async fn read_settled(&self, admit: &NativePromptAdmission<'_>) -> Result<bool> {
        #[cfg(feature = "fixtures")]
        if self.daemon.fake.is_some() {
            admit.check_local().await?;
            return Ok(false);
        }
        let client = self.client.native()?;
        let expected = self
            .daemon
            .native_turns
            .settlement_fence(client, self.thread_id)?;
        let thread = admit
            .finish(
                client
                    .thread_read_full_gated(self.thread_id, admit.gate())
                    .await,
            )?
            .thread;
        let state = RecoveryState::from_thread(&thread);
        Ok(state.id.as_deref() == Some(self.thread_id)
            && state.status.as_deref() == Some("idle")
            && self.daemon.native_turns.settle_unaddressed_start(&expected))
    }

    pub(crate) async fn read_active(
        &self,
        admit: &NativePromptAdmission<'_>,
    ) -> Result<RecoveryState> {
        #[cfg(feature = "fixtures")]
        if self.daemon.fake.is_some() && !self.daemon.thread_cache.contains_key(self.thread_id) {
            admit.check_local().await?;
            return Ok(RecoveryState::from_thread(
                &json!({"id":self.thread_id,"status":{"type":"notLoaded"}}),
            ));
        }
        Ok(RecoveryState::from_thread(
            &self
                .daemon
                .read_recovery_thread(&self.client, self.thread_id, admit)
                .await?,
        ))
    }
    pub(crate) async fn read(
        &self,
        expected_turn: Option<&str>,
        admit: &NativePromptAdmission<'_>,
    ) -> Result<RecoveryRead> {
        let thread = self
            .daemon
            .read_recovery_thread(&self.client, self.thread_id, admit)
            .await?;
        let terminal = expected_turn
            .and_then(|id| {
                thread
                    .get("turns")
                    .and_then(Value::as_array)
                    .and_then(|turns| {
                        turns.iter().find(|turn| {
                            turn.get("id").and_then(Value::as_str) == Some(id)
                                && matches!(
                                    turn.get("status").and_then(Value::as_str),
                                    Some("completed" | "failed" | "interrupted")
                                )
                        })
                    })
            })
            .cloned();
        Ok(RecoveryRead {
            state: RecoveryState::from_thread(&thread),
            terminal,
        })
    }
    pub(crate) async fn resume(
        &self,
        config: ThreadConfig,
        admit: &NativePromptAdmission<'_>,
    ) -> Result<RecoveryState> {
        let thread = self
            .daemon
            .resume_recovery_thread(&self.client, self.thread_id, config, admit)
            .await?;
        Ok(RecoveryState::from_thread(&thread))
    }
    pub(crate) fn retain_subscription(&self) {
        self.daemon
            .thread_cache
            .insert(self.thread_id.to_owned(), self.card_id.to_owned());
    }
    pub(crate) fn clear_hibernating_release(&self) {
        self.daemon.hibernating_releases.remove(self.thread_id);
    }
}

impl RecoveryThread<'_> {
    pub(crate) async fn reconcile_canonical_end(
        &self,
        repo: &dyn crate::db::Repo,
        accepted: &crate::harness::input_dispatch::end_transition::AcceptedRecovery,
        admit: &NativePromptAdmission<'_>,
        admission: Arc<dyn crate::harness::backend::factory::ActiveRecoveryAdmission>,
    ) -> Result<()> {
        use crate::harness::backend::ExecutionEndScope;
        if accepted.backend_id() != self.daemon.backend_id().as_str() {
            return Err(CalmError::Conflict(
                "native recovery backend namespace changed".into(),
            ));
        }
        let target = accepted.target();
        let ExecutionEndScope::NativeTurn {
            thread_id,
            native_turn_id,
        } = &target.scope
        else {
            return Err(CalmError::Conflict(
                "recovery requires an exact accepted native turn".into(),
            ));
        };
        if thread_id != self.thread_id || target.card_id != self.card_id {
            return Err(CalmError::Conflict(
                "native recovery names another conversation".into(),
            ));
        }
        let client = self.client.native()?;
        let fence = self
            .daemon
            .native_turns
            .settlement_fence(client, self.thread_id)?;
        // A cold thread can expose its exact past terminal before subscription restore.
        // This records no live control; ordinary restore still requires idle before issuance.
        let read = self.read(Some(native_turn_id), admit).await?;
        if read.state.id.as_deref() != Some(self.thread_id)
            || !matches!(read.state.status.as_deref(), Some("idle" | "notLoaded"))
        {
            return Err(CalmError::Conflict(
                "original native execution has not confirmed completion".into(),
            ));
        }
        let terminal = read.terminal.ok_or_else(|| {
            CalmError::Conflict("original native completion is unavailable".into())
        })?;
        admit.check_local().await?;
        if !self
            .daemon
            .native_turns
            .recovery_fence_matches(&fence, target)
        {
            return Err(CalmError::Conflict(
                "native recovery activity changed".into(),
            ));
        }
        let receipt = canonical_terminal_receipt(thread_id, native_turn_id, &terminal)?;
        let target = target.clone();
        let db = repo.sqlite_db().ok_or(CalmError::ExecutionUnavailable)?;
        let mut tx = crate::db::sqlite::begin_immediate_tx(&db).await?;
        admission.validate_tx(&mut tx).await?;
        accepted.validate_tx(&mut tx).await?;
        admission.check_current()?;
        if !self
            .daemon
            .native_turns
            .recovery_fence_matches(&fence, &target)
        {
            return Err(CalmError::Conflict("native recovery source changed".into()));
        }
        let carrier = crate::db::sqlite::conversation::current_input_fact_carrier_tx(
            &mut tx,
            &target.card_id,
            &target.worker_session_id,
        )
        .await?;
        receipt.settle_tx(&mut tx, &carrier).await?;
        admission.check_current()?;
        if !self
            .daemon
            .native_turns
            .recovery_fence_matches(&fence, &target)
        {
            return Err(CalmError::Conflict("native recovery source changed".into()));
        }
        tx.commit().await?;
        if !self
            .daemon
            .native_turns
            .retain_canonical_recovery(&fence, accepted.target(), &terminal)
        {
            return Err(CalmError::Conflict("native recovery source changed".into()));
        }
        Ok(())
    }
}

pub(crate) struct PreparedTerminalRecovery {
    pub(crate) native: super::IdleDetachGuard,
    pub(crate) transcript: crate::harness::transcript_receipt::CanonicalTerminalReceipt,
}

impl SharedCodexAppServer {
    /// Only the native owner can prove an ended thread is safe to retire. The
    /// source/issuance park survives this RPC scope and is rechecked at commit.
    pub(crate) async fn terminal_recovery_guard(
        &self,
        target: &RecoveryTarget,
        turn: &str,
        admit: &NativePromptAdmission<'_>,
    ) -> Result<PreparedTerminalRecovery> {
        self.verify_backend_isolation().await?;
        self.with_exact_thread(target, async |native| {
            let client = native.client.native()?;
            let fence = self
                .native_turns
                .settlement_fence(client, native.thread_id)?;
            let thread = self
                .read_recovery_thread(&native.client, native.thread_id, admit)
                .await?;
            let latest = thread
                .get("turns")
                .and_then(Value::as_array)
                .and_then(|turns| turns.last());
            if thread.get("id").and_then(Value::as_str) != Some(native.thread_id)
                || !matches!(
                    thread.pointer("/status/type").and_then(Value::as_str),
                    Some("idle" | "systemError" | "notLoaded")
                )
                || !latest.is_some_and(|end| {
                    end.get("id").and_then(Value::as_str) == Some(turn)
                        && end.get("status").and_then(Value::as_str) == Some("failed")
                })
            {
                return Err(CalmError::Conflict(
                    "original failed terminal is not confirmed by the native owner".into(),
                ));
            }
            let terminal = latest
                .ok_or_else(|| CalmError::Conflict("native terminal is unavailable".into()))?;
            let transcript = canonical_terminal_receipt(native.thread_id, turn, terminal)?;
            // The provider retains its error flag after END and resume; only a
            // new turn clears it. Retirement requires inactivity, not a new turn.
            let resumed = native.resume(ThreadConfig::NoMcp, admit).await?;
            if resumed.id.as_deref() != Some(native.thread_id)
                || !matches!(resumed.status.as_deref(), Some("idle" | "systemError"))
            {
                return Err(CalmError::Conflict(
                    "native cleanup is not confirmed inactive".into(),
                ));
            }
            admit.check_local().await?;
            Ok(PreparedTerminalRecovery {
                native: self.native_turns.park_terminal_recovery(fence, turn)?,
                transcript,
            })
        })
        .await
    }
}

// thread/read(includeTurns=true) hydrates full items. Older protocol versions
// omit itemsView; an explicit partial view must never authorize retirement.
fn canonical_terminal_receipt(
    thread: &str,
    turn: &str,
    terminal: &Value,
) -> Result<crate::harness::transcript_receipt::CanonicalTerminalReceipt> {
    if terminal
        .get("itemsView")
        .is_some_and(|view| view.as_str() != Some("full"))
    {
        return Err(CalmError::Conflict(
            "native terminal items are not complete".into(),
        ));
    }
    crate::harness::transcript_receipt::CanonicalTerminalReceipt::from_native(
        thread, turn, terminal,
    )
}
