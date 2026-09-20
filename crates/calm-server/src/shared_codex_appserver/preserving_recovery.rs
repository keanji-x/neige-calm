//! Exact-thread recovery. The caller owns the card/reset and track/delete
//! fences and has quiesced any failed harness before entering here.
use super::*;
use crate::db::sqlite::{session_resume_system_error_tx, session_system_error_recovery_matches_tx};
use crate::session_projection_repo::WorkerSessionProjection;
use serde_json::Value;
#[cfg(feature = "fixtures")]
use serde_json::json;

impl SharedCodexAppServer {
    pub(crate) async fn resume_system_error_conversation(
        &self,
        runtime: &WorkerSessionProjection,
        thread_id: &str,
    ) -> Result<Option<(i64, String)>> {
        // Same order as daemon replacement -> cached-thread resume. A daemon
        // cannot turn a hot read into a cold resume while credentials are chosen.
        let _transition = self.transition_serial.lock().await;
        let _replay = self.resume_replay_serial.lock().await;
        if self.turn_thread_is_sealed(thread_id) {
            return Err(CalmError::Conflict("conversation is being deleted".into()));
        }
        let snapshot = runtime.handle_state_json.clone().ok_or_else(|| {
            CalmError::Conflict("conversation recovery snapshot is missing".into())
        })?;
        let role = self
            .repo
            .card_role_get(&runtime.card_id)
            .await?
            .ok_or_else(|| CalmError::Conflict("conversation role is missing".into()))?;
        let card = self
            .repo
            .card_get(&runtime.card_id)
            .await?
            .ok_or_else(|| CalmError::NotFound("conversation no longer exists".into()))?;
        let profile = crate::harness::profile::HarnessProfile::from_card(&card, role)
            .ok_or_else(|| CalmError::Conflict("card is not a harness conversation".into()))?;
        self.validate_recovery_carrier(runtime, thread_id, &snapshot)
            .await?;
        // thread/read and thread/resume share Codex's global exclusive thread queue, so after a
        // lost resume response this read waits for that resume to settle, across connections.
        let before = self.read_recovery_thread(thread_id).await?;
        let cold = match before.get("status").and_then(|s| s.get("type")).and_then(Value::as_str) {
            Some("notLoaded") => true,
            Some("idle" | "systemError") => false,
            _ => return Err(CalmError::ServiceUnavailable(
                "The original conversation still has an active or unknown turn; retry after it settles. History is retained.".into())),
        };
        if before.get("id").and_then(Value::as_str) != Some(thread_id) {
            return Err(CalmError::Conflict(
                "provider returned a different conversation".into(),
            ));
        }
        let config = if cold && profile.mcp_role().is_some() {
            let card = runtime.card_id.clone();
            let id = runtime.id.clone();
            let thread = thread_id.to_owned();
            let expected = snapshot.clone();
            // Persist before RPC: a lost response can leave a loaded thread.
            // Its token must already be known on retry, while Failed continues
            // to deny that token authority until the final restore commits.
            let raw = write_in_tx_typed(self.repo.as_ref(), move |tx| {
                Box::pin(async move {
                    if !session_system_error_recovery_matches_tx(tx, &card, &id, &thread, &expected)
                        .await?
                    {
                        return Err(CalmError::Conflict(
                            "conversation changed during recovery".into(),
                        ));
                    }
                    mint_and_persist_card_token(tx, &card, &id).await
                })
            })
            .await?;
            ThreadConfig::McpShell {
                role,
                socket_path: self.kernel_mcp_socket_path.clone(),
                raw_token: raw,
            }
        } else {
            ThreadConfig::NoMcp
        };
        let resumed = self.resume_recovery_thread(thread_id, config).await?;
        if resumed.get("id").and_then(Value::as_str) != Some(thread_id)
            || !matches!(
                resumed
                    .get("status")
                    .and_then(|s| s.get("type"))
                    .and_then(Value::as_str),
                Some("idle" | "systemError")
            )
        {
            return Err(CalmError::ServiceUnavailable(
                "The original conversation could not be confirmed idle; history is retained. Retry shortly.".into()));
        }
        // The old loop may have been quiesced before its queued completion.
        // Recover the exact last turn's terminal outcome from provider history,
        // without replaying its inputs or relying on a future notification.
        let outcome = self
            .retain_recovered_outcome(runtime, thread_id, &before)
            .await?;
        let card = runtime.card_id.clone();
        let id = runtime.id.clone();
        let thread = thread_id.to_owned();
        let restored = write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move {
                Ok(session_resume_system_error_tx(tx, &card, &id, &thread, &snapshot).await?)
            })
        })
        .await?;
        if !restored {
            return Err(CalmError::Conflict(
                "conversation changed during recovery; message was not sent".into(),
            ));
        }
        self.thread_cache
            .insert(thread_id.to_owned(), runtime.card_id.clone());
        Ok(outcome)
    }

    async fn retain_recovered_outcome(
        &self,
        runtime: &WorkerSessionProjection,
        thread_id: &str,
        thread: &Value,
    ) -> Result<Option<(i64, String)>> {
        let Some(turn_id) = runtime
            .handle_state_json
            .as_ref()
            .and_then(|s| s.get("last_turn_id"))
            .and_then(Value::as_str)
        else {
            return Ok(None);
        };
        let Some(turn) = thread
            .get("turns")
            .and_then(Value::as_array)
            .and_then(|turns| {
                turns.iter().find(|turn| {
                    turn.get("id").and_then(Value::as_str) == Some(turn_id)
                        && matches!(
                            turn.get("status").and_then(Value::as_str),
                            Some("completed" | "failed" | "interrupted")
                        )
                })
            })
        else {
            return Ok(None);
        };
        let card = self
            .repo
            .card_get(&runtime.card_id)
            .await?
            .ok_or_else(|| CalmError::NotFound("conversation disappeared".into()))?;
        let id = crate::harness::turn_outcome::record(
            self.repo.as_ref(),
            &runtime.id,
            &runtime.card_id,
            card.track_id.as_str(),
            thread_id,
            turn_id,
            turn,
        )
        .await?;
        Ok(Some((id, turn_id.to_owned())))
    }

    async fn validate_recovery_carrier(
        &self,
        runtime: &WorkerSessionProjection,
        thread_id: &str,
        snapshot: &Value,
    ) -> Result<()> {
        let card = runtime.card_id.clone();
        let id = runtime.id.clone();
        let thread = thread_id.to_owned();
        let expected = snapshot.clone();
        let valid = write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move {
                Ok(
                    session_system_error_recovery_matches_tx(tx, &card, &id, &thread, &expected)
                        .await?,
                )
            })
        })
        .await?;
        if valid {
            Ok(())
        } else {
            Err(CalmError::Conflict(
                "conversation is no longer eligible for recovery; history is retained".into(),
            ))
        }
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
            .thread_resume_with_config(thread_id, config.to_wire_config())
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
