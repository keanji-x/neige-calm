//! Kept unaddressed starts use the actual driver, HTTP keep, and canonical loop.
use super::*;
use crate::harness::backend::SettledActivity;
use crate::harness::native_admission::NativePromptAdmission;

fn admission() -> NativePromptAdmission<'static> {
    NativePromptAdmission::new(|| Ok(()), || Box::pin(async { Ok(()) }))
}
async fn settled(fx: &Fixture) -> bool {
    matches!(
        fx.backend
            .settled_activity(&fx.owner.card, &fx.owner.thread, &admission())
            .await
            .unwrap(),
        SettledActivity::Settled
    )
}
async fn finished(fx: &Fixture) {
    let target = fx.target().await;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if fx
                .backend
                .qualify_execution_end(&target)
                .await
                .unwrap()
                .is_some()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn canonical_claude_settled_exact_exit_and_binding_admission() {
    let fx = Fixture::new("exit").await;
    assert!(!settled(&fx).await, "absence is not settlement evidence");
    fx.dispatch().await.unwrap();
    finished(&fx).await;
    assert!(
        settled(&fx).await,
        "exact joined driver and cleanup qualify"
    );
    for (card, thread) in [
        ("other-card", fx.owner.thread.as_str()),
        (fx.owner.card.as_str(), "other-thread"),
    ] {
        assert!(matches!(
            fx.backend
                .settled_activity(card, thread, &admission())
                .await
                .unwrap(),
            SettledActivity::Held
        ));
    }
    let refused = NativePromptAdmission::new(
        || Err(crate::error::CalmError::Conflict("refused".into())),
        || Box::pin(async { Ok(()) }),
    );
    assert!(matches!(
        fx.backend
            .settled_activity(&fx.owner.card, &fx.owner.thread, &refused)
            .await
            .unwrap(),
        SettledActivity::Held
    ));
    sqlx::query(
        "UPDATE cards SET payload=json_set(payload,'$.planner_provider','codex') WHERE id=?1",
    )
    .bind(&fx.owner.card)
    .execute(fx.owner.repo.pool())
    .await
    .unwrap();
    assert!(!settled(&fx).await, "changed binding cannot qualify");
    assert_eq!(fx.session.user_line_writes_for_test(), 1);
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_settled_live_and_cleanup_pending_hold() {
    let fx = Fixture::new("stall").await;
    let pause = crate::claude_planner::session::SettlePause {
        entered: Arc::default(),
        release: Arc::default(),
    };
    fx.session
        .set_before_turn_completed_pause_for_test(pause.clone());
    fx.dispatch().await.unwrap();
    assert!(!settled(&fx).await, "live driver must hold");
    let target = fx.target().await;
    fx.backend
        .interrupt_execution(&target, &admission())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(25), pause.entered.notified())
        .await
        .unwrap();
    assert!(!settled(&fx).await, "driver cleanup still owns completion");
    pause.release.notify_one();
    finished(&fx).await;
    assert!(settled(&fx).await);
    fx.session.shutdown().await.unwrap();
}

async fn application(fx: &Fixture) -> (crate::state::AppState, axum::Router) {
    use crate::plugin_host::{PluginHost, PluginRegistry};
    use crate::state::{AppState, CodexClient, DaemonClient, WriteContext};
    let roles = crate::card_role_cache::CardRoleCache::new();
    roles.seed_from_db(fx.owner.repo.pool()).await.unwrap();
    let areas = crate::track_area_cache::TrackAreaCache::new();
    areas.seed_from_db(fx.owner.repo.pool()).await.unwrap();
    let events = crate::event::EventBus::new();
    let state = AppState::from_parts(
        fx.owner.repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            fx.owner.repo.clone(),
            fx.dir.path().join("plugins"),
            fx.dir.path().join("plugin-data"),
            vec![],
            events,
            WriteContext::new(roles.clone(), areas.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(roles),
        Some(areas),
    );
    let app = crate::routes::application_router(state.clone(), fx.auth.clone());
    (state, app)
}

#[tokio::test]
async fn canonical_claude_settled_keep_lost_report_dispatches_next_once() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let fx = Arc::new(Fixture::new("stall").await);
    let pause = crate::claude_planner::session::SettlePause {
        entered: Arc::default(),
        release: Arc::default(),
    };
    fx.session.hold_dispatch_result_for_test(pause.clone());
    let dispatch = tokio::spawn({
        let fx = fx.clone();
        async move { fx.dispatch().await }
    });
    tokio::time::timeout(Duration::from_secs(10), pause.entered.notified())
        .await
        .unwrap();
    dispatch.abort();
    let Err(cancelled) = dispatch.await else {
        panic!("dispatch must be cancelled")
    };
    assert!(cancelled.is_cancelled());
    pause.release.notify_one();
    let before = fx.checkpoint().await;
    let attempt = &before.attempts[0];
    assert!(attempt.correlation.local_turn_id.is_none());
    let harness = fx.start_loop().await;
    let (state, app) = application(&fx).await;
    assert!(
        state
            .harness
            .try_reserve(fx.owner.worker.clone())
            .unwrap()
            .install(harness.clone())
    );
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/cards/{}/conversation/input/keep",
                    fx.owner.card
                ))
                .header(
                    header::COOKIE,
                    format!("{}={}", crate::auth::SESSION_COOKIE, fx.login),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .header("Idempotency-Key", "keep")
                .body(Body::from(
                    serde_json::json!({"carrier_id":fx.owner.worker,
            "submission_id":attempt.attempt.submission_id,"attempt_id":attempt.attempt.attempt_id,
            "revision":attempt.revision.to_string()})
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["waiting_for_settlement"], true);
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
        .await
        .unwrap();
    let accepted = crate::conversation_inputs::accept_human_tx(
        &mut tx,
        &fx.source.current_principal().unwrap(),
        &mut carrier,
        HumanPayload {
            text: "following kept input".into(),
            bound_attachments: vec![],
        },
    )
    .await
    .unwrap();
    let AcceptedInputRef::Human { message } = accepted.input_ref else {
        panic!("human")
    };
    tx.commit().await.unwrap();
    harness.refresh_canonical_input().await.unwrap();
    harness
        .observe_canonical_source(message, fx.source.clone())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert_eq!(
        fx.session.user_line_writes_for_test(),
        1,
        "live kept driver permits no following write"
    );
    assert_eq!(fx.checkpoint().await.attempts.len(), 1);
    // End the exact local process through its existing signal owner. The lost
    // report has no addressable durable target, so construct no synthetic END.
    std::fs::write(fx.dir.path().join("scenario"), "exit").unwrap();
    let turn = fx
        .session
        .active_turn_id_for_thread(&fx.owner.thread)
        .unwrap();
    assert!(matches!(
        fx.session.turn_interrupt(&fx.owner.thread, &turn).await,
        provider::control::InterruptOutcome::LocalSignalDelivered
    ));
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if fx.session.user_line_writes_for_test() == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("settled kept driver releases following canonical input");
    fx.phase("turn_completed").await;
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert_eq!(fx.session.user_line_writes_for_test(), 2);
    assert_eq!(fx.checkpoint().await.attempts.len(), 2);
    let lines = std::fs::read_to_string(fx.dir.path().join("stdin")).unwrap();
    assert_eq!(lines.lines().count(), 1, "stall consumed no original line");
    assert!(lines.contains("following kept input"));
    harness.shutdown().await.unwrap();
    state.dispatcher.retire();
}

#[tokio::test]
async fn canonical_claude_settled_serializes_with_driver_start() {
    let fx = Arc::new(Fixture::new("exit").await);
    fx.dispatch().await.unwrap();
    finished(&fx).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let observation = tokio::spawn({
        let fx = fx.clone();
        let entered = entered.clone();
        let release = release.clone();
        async move {
            let admission = NativePromptAdmission::new(
                || Ok(()),
                move || {
                    let entered = entered.clone();
                    let release = release.clone();
                    Box::pin(async move {
                        entered.notify_one();
                        release.notified().await;
                        Ok(())
                    })
                },
            );
            fx.backend
                .settled_activity(&fx.owner.card, &fx.owner.thread, &admission)
                .await
                .unwrap()
        }
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let starting = tokio::spawn({
        let fx = fx.clone();
        async move {
            let proof = crate::harness::backend::dispatch_tests::proof();
            fx.backend
                .dispatch_input(
                    None,
                    &crate::harness::backend::dispatch_tests::logical_turn_id(),
                    proof.attempt(),
                    &fx.owner.thread,
                    &proof,
                    vec![InputItem::Text {
                        text: "concurrent start".into(),
                    }],
                    &TurnModelSelection::inherit(),
                    PlannerPermissionMode::Never,
                    &admission(),
                )
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!starting.is_finished());
    assert_eq!(fx.session.user_line_writes_for_test(), 1);
    release.notify_one();
    assert!(matches!(
        observation.await.unwrap(),
        SettledActivity::Settled
    ));
    assert!(starting.await.unwrap().outcome().is_ok());
    assert_eq!(fx.session.user_line_writes_for_test(), 2);
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_settled_failed_new_start_discards_prior_evidence() {
    let fx = Fixture::new("exit").await;
    fx.dispatch().await.unwrap();
    finished(&fx).await;
    assert!(settled(&fx).await);
    let worker = fx.owner.worker.clone();
    fx.session.set_after_spawn_hook_for_test(Arc::new(move || {
        crate::claude_planner::stop::fail_claude_planner_stop_for_test(&worker);
    }));
    let proof = crate::harness::backend::dispatch_tests::proof();
    let refused = NativePromptAdmission::new(
        || {
            Err(crate::error::CalmError::Conflict(
                "refused new start".into(),
            ))
        },
        || Box::pin(async { Ok(()) }),
    );
    let report = fx
        .backend
        .dispatch_input(
            None,
            &crate::harness::backend::dispatch_tests::logical_turn_id(),
            proof.attempt(),
            &fx.owner.thread,
            &proof,
            vec![InputItem::Text {
                text: "refused new start".into(),
            }],
            &TurnModelSelection::inherit(),
            PlannerPermissionMode::Never,
            &refused,
        )
        .await;
    crate::claude_planner::stop::clear_claude_planner_stop_failure_for_test(&fx.owner.worker);
    assert!(report.outcome().is_err());
    assert!(
        !settled(&fx).await,
        "old cleanup cannot certify a newer failed start"
    );
    fx.session.shutdown().await.unwrap();
}
