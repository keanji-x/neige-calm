//! #1704 S2 — `claude_permissions_policy` on `PATCH /api/tracks/:id`: the
//! production route, the shared writer's root-only rule, the wire.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{EditAuthor, EventBus};
use calm_server::ids::ActorId;
use calm_server::model::{NewArea, NewTrack};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, DaemonClient};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn boot() -> (AppState, String, String, Arc<dyn Repo>) {
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "policy".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "policy".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let events = EventBus::new();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-claude-policy-plugins"),
            Vec::new(),
            events,
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::track_area_cache::TrackAreaCache::new(),
            ),
        )),
        Arc::new(calm_server::state::CodexClient::new_stub()),
        None,
        None,
    );
    let _ = EditAuthor::User;
    (state, track.id.to_string(), area.id.to_string(), repo)
}

fn app(state: AppState) -> axum::Router {
    routes::tracks::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state)
}

async fn patch(
    state: AppState,
    track_id: &str,
    actor: Option<&str>,
    body: Value,
) -> axum::http::Response<Body> {
    let mut request = Request::builder()
        .method("PATCH")
        .uri(format!("/api/tracks/{track_id}"))
        .header("content-type", "application/json");
    if let Some(actor) = actor {
        request = request.header("X-Calm-Actor", actor);
    }
    app(state)
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

async fn get(state: AppState, uri: &str) -> Value {
    let response = app(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{uri}");
    body_json(response).await
}

async fn body_text(response: axum::http::Response<Body>) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

async fn body_json(response: axum::http::Response<Body>) -> Value {
    serde_json::from_str(&body_text(response).await).unwrap()
}

async fn column(repo: &Arc<dyn Repo>, track_id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT claude_permissions_policy FROM tracks WHERE id = ?1")
        .bind(track_id)
        .fetch_one(&repo.sqlite_pool().unwrap())
        .await
        .unwrap()
}

async fn updated_events(repo: &Arc<dyn Repo>) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'track.updated'")
        .fetch_one(&repo.sqlite_pool().unwrap())
        .await
        .unwrap()
}

async fn event_count(repo: &Arc<dyn Repo>) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(&repo.sqlite_pool().unwrap())
        .await
        .unwrap()
}

fn policy() -> Value {
    json!({
        "edit": ["src/**", "tests/**"],
        "bash": ["git", "python3 -m unittest"],
        "deny": ["git rebase"]
    })
}

/// A user PATCH writes the policy (trimmed), every read carries it (the
/// PATCH body, the detail, the area list, the window list), exactly one
/// `track.updated` lands; a policy-only PATCH is never short-circuited; a
/// present null clears it.
#[tokio::test]
async fn user_patch_writes_the_policy_and_every_read_carries_it() {
    let (state, track_id, area_id, repo) = boot().await;
    assert_eq!(column(&repo, &track_id).await, None);
    let before = updated_events(&repo).await;
    let all_before = event_count(&repo).await;

    let mut untrimmed = policy();
    untrimmed["edit"] = json!([" src/** ", "tests/**"]);
    let response = patch(
        state.clone(),
        &track_id,
        None,
        json!({"claude_permissions_policy": untrimmed}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["claude_permissions_policy"], policy(), "{body}");
    assert_eq!(updated_events(&repo).await, before + 1);
    assert_eq!(
        event_count(&repo).await,
        all_before + 1,
        "exactly one event"
    );
    assert_eq!(
        column(&repo, &track_id).await.as_deref(),
        Some(
            r#"{"edit":["src/**","tests/**"],"bash":["git","python3 -m unittest"],"deny":["git rebase"]}"#
        )
    );

    let detail = get(state.clone(), &format!("/api/tracks/{track_id}")).await;
    assert_eq!(detail["track"]["claude_permissions_policy"], policy());
    let listed = get(state.clone(), &format!("/api/areas/{area_id}/tracks")).await;
    assert_eq!(listed[0]["claude_permissions_policy"], policy(), "{listed}");
    let window = get(state.clone(), &format!("/api/tracks?area_id={area_id}")).await;
    assert_eq!(window[0]["claude_permissions_policy"], policy(), "{window}");

    // Re-sending the same policy is still a write (one more event), never an
    // empty-patch short-circuit.
    let response = patch(
        state.clone(),
        &track_id,
        None,
        json!({"claude_permissions_policy": policy()}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(updated_events(&repo).await, before + 2);

    // A present null clears; the key stays on the wire as null.
    let response = patch(
        state.clone(),
        &track_id,
        None,
        json!({"claude_permissions_policy": null}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["claude_permissions_policy"], Value::Null, "{body}");
    assert!(body.get("claude_permissions_policy").is_some());
    assert_eq!(column(&repo, &track_id).await, None);
    assert_eq!(updated_events(&repo).await, before + 3);
    let detail = get(state, &format!("/api/tracks/{track_id}")).await;
    assert_eq!(detail["track"]["claude_permissions_policy"], Value::Null);
}

/// The value is refused under its own name: an empty policy is 400
/// (`declares nothing`), S1's scope rules are 400 with S1's reason under
/// `claude_permissions_policy`, a wrong shape is the body rejection (422)
/// naming the field. Nothing is written and no event lands.
#[tokio::test]
async fn invalid_policies_are_refused_under_the_field_name() {
    let (state, track_id, _, repo) = boot().await;
    let before = event_count(&repo).await;
    for (body, reason) in [
        (
            json!({"claude_permissions_policy": {}}),
            "claude_permissions_policy declares nothing; send null to clear the policy",
        ),
        (
            json!({"claude_permissions_policy": {"deny": []}}),
            "claude_permissions_policy declares nothing; send null to clear the policy",
        ),
        (
            json!({"claude_permissions_policy": {"bash": ["git push"]}}),
            "claude_permissions_policy.bash[0] 'git push': floor command, always asks; \
             put it in deny or omit it",
        ),
        (
            json!({"claude_permissions_policy": {"edit": ["/etc/**"]}}),
            "claude_permissions_policy.edit[0]: must be relative to the terminal cwd",
        ),
        (
            json!({"claude_permissions_policy": {"bash": ["git"], "deny": ["git"]}}),
            "claude_permissions_policy.deny[0]: also in bash[0]",
        ),
    ] {
        let response = patch(state.clone(), &track_id, None, body.clone()).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
        let error = body_json(response).await;
        assert_eq!(error["code"], "bad_request", "{body}");
        assert_eq!(error["error"], format!("bad request: {reason}"), "{body}");
    }
    for body in [
        json!({"claude_permissions_policy": ["**"]}),
        json!({"claude_permissions_policy": {"allow": []}}),
        json!({"claude_permissions_policy": {"bash": "git"}}),
        json!({"claude_permissions_policy": {"edit": ["**"], "deny": null}}),
    ] {
        let response = patch(state.clone(), &track_id, None, body.clone()).await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{body}"
        );
        let text = body_text(response).await;
        assert!(text.contains("claude_permissions_policy"), "{body}: {text}");
    }
    assert_eq!(column(&repo, &track_id).await, None);
    assert_eq!(event_count(&repo).await, before);
}

/// User-only (the reason is asserted, not just the status: an `ai:codex`
/// REST request is refused further downstream too); a workspace re-point
/// travels alone; a child track is 409 naming the root.
#[tokio::test]
async fn policy_is_user_only_travels_alone_and_is_tree_root_only() {
    let (state, track_id, area_id, repo) = boot().await;
    let before = event_count(&repo).await;

    let response = patch(
        state.clone(),
        &track_id,
        Some("ai:codex"),
        json!({"claude_permissions_policy": policy()}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let text = body_text(response).await;
    assert!(
        text.contains("claude_permissions_policy") && text.contains("user-only"),
        "403 must come from the user-only policy gate, got {text}"
    );
    assert_eq!(column(&repo, &track_id).await, None);
    assert_eq!(event_count(&repo).await, before);

    let response = patch(
        state.clone(),
        &track_id,
        None,
        json!({
            "claude_permissions_policy": policy(),
            "workspace": {"kind": "attached", "path": "/tmp"}
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let text = body_text(response).await;
    assert!(
        text.contains("workspace changes must be sent on their own"),
        "{text}"
    );
    assert_eq!(column(&repo, &track_id).await, None);
    assert_eq!(event_count(&repo).await, before);

    let child = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area_id.clone().into(),
            title: "child".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let child_id = child.id.to_string();
    sqlx::query("UPDATE tracks SET parent_track_id=?1 WHERE id=?2")
        .bind(&track_id)
        .bind(&child_id)
        .execute(&repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let before = event_count(&repo).await;
    let response = patch(
        state.clone(),
        &child_id,
        None,
        json!({"claude_permissions_policy": policy()}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let text = body_text(response).await;
    assert!(
        text.contains("claude_permissions_policy is tree-root-only")
            && text.contains(&format!("child of {track_id}")),
        "{text}"
    );
    assert_eq!(column(&repo, &child_id).await, None);
    assert_eq!(
        event_count(&repo).await,
        before,
        "a refused child PATCH emits nothing"
    );
    // The root still takes it, and the child's own row stays null.
    let response = patch(
        state.clone(),
        &track_id,
        None,
        json!({"claude_permissions_policy": policy()}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let detail = get(state, &format!("/api/tracks/{child_id}")).await;
    assert_eq!(
        detail["track"]["claude_permissions_policy"],
        Value::Null,
        "a child shows the raw column"
    );
    let _ = ActorId::User;
}
