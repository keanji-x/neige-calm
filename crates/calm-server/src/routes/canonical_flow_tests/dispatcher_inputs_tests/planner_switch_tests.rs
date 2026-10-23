use super::*;
use crate::dispatcher::planner_admission::PlannerAdmission;
use crate::event::{EditAuthor, Event, EventScope};
use crate::ids::{ActorId, CardId};
use crate::test_seams::{PausePoint, install_pause_for_test};
use calm_truth::db::RepoEventWrite as _;

async fn event(flow: &Flow, body: &str) -> (Event, i64) {
    let event = Event::TrackReportEdited {
        track_id: flow.track.clone().into(),
        card_id: flow.card.clone().into(),
        author: EditAuthor::User,
        author_plugin_id: None,
        edit_id: body.into(),
        summary_before: String::new(),
        summary_after: String::new(),
        body_before: String::new(),
        body_after: body.into(),
        agent_message: None,
    };
    let id = flow
        .fixture
        .repo
        .log_pure_event(
            ActorId::KernelDispatcher,
            EventScope::Track {
                track: flow.track.clone().into(),
                area: flow.area.clone().into(),
            },
            None,
            &flow.state.events,
            &flow.state.card_role_cache,
            &flow.state.track_area_cache,
            event.clone(),
        )
        .await
        .unwrap();
    (event, id)
}

async fn changed(flow: &Flow, role: bool) {
    let mut tx = flow.fixture.repo.pool().begin().await.unwrap();
    if role {
        sqlx::query("UPDATE cards SET role='assistant' WHERE id=?1")
            .bind(&flow.card)
            .execute(&mut *tx)
            .await
            .unwrap();
    }
    sqlx::query("UPDATE tracks SET planner_revision=planner_revision+1 WHERE id=?1")
        .bind(&flow.track)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn snapshot(flow: &Flow) -> CanonicalHarnessSnapshot {
    let json: String =
        sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id=?1")
            .bind(&flow.worker)
            .fetch_one(flow.fixture.repo.pool())
            .await
            .unwrap();
    serde_json::from_str(&json).unwrap()
}

async fn finish(mut flow: Flow) {
    flow.state.dispatcher.retire();
    // Assertions are complete. Restore the original Human turn's role solely
    // so the protocol peer can finish and shut down its original live owner.
    sqlx::query("UPDATE cards SET role='planner' WHERE id=?1")
        .bind(&flow.card)
        .execute(flow.fixture.repo.pool())
        .await
        .unwrap();
    flow.end(TURN).await;
    flow.fixture
        .wait_phase(&flow.track, HarnessPhaseTag::TurnCompleted)
        .await;
    flow.close().await;
}

#[tokio::test]
async fn planner_switch_dispatcher_stale_role_read() {
    let flow = Flow::running().await;
    flow.state.dispatcher.abort_event_listener_for_test();
    let (_, id) = event(&flow, SYSTEM_BODY).await;
    let before = snapshot(&flow).await;
    let pause = PausePoint::default();
    install_pause_for_test("planner-reconcile-before-push", &flow.card, pause.clone());
    let dispatcher = flow.state.dispatcher.clone();
    let reconcile = tokio::spawn(async move { dispatcher.reconcile_tick_for_test().await });
    tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
        .await
        .unwrap();
    // SQL simulates the committed switch while the cache still advertises Planner.
    changed(&flow, true).await;
    pause.release.notify_one();
    reconcile.await.unwrap();
    let after = snapshot(&flow).await;
    assert!(id > before.header.push_watermark);
    assert_eq!(after.canonical_inputs, before.canonical_inputs);
    assert_eq!(after.header.push_watermark, before.header.push_watermark);
    finish(flow).await;
}

#[tokio::test]
async fn planner_switch_dispatcher_baseline() {
    let flow = Flow::running().await;
    flow.state.dispatcher.abort_event_listener_for_test();
    let (old, baseline) = event(&flow, "before new tenure").await;
    let before = snapshot(&flow).await;
    sqlx::query("UPDATE tracks SET planner_revision=1,planner_event_baseline=?1 WHERE id=?2")
        .bind(baseline)
        .bind(&flow.track)
        .execute(flow.fixture.repo.pool())
        .await
        .unwrap();
    flow.state
        .dispatcher
        .catch_up_push(flow.track.clone().into(), old, baseline)
        .await;
    assert_eq!(snapshot(&flow).await, before);
    flow.state.dispatcher.reconcile_tick_for_test().await;
    assert_eq!(snapshot(&flow).await, before);
    let (new, id) = event(&flow, "after new tenure").await;
    flow.state
        .dispatcher
        .catch_up_push(flow.track.clone().into(), new, id)
        .await;
    let after = snapshot(&flow).await;
    let ids: Vec<_> = after
        .canonical_inputs
        .pending
        .iter()
        .filter_map(PendingInput::envelope_id)
        .collect();
    assert_eq!(ids, [id]);
    assert_eq!(after.header.push_watermark, id);
    finish(flow).await;
}

#[tokio::test]
async fn planner_switch_dispatcher_accept_rechecks_revision() {
    for role_only in [false, true] {
        let flow = Flow::running().await;
        flow.state.dispatcher.abort_event_listener_for_test();
        let (event, id) = event(&flow, SYSTEM_BODY).await;
        let before = snapshot(&flow).await;
        let pause = PausePoint::default();
        install_pause_for_test("planner-events-before-accept", &flow.card, pause.clone());
        let dispatcher = flow.state.dispatcher.clone();
        let track = flow.track.clone();
        let push =
            tokio::spawn(async move { dispatcher.catch_up_push(track.into(), event, id).await });
        tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
            .await
            .unwrap();
        if role_only {
            sqlx::query("UPDATE cards SET role='assistant' WHERE id=?1")
                .bind(&flow.card)
                .execute(flow.fixture.repo.pool())
                .await
                .unwrap();
        } else {
            changed(&flow, false).await;
        }
        pause.release.notify_one();
        push.await.unwrap();
        assert_eq!(snapshot(&flow).await, before);
        finish(flow).await;
    }
}

#[tokio::test]
async fn planner_switch_dispatcher_old_wake_refused() {
    let flow = Flow::running().await;
    flow.report(SYSTEM_BODY).await;
    flow.state.dispatcher.abort_event_listener_for_test();
    let sources = flow
        .state
        .harness
        .canonical_sources(&flow.worker, &flow.track.clone().into());
    let source = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            for input in flow
                .fixture
                .carrier(&flow.track)
                .await
                .2
                .canonical_inputs
                .pending
            {
                if matches!(input, PendingInput::System { .. })
                    && let Some(source) = sources.source(&input.identity())
                {
                    return source;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let crate::conversation_inputs::InputSource::Dispatcher(source) = source else {
        panic!("Dispatcher source")
    };
    let mut tx = crate::db::sqlite::begin_immediate_tx(flow.fixture.repo.db())
        .await
        .unwrap();
    let carrier =
        crate::db::sqlite::conversation::current_input_content_carrier_tx(&mut tx, &flow.card)
            .await
            .unwrap();
    source
        .authorize_resource_tx(&mut tx, &carrier)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    sqlx::query("UPDATE cards SET role='assistant' WHERE id=?1")
        .bind(&flow.card)
        .execute(flow.fixture.repo.pool())
        .await
        .unwrap();
    let mut tx = crate::db::sqlite::begin_immediate_tx(flow.fixture.repo.db())
        .await
        .unwrap();
    assert!(
        source
            .authorize_resource_tx(&mut tx, &carrier)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    sqlx::query("UPDATE cards SET role='planner' WHERE id=?1")
        .bind(&flow.card)
        .execute(flow.fixture.repo.pool())
        .await
        .unwrap();
    // A queued source still holds its old carrier object after the pointer moves.
    sqlx::query("UPDATE cards SET session_id=NULL WHERE id=?1")
        .bind(&flow.card)
        .execute(flow.fixture.repo.pool())
        .await
        .unwrap();
    let mut tx = crate::db::sqlite::begin_immediate_tx(flow.fixture.repo.db())
        .await
        .unwrap();
    assert!(
        source
            .authorize_resource_tx(&mut tx, &carrier)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    sqlx::query("UPDATE cards SET session_id=?1 WHERE id=?2")
        .bind(&flow.worker)
        .bind(&flow.card)
        .execute(flow.fixture.repo.pool())
        .await
        .unwrap();
    changed(&flow, false).await;
    let mut tx = crate::db::sqlite::begin_immediate_tx(flow.fixture.repo.db())
        .await
        .unwrap();
    let carrier =
        crate::db::sqlite::conversation::current_input_content_carrier_tx(&mut tx, &flow.card)
            .await
            .unwrap();
    assert!(
        source
            .authorize_resource_tx(&mut tx, &carrier)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    let wake = crate::planner_hibernation::WakeContext::new(
        crate::state::RouteState::from_ref(&flow.state),
        flow.fixture.repo.clone(),
        crate::state::CodexShellState::from_ref(&flow.state),
    );
    let result = wake
        .wake_canonical(
            &flow.card.clone().into(),
            crate::conversation_inputs::InputSource::Dispatcher(source),
        )
        .await;
    assert!(
        matches!(result, Err(crate::error::CalmError::Forbidden(ref reason))
        if reason == "Dispatcher Planner tenure changed")
    );
    finish(flow).await;
}

#[tokio::test]
async fn planner_switch_dispatcher_reopen_rechecks_tenure() {
    let flow = Flow::running().await;
    let route = crate::state::RouteState::from_ref(&flow.state);
    let shell = crate::state::CodexShellState::from_ref(&flow.state);
    let wake =
        crate::planner_hibernation::WakeContext::new(route, flow.fixture.repo.clone(), shell);
    let target = PlannerAdmission::discover(flow.fixture.repo.as_ref(), &flow.track.clone().into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(target.card, CardId::from(flow.card.clone()));
    assert!(wake.ensure_planner_reopen(&target).await.unwrap().is_some());
    sqlx::query("UPDATE cards SET role='assistant' WHERE id=?1")
        .bind(&flow.card)
        .execute(flow.fixture.repo.pool())
        .await
        .unwrap();
    assert!(wake.ensure_planner_reopen(&target).await.unwrap().is_none());
    sqlx::query("UPDATE cards SET role='planner' WHERE id=?1")
        .bind(&flow.card)
        .execute(flow.fixture.repo.pool())
        .await
        .unwrap();
    let pause = PausePoint::default();
    install_pause_for_test("planner-reopen-before-fence", &flow.card, pause.clone());
    let waking = tokio::spawn(async move { wake.ensure_planner_reopen(&target).await });
    tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
        .await
        .unwrap();
    changed(&flow, false).await;
    pause.release.notify_one();
    assert!(waking.await.unwrap().unwrap().is_none());
    finish(flow).await;
}
