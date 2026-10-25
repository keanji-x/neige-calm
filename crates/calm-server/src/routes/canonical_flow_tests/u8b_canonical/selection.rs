//! Storage selection follows resource ownership, independent of live readiness.
use super::*;

#[tokio::test]
async fn u8b_canonical_managed_attach_keeps_historical_start_failure() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let a = area(&app, &cookie, "selection").await;
    let mut managed = body(&a, "codex");
    managed["attach_folder"] = json!(true);
    managed["template_id"] = json!("small-change");
    managed["first_message"] = json!(FIRST_BODY);
    // The same unavailable provider must not select a different storage owner.
    let path = repo(&fixture);
    let mut attached = managed.clone();
    attached["cwd"] = json!(path);
    attached["isolation"] = json!("none");
    let (status, created) = create(&app, &cookie, "borrowed", attached).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let (_, _, frame) = fixture.carrier(created["id"].as_str().unwrap()).await;
    assert_eq!(frame.canonical_inputs.pending.len(), 1);
    assert!(!state.shared_codex_appserver.is_running());
    let (status, failed) = create(&app, &cookie, "managed", managed).await;
    assert!(status.is_server_error(), "{status}: {failed}");
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn u8b_nonhuman_registered_mcp_add_keeps_attached_historical_storage() {
    use crate::mcp_server::registry::{ToolCallIdentity, ToolRegistry};
    use crate::mcp_server::tools::track_add::{TOOL_TRACK_ADD, register_into};
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let a = area(&app, &cookie, "agent creator").await;
    let mut request = body(&a, "codex");
    request["admin"] = json!(true);
    let creator = post(&app, &cookie, "/api/tracks", "creator", request).await;
    let track = creator["id"].as_str().unwrap();
    let (card, worker, _) = fixture.carrier(track).await;
    let identity = ToolCallIdentity {
        card_id: card,
        role: crate::model::CardRole::Planner,
        provider: crate::session_projection_repo::AgentProvider::Codex,
        session_id: worker,
        track_id: Some(track.into()),
        area_id: a["id"].as_str().unwrap().into(),
        thread_id: "card-bound".into(),
    };
    assert!(matches!(
        identity.to_principal(),
        Some(calm_types::worker::Principal::Agent { .. })
    ));
    let mut registry = ToolRegistry::new();
    register_into(&mut registry);
    let add = registry.lookup(TOOL_TRACK_ADD).unwrap();
    let path = repo(&fixture);
    let args = json!({"template_id":"small-change", "title":"Agent added",
        "cwd":path,"attach_folder":true,"idempotency_key":"agent-add",
        "text":FIRST_BODY,"message":"Create an attached track"});
    let context = crate::state::RouteState::from_ref(&state).mcp_context;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let adding = add(context.clone(), identity.clone(), args.clone());
    let native = async {
        let call = super::super::native_attempt_recovery_tests::initial_request(&mut peer).await;
        send(
            &mut peer,
            json!({"id":call["id"],"result":{"turn":{"id":TURN}}}),
        )
        .await;
        notify_thread(
            &state.shared_codex_appserver,
            &mut peer,
            THREAD,
            "turn/started",
            json!({"id":TURN,"status":"inProgress"}),
        )
        .await;
        notify_thread(
            &state.shared_codex_appserver,
            &mut peer,
            THREAD,
            "turn/completed",
            json!({"id":TURN,"status":"completed"}),
        )
        .await;
    };
    let (created, ()) = tokio::join!(adding, native);
    let created = created.unwrap().into_structured();
    let id = created["track_id"].as_str().unwrap();
    let saved = fixture.repo.track_get(id).await.unwrap().unwrap();
    assert_eq!(saved.creator_track_id.as_deref(), Some(track));
    assert_eq!(saved.workspace.path, path.to_str().unwrap());
    assert!(std::path::Path::new(saved.workspace.agent_cwd()).is_dir());
    let canonical: bool = sqlx::query_scalar(concat!(
        "SELECT EXISTS(SELECT 1 FROM conversation_messages m JOIN cards c ON c.id=m.card_id ",
        "WHERE c.track_id=?1)"
    ))
    .bind(id)
    .fetch_one(fixture.repo.pool())
    .await
    .unwrap();
    assert!(!canonical, "MCP first input retains historical attribution");
    let snapshot: String = sqlx::query_scalar(concat!(
        "SELECT ws.handle_state_json FROM worker_sessions ws JOIN cards c ON c.session_id=ws.id ",
        "WHERE c.track_id=?1 AND c.role='planner'"
    ))
    .bind(id)
    .fetch_one(fixture.repo.pool())
    .await
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&snapshot).unwrap()["schema_version"],
        crate::harness::snapshot::HARNESS_SNAPSHOT_SCHEMA_VERSION
    );
    assert_eq!(
        add(context, identity, args)
            .await
            .unwrap()
            .into_structured(),
        created
    );
    shutdown(&fixture, &state).await;
}
