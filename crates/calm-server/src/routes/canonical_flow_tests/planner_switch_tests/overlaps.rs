//! Deterministic ingress after the switch has parked both captured carriers.
use super::*;
use crate::test_seams::{PausePoint, install_pause_for_test};

#[tokio::test]
async fn planner_switch_native_binding_after_capture_refused() {
    let mut p = Pair::with_provider("claude").await;
    let (_, before) = assistant_canonical::carrier(&p.fixture, &p.cards[1]).await;
    let attempt = &before.canonical_inputs.attempts[0];
    post(
        &p.app,
        &p.cookie,
        &format!("/api/cards/{}/conversation/input/keep", p.cards[1]),
        "keep-before-overlap",
        json!({"carrier_id":p.workers[1],"submission_id":attempt.attempt.submission_id,
            "attempt_id":attempt.attempt.attempt_id,"revision":attempt.revision.to_string()}),
    )
    .await;
    claude_creation_tests::assistant::wait_frame(&p.fixture, &p.cards[1], |f| {
        f.idle_for_lifecycle() && f.canonical_inputs.referenced_submissions().is_empty()
    })
    .await;
    let pause = PausePoint::default();
    install_pause_for_test("planner-switch-before-commit", &p.cards[0], pause.clone());
    let route = RouteState::from_ref(&p.state);
    let human = p.human.clone();
    let switch_request = p.request();
    let switching = tokio::spawn(async move {
        planner_switch::switch_planner(&route, &Actor("user".into()), &human, switch_request).await
    });
    tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
        .await
        .unwrap();

    // Use the same attribution owner as native session registration, not a SQL
    // edit of agent_session_id. Preserve the existing native thread identity.
    let worker = p.workers[1].clone();
    let stored = p
        .fixture
        .repo
        .session_get(&worker.clone().into())
        .await
        .unwrap()
        .unwrap();
    let native = uuid::Uuid::new_v4().to_string();
    let registered = native.clone();
    crate::db::write_in_tx_typed(p.state.repo.as_ref(), move |tx| {
        Box::pin(async move {
            crate::db::sqlite::session_bind_attribution_tx(
                tx,
                &worker,
                crate::session_projection_repo::ThreadAttribution {
                    worker_session_id: worker.clone(),
                    provider: crate::session_projection_repo::AgentProvider::Claude,
                    thread_id: stored.thread_id,
                    session_id: Some(registered),
                    active_turn_id: stored.active_turn_id,
                },
            )
            .await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    let bound: Option<String> =
        sqlx::query_scalar("SELECT agent_session_id FROM worker_sessions WHERE id=?1")
            .bind(&p.workers[1])
            .fetch_one(p.fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(bound.as_deref(), Some(native.as_str()));
    pause.release.notify_one();

    let switch = async { switching.await.unwrap() };
    tokio::pin!(switch);
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            tokio::select! {
                result = &mut switch => break result,
                call = request(&mut p.peer) => {
                    assert!(matches!(call["method"].as_str(), Some("thread/unsubscribe" | "turn/interrupt" | "config/read")), "{call}");
                    send(&mut p.peer, json!({"id":call["id"],"result":{}})).await;
                }
            }
        }
    }).await.unwrap();
    assert_eq!(
        result.as_ref().err().and_then(code),
        Some("planner_changed"),
        "native registration succeeded; switch must refuse (switch succeeded: {})",
        result.is_ok()
    );
    p.unchanged().await;
    assert_eq!(http::detail(&p).await["planner_revision"], 0);
    p.close().await;
}
