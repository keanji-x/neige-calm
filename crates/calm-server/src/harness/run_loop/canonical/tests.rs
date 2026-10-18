//! Exercise the actual scheduler with an established backend. Native activation
//! is deliberately separate; these fixtures do not pretend to authorize start.
use super::*;
use crate::auth::HumanInputSource;
use crate::db::prelude::*;
use crate::db::sqlite::conversation::human_plan_for_pending_prefix_tx;
use crate::db::sqlite::{begin_immediate_tx, session_set_handle_state_tx};
use crate::harness::run_loop::completed_commit_tests::Fixture as ExistingRuntime;
use crate::shared_codex_appserver::native_owner_tests::{connect, notify_thread, request, send};
use calm_types::conversation::MessageRevisionRef;
use calm_types::conversation::{HumanPayload, InputDeliveryFact};
use serde_json::json;

mod content_tests;
mod control_tests;
mod selection_authority_tests;

struct Fixture {
    original: ExistingRuntime,
    harness: PlannerHarness,
    source: HumanInputSource,
    message: MessageRevisionRef,
    auth: crate::auth::AuthState,
    session: String,
    daemon: Arc<crate::shared_codex_appserver::SharedCodexAppServer>,
    registry: crate::harness::registry::HarnessRegistry,
}

impl Fixture {
    async fn new() -> Self {
        Self::with_config(HarnessConfig::default()).await
    }

    async fn with_config(config: HarnessConfig) -> Self {
        let original = ExistingRuntime::new().await;
        let inner = &original.harness.inner;
        let auth = crate::auth::AuthState::new(crate::auth::AuthConfig {
            username: Some("Owner".into()),
            password: Some("fixture-password".into()),
            dev_autologin: false,
            display_name: "Owner".into(),
        });
        let session = auth.sessions.create(
            crate::auth::SessionIdentity::owner(&auth.config),
            crate::auth::SessionAuthority::PasswordLogin,
        );
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            format!("{}={session}", crate::auth::SESSION_COOKIE)
                .parse()
                .unwrap(),
        );
        let (status, source) = crate::auth::authenticated_source(auth.clone(), headers).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        let source = source.unwrap();
        let mut snapshot = CanonicalHarnessSnapshot::dormant(0);
        snapshot.header.last_thread_id = inner.thread_id.read().await.clone();
        let mut tx = begin_immediate_tx(original.repo.pool()).await.unwrap();
        assert!(
            session_set_handle_state_tx(
                &mut tx,
                &inner.worker_session_id,
                Some(serde_json::to_value(&snapshot).unwrap())
            )
            .await
            .unwrap()
        );
        let mut carrier =
            current_input_carrier_tx(&mut tx, inner.card_id.as_str(), &inner.worker_session_id)
                .await
                .unwrap();
        let accepted = crate::conversation_inputs::accept_human_tx(
            &mut tx,
            &source.current_principal().unwrap(),
            &mut carrier,
            HumanPayload {
                text: "canonical scheduler human body".into(),
                bound_attachments: vec![],
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let AcceptedInputRef::Human { message } = accepted.input_ref else {
            panic!("human")
        };
        // The old queue fixture uses the fake turn/start shortcut. This test
        // needs the real transport and native receipt owner on its actual row.
        let daemon =
            crate::shared_codex_appserver::SharedCodexAppServer::new_stub(original.repo.clone());
        let actual_card = original
            .repo
            .card_get(inner.card_id.as_str())
            .await
            .unwrap()
            .unwrap();
        let backend = PlannerBackend::codex(
            daemon.clone(),
            actual_card.id.as_str(),
            actual_card.track_id.as_str(),
            &inner.worker_session_id,
        )
        .unwrap();
        let registry = crate::harness::registry::HarnessRegistry::default();
        let harness = PlannerHarness::run(PlannerHarnessParams {
            worker_session_id: inner.worker_session_id.clone(),
            track_id: inner.track_id.clone(),
            card_id: inner.card_id.clone(),
            thread_id: inner.thread_id.read().await.clone(),
            repo: original.repo.clone(),
            events: inner.events.clone(),
            card_role_cache: inner.card_role_cache.clone(),
            track_area_cache: inner.track_area_cache.clone(),
            backend,
            registry: registry.clone(),
            config,
            snapshot: HarnessRuntimeSnapshot::decode(serde_json::to_value(snapshot).unwrap())
                .unwrap(),
        })
        .await
        .unwrap();
        assert!(
            registry
                .try_reserve(inner.worker_session_id.clone())
                .unwrap()
                .install(harness.clone())
        );
        Self {
            original,
            harness,
            source,
            message,
            auth,
            session,
            daemon,
            registry,
        }
    }

    async fn frame(&self) -> CanonicalHarnessSnapshot {
        let inner = &self.harness.inner;
        let row = self
            .original
            .repo
            .session_get(&calm_types::worker::WorkerSessionId(
                inner.worker_session_id.clone(),
            ))
            .await
            .unwrap()
            .unwrap();
        serde_json::from_value(row.handle_state_json.unwrap()).unwrap()
    }

    async fn wait_phase(&self, phase: HarnessPhaseTag) -> CanonicalHarnessSnapshot {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let frame = self.frame().await;
                if frame.header.phase == phase {
                    return frame;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap()
    }
}

#[tokio::test]
async fn real_loop_requires_live_source_before_claim_and_preserves_pending_head() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    let before = fx.frame().await;
    assert_eq!(before.header.phase, HarnessPhaseTag::Idle);
    assert_eq!(before.canonical_inputs.pending.len(), 1);
    assert!(before.canonical_inputs.active.is_none());
    assert!(before.canonical_inputs.attempts.is_empty());
    let mut feedback = None;
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            feedback = fx.harness.issuance_block().await;
            if feedback.as_deref() == Some("A current session must authorize this saved input.") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    fx.auth.sessions.remove(&fx.session).unwrap();
    assert!(
        fx.harness
            .observe_canonical_source(fx.message.clone(), fx.source.clone())
            .await
            .is_err()
    );
    assert_eq!(fx.frame().await, before);
    peer.close(None).await.unwrap();
    fx.harness.shutdown().await.unwrap();
    assert_eq!(
        feedback.as_deref(),
        Some("A current session must authorize this saved input.")
    );
}

#[tokio::test]
async fn real_loop_claims_dispatches_publishes_running_and_consumes_actual_native_end() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    fx.harness
        .observe_canonical_source(fx.message.clone(), fx.source.clone())
        .await
        .unwrap();
    let call = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let call = request(&mut peer).await;
            if call["method"] == "turn/start" {
                break call;
            }
            assert_eq!(call["method"], "config/read");
            send(&mut peer, json!({"id":call["id"],"result":{"config":{}}})).await;
        }
    })
    .await
    .unwrap();
    let crossing = fx.frame().await;
    assert_eq!(crossing.header.phase, HarnessPhaseTag::IssuingTurn);
    assert!(crossing.canonical_inputs.pending.is_empty());
    assert!(matches!(
        crossing.canonical_inputs.attempts[0].delivery,
        InputDeliveryFact::CrossingUnknown
    ));
    let row = fx
        .original
        .repo
        .session_get(&calm_types::worker::WorkerSessionId(
            fx.harness.inner.worker_session_id.clone(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.state,
        calm_types::worker::WorkerSessionState::TurnPending
    );
    assert!(
        !serde_json::to_string(&crossing)
            .unwrap()
            .contains("canonical scheduler human body")
    );
    let native_input = call["params"]["input"].to_string();
    assert!(native_input.contains("canonical scheduler human body"));
    let thread = call["params"]["threadId"].as_str().unwrap();
    notify_thread(
        &fx.daemon,
        &mut peer,
        thread,
        "turn/started",
        json!({"id":"actual-turn","status":"inProgress"}),
    )
    .await;
    send(
        &mut peer,
        json!({"id":call["id"],"result":{"turn":{"id":"actual-turn"}}}),
    )
    .await;
    let running = fx.wait_phase(HarnessPhaseTag::TurnRunning).await;
    assert_eq!(running.header.last_turn_id.as_deref(), Some("actual-turn"));
    assert!(matches!(
        running.canonical_inputs.attempts[0].delivery,
        InputDeliveryFact::Accepted { .. }
    ));
    notify_thread(
        &fx.daemon,
        &mut peer,
        thread,
        "turn/completed",
        json!({"id":"actual-turn","status":"completed"}),
    )
    .await;
    let done = fx.wait_phase(HarnessPhaseTag::TurnCompleted).await;
    assert!(done.canonical_inputs.active.is_none());
    assert_eq!(
        done.canonical_inputs.attempts,
        running.canonical_inputs.attempts
    );
    assert_eq!(
        fx.harness.runtime_header().await.unwrap().phase,
        HarnessPhaseTag::TurnCompleted
    );
    assert!(
        fx.harness.snapshot().await.is_err(),
        "canonical cannot expose an empty historical body queue"
    );
    let mut stopping = tokio::spawn({
        let handle = fx.harness.clone();
        async move { handle.shutdown().await }
    });
    tokio::select! {
        stopped=&mut stopping => stopped.unwrap().unwrap(),
        call=request(&mut peer) => {
            assert_eq!(call["method"],"turn/interrupt");
            send(&mut peer,json!({"id":call["id"],"result":{}})).await;
            stopping.await.unwrap().unwrap();
        }
    }
}

#[tokio::test]
async fn canonical_handle_refuses_historical_body_and_control_ingress_without_mutation() {
    let fx = Fixture::new().await;
    let before = fx.frame().await;
    assert!(
        fx.harness
            .observe(Observation::TrackGoal {
                text: "old machine body".into()
            })
            .is_err()
    );
    assert!(
        fx.harness
            .observe_user_message_durable("old body".into(), vec![], SendKey::unique_for_test())
            .await
            .is_err()
    );
    assert!(fx.harness.compact().await.is_err());
    assert!(fx.harness.interrupt("manual".into()).await.is_err());
    assert!(!fx.harness.hibernate_if_idle().await.unwrap());
    assert!(
        fx.harness
            .steer_pending_entry(
                QueueEntryId::from_wire("old-body-entry".into()),
                1,
                ActorId::User
            )
            .await
            .is_err()
    );
    assert!(
        fx.harness
            .replace_turn_durable(
                "old turn".into(),
                "replacement body".into(),
                vec![],
                SendKey::unique_for_test()
            )
            .await
            .is_err()
    );
    assert_eq!(fx.frame().await, before);
    fx.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn real_loop_unknown_report_keeps_issuing_claim_without_rebuffer_or_redispatch() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    fx.harness
        .observe_canonical_source(fx.message.clone(), fx.source.clone())
        .await
        .unwrap();
    let call = request(&mut peer).await;
    assert_eq!(call["method"], "turn/start");
    send(&mut peer,json!({"id":call["id"],"error":{"code":-32000,"message":"private diagnostic must not enter the frame"}})).await;
    let settled = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let current = fx.frame().await;
            if current
                .canonical_inputs
                .attempts
                .first()
                .is_some_and(|attempt| matches!(attempt.delivery, InputDeliveryFact::Unknown))
            {
                break current;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(settled.header.phase, HarnessPhaseTag::IssuingTurn);
    assert!(settled.canonical_inputs.pending.is_empty());
    assert!(settled.canonical_inputs.active.is_some());
    fx.harness
        .observe_canonical_source(fx.message.clone(), fx.source.clone())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(fx.frame().await, settled);
    assert!(
        !serde_json::to_string(&settled)
            .unwrap()
            .contains("private diagnostic")
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM conversation_input_submissions")
        .fetch_one(fx.original.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
    // No native turn was acknowledged, so cleanup must not fabricate an END.
    fx.harness.shutdown().await.unwrap();
}
