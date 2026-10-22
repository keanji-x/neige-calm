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

impl SharedCodexAppServer {
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
            RecoveryClient::Native(self.connected_client().await?)
        };
        #[cfg(not(feature = "fixtures"))]
        let client = RecoveryClient::Native(self.connected_client().await?);
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
        &self.daemon.kernel_mcp_socket_path
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
