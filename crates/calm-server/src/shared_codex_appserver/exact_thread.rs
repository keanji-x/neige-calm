//! Native exact-thread operations under the daemon's existing transition/replay boundary.
use super::*;
use crate::harness::backend::factory::recovery::RecoveryTarget;
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

pub(crate) struct RecoveryThread<'a> {
    daemon: &'a SharedCodexAppServer,
    target: &'a RecoveryTarget,
}

impl SharedCodexAppServer {
    pub(crate) async fn with_exact_thread<T>(
        &self,
        target: &RecoveryTarget,
        run: impl for<'scope> AsyncFnOnce(RecoveryThread<'scope>) -> Result<T>,
    ) -> Result<T> {
        let _transition = self.transition_serial.lock().await;
        let _replay = self.resume_replay_serial.lock().await;
        if self.thread_seals.is_sealed(target.thread_id()) {
            return Err(CalmError::Conflict("conversation is being deleted".into()));
        }
        run(RecoveryThread {
            daemon: self,
            target,
        })
        .await
    }
    async fn read_recovery_thread(&self, thread_id: &str) -> Result<Value> {
        #[cfg(feature = "fixtures")]
        if self.fake.is_some() {
            let status = if self.active_turn_id_for_thread(thread_id).is_some() {
                "active"
            } else {
                "systemError"
            };
            return Ok(json!({"id":thread_id,"status":{"type":status}}));
        }
        Ok(self
            .connected_client()
            .await?
            .thread_read_full(thread_id)
            .await?
            .thread)
    }

    async fn resume_recovery_thread(&self, thread_id: &str, config: ThreadConfig) -> Result<Value> {
        #[cfg(feature = "fixtures")]
        if let Some(fake) = self.fake.as_ref() {
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
        Ok(self
            .connected_client()
            .await?
            .thread_resume_with_config(thread_id, config.to_wire_config()?)
            .await?
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
        self.target.thread_id()
    }
    pub(crate) fn socket_path(&self) -> &std::path::Path {
        &self.daemon.kernel_mcp_socket_path
    }
    pub(crate) async fn read_active(&self) -> Result<RecoveryState> {
        #[cfg(feature = "fixtures")]
        if self.daemon.fake.is_some()
            && !self
                .daemon
                .thread_cache
                .contains_key(self.target.thread_id())
        {
            return Ok(RecoveryState::from_thread(
                &json!({"id":self.target.thread_id(),"status":{"type":"notLoaded"}}),
            ));
        }
        Ok(RecoveryState::from_thread(
            &self
                .daemon
                .read_recovery_thread(self.target.thread_id())
                .await?,
        ))
    }
    pub(crate) async fn read(&self, expected_turn: Option<&str>) -> Result<RecoveryRead> {
        let thread = self
            .daemon
            .read_recovery_thread(self.target.thread_id())
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
    pub(crate) async fn resume(&self, config: ThreadConfig) -> Result<RecoveryState> {
        let thread = self
            .daemon
            .resume_recovery_thread(self.target.thread_id(), config)
            .await?;
        Ok(RecoveryState::from_thread(&thread))
    }
    pub(crate) fn retain_subscription(&self) {
        self.daemon.thread_cache.insert(
            self.target.thread_id().to_owned(),
            self.target.card_id().to_owned(),
        );
    }
    pub(crate) fn clear_hibernating_release(&self) {
        self.daemon
            .hibernating_releases
            .remove(self.target.thread_id());
    }
}
