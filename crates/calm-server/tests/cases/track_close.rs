//! `calm.track.close`: the Planner closes its own track, and every write tool refuses the removed
//! `lifecycle` key the same way.

use super::*;
use calm_server::mcp_server::tools::track_report_blocks::TOOL_REPORT_COMMIT;
use calm_server::mcp_server::tools::track_state::TOOL_TRACK_CLOSE;

#[tokio::test]
async fn planner_close_stamps_closed_at_and_refuses_a_lifecycle_key() {
    let boot = boot().await;
    let mut rx = boot.ctx.events.subscribe();

    let out = call_tool(
        &boot,
        TOOL_TRACK_CLOSE,
        planner_identity(&boot),
        json!({ "message": "goal met" }),
    )
    .await
    .expect("the planner closes its track");
    let closed_at = out["closed_at"]
        .as_i64()
        .expect("closed_at is a unix-ms time");
    let track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(track.closed_at, Some(closed_at));

    let envelope = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("the close is broadcast")
        .expect("bus open");
    assert!(
        matches!(envelope.actor, ActorId::AiPlannerSession(_)),
        "{:?}",
        envelope.actor
    );
    match envelope.event {
        Event::TrackUpdated(payload) => {
            assert_eq!(payload.closed_at, Some(closed_at));
            assert_eq!(payload.agent_message.as_deref(), Some("goal met"));
        }
        other => panic!("a close emits TrackUpdated, got {other:?}"),
    }

    let again = call_tool(
        &boot,
        TOOL_TRACK_CLOSE,
        planner_identity(&boot),
        json!({ "message": "still done" }),
    )
    .await
    .expect("closing a closed track is a no-op");
    assert_eq!(again["closed_at"], json!(closed_at));
    let no_event = tokio::time::timeout(std::time::Duration::from_millis(150), rx.recv()).await;
    assert!(no_event.is_err(), "a no-op close emitted {no_event:?}");

    let err = call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        json!({ "ops": [], "message": "old habit", "if_doc_rev": 0, "lifecycle": "done" }),
    )
    .await
    .expect_err("a lifecycle key is refused");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(
        err.message.contains(
            "`lifecycle` is removed: close with calm.track.close; ask with \
             calm.user.notify or calm.ratify.request"
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn track_close_needs_a_message_and_the_planner_role() {
    let boot = boot().await;
    let err = call_tool(&boot, TOOL_TRACK_CLOSE, planner_identity(&boot), json!({}))
        .await
        .expect_err("a close states why");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("message must be non-empty"), "{err:?}");

    let err = call_tool(
        &boot,
        TOOL_TRACK_CLOSE,
        worker_identity(&boot),
        json!({ "message": "not mine" }),
    )
    .await
    .expect_err("a worker cannot close the track");
    assert!(err.message.contains("requires role=Planner"), "{err:?}");
    assert!(
        boot.repo
            .track_get(boot.track_id.as_str())
            .await
            .unwrap()
            .unwrap()
            .is_open()
    );
}
