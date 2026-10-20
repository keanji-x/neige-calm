//! Retire only an unchanged, unaddressed native start after an authoritative
//! exact-thread idle read. This never creates END or changes delivery evidence.
use super::*;

impl NativeTurns {
    pub(in crate::shared_codex_appserver) fn settlement_fence(
        &self,
        client: &Arc<CodexAppServer>,
        thread: &str,
    ) -> Result<SettlementFence> {
        let state = self.lock();
        let source = Arc::downgrade(client);
        if !state.is_current(&source) {
            return Err(CalmError::Conflict(
                "native settlement source changed".into(),
            ));
        }
        let slot = state.slots.get(thread);
        if slot.is_some_and(|slot| slot.steering.is_some()) {
            return Err(CalmError::Conflict(
                "native settlement has an in-flight steer".into(),
            ));
        }
        Ok(SettlementFence {
            source,
            thread: thread.into(),
            pending: slot.and_then(|slot| slot.pending.as_ref().map(|p| p.ticket.clone())),
            current: slot.and_then(|slot| slot.current.as_ref().map(|c| c.turn.clone())),
            candidate: slot.and_then(|slot| {
                slot.pending
                    .as_ref()
                    .and_then(|p| p.candidate.as_ref().map(|c| c.turn.clone()))
            }),
            ambiguous: slot.is_some_and(|slot| slot.pending.as_ref().is_some_and(|p| p.ambiguous)),
            unresolved: slot.is_some_and(|slot| slot.unresolved),
        })
    }

    pub(in crate::shared_codex_appserver) fn settle_unaddressed_start(
        &self,
        expected: &SettlementFence,
    ) -> bool {
        let mut state = self.lock();
        if !state.is_current(&expected.source) {
            return false;
        }
        let Some(slot) = state.slots.get_mut(&expected.thread) else {
            return expected.pending.is_none()
                && expected.current.is_none()
                && !expected.unresolved;
        };
        let same_pending = match (&expected.pending, &slot.pending) {
            (None, None) => true,
            (Some(expected), Some(actual)) => Arc::ptr_eq(expected, &actual.ticket),
            (None, Some(_)) | (Some(_), None) => false,
        };
        if slot.steering.is_some()
            || !same_pending
            || slot.unresolved != expected.unresolved
            || slot.current.as_ref().map(|c| &c.turn) != expected.current.as_ref()
            || slot
                .pending
                .as_ref()
                .and_then(|p| p.candidate.as_ref().map(|c| &c.turn))
                != expected.candidate.as_ref()
            || slot.pending.as_ref().is_some_and(|p| p.ambiguous) != expected.ambiguous
        {
            return false;
        }
        // Keep an observed candidate as correlation, with no manufactured END
        // or Accepted association. Idle retires only its control/start blocker.
        if let Some(pending) = slot.pending.take()
            && let Some(mut candidate) = pending.candidate
        {
            candidate.retire_control_hint();
            slot.current = Some(candidate);
        }
        if let Some(current) = &mut slot.current {
            current.retire_control_hint();
        }
        slot.unresolved = false;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared_codex_appserver::native_owner_tests::connect;

    #[tokio::test]
    async fn canonical_exact_thread_settlement_fences_source_ticket_and_known_candidate() {
        let repo = Arc::new(
            crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
                .await
                .unwrap(),
        );
        let daemon = SharedCodexAppServer::new_stub(repo);
        let (client, _peer) = connect(&daemon).await;
        let owner = &daemon.native_turns;
        let first = owner
            .prepare_start(&client, "thread", NativeInput::Legacy)
            .unwrap();
        let stale = owner.settlement_fence(&client, "thread").unwrap();
        assert!(owner.not_started(&client, "thread", &first));
        let second = owner
            .prepare_start(&client, "thread", NativeInput::Legacy)
            .unwrap();
        assert!(!owner.settle_unaddressed_start(&stale));
        assert!(
            owner
                .prepare_start(&client, "thread", NativeInput::Legacy)
                .is_err()
        );
        let candidate_fence = owner.settlement_fence(&client, "thread").unwrap();
        assert!(owner.observe(
            &Arc::downgrade(&client),
            &Notification::TurnStarted {
                thread_id: "thread".into(),
                turn: serde_json::json!({"id":"known"}),
            }
        ));
        assert!(!owner.settle_unaddressed_start(&candidate_fence));
        assert!(owner.settlement_fence(&client, "thread").is_ok());
        assert!(!owner.not_started(&client, "thread", &second));
        let (_replacement, _other_peer) = connect(&daemon).await;
        assert!(!owner.settle_unaddressed_start(&stale));
        assert!(!owner.settle_unaddressed_start(&candidate_fence));
    }
}
