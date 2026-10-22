//! S3 exercises the real close handler against Tracks created by the real add entry point.
use super::*;
use calm_server::mcp_server::tools::track_state::TOOL_TRACK_CLOSE;
use calm_truth::db::RepoRead;

async fn close(boot: &Boot, caller: &Caller, target: &str) -> Result<Value, RpcError> {
    let handler = boot.registry.lookup(TOOL_TRACK_CLOSE).unwrap();
    handler(
        boot.ctx.clone(),
        caller.identity.clone(),
        json!({"track_id": target, "message": "settled"}),
    )
    .await
    .map(calm_server::mcp_server::result::ToolResult::into_structured)
}

async fn unchanged(boot: &Boot, target: &str, events: i64) {
    assert!(
        boot.repo
            .track_get(target)
            .await
            .unwrap()
            .unwrap()
            .is_open()
    );
    assert_eq!(
        boot.count("SELECT COUNT(*) FROM events WHERE kind='track.updated'")
            .await,
        events
    );
}

#[tokio::test]
async fn admin_closes_a_track_it_added_with_message() {
    let boot = boot(1).await;
    let (caller, planner) = boot.user_track("admin").await;
    let target = boot.added(&planner, "first").await;
    let before = boot
        .count("SELECT COUNT(*) FROM events WHERE kind='track.updated'")
        .await;
    let out = close(&boot, &planner, &target).await.unwrap();
    assert!(out["closed_at"].is_string());
    assert!(
        boot.repo
            .track_get(&caller)
            .await
            .unwrap()
            .unwrap()
            .is_open()
    );
    assert!(
        !boot
            .repo
            .track_get(&target)
            .await
            .unwrap()
            .unwrap()
            .is_open()
    );
    let (scope, actor, payload): (String, String, String) = sqlx::query_as(
        "SELECT scope_track, actor, payload FROM events WHERE kind='track.updated' ORDER BY id DESC LIMIT 1")
        .fetch_one(boot.repo.pool()).await.unwrap();
    assert_eq!(scope, target);
    assert_eq!(
        serde_json::from_str::<calm_server::ids::ActorId>(&actor).unwrap(),
        planner.identity.to_actor_id()
    );
    assert_eq!(
        serde_json::from_str::<Value>(&payload).unwrap()["agent_message"],
        "settled"
    );
    assert_eq!(
        boot.count("SELECT COUNT(*) FROM events WHERE kind='track.updated'")
            .await,
        before + 1
    );
    assert_eq!(close(&boot, &planner, &target).await.unwrap(), out);
    assert_eq!(
        boot.count("SELECT COUNT(*) FROM events WHERE kind='track.updated'")
            .await,
        before + 1
    );
    boot.added(&planner, "freed-slot").await;
}

#[tokio::test]
async fn non_admin_close_of_other_track_is_refused() {
    let boot = boot(16).await;
    let (_, planner) = boot.user_track_admin("ordinary", false).await;
    let (target, _) = boot.user_track_admin("target", false).await;
    // Persisted provenance with no grant: authority is independent of ownership.
    sqlx::query("UPDATE tracks SET creator_track_id=?1, creator_key='owned' WHERE id=?2")
        .bind(&planner.identity.track_id)
        .bind(&target)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let events = boot
        .count("SELECT COUNT(*) FROM events WHERE kind='track.updated'")
        .await;
    let error = close(&boot, &planner, &target).await.unwrap_err();
    assert_eq!(error.code, -32403, "{error:?}");
    assert_eq!(error.data.unwrap()["refusal"], "not_admin");
    unchanged(&boot, &target, events).await;
}

#[tokio::test]
async fn own_close_with_own_track_id_needs_no_admin() {
    let boot = boot(16).await;
    let (own, planner) = boot.user_track_admin("ordinary", false).await;
    assert!(close(&boot, &planner, &own).await.unwrap()["closed_at"].is_string());
}

#[tokio::test]
async fn admin_close_of_track_not_added_by_caller_is_unknown() {
    let boot = boot(16).await;
    let (_, planner) = boot.user_track("admin").await;
    let (foreign, foreign_planner) = boot.user_track("foreign").await;
    let foreign_added = boot.added(&foreign_planner, "foreign-added").await;
    forge_in_flight(&boot, &foreign).await;
    let before = boot
        .count("SELECT COUNT(*) FROM events WHERE kind='track.updated'")
        .await;
    for target in [&foreign, &foreign_added, "missing-track"] {
        let error = close(&boot, &planner, target).await.unwrap_err();
        assert_eq!(error.code, -32404, "{error:?}");
        assert_eq!(error.data.unwrap(), json!({"refusal": "unknown_track"}));
    }
    unchanged(&boot, &foreign, before).await;
}

#[tokio::test]
async fn admin_close_refuses_running_work_listing_keys() {
    let boot = boot(16).await;
    let (_, planner) = boot.user_track("admin").await;
    let target = boot.added(&planner, "busy").await;
    for status in ["dispatched", "running", "verifying"] {
        sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,created_at_ms,updated_at_ms) \
            VALUES(?,?,?,'codex','work','{}',?,1,1)")
            .bind(status).bind(&target).bind(format!("work-{status}")).bind(status)
            .execute(boot.repo.pool()).await.unwrap();
    }
    let before = boot
        .count("SELECT COUNT(*) FROM events WHERE kind='track.updated'")
        .await;
    let error = close(&boot, &planner, &target).await.unwrap_err();
    assert_eq!(error.code, -32409, "{error:?}");
    assert_eq!(
        error.data.unwrap(),
        json!({"refusal": "active_work", "active": [
            {"key": "work-dispatched", "status": "dispatched"},
            {"key": "work-running", "status": "running"},
            {"key": "work-verifying", "status": "verifying"}
        ]})
    );
    unchanged(&boot, &target, before).await;
    sqlx::query("UPDATE tasks SET status='done' WHERE track_id=?")
        .bind(&target)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    close(&boot, &planner, &target).await.unwrap();
}

#[tokio::test]
async fn admin_close_after_revoke_commits_nothing() {
    let boot = boot(16).await;
    let (caller, planner) = boot.user_track("admin").await;
    let target = boot.added(&planner, "revoked").await;
    let (status, body) = send(
        boot.app.clone(),
        "PATCH",
        &format!("/api/tracks/{caller}"),
        Some(json!({"admin": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let before = boot
        .count("SELECT COUNT(*) FROM events WHERE kind='track.updated'")
        .await;
    let error = close(&boot, &planner, &target).await.unwrap_err();
    assert_eq!(error.code, -32403, "{error:?}");
    assert_eq!(error.data.unwrap()["refusal"], "not_admin");
    unchanged(&boot, &target, before).await;
}

#[tokio::test]
async fn admin_close_other_from_a_superseded_session_is_refused() {
    let boot = boot(16).await;
    let (_, planner) = boot.user_track("admin").await;
    let target = boot.added(&planner, "stale").await;
    sqlx::query("UPDATE worker_sessions SET state='superseded' WHERE id=?")
        .bind(&planner.identity.session_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let before = boot
        .count("SELECT COUNT(*) FROM events WHERE kind='track.updated'")
        .await;
    let error = close(&boot, &planner, &target).await.unwrap_err();
    assert_eq!(error.code, -32403, "{error:?}");
    assert!(
        error.message.contains("recorder gate denied track_close"),
        "{error:?}"
    );
    unchanged(&boot, &target, before).await;
}

#[tokio::test]
async fn admin_area_ls_rows_carry_creator_track_id() {
    let boot = boot(16).await;
    let (caller, planner) = boot.user_track("admin").await;
    let target = boot.added(&planner, "listed").await;
    let handler = boot.registry.lookup("neige_area_ls").unwrap();
    let out = handler(boot.ctx.clone(), planner.identity, json!({}))
        .await
        .unwrap()
        .into_structured();
    let rows = out["tracks"].as_array().unwrap();
    let added = rows.iter().find(|row| row["track_id"] == target).unwrap();
    assert_eq!(added["creator_track_id"], caller);
    let own = rows.iter().find(|row| row["track_id"] == caller).unwrap();
    assert_eq!(own.get("creator_track_id"), Some(&Value::Null));
}

async fn forge_in_flight(boot: &Boot, target: &str) {
    sqlx::query(
        "INSERT INTO operations (id, operation_key, kind, payload_hash, target_type, target_id, \
        target_json, payload_json, phase, created_at_ms, updated_at_ms) \
        VALUES ('forge', 'forge', ?1, 'h', 'track', ?2, '{}', '{}', 'spawn_started', 1, 1)",
    )
    .bind(calm_server::operation::forge_action_adapter::FORGE_ACTION_KIND)
    .bind(target)
    .execute(boot.repo.pool())
    .await
    .unwrap();
}

#[tokio::test]
async fn admin_close_refuses_an_in_flight_forge_action() {
    let boot = boot(16).await;
    let (_, planner) = boot.user_track("admin").await;
    let target = boot.added(&planner, "forge").await;
    forge_in_flight(&boot, &target).await;
    let before = boot
        .count("SELECT COUNT(*) FROM events WHERE kind='track.updated'")
        .await;
    let error = close(&boot, &planner, &target).await.unwrap_err();
    assert_eq!(error.code, -32409, "{error:?}");
    assert!(error.message.contains("forge-action"), "{error:?}");
    unchanged(&boot, &target, before).await;
}

async fn plugin_template_track(boot: &Boot, admin: bool) -> (String, Caller) {
    let (status, track) = send(
        boot.app.clone(),
        "POST",
        "/api/tracks",
        Some(json!({"planner_provider":"codex", "area_id":boot.area_id,
            "template_id":"dev", "admin":admin,
            "theme":{"fg":[255,255,255],"bg":[0,0,0]}})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{track}");
    assert_eq!(track["plugin_scope"], "gitforge");
    assert_eq!(track["template_id"], "dev");
    assert_eq!(track["admin_granted_at"].is_i64(), admin);
    let id = track["id"].as_str().unwrap().to_string();
    let caller = boot.planner_of(&id).await;
    (id, caller)
}

async fn plugin_admin_lifecycle(boot: &Boot, id: &str, planner: &Caller) {
    let target = boot.added(planner, "plugin-added").await;
    let retained = boot.added(planner, "plugin-retained").await;
    close(boot, planner, &target).await.unwrap();
    assert!(
        !boot
            .repo
            .track_get(&target)
            .await
            .unwrap()
            .unwrap()
            .is_open()
    );
    let (status, body) = send(
        boot.app.clone(),
        "PATCH",
        &format!("/api/tracks/{id}"),
        Some(json!({"admin":false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let events = boot
        .count("SELECT COUNT(*) FROM events WHERE kind IN ('track.created','track.updated')")
        .await;
    let tracks = boot.track_count().await;
    let error = boot
        .add(planner, boot.args("plugin-revoked"))
        .await
        .unwrap_err();
    assert_eq!(error.data.unwrap()["refusal"], "not_admin");
    let error = close(boot, planner, &retained).await.unwrap_err();
    assert_eq!(error.data.unwrap()["refusal"], "not_admin");
    assert!(
        boot.repo
            .track_get(&retained)
            .await
            .unwrap()
            .unwrap()
            .is_open()
    );
    assert_eq!(boot.track_count().await, tracks);
    assert_eq!(
        boot.count("SELECT COUNT(*) FROM events WHERE kind IN ('track.created','track.updated')")
            .await,
        events
    );
    let (_, detail) = send(boot.app.clone(), "GET", &format!("/api/tracks/{id}"), None).await;
    assert_eq!(detail["can_admin"], true, "{detail}");
    let stored = boot.repo.track_get(id).await.unwrap().unwrap();
    assert_eq!(stored.plugin_scope.as_deref(), Some("gitforge"));
    assert!(stored.admin_granted_at.is_none());
}

#[tokio::test]
async fn plugin_template_admin_post_grant_add_close_and_revoke() {
    let boot = boot_with_plugins(16, None, true).await;
    let (id, planner) = plugin_template_track(&boot, true).await;
    plugin_admin_lifecycle(&boot, &id, &planner).await;
}

#[tokio::test]
async fn plugin_template_admin_patch_requires_explicit_grant() {
    let boot = boot_with_plugins(16, None, true).await;
    let (id, planner) = plugin_template_track(&boot, false).await;
    let events = boot
        .count("SELECT COUNT(*) FROM events WHERE kind IN ('track.created','track.updated')")
        .await;
    let tracks = boot.track_count().await;
    let error = boot
        .add(&planner, boot.args("ungranted"))
        .await
        .unwrap_err();
    assert_eq!(error.data.unwrap()["refusal"], "not_admin");
    assert_eq!(
        boot.count("SELECT COUNT(*) FROM events WHERE kind IN ('track.created','track.updated')")
            .await,
        events
    );
    assert_eq!(boot.track_count().await, tracks);
    let (status, body) = send(
        boot.app.clone(),
        "PATCH",
        &format!("/api/tracks/{id}"),
        Some(json!({"admin":true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["admin_granted_at"].is_i64(), "{body}");
    plugin_admin_lifecycle(&boot, &id, &planner).await;
}

#[tokio::test]
async fn plugin_template_admin_ask_requires_user_answer() {
    use calm_server::mcp_server::tools::user_ask::TOOL_USER_ASK;
    let boot = boot_with_plugins(16, None, true).await;
    let (id, planner) = plugin_template_track(&boot, false).await;
    let ask = boot.registry.lookup(TOOL_USER_ASK).unwrap()(
        boot.ctx.clone(),
        planner.identity.clone(),
        json!({"action":"grant_admin","questions":[{"title":"Manage Tracks?"}]}),
    )
    .await
    .unwrap()
    .into_structured();
    assert!(
        boot.repo
            .track_get(&id)
            .await
            .unwrap()
            .unwrap()
            .admin_granted_at
            .is_none()
    );
    let error = boot.add(&planner, boot.args("pending")).await.unwrap_err();
    assert_eq!(error.data.unwrap()["refusal"], "not_admin");
    let (status, body) = send(
        boot.app.clone(),
        "POST",
        &format!("/api/tracks/{id}/asks/{}/answer", ask["ask_id"]),
        Some(json!({"answers":[{"option":0}]})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(
        boot.repo
            .track_get(&id)
            .await
            .unwrap()
            .unwrap()
            .admin_granted_at
            .is_some()
    );
    plugin_admin_lifecycle(&boot, &id, &planner).await;
}
