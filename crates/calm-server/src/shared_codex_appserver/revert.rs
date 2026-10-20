//! Native history-cut facts; the historical edit owner still decides whether to start.
use super::*;
use provider::codex::delivery::RevertDelivery;

impl SharedCodexAppServer {
    pub async fn thread_revert_delivery(
        &self,
        thread_id: &str,
        before_turn_id: &str,
    ) -> RevertDelivery<control::StartError> {
        if let Err(error) = self.require_authentication() {
            return RevertDelivery::NotWritten {
                error: error.into(),
            };
        }
        if self.thread_seals.is_sealed(thread_id) {
            return RevertDelivery::NotWritten {
                error: control::StartError::Host(CalmError::Conflict(format!(
                    "thread {thread_id} is sealed because its track is being deleted"
                ))),
            };
        }
        #[cfg(feature = "fixtures")]
        if let Some(fake) = &self.fake {
            fake.reverted_threads
                .lock()
                .expect("fake shared codex reverted threads mutex poisoned")
                .push((
                    thread_id.to_owned(),
                    before_turn_id.to_owned(),
                    fake.next_turn.load(Ordering::SeqCst).saturating_sub(1),
                ));
            if fake.revert_turn_not_found.load(Ordering::SeqCst) {
                return RevertDelivery::Unknown {
                    error: control::StartError::Refused(CalmError::CodexRefused(format!(
                        "thread/revert failed: turn not found: {before_turn_id} (code -32600)"
                    ))),
                };
            }
            let result = fake
                .revert_result
                .lock()
                .expect("fake revert result")
                .clone()
                .unwrap_or_else(|| serde_json::json!({"thread":{"id":thread_id}}));
            let receipt =
                match serde_json::from_value::<provider::codex::ThreadRevertResult>(result) {
                    Ok(receipt) => receipt,
                    Err(error) => {
                        return RevertDelivery::Unknown {
                            error: control::StartError::Host(error.into()),
                        };
                    }
                };
            if let Err(error) = receipt.confirm_thread(thread_id) {
                return RevertDelivery::Unknown {
                    error: error.into(),
                };
            }
            self.native_turns
                .retire_if(thread_id, |_, active| active == before_turn_id);
            return RevertDelivery::NativeAck;
        }
        let client = match self.authenticated_client().await {
            Ok(client) => client,
            Err(error) => {
                return RevertDelivery::NotWritten {
                    error: error.into(),
                };
            }
        };
        match client
            .thread_revert_delivery(thread_id, before_turn_id)
            .await
        {
            RevertDelivery::NativeAck => {
                self.native_turns.retire_from_source(
                    &Arc::downgrade(&client),
                    thread_id,
                    before_turn_id,
                );
                RevertDelivery::NativeAck
            }
            RevertDelivery::NotWritten { error } => RevertDelivery::NotWritten {
                error: error.into(),
            },
            RevertDelivery::Unknown { error } => RevertDelivery::Unknown {
                error: error.into(),
            },
        }
    }
}
