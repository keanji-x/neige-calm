//! Committed input setup; actual claim/dispatch, native peer and live source.
use super::*;
use crate::db::sqlite::session_set_handle_state_tx;
use crate::harness::run_loop::canonical::tests::Fixture;
use crate::shared_codex_appserver::native_owner_tests::{Peer, connect};

pub(super) struct RetryFixture {
    pub(super) fx: Fixture,
    pub(super) identity: CanonicalPendingIdentity,
    pub(super) predecessor: InputAttempt,
    pub(super) client: Arc<provider::codex::CodexAppServer>,
    pub(super) original_call: Value,
    pub(super) original_dispatch: tokio::task::JoinHandle<
        crate::error::Result<crate::harness::input_dispatch::CommittedInputReport>,
    >,
}
impl RetryFixture {
    pub(super) async fn new() -> (Self, Peer) {
        Self::with_system(false).await
    }

    pub(super) async fn with_system(system: bool) -> (Self, Peer) {
        let fx = Fixture::new().await;
        // Hold a real native response after its durable crossing marker. The
        // original execution is never fabricated as ended or redispatched.
        let inner = &fx.harness.inner;
        let mut tx = begin_immediate_tx(fx.original.repo.db()).await.unwrap();
        let mut carrier =
            current_input_carrier_tx(&mut tx, inner.card_id.as_str(), &inner.worker_session_id)
                .await
                .unwrap();
        let original_execution = if system {
            // Seed a retained mixed producer plan through canonical storage. Its
            // original source is not recreated; only the new Human repeat is live.
            let human_plan = pending_plan_for_prefix_tx(
                &mut tx,
                &carrier,
                std::num::NonZeroU32::new(1).unwrap(),
                vec![],
            )
            .await
            .unwrap();
            let (execution, _) = crate::conversation_inputs::authorize_execution_tx(
                &mut tx,
                &fx.source,
                &carrier,
                &human_plan,
            )
            .await
            .unwrap();
            let mut frame: CanonicalHarnessSnapshot =
                serde_json::from_value(carrier.snapshot().clone()).unwrap();
            let observation_id = SystemObservationId::parse(&"a".repeat(32)).unwrap();
            crate::db::sqlite::conversation::allocate_presentation_tx(
                &mut tx,
                inner.card_id.as_str(),
                crate::db::sqlite::conversation::PresentationSource::SystemObservation(
                    &observation_id,
                ),
            )
            .await
            .unwrap();
            frame.canonical_inputs.pending.push(PendingInput::System {
                observation_id,
                observation: SystemObservation::try_from(
                    calm_types::observation::Observation::SystemContext {
                        text: "frozen System provenance".into(),
                    },
                )
                .unwrap(),
                queue_position: 1,
                queued_at_ms: 1,
                envelope_id: None,
                addressable: true,
            });
            frame.canonical_inputs.next_queue_position = 2;
            session_set_handle_state_tx(
                &mut tx,
                &inner.worker_session_id,
                Some(serde_json::to_value(frame).unwrap()),
            )
            .await
            .unwrap();
            carrier =
                current_input_carrier_tx(&mut tx, inner.card_id.as_str(), &inner.worker_session_id)
                    .await
                    .unwrap();
            Some(execution)
        } else {
            None
        };
        let plan = pending_plan_for_prefix_tx(
            &mut tx,
            &carrier,
            std::num::NonZeroU32::new(if system { 2 } else { 1 }).unwrap(),
            vec![
                SystemRenderContext::new(
                    HarnessInputPresentation::System,
                    "frozen retry context".into(),
                )
                .unwrap(),
            ],
        )
        .await
        .unwrap();
        let claimed = match original_execution {
            None => crate::conversation_inputs::claim_initial_tx(
                &mut tx,
                &fx.source,
                &mut carrier,
                plan,
            )
            .await
            .unwrap(),
            Some(execution) => {
                let claimed = crate::db::sqlite::conversation::claim_initial_submission_tx(
                    &mut tx,
                    &mut carrier,
                    plan,
                    &execution,
                )
                .await
                .unwrap();
                let thread = carrier.snapshot()["last_thread_id"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                crate::db::sqlite::conversation::begin_input_attempt_tx(
                    &mut tx,
                    &mut carrier,
                    InputAttempt {
                        submission_id: claimed.submission_id.clone(),
                        attempt_id: InputAttemptId::parse(&crate::model::new_id()).unwrap(),
                    },
                    InputCorrelationFact {
                        provider: calm_types::runtime::AgentProvider::Codex,
                        thread_id: thread,
                        native_session_id: None,
                        native_turn_id: None,
                        local_turn_id: None,
                        rpc_request_id: None,
                    },
                    FrozenInputInvocation {
                        model: Some("frozen-retry-model".into()),
                        effort: Some("high".into()),
                        permission: PlannerPermissionMode::Never,
                    },
                )
                .await
                .unwrap();
                claimed
            }
        };
        tx.commit().await.unwrap();
        let (client, mut peer) = connect(&fx.daemon).await;
        let original_dispatch = if system {
            tokio::spawn(std::future::pending())
        } else {
            tokio::spawn({
                let (repo, source, card, runtime, backend, submission) = (
                    fx.original.repo.clone(),
                    fx.source.clone(),
                    inner.card_id.to_string(),
                    inner.worker_session_id.clone(),
                    inner.backend.clone(),
                    claimed.submission_id,
                );
                async move {
                    crate::harness::input_dispatch::initial(
                        repo.as_ref(),
                        source,
                        &card,
                        &runtime,
                        &submission,
                        &backend,
                        &TurnModelSelection {
                            model: Some("frozen-retry-model".into()),
                            effort: Some("high".into()),
                        },
                        PlannerPermissionMode::Never,
                        &backend.handoff_for_test().await,
                    )
                    .await
                }
            })
        };
        let original_call = if system {
            Value::Null
        } else {
            tokio::time::timeout(Duration::from_secs(5), request(&mut peer))
                .await
                .unwrap()
        };
        if !system {
            assert_eq!(original_call["method"], "turn/start");
        }
        let mut tx = begin_immediate_tx(fx.original.repo.db()).await.unwrap();
        let carrier =
            current_input_carrier_tx(&mut tx, inner.card_id.as_str(), &inner.worker_session_id)
                .await
                .unwrap();
        let predecessor = carrier.checkpoint().attempts[0].attempt.clone();
        // Install only the typed committed result for this layer's driver tests.
        // No availability/replay/claim/release reducer is duplicated here.
        let mut frame: CanonicalHarnessSnapshot =
            serde_json::from_value(carrier.snapshot().clone()).unwrap();
        let retry_intent_id = RetryIntentId::parse(&crate::model::new_id()).unwrap();
        let position = frame.canonical_inputs.next_queue_position;
        let queued_at_ms = crate::model::now_ms();
        frame.canonical_inputs.next_queue_position += 1;
        frame
            .canonical_inputs
            .pending
            .push(PendingInput::ExplicitSuccessor {
                retry_intent_id: retry_intent_id.clone(),
                queue_position: position,
                queued_at_ms,
            });
        frame.canonical_inputs.attempts[0].revision = 1;
        frame
            .canonical_inputs
            .receipts
            .push(HumanInputResolutionReceipt {
                key: "internal-driver-intent".into(),
                principal: LEGACY_OWNER_USER_ID.into(),
                fingerprint: "f".repeat(64),
                decision: HumanInputResolutionDecision::Retry {
                    predecessor: KeepConversationInputRequest {
                        carrier_id: inner.worker_session_id.clone(),
                        submission_id: predecessor.submission_id.as_str().into(),
                        attempt_id: predecessor.attempt_id.as_str().into(),
                        revision: "0".into(),
                    },
                    retry_intent_id: retry_intent_id.clone(),
                    revision: "1".into(),
                    waiting_for_settlement: true,
                },
            });
        session_set_handle_state_tx(
            &mut tx,
            &inner.worker_session_id,
            Some(serde_json::to_value(&frame).unwrap()),
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO conversation_input_retry_relations(retry_intent_id,card_id,carrier_id,
        predecessor_submission_id,root_submission_id,predecessor_attempt_id,resolution_key,
        principal_user_id,fingerprint,queue_position,created_at_ms) VALUES(?1,?2,?3,?4,?4,?5,
        'internal-driver-intent',?6,?7,?8,?9)",
        )
        .bind(retry_intent_id.as_str())
        .bind(inner.card_id.as_str())
        .bind(&inner.worker_session_id)
        .bind(predecessor.submission_id.as_str())
        .bind(predecessor.attempt_id.as_str())
        .bind(LEGACY_OWNER_USER_ID)
        .bind("f".repeat(64))
        .bind(position as i64)
        .bind(queued_at_ms)
        .execute(&mut **tx)
        .await
        .unwrap();
        sqlx::query("UPDATE conversation_submission_inputs SET effective=0,retirement_reason='human_resolution',
        retired_by_retry_intent_id=?1 WHERE submission_id=?2")
            .bind(retry_intent_id.as_str()).bind(predecessor.submission_id.as_str()).execute(&mut **tx).await.unwrap();
        tx.commit().await.unwrap();
        (
            Self {
                fx,
                identity: CanonicalPendingIdentity::ExplicitSuccessor { retry_intent_id },
                predecessor,
                client,
                original_call,
                original_dispatch,
            },
            peer,
        )
    }
    pub(super) async fn attach(&self) {
        self.fx
            .harness
            .observe_canonical_source(self.identity.clone(), self.fx.source.clone())
            .await
            .unwrap();
    }
    pub(super) async fn read_activity(&self, peer: &mut Peer, status: &str) {
        let call = tokio::time::timeout(Duration::from_secs(5), request(peer))
            .await
            .unwrap();
        assert_eq!(
            call["method"], "thread/read",
            "retry must not sample config/model/context"
        );
        let thread = self.fx.frame().await.header.last_thread_id.unwrap();
        assert_eq!(call["params"]["threadId"], thread);
        send(peer, json!({"id":call["id"],"result":{"thread":{"id":thread,"status":{"type":status},"turns":[]}}})).await;
    }
    pub(super) async fn start(&self, peer: &mut Peer) -> Value {
        // Before claim and again before native send, use the exact thread owner.
        self.read_activity(peer, "idle").await;
        self.read_activity(peer, "idle").await;
        let call = tokio::time::timeout(Duration::from_secs(5), request(peer))
            .await
            .unwrap();
        assert_eq!(call["method"], "turn/start");
        assert_eq!(call["params"]["model"], "frozen-retry-model");
        let crossing = self.fx.frame().await;
        assert_eq!(crossing.canonical_inputs.attempts.len(), 2);
        assert_ne!(
            crossing.canonical_inputs.attempts[1].attempt,
            self.predecessor
        );
        assert_eq!(
            crossing.canonical_inputs.attempts[1].invocation,
            InputInvocation::Recorded {
                configuration: FrozenInputInvocation {
                    model: Some("frozen-retry-model".into()),
                    effort: Some("high".into()),
                    permission: PlannerPermissionMode::Never
                }
            }
        );
        assert!(
            call["params"]["input"]
                .to_string()
                .contains("frozen retry context")
        );
        assert!(
            call["params"]["input"]
                .to_string()
                .contains("canonical scheduler human body")
        );
        call
    }
}

impl Drop for RetryFixture {
    fn drop(&mut self) {
        self.original_dispatch.abort();
    }
}
