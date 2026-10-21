//! Retire only an unchanged, unaddressed native start after an authoritative
//! exact-thread idle read. This never creates END or changes delivery evidence.
use super::*;

impl NativeTurns {
    pub(in crate::shared_codex_appserver) fn admit_start(
        &self,
        client: &Arc<CodexAppServer>,
        thread: &str,
        ticket: &Arc<StartTicket>,
    ) -> Result<()> {
        let state = self.lock();
        let slot = state
            .slots
            .get(thread)
            .ok_or_else(|| CalmError::Conflict("native start slot changed".into()))?;
        let exact = state.is_current(&Arc::downgrade(client))
            && slot.pending.as_ref().is_some_and(|pending| {
                Arc::ptr_eq(&pending.ticket, ticket)
                    && pending.candidate.is_none()
                    && !pending.ambiguous
            });
        let require_settled = match &ticket.input {
            NativeInput::Legacy => false,
            NativeInput::Canonical(input) => input.require_settled,
        };
        if (require_settled
            && (!Arc::ptr_eq(&slot.activity, &ticket.activity)
                || !slot
                    .settled_activity
                    .as_ref()
                    .is_some_and(|activity| Arc::ptr_eq(activity, &slot.activity))))
            || !exact
            || slot.unresolved
            || slot.steering.is_some()
            || (matches!(&ticket.input, NativeInput::Canonical(_))
                && slot
                    .current
                    .as_ref()
                    .is_some_and(NativeExecution::needs_control))
        {
            return Err(CalmError::Conflict(
                "native source or exact-thread activity changed before send".into(),
            ));
        }
        Ok(())
    }

    pub(in crate::shared_codex_appserver) fn settlement_fence(
        &self,
        client: &Arc<CodexAppServer>,
        thread: &str,
    ) -> Result<SettlementFence> {
        let mut state = self.lock();
        let source = Arc::downgrade(client);
        if !state.is_current(&source) {
            return Err(CalmError::Conflict(
                "native settlement source changed".into(),
            ));
        }
        let slot = &*state.slots.entry(thread.into()).or_default();
        if slot.steering.is_some() {
            return Err(CalmError::Conflict(
                "native settlement has an in-flight steer".into(),
            ));
        }
        Ok(SettlementFence {
            source,
            thread: thread.into(),
            activity: slot.activity.clone(),
            pending: slot.pending.as_ref().map(|pending| pending.ticket.clone()),
            current: slot.current.as_ref().map(|current| current.turn.clone()),
            candidate: slot.pending.as_ref().and_then(|pending| {
                pending
                    .candidate
                    .as_ref()
                    .map(|candidate| candidate.turn.clone())
            }),
            ambiguous: slot
                .pending
                .as_ref()
                .is_some_and(|pending| pending.ambiguous),
            unresolved: slot.unresolved,
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
            return false;
        };
        let same_pending = match (&expected.pending, &slot.pending) {
            (None, None) => true,
            (Some(expected), Some(actual)) => Arc::ptr_eq(expected, &actual.ticket),
            (None, Some(_)) | (Some(_), None) => false,
        };
        if !Arc::ptr_eq(&slot.activity, &expected.activity)
            || slot.steering.is_some()
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
        if let Some(pending) = slot.pending.take() {
            for other in pending.other_candidates.into_values() {
                slot.retain_observed(other);
            }
            if let Some(mut candidate) = pending.candidate {
                candidate.retire_control_hint();
                if let Some(previous) = slot.current.replace(candidate) {
                    slot.retain_observed(previous);
                }
            }
        }
        if let Some(current) = &mut slot.current {
            current.retire_control_hint();
        }
        slot.unresolved = false;
        slot.unresolved_candidates.clear();
        slot.settled_activity = Some(slot.activity.clone());
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
    #[tokio::test]
    async fn canonical_retry_idle_witness_expires_before_claim_and_after_gate_wait() {
        use calm_types::conversation::*;
        let repo = Arc::new(
            crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
                .await
                .unwrap(),
        );
        let daemon = SharedCodexAppServer::new_stub(repo);
        let (client, _peer) = connect(&daemon).await;
        let owner = &daemon.native_turns;
        let old = owner
            .prepare_start(&client, "thread", NativeInput::Legacy)
            .unwrap();
        owner.accepted(&client, "thread", &old, "original");
        let frozen = SubmissionRenderPlan::V1 {
            segments: vec![SubmissionRenderSegment::Input {
                input_position: 0,
                input: SubmissionRenderInput::System {
                    observation_id: SystemObservationId::parse(&"a".repeat(32)).unwrap(),
                    payload: PrivateHistoryPayload::Present {
                        value: SystemObservation::try_from(
                            calm_types::observation::Observation::SystemContext {
                                text: "frozen".into(),
                            },
                        )
                        .unwrap(),
                    },
                },
            }],
        };
        let proof = SubmissionDispatchProof::from_stored_plan(
            InputAttempt {
                submission_id: InputSubmissionId::parse(&"b".repeat(32)).unwrap(),
                attempt_id: InputAttemptId::parse(&"c".repeat(32)).unwrap(),
            },
            &frozen,
            SubmissionRenderDigest::parse(&format!(
                "{:x}",
                <sha2::Sha256 as sha2::Digest>::digest(serde_json::to_vec(&frozen).unwrap())
            ))
            .unwrap(),
        )
        .unwrap();
        let input = |require_settled| {
            NativeInput::canonical(
                CodexHarnessBinding::new("card", "track", "runtime").unwrap(),
                &LogicalTurnId::parse(&"e".repeat(32)).unwrap(),
                &proof,
                require_settled,
            )
        };
        assert!(
            owner
                .prepare_start(&client, "thread", input(false))
                .is_err(),
            "canonical starts cannot displace a known live native execution"
        );
        let idle = owner.settlement_fence(&client, "thread").unwrap();
        assert!(owner.settle_unaddressed_start(&idle));
        assert!(owner.observe(
            &Arc::downgrade(&client),
            &Notification::TurnStarted {
                thread_id: "thread".into(),
                turn: serde_json::json!({"id":"original"})
            }
        ));
        assert!(
            owner.prepare_start(&client, "thread", input(true)).is_err(),
            "same-turn native facts invalidate an earlier idle witness"
        );
        let idle = owner.settlement_fence(&client, "thread").unwrap();
        assert!(owner.settle_unaddressed_start(&idle));
        let pending = owner.prepare_start(&client, "thread", input(true)).unwrap();
        assert!(owner.admit_start(&client, "thread", &pending).is_ok());
        assert!(owner.observe(
            &Arc::downgrade(&client),
            &Notification::TurnStarted {
                thread_id: "thread".into(),
                turn: serde_json::json!({"id":"original"})
            }
        ));
        assert!(
            owner.admit_start(&client, "thread", &pending).is_err(),
            "the final gate must re-check after all source/sink waits"
        );
    }
}
