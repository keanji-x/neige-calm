//! `neige_track_close`: the Planner closes its own track, and every write tool refuses the removed
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
        json!({ "ops": [], "message": "old habit", "lifecycle": "done" }),
    )
    .await
    .expect_err("a lifecycle key is refused");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(
        err.message.contains(
            "`lifecycle` is removed: close with neige_track_close; ask with \
             neige_user_notify or neige_ratify_request"
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

/// Rows of one event kind the close path persisted, counted in the log rather than on the bus.
async fn persisted(boot: &Boot, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = ?1")
        .bind(kind)
        .fetch_one(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap()
}

/// Two closes in flight at once: the second transaction re-reads the track, sees it closed and
/// writes nothing, so exactly one `track.updated` is persisted and both calls report one time.
#[tokio::test]
async fn concurrent_closes_persist_one_track_updated() {
    let boot = boot().await;
    let before = persisted(&boot, "track.updated").await;
    let (first, second) = tokio::join!(
        call_tool(
            &boot,
            TOOL_TRACK_CLOSE,
            planner_identity(&boot),
            json!({ "message": "first" }),
        ),
        call_tool(
            &boot,
            TOOL_TRACK_CLOSE,
            planner_identity(&boot),
            json!({ "message": "second" }),
        ),
    );
    let (first, second) = (first.expect("first close"), second.expect("second close"));
    assert_eq!(first["closed_at"], second["closed_at"]);
    assert_eq!(persisted(&boot, "track.updated").await, before + 1);
}

/// The recorder gate runs inside the close transaction: a session superseded after the transport
/// check is refused there, and the track stays open.
#[tokio::test]
async fn a_superseded_session_close_is_denied_in_the_transaction() {
    let boot = boot().await;
    sqlx::query("UPDATE worker_sessions SET state = 'superseded' WHERE id = ?1")
        .bind(PLANNER_SESSION_ID)
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .expect("supersede the planner session");
    let before = persisted(&boot, "track.updated").await;

    let err = call_tool(
        &boot,
        TOOL_TRACK_CLOSE,
        planner_identity(&boot),
        json!({ "message": "stale session" }),
    )
    .await
    .expect_err("a superseded session cannot close the track");
    assert_eq!(err.code, -32403);
    assert!(
        err.message.contains("recorder gate denied track_close"),
        "{err:?}"
    );
    assert!(
        boot.repo
            .track_get(boot.track_id.as_str())
            .await
            .unwrap()
            .unwrap()
            .is_open()
    );
    assert_eq!(persisted(&boot, "track.updated").await, before);
}

/// An area chat is never closed; the refusal lives in the one track writer, so the Planner's
/// close meets the same rule as the REST PATCH.
#[tokio::test]
async fn planner_close_of_an_area_chat_track_is_refused() {
    let boot = boot().await;
    sqlx::query("UPDATE tracks SET purpose = ?1 WHERE id = ?2")
        .bind(calm_server::AREA_CHAT_PURPOSE)
        .bind(boot.track_id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .expect("mark the track as an area chat");

    let err = call_tool(
        &boot,
        TOOL_TRACK_CLOSE,
        planner_identity(&boot),
        json!({ "message": "done chatting" }),
    )
    .await
    .expect_err("an area chat cannot be closed");
    assert_eq!(err.code, -32403);
    assert!(
        err.message
            .contains("an area chat track cannot be closed or reopened"),
        "{err:?}"
    );
    assert!(
        boot.repo
            .track_get(boot.track_id.as_str())
            .await
            .unwrap()
            .unwrap()
            .is_open()
    );
}
