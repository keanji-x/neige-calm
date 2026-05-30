//! Integration tests for the cove ↔ folder mapping surface introduced
//! in issue #250 PR 1.
//!
//! Coverage matrix (17 cases):
//!
//!   1. `post_then_get_returns_the_folder`
//!   2. `post_same_path_twice_409_equal`
//!   3. `post_ancestor_when_descendant_exists_409_ancestor`
//!   4. `post_descendant_when_ancestor_exists_409_descendant`
//!   5. `post_non_absolute_path_400`
//!   6. `post_trailing_slash_is_normalized_and_conflicts`
//!   7. `delete_removes_the_folder`
//!   8. `resolve_hits_self`
//!   9. `resolve_hits_descendant`
//!  10. `resolve_picks_longest_prefix`
//!  11. `resolve_miss_returns_200_null`
//!  12. `resolve_non_absolute_path_400`
//!  13. `cascade_delete_cove_drops_its_folders`
//!  14. `post_to_unknown_cove_returns_404`
//!  15. `get_returns_only_own_cove_folders`
//!  16. `cross_cove_overlap_409_descendant`
//!  17. `delete_with_mismatched_cove_id_returns_404`
//!
//! No daemon binary is required — cove_folders is pure CRUD against
//! the sqlite repo, no card / terminal side-effects.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::NewCove;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::wave_cove_cache::WaveCoveCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    cove_id: String,
    repo: Arc<dyn Repo>,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "folders-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();

    // `cove_folders` never needs the session daemon — the DaemonClient
    // here is a stub pointing at /dev/null. Boot mirrors
    // `cards_deletable.rs` so future contributors recognize the shape.
    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: None,
    });
    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    let wave_cove_cache = WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let state = AppState::from_parts(
        repo.clone(),
        events,
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-cove-folders-test"),
            Vec::new(),
            EventBus::new(),
            card_role_cache.clone(),
            wave_cove_cache.clone(),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache),
        Some(wave_cove_cache),
    );

    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);

    Boot {
        app,
        cove_id: cove.id.to_string(),
        repo,
        _tmp: tmp,
    }
}

async fn post(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

async fn get(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

async fn delete(app: axum::Router, uri: &str) -> StatusCode {
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    resp.status()
}

// (1) ---------------------------------------------------------------

#[tokio::test]
async fn post_then_get_returns_the_folder() {
    let b = boot().await;
    let (status, body) = post(
        b.app.clone(),
        &format!("/api/coves/{}/folders", b.cove_id),
        json!({"path": "/a"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(body["path"].as_str().unwrap(), "/a");
    assert_eq!(body["cove_id"].as_str().unwrap(), b.cove_id);

    let (status, body) = get(b.app.clone(), &format!("/api/coves/{}/folders", b.cove_id)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["path"].as_str().unwrap(), "/a");
}

// (2) ---------------------------------------------------------------

#[tokio::test]
async fn post_same_path_twice_409_equal() {
    let b = boot().await;
    let uri = format!("/api/coves/{}/folders", b.cove_id);
    let (s1, _) = post(b.app.clone(), &uri, json!({"path": "/a"})).await;
    assert_eq!(s1, StatusCode::CREATED);
    let (s2, body) = post(b.app.clone(), &uri, json!({"path": "/a"})).await;
    assert_eq!(s2, StatusCode::CONFLICT);
    assert_eq!(body["conflict_kind"].as_str().unwrap(), "equal");
    assert_eq!(body["conflict_path"].as_str().unwrap(), "/a");
    assert!(body["folder_id"].is_number());
}

// (3) ---------------------------------------------------------------

#[tokio::test]
async fn post_ancestor_when_descendant_exists_409_ancestor() {
    let b = boot().await;
    let uri = format!("/api/coves/{}/folders", b.cove_id);
    let (s1, _) = post(b.app.clone(), &uri, json!({"path": "/a/b"})).await;
    assert_eq!(s1, StatusCode::CREATED);
    let (s2, body) = post(b.app.clone(), &uri, json!({"path": "/a"})).await;
    assert_eq!(s2, StatusCode::CONFLICT);
    assert_eq!(body["conflict_kind"].as_str().unwrap(), "ancestor");
    assert_eq!(body["conflict_path"].as_str().unwrap(), "/a/b");
}

// (4) ---------------------------------------------------------------

#[tokio::test]
async fn post_descendant_when_ancestor_exists_409_descendant() {
    let b = boot().await;
    let uri = format!("/api/coves/{}/folders", b.cove_id);
    let (s1, _) = post(b.app.clone(), &uri, json!({"path": "/a"})).await;
    assert_eq!(s1, StatusCode::CREATED);
    let (s2, body) = post(b.app.clone(), &uri, json!({"path": "/a/b"})).await;
    assert_eq!(s2, StatusCode::CONFLICT);
    assert_eq!(body["conflict_kind"].as_str().unwrap(), "descendant");
    assert_eq!(body["conflict_path"].as_str().unwrap(), "/a");
}

// (5) ---------------------------------------------------------------

#[tokio::test]
async fn post_non_absolute_path_400() {
    let b = boot().await;
    let (status, body) = post(
        b.app.clone(),
        &format!("/api/coves/{}/folders", b.cove_id),
        json!({"path": "relative/path"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"].as_str().unwrap(), "bad_request");
}

// (6) ---------------------------------------------------------------

#[tokio::test]
async fn post_trailing_slash_is_normalized_and_conflicts() {
    let b = boot().await;
    let uri = format!("/api/coves/{}/folders", b.cove_id);
    let (s1, body1) = post(b.app.clone(), &uri, json!({"path": "/a/"})).await;
    assert_eq!(s1, StatusCode::CREATED);
    // Server normalizes — the stored path drops the trailing slash.
    assert_eq!(body1["path"].as_str().unwrap(), "/a");

    let (s2, body2) = post(b.app.clone(), &uri, json!({"path": "/a"})).await;
    assert_eq!(s2, StatusCode::CONFLICT);
    assert_eq!(body2["conflict_kind"].as_str().unwrap(), "equal");
}

// (7) ---------------------------------------------------------------

#[tokio::test]
async fn delete_removes_the_folder() {
    let b = boot().await;
    let uri = format!("/api/coves/{}/folders", b.cove_id);
    let (_, body) = post(b.app.clone(), &uri, json!({"path": "/a"})).await;
    let folder_id = body["id"].as_i64().unwrap();

    let status = delete(
        b.app.clone(),
        &format!("/api/coves/{}/folders/{folder_id}", b.cove_id),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, list) = get(b.app.clone(), &uri).await;
    assert_eq!(list.as_array().unwrap().len(), 0);
}

// (8) ---------------------------------------------------------------

#[tokio::test]
async fn resolve_hits_self() {
    let b = boot().await;
    let (_, body) = post(
        b.app.clone(),
        &format!("/api/coves/{}/folders", b.cove_id),
        json!({"path": "/a"}),
    )
    .await;
    let folder_id = body["id"].as_i64().unwrap();

    let (status, body) = get(b.app.clone(), "/api/coves/resolve?path=/a").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["cove_id"].as_str().unwrap(), b.cove_id);
    assert_eq!(body["folder_id"].as_i64().unwrap(), folder_id);
    assert_eq!(body["folder_path"].as_str().unwrap(), "/a");
}

// (9) ---------------------------------------------------------------

#[tokio::test]
async fn resolve_hits_descendant() {
    let b = boot().await;
    post(
        b.app.clone(),
        &format!("/api/coves/{}/folders", b.cove_id),
        json!({"path": "/a"}),
    )
    .await;

    let (status, body) = get(b.app.clone(), "/api/coves/resolve?path=/a/b/c").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["folder_path"].as_str().unwrap(), "/a");
}

// (10) --------------------------------------------------------------

#[tokio::test]
async fn resolve_picks_longest_prefix() {
    // The route-level conflict check forbids ancestor/descendant
    // overlap across the table, so `/a` and `/a/b` can never both be
    // present via the public surface. To still exercise the
    // longest-prefix branch of the resolve algorithm, this test seeds
    // both rows through the raw repo (the same code path replay
    // would use to restore a corrupted DB) and then asks the resolve
    // endpoint to pick the more specific claim. The test guards
    // against a future regression where the resolve handler ignores
    // path length and returns the first match it sees.
    let b = boot().await;
    b.repo.cove_folder_create(&b.cove_id, "/a").await.unwrap();
    b.repo.cove_folder_create(&b.cove_id, "/a/b").await.unwrap();

    let (status, body) = get(b.app.clone(), "/api/coves/resolve?path=/a/b/c").await;
    assert_eq!(status, StatusCode::OK);
    // More-specific (longest-prefix) wins.
    assert_eq!(body["folder_path"].as_str().unwrap(), "/a/b");
}

// (11) --------------------------------------------------------------

#[tokio::test]
async fn resolve_miss_returns_200_null() {
    let b = boot().await;
    // No claims at all — resolve should still return 200 with body == null.
    let (status, body) = get(b.app.clone(), "/api/coves/resolve?path=/anywhere").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_null(), "expected null body, got {body}");
}

// (12) --------------------------------------------------------------

#[tokio::test]
async fn resolve_non_absolute_path_400() {
    let b = boot().await;
    let (status, body) = get(b.app.clone(), "/api/coves/resolve?path=relative").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"].as_str().unwrap(), "bad_request");
}

// (13) --------------------------------------------------------------

#[tokio::test]
async fn cascade_delete_cove_drops_its_folders() {
    let b = boot().await;
    post(
        b.app.clone(),
        &format!("/api/coves/{}/folders", b.cove_id),
        json!({"path": "/cascade-target"}),
    )
    .await;

    // Sanity-check the row exists before the cove deletion.
    let pre = b.repo.cove_folders_by_cove(&b.cove_id).await.unwrap();
    assert_eq!(pre.len(), 1);

    // Drop the cove via the REST surface (the route handler does the
    // terminal-reap + cove_delete dance; cove_folders rows ride the
    // FK cascade declared in migration 0015).
    let status = delete(b.app.clone(), &format!("/api/coves/{}", b.cove_id)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let post_drop = b.repo.cove_folders_by_cove(&b.cove_id).await.unwrap();
    assert_eq!(
        post_drop.len(),
        0,
        "cove_folders rows should cascade away with their cove"
    );
}

// (14) --------------------------------------------------------------

#[tokio::test]
async fn post_to_unknown_cove_returns_404() {
    let b = boot().await;
    // The cove_id in the path is a well-formed UUID that simply has
    // no row in `coves`. The repo layer surfaces this as NotFound
    // (see sqlite::cove_folder_create) instead of leaking the raw FK
    // error to the REST caller.
    let bogus = "00000000-0000-0000-0000-000000000000";
    let (status, _) = post(
        b.app.clone(),
        &format!("/api/coves/{bogus}/folders"),
        json!({"path": "/x"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// (15) --------------------------------------------------------------

#[tokio::test]
async fn get_returns_only_own_cove_folders() {
    let b = boot().await;
    // Boot already created cove A (`b.cove_id`); add a second cove B
    // alongside it and claim a non-overlapping path under each.
    let cove_b = b
        .repo
        .cove_create(NewCove {
            name: "folders-test-b".into(),
            color: "#111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let cove_b_id = cove_b.id.to_string();

    let (sa, _) = post(
        b.app.clone(),
        &format!("/api/coves/{}/folders", b.cove_id),
        json!({"path": "/path-a"}),
    )
    .await;
    assert_eq!(sa, StatusCode::CREATED);
    let (sb, _) = post(
        b.app.clone(),
        &format!("/api/coves/{cove_b_id}/folders"),
        json!({"path": "/path-b"}),
    )
    .await;
    assert_eq!(sb, StatusCode::CREATED);

    let (status, list_a) = get(b.app.clone(), &format!("/api/coves/{}/folders", b.cove_id)).await;
    assert_eq!(status, StatusCode::OK);
    let arr_a = list_a.as_array().unwrap();
    assert_eq!(arr_a.len(), 1);
    assert_eq!(arr_a[0]["path"].as_str().unwrap(), "/path-a");
    assert_eq!(arr_a[0]["cove_id"].as_str().unwrap(), b.cove_id);

    let (status, list_b) = get(b.app.clone(), &format!("/api/coves/{cove_b_id}/folders")).await;
    assert_eq!(status, StatusCode::OK);
    let arr_b = list_b.as_array().unwrap();
    assert_eq!(arr_b.len(), 1);
    assert_eq!(arr_b[0]["path"].as_str().unwrap(), "/path-b");
    assert_eq!(arr_b[0]["cove_id"].as_str().unwrap(), cove_b_id);
}

// (16) --------------------------------------------------------------

#[tokio::test]
async fn cross_cove_overlap_409_descendant() {
    // Cases (3) and (4) already cover ancestor/descendant overlap
    // within a single cove; this case pins that the conflict check
    // is correctly cove-agnostic — i.e. cove B cannot claim a path
    // that overlaps with a claim already held by cove A. Was
    // previously covered only by an e2e spec; folded down to a Rust
    // integration test to keep the conflict-invariant coverage in
    // one place (see also the dropped `create-folder refuses
    // ancestor/descendant overlap` block in
    // `web/e2e/a11y-cwd-resolve.spec.ts`).
    let b = boot().await;
    let cove_b = b
        .repo
        .cove_create(NewCove {
            name: "folders-test-cross".into(),
            color: "#333".into(),
            sort: None,
        })
        .await
        .unwrap();
    let cove_b_id = cove_b.id.to_string();

    // Cove A claims the parent.
    let (s1, _) = post(
        b.app.clone(),
        &format!("/api/coves/{}/folders", b.cove_id),
        json!({"path": "/cross/parent"}),
    )
    .await;
    assert_eq!(s1, StatusCode::CREATED);

    // Cove B tries to claim a descendant of cove A's path → 409.
    let (s2, body) = post(
        b.app.clone(),
        &format!("/api/coves/{cove_b_id}/folders"),
        json!({"path": "/cross/parent/child"}),
    )
    .await;
    assert_eq!(s2, StatusCode::CONFLICT);
    assert_eq!(body["conflict_kind"].as_str().unwrap(), "descendant");
    // The conflict body names the existing claim's cove (A), not
    // the caller's cove (B) — the frontend needs this to render a
    // meaningful "owned by <other cove>" message.
    assert_eq!(body["cove_id"].as_str().unwrap(), b.cove_id);
    assert_eq!(body["conflict_path"].as_str().unwrap(), "/cross/parent");

    // Reverse direction: cove B tries to claim an ancestor of an
    // existing cove-A deep claim → 409 ancestor.
    let (s3, _) = post(
        b.app.clone(),
        &format!("/api/coves/{}/folders", b.cove_id),
        json!({"path": "/cross/deep/inner"}),
    )
    .await;
    assert_eq!(s3, StatusCode::CREATED);
    let (s4, body) = post(
        b.app.clone(),
        &format!("/api/coves/{cove_b_id}/folders"),
        json!({"path": "/cross/deep"}),
    )
    .await;
    assert_eq!(s4, StatusCode::CONFLICT);
    assert_eq!(body["conflict_kind"].as_str().unwrap(), "ancestor");
    assert_eq!(body["cove_id"].as_str().unwrap(), b.cove_id);
    assert_eq!(body["conflict_path"].as_str().unwrap(), "/cross/deep/inner");
}

// (17) --------------------------------------------------------------

#[tokio::test]
async fn delete_with_mismatched_cove_id_returns_404() {
    let b = boot().await;
    // Cove A is `b.cove_id`; add a second cove B to mismatch against.
    let cove_b = b
        .repo
        .cove_create(NewCove {
            name: "folders-test-b".into(),
            color: "#222".into(),
            sort: None,
        })
        .await
        .unwrap();
    let cove_b_id = cove_b.id.to_string();

    // Claim a folder under cove A.
    let (_, body) = post(
        b.app.clone(),
        &format!("/api/coves/{}/folders", b.cove_id),
        json!({"path": "/owned-by-a"}),
    )
    .await;
    let folder_id = body["id"].as_i64().unwrap();

    // Deleting via cove B's URL must not succeed — the route checks
    // the folder's cove_id matches the path segment and surfaces a
    // mismatch as NotFound (intentionally not 403, see route doc).
    let status = delete(
        b.app.clone(),
        &format!("/api/coves/{cove_b_id}/folders/{folder_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The folder still exists under cove A.
    let (_, list) = get(b.app.clone(), &format!("/api/coves/{}/folders", b.cove_id)).await;
    let arr = list.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"].as_i64().unwrap(), folder_id);
}
