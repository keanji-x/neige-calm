use super::*;

#[tokio::test]
async fn track_add_refuses_a_non_admin_planner() {
    let boot = boot(16).await;
    let (_, planner) = boot.user_track_admin("ordinary", false).await;
    let before = boot.count("SELECT COUNT(*) FROM events").await;
    let tracks = boot.track_count().await;
    let operations = boot.count("SELECT COUNT(*) FROM operations").await;
    let error = boot.add(&planner, boot.args("denied")).await.unwrap_err();
    assert_eq!(error.code, -32403, "{error:?}");
    assert_eq!(error.data.unwrap()["refusal"], "not_admin");
    assert!(error.message.contains("neige_user_ask"));
    assert!(error.message.contains("grant_admin"));
    assert_eq!(boot.track_count().await, tracks);
    assert_eq!(
        boot.count("SELECT COUNT(*) FROM operations").await,
        operations
    );
    assert_eq!(boot.count("SELECT COUNT(*) FROM events").await, before);
    assert_eq!(
        boot.count("SELECT COUNT(*) FROM track_create_idempotency")
            .await,
        0
    );
}

#[tokio::test]
async fn track_add_after_revoke_commits_nothing() {
    let gate = Arc::new(routes::tracks::TrackCreateMintGate::new());
    let boot = Arc::new(boot_with_gate(16, Some(gate.clone())).await);
    let (id, planner) = boot.user_track("admin").await;
    let tracks = boot.track_count().await;
    let operations = boot.count("SELECT COUNT(*) FROM operations").await;
    let pending = tokio::spawn({
        let boot = boot.clone();
        async move { boot.add(&planner, boot.args("revoked")).await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), gate.reached.wait())
        .await
        .unwrap();
    let (status, body) = send(
        boot.app.clone(),
        "PATCH",
        &format!("/api/tracks/{id}"),
        Some(json!({"admin": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let events = boot.count("SELECT COUNT(*) FROM events").await;
    gate.released.wait().await;
    let error = pending.await.unwrap().unwrap_err();
    assert_eq!(error.code, -32403, "{error:?}");
    assert_eq!(error.data.unwrap()["refusal"], "not_admin");
    assert_eq!(boot.track_count().await, tracks);
    assert_eq!(
        boot.count("SELECT COUNT(*) FROM operations").await,
        operations
    );
    assert_eq!(boot.count("SELECT COUNT(*) FROM events").await, events);
    assert_eq!(
        boot.count("SELECT COUNT(*) FROM track_create_idempotency")
            .await,
        0
    );
}

#[cfg(unix)]
#[tokio::test]
async fn track_add_admits_an_admin_planner() {
    use crate::support::mcp::{connect, handshake, recv_frame, send_frame};
    let boot = boot(16).await;
    let (_, planner) = boot.user_track("admin").await;
    let token = calm_server::mcp_server::auth::CardMcpToken::generate();
    let hash = calm_server::mcp_server::auth::hash_token(token.as_str());
    let mut tx = boot.repo.pool().begin().await.unwrap();
    calm_server::db::sqlite::session_mcp_token_set_tx(&mut tx, &planner.identity.session_id, &hash)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let path = boot._tmp.path().join("admin.sock");
    let _server = calm_server::mcp_server::McpServer::spawn_with_context(
        boot.ctx.clone(),
        path.clone(),
        PathBuf::from("/nonexistent-shim"),
        calm_server::mcp_server::build_default_registry(),
    )
    .await
    .unwrap();
    let (mut rd, mut wr) = connect(&path).await;
    handshake(&mut rd, &mut wr, token.as_str()).await;
    send_frame(&mut wr, json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":TOOL_TRACK_ADD,"arguments":boot.args("socket-admin")}})).await;
    let response = recv_frame(&mut rd).await;
    assert!(response.get("error").is_none(), "{response}");
    assert!(
        response["result"].get("isError").is_none_or(|v| v == false),
        "{response}"
    );
    let track = boot.repo.pool();
    let admin: Option<i64> =
        sqlx::query_scalar("SELECT admin_granted_at FROM tracks WHERE creator_key='socket-admin'")
            .fetch_one(track)
            .await
            .unwrap();
    assert_eq!(admin, None);
}

#[tokio::test]
async fn track_add_admin_eligibility_refuses_created_creator() {
    let boot = boot(16).await;
    let (_, planner) = boot.user_track("portfolio").await;
    let added = boot.added(&planner, "k1").await;
    let (status, _) = send(
        boot.app.clone(),
        "PATCH",
        &format!("/api/tracks/{added}"),
        Some(json!({"admin": true})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let added_planner = boot.planner_of(&added).await;
    let before = boot.track_count().await;
    assert_forbidden(
        boot.add(&added_planner, boot.args("k2")).await,
        "an added Track may not add a Track",
    );
    assert_eq!(boot.track_count().await, before);
}

#[tokio::test]
async fn track_add_admin_eligibility_refuses_child_creator() {
    let boot = boot(16).await;
    let (parent, _) = boot.user_track("parent").await;
    let (creator, planner) = boot.user_track_admin("child", false).await;
    sqlx::query("UPDATE tracks SET parent_track_id = ?1 WHERE id = ?2")
        .bind(&parent)
        .bind(&creator)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let (status, _) = send(
        boot.app.clone(),
        "PATCH",
        &format!("/api/tracks/{creator}"),
        Some(json!({"admin": true})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let before = boot.track_count().await;
    assert_forbidden(
        boot.add(&planner, boot.args("k1")).await,
        "a child Track may not add a Track",
    );
    assert_eq!(boot.track_count().await, before);
}
