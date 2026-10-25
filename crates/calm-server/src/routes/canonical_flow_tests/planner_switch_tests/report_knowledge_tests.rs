//! Two real sessions on the same Report keep independent knowledge.
use super::*;
use crate::mcp_server::{registry::ConnectionIdentity, transport::dispatch_tools_call};
use calm_types::conversation::PendingInput;

async fn mcp(p: &Pair, thread: &str, name: &str, arguments: Value) {
    dispatch_tools_call(
        &p.state.mcp_context(),
        None,
        json!({"name":name,"arguments":arguments,"_meta":{"threadId":thread}}),
        &ConnectionIdentity::DaemonTrust,
        &crate::mcp_server::build_default_registry(),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn report_knowledge_delta_two_actual_sessions_keep_independent_baselines() {
    let p = Pair::new().await;
    mcp(&p, THREAD, "neige_report_read", json!({})).await;
    mcp(
        &p,
        THREAD,
        "neige_report_write",
        json!({"body":"# One\n\none\n\n# Two\n\ntwo\n", "message":"seed"}),
    )
    .await;
    let payload: String =
        sqlx::query_scalar("SELECT payload FROM cards WHERE track_id=? AND kind='track-report'")
            .bind(&p.track)
            .fetch_one(p.fixture.repo.pool())
            .await
            .unwrap();
    let blocks = serde_json::from_str::<crate::track_report::TrackReportPayload>(&payload)
        .unwrap()
        .blocks
        .unwrap();
    let (first, second) = (&blocks[0], &blocks[1]);
    let track = p.track.clone().into();
    assert!(
        crate::report_read_ledger::delta::for_track(p.fixture.repo.as_ref(), &track, &p.workers[0])
            .await
            .unwrap()
            .is_empty()
    );
    let unread =
        crate::report_read_ledger::delta::for_track(p.fixture.repo.as_ref(), &track, &p.workers[1])
            .await
            .unwrap();
    assert!(
        unread.contains(&first.id) && unread.contains(&second.id),
        "the other session starts unread: {unread}"
    );
    mcp(
        &p,
        "switch-assistant",
        "neige_report_read",
        json!({"blocks":[first.id]}),
    )
    .await;
    p.state
        .harness
        .get(&p.workers[0])
        .unwrap()
        .pause_issuance_for_dev();
    let response = p.app.clone().oneshot(Request::patch(format!("/api/tracks/{}/report/blocks/{}", p.track, second.id))
        .header(header::COOKIE, &p.cookie).header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"kind":"prose","markdown":"# Two\n\nnew external\n","payload":null,"ifBlockRev":second.rev}).to_string())).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let event = p
        .fixture
        .repo
        .events_for_track(&p.track, &["track.report_edited"], None)
        .await
        .unwrap()
        .into_iter()
        .last()
        .unwrap();
    p.state
        .dispatcher
        .catch_up_push(track.clone(), event.event, event.id)
        .await;
    let (_, current, frame) = p.fixture.carrier(&p.track).await;
    assert_eq!(current, p.workers[0]);
    let text = frame
        .canonical_inputs
        .pending
        .iter()
        .find_map(|input| match input {
            PendingInput::System {
                envelope_id: Some(id),
                observation,
                ..
            } if *id == event.id => Some(observation.observation().to_turn_text()),
            _ => None,
        })
        .expect("the actual Planner Dispatcher accepts the event");
    assert!(
        text.contains(&second.id)
            && text.contains("two")
            && text.contains("new external")
            && text.contains("block_diff"),
        "the Planner retains its historical baseline: {text}"
    );
    assert!(
        !text.contains(&first.id),
        "known unchanged content must not echo: {text}"
    );
    let assistant =
        crate::report_read_ledger::delta::for_track(p.fixture.repo.as_ref(), &track, &p.workers[1])
            .await
            .unwrap();
    assert!(
        assistant.contains(&second.id) && assistant.contains("\"known_rev\":null"),
        "the assistant has not read this block: {assistant}"
    );
    assert!(
        !assistant.contains(&first.id) && !assistant.contains("new external"),
        "the assistant gets references for unknown content: {assistant}"
    );
    p.close().await;
}
