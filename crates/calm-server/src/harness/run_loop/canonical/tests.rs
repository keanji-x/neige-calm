#![cfg(test)]
//! Exercise the actual scheduler with an established backend. Native activation
//! is deliberately separate; these fixtures do not pretend to authorize start.
use super::*;
use crate::auth::HumanInputSource;
use crate::db::prelude::*;
use crate::db::sqlite::conversation::human_plan_for_pending_prefix_tx;
use crate::db::sqlite::{begin_immediate_tx, session_set_handle_state_tx};
use crate::harness::run_loop::completed_commit_tests::Fixture as ExistingRuntime;
use crate::shared_codex_appserver::native_owner_tests::{connect, notify_thread, request, send};
use calm_truth::db::RepoRead;
use calm_types::conversation::MessageRevisionRef;
use calm_types::conversation::{AcceptedInputRef, HumanPayload, InputDeliveryFact};
use serde_json::json;

mod compact_tests;
mod content_tests;
mod control_tests;
mod objective_tests;
mod phase_diagnostics;
mod planner_compact_tests;
use phase_diagnostics::PhaseWaitStage;
mod selection_authority_tests;

pub(super) struct Fixture {
    pub(super) original: ExistingRuntime,
    pub(super) harness: PlannerHarness,
    pub(super) source: HumanInputSource,
    pub(super) message: MessageRevisionRef,
    pub(super) auth: crate::auth::AuthState,
    pub(super) session: String,
    pub(super) daemon: Arc<crate::shared_codex_appserver::SharedCodexAppServer>,
    pub(super) registry: crate::harness::registry::HarnessRegistry,
}

impl Fixture {
    pub(super) async fn new() -> Self {
        Self::with_config(HarnessConfig::default()).await
    }

    async fn with_config(config: HarnessConfig) -> Self {
        Self::with_usage(config, None, false).await
    }

    async fn with_usage(
        config: HarnessConfig,
        usage: Option<TokenUsage>,
        invalidated: bool,
    ) -> Self {
        Self::with_source(config, usage, invalidated, None).await
    }

    async fn with_source(
        config: HarnessConfig,
        usage: Option<TokenUsage>,
        invalidated: bool,
        shared: Option<Arc<crate::shared_codex_appserver::SharedCodexAppServer>>,
    ) -> Self {
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
        let thread = format!("thread-{}", uuid::Uuid::new_v4());
        *inner.thread_id.write().await = Some(thread.clone());
        sqlx::query("UPDATE worker_sessions SET thread_id=?1 WHERE id=?2")
            .bind(&thread)
            .bind(&inner.worker_session_id)
            .execute(original.repo.db().writer_pool())
            .await
            .unwrap();
        snapshot.header.last_thread_id = Some(thread);
        snapshot.header.token_usage = usage;
        snapshot.header.context_usage_invalidated = invalidated;
        let mut tx = begin_immediate_tx(original.repo.db()).await.unwrap();
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
        let daemon = shared.unwrap_or_else(|| {
            crate::shared_codex_appserver::SharedCodexAppServer::new_stub(original.repo.clone())
        });
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
        let application = crate::auth::ValidatedApplicationAuth::from_state(auth.clone()).unwrap();
        registry
            .activate_maintenance_execution(application.execution)
            .unwrap();
        let harness = PlannerHarness::run(PlannerHarnessParams {
            track_delete_locks: Default::default(),
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

    pub(super) async fn frame(&self) -> CanonicalHarnessSnapshot {
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

    pub(super) async fn wait_phase(&self, phase: HarnessPhaseTag) -> CanonicalHarnessSnapshot {
        self.wait_phase_at(phase, PhaseWaitStage::Unspecified).await
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
    let track_guard = crate::per_card_lock::lock_key(
        &fx.harness.inner.track_delete_locks,
        fx.harness.inner.track_id.as_str(),
    )
    .await;
    fx.harness
        .observe_canonical_source(fx.message.clone(), fx.source.clone())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), request(&mut peer))
            .await
            .is_err(),
        "worktree clean's track fence must block canonical native reads and claim"
    );
    let waiting = fx.frame().await;
    assert_eq!(waiting.header.phase, HarnessPhaseTag::Idle);
    assert_eq!(waiting.canonical_inputs.pending.len(), 1);
    assert!(waiting.canonical_inputs.active.is_none());
    drop(track_guard);
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
    let released = crate::per_card_lock::try_lock_key(
        &fx.harness.inner.track_delete_locks,
        fx.harness.inner.track_id.as_str(),
    )
    .expect("durable pending claim releases the track fence before native reply");
    drop(released);
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
    send(&mut peer,json!({"id":call["id"],"error":{"code":"malformed","message":"private diagnostic must not enter the frame"}})).await;
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

#[tokio::test]
async fn context_usage_canonical_pre_feature_claim_is_sent_unchanged() {
    assert_resumed_context_usage(None).await;
}

#[tokio::test]
async fn context_usage_canonical_resumed_claim_replays_identical_text() {
    assert_resumed_context_usage(Some("[上下文] 50%（142000/272000 tokens）")).await;
}

async fn assert_resumed_context_usage(usage: Option<&str>) {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    let mut contexts = vec![
        SystemRenderContext::new(
            HarnessInputPresentation::SystemPlannerTurnBudget,
            "[回合预算] 30 分钟，剩 300 秒时收尾".into(), // Historical persisted plan.
        )
        .unwrap(),
    ];
    if let Some(text) = usage {
        contexts.push(
            SystemRenderContext::new(
                HarnessInputPresentation::SystemPlannerContextUsage,
                text.into(),
            )
            .unwrap(),
        );
    }
    // Persist a composed claim before the scheduler has a live source. Resume
    // must hydrate this immutable plan without additions or a fresh reading.
    let mut tx = begin_immediate_tx(fx.original.repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(
        &mut tx,
        fx.harness.inner.card_id.as_str(),
        &fx.harness.inner.worker_session_id,
    )
    .await
    .unwrap();
    let plan = human_plan_for_pending_prefix_tx(
        &mut tx,
        &carrier,
        std::num::NonZeroU32::new(1).unwrap(),
        contexts,
    )
    .await
    .unwrap();
    let expected = crate::harness::input_dispatch::hydrate_for_test(
        &mut tx,
        fx.harness.inner.card_id.as_str(),
        &plan,
    )
    .await
    .unwrap();
    let claim =
        crate::conversation_inputs::claim_initial_tx(&mut tx, &fx.source, &mut carrier, plan)
            .await
            .unwrap();
    let mut snapshot: CanonicalHarnessSnapshot =
        serde_json::from_value(carrier.snapshot().clone()).unwrap();
    snapshot.header.turn_budget = budget::FrozenBudget::Planner {
        budget: crate::planner_turn_budget::PlannerTurnBudget::default(),
        start_attempt: QueueEntryId::from_wire(claim.submission_id.as_str().into()),
        warning_attempted: false,
        context_usage: calm_types::harness::budget::FrozenContextUsage::RenderPlan,
    };
    let mut header = header_value(&snapshot).unwrap();
    if usage.is_none() {
        header["turn_budget"]
            .as_object_mut()
            .unwrap()
            .remove("context_usage");
    }
    persist_runtime_frame_tx(
        &mut tx,
        fx.harness.inner.card_id.as_str(),
        &fx.harness.inner.worker_session_id,
        &RuntimeExecutionFence::from_carrier(&carrier),
        header,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    fx.harness
        .observe_canonical_source(fx.message.clone(), fx.source.clone())
        .await
        .unwrap();
    let call = request(&mut peer).await;
    assert_eq!(call["method"], "turn/start");
    let [InputItem::Text { text: expected }] = expected.as_slice() else {
        panic!("one text input")
    };
    assert_eq!(call["params"]["input"][0]["text"], *expected);
    assert_eq!(call["params"]["input"].as_array().unwrap().len(), 1);
    let input = call["params"]["input"].to_string();
    assert_eq!(
        input.matches("[上下文]").count(),
        usize::from(usage.is_some())
    );
    assert!(!input.contains("not recorded"));
    let mut tx = begin_immediate_tx(fx.original.repo.db()).await.unwrap();
    let stored = crate::db::sqlite::conversation::submission_plan_tx(&mut tx, &claim.submission_id)
        .await
        .unwrap();
    assert_eq!(stored.plan, claim.plan);
    tx.rollback().await.unwrap();
    send(
        &mut peer,
        json!({"id":call["id"],"error":{"code":-32000,"message":"fixture rejected"}}),
    )
    .await;
    peer.close(None).await.unwrap();
    fx.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn context_usage_canonical_freezes_context_at_claim() {
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    fx.harness
        .observe_canonical_source(fx.message.clone(), fx.source.clone())
        .await
        .unwrap();
    let call = request(&mut peer).await;
    assert_eq!(call["method"], "turn/start");
    let input = call["params"]["input"][0]["text"].as_str().unwrap();
    assert!(!input.contains("[回合预算]"));
    assert_eq!(input.matches("[上下文]").count(), 1);
    assert!(input.contains("[上下文] 未知（no reading yet）"), "{input}");
    let frozen = fx.frame().await;
    let wire = serde_json::to_string(&frozen).unwrap();
    assert_eq!(
        serde_json::to_value(&frozen).unwrap()["turn_budget"]["context_usage"],
        json!({"kind":"render_plan"})
    );
    // Render data belongs to the submission, while the runtime snapshot stays body-free.
    let submission = &frozen
        .canonical_inputs
        .active
        .as_ref()
        .unwrap()
        .submission_ids[0];
    let mut tx = begin_immediate_tx(fx.original.repo.db()).await.unwrap();
    let stored = crate::db::sqlite::conversation::submission_plan_tx(&mut tx, submission)
        .await
        .unwrap();
    let before = crate::harness::input_dispatch::hydrate_for_test(
        &mut tx,
        fx.harness.inner.card_id.as_str(),
        &stored.plan,
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    let [InputItem::Text { text: before_text }] = before.as_slice() else {
        panic!("one text input")
    };
    assert_eq!(before_text, input);
    let thread = call["params"]["threadId"].as_str().unwrap();
    notify_thread(
        &fx.daemon,
        &mut peer,
        thread,
        "turn/started",
        json!({"id":"usage-turn"}),
    )
    .await;
    send(
        &mut peer,
        json!({"id":call["id"],"result":{"turn":{"id":"usage-turn"}}}),
    )
    .await;
    fx.wait_phase(HarnessPhaseTag::TurnRunning).await;
    send(&mut peer, json!({"method":"thread/tokenUsage/updated","params":{
        "threadId":thread,"turnId":"usage-turn",
        "tokenUsage":{"last":{"totalTokens":142000},"total":{"totalTokens":9999999},"modelContextWindow":272000}
    }})).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if fx
                .frame()
                .await
                .header
                .token_usage
                .as_ref()
                .is_some_and(|u| u.used_tokens == 142000)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let restored: CanonicalHarnessSnapshot = serde_json::from_str(&wire).unwrap();
    assert_eq!(restored, frozen);
    let mut tx = begin_immediate_tx(fx.original.repo.db()).await.unwrap();
    let stored = crate::db::sqlite::conversation::submission_plan_tx(&mut tx, submission)
        .await
        .unwrap();
    let after = crate::harness::input_dispatch::hydrate_for_test(
        &mut tx,
        fx.harness.inner.card_id.as_str(),
        &stored.plan,
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    let [InputItem::Text { text: after_text }] = after.as_slice() else {
        panic!("one text input")
    };
    assert_eq!(after_text, before_text);
    notify_thread(
        &fx.daemon,
        &mut peer,
        thread,
        "turn/completed",
        json!({"id":"usage-turn","status":"completed"}),
    )
    .await;
    fx.wait_phase(HarnessPhaseTag::TurnCompleted).await;
    peer.close(None).await.unwrap();
    fx.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn context_usage_canonical_known_and_invalidated_readings() {
    for (used_tokens, context_window, invalidated, expected) in [
        (-1, Some(258400), false, "[上下文] 未知（invalid reading）"),
        (
            i64::MIN,
            Some(258400),
            false,
            "[上下文] 未知（invalid reading）",
        ),
        (
            142000,
            Some(272000),
            false,
            "[上下文] 50%（142000/272000 tokens）",
        ),
        (
            142000,
            Some(272000),
            true,
            "[上下文] 未知（reset by compaction）",
        ),
        (142000, None, false, "[上下文] 未知（window unknown）"),
        (
            272001,
            Some(272000),
            false,
            "[上下文] 未知（reading exceeds window）",
        ),
    ] {
        let fx = Fixture::with_usage(
            HarnessConfig::default(),
            Some(TokenUsage {
                used_tokens,
                total_tokens: 9999999,
                context_window,
                at_ms: 1,
            }),
            invalidated,
        )
        .await;
        let (_client, mut peer) = connect(&fx.daemon).await;
        fx.harness
            .observe_canonical_source(fx.message.clone(), fx.source.clone())
            .await
            .unwrap();
        let call = request(&mut peer).await;
        assert_eq!(call["method"], "turn/start");
        let input = call["params"]["input"][0]["text"].as_str().unwrap();
        assert!(input.contains(expected), "{input}");
        if invalidated
            || used_tokens < 0
            || context_window.is_none()
            || used_tokens > context_window.unwrap()
        {
            assert!(!input.contains("% used"), "{input}");
        }
        send(
            &mut peer,
            json!({"id":call["id"],"error":{"code":"malformed","message":"unknown"}}),
        )
        .await;
        peer.close(None).await.unwrap();
        fx.harness.shutdown().await.unwrap();
    }
}
