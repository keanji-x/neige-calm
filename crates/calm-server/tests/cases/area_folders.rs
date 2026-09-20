//! Integration tests for the area ↔ folder mapping surface; pure CRUD, no daemon binary required.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::NewArea;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::track_area_cache::TrackAreaCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    area_id: String,
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
    let area = repo
        .area_create(NewArea {
            name: "folders-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();

    // `area_folders` never needs the session daemon — the DaemonClient is a stub pointing at /dev/null.
    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: None,
    });
    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    let track_area_cache = TrackAreaCache::new();
    repo.seed_track_area_cache(&track_area_cache).await.unwrap();
    let state = AppState::from_parts(
        repo.clone(),
        events,
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-area-folders-test"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                card_role_cache.clone(),
                track_area_cache.clone(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache),
        Some(track_area_cache),
    );

    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);

    Boot {
        app,
        area_id: area.id.to_string(),
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

#[tokio::test]
async fn post_then_get_returns_the_folder() {
    let b = boot().await;
    let (status, body) = post(
        b.app.clone(),
        &format!("/api/areas/{}/folders", b.area_id),
        json!({"path": "/a"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(body["path"].as_str().unwrap(), "/a");
    assert_eq!(body["area_id"].as_str().unwrap(), b.area_id);

    let (status, body) = get(b.app.clone(), &format!("/api/areas/{}/folders", b.area_id)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["path"].as_str().unwrap(), "/a");
}

#[tokio::test]
async fn post_same_path_twice_409_equal() {
    let b = boot().await;
    let uri = format!("/api/areas/{}/folders", b.area_id);
    let (s1, _) = post(b.app.clone(), &uri, json!({"path": "/a"})).await;
    assert_eq!(s1, StatusCode::CREATED);
    let (s2, body) = post(b.app.clone(), &uri, json!({"path": "/a"})).await;
    assert_eq!(s2, StatusCode::CONFLICT);
    assert_eq!(body["conflict_kind"].as_str().unwrap(), "equal");
    assert_eq!(body["conflict_path"].as_str().unwrap(), "/a");
    assert!(body["folder_id"].is_number());
}

#[tokio::test]
async fn post_ancestor_when_descendant_exists_409_ancestor() {
    let b = boot().await;
    let uri = format!("/api/areas/{}/folders", b.area_id);
    let (s1, _) = post(b.app.clone(), &uri, json!({"path": "/a/b"})).await;
    assert_eq!(s1, StatusCode::CREATED);
    let (s2, body) = post(b.app.clone(), &uri, json!({"path": "/a"})).await;
    assert_eq!(s2, StatusCode::CONFLICT);
    assert_eq!(body["conflict_kind"].as_str().unwrap(), "ancestor");
    assert_eq!(body["conflict_path"].as_str().unwrap(), "/a/b");
}

#[tokio::test]
async fn post_descendant_when_ancestor_exists_409_descendant() {
    let b = boot().await;
    let uri = format!("/api/areas/{}/folders", b.area_id);
    let (s1, _) = post(b.app.clone(), &uri, json!({"path": "/a"})).await;
    assert_eq!(s1, StatusCode::CREATED);
    let (s2, body) = post(b.app.clone(), &uri, json!({"path": "/a/b"})).await;
    assert_eq!(s2, StatusCode::CONFLICT);
    assert_eq!(body["conflict_kind"].as_str().unwrap(), "descendant");
    assert_eq!(body["conflict_path"].as_str().unwrap(), "/a");
}

#[tokio::test]
async fn post_non_absolute_path_400() {
    let b = boot().await;
    let (status, body) = post(
        b.app.clone(),
        &format!("/api/areas/{}/folders", b.area_id),
        json!({"path": "relative/path"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"].as_str().unwrap(), "bad_request");
}

#[tokio::test]
async fn post_trailing_slash_is_normalized_and_conflicts() {
    let b = boot().await;
    let uri = format!("/api/areas/{}/folders", b.area_id);
    let (s1, body1) = post(b.app.clone(), &uri, json!({"path": "/a/"})).await;
    assert_eq!(s1, StatusCode::CREATED);
    assert_eq!(body1["path"].as_str().unwrap(), "/a");

    let (s2, body2) = post(b.app.clone(), &uri, json!({"path": "/a"})).await;
    assert_eq!(s2, StatusCode::CONFLICT);
    assert_eq!(body2["conflict_kind"].as_str().unwrap(), "equal");
}

#[tokio::test]
async fn delete_removes_the_folder() {
    let b = boot().await;
    let uri = format!("/api/areas/{}/folders", b.area_id);
    let (_, body) = post(b.app.clone(), &uri, json!({"path": "/a"})).await;
    let folder_id = body["id"].as_i64().unwrap();

    let status = delete(
        b.app.clone(),
        &format!("/api/areas/{}/folders/{folder_id}", b.area_id),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, list) = get(b.app.clone(), &uri).await;
    assert_eq!(list.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn resolve_hits_self() {
    let b = boot().await;
    let (_, body) = post(
        b.app.clone(),
        &format!("/api/areas/{}/folders", b.area_id),
        json!({"path": "/a"}),
    )
    .await;
    let folder_id = body["id"].as_i64().unwrap();

    let (status, body) = get(b.app.clone(), "/api/areas/resolve?path=/a").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["area_id"].as_str().unwrap(), b.area_id);
    assert_eq!(body["folder_id"].as_i64().unwrap(), folder_id);
    assert_eq!(body["folder_path"].as_str().unwrap(), "/a");
}

#[tokio::test]
async fn resolve_hits_descendant() {
    let b = boot().await;
    post(
        b.app.clone(),
        &format!("/api/areas/{}/folders", b.area_id),
        json!({"path": "/a"}),
    )
    .await;

    let (status, body) = get(b.app.clone(), "/api/areas/resolve?path=/a/b/c").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["folder_path"].as_str().unwrap(), "/a");
}

#[tokio::test]
async fn resolve_tolerates_corrupt_overlapping_rows() {
    // Both rows are seeded through the unchecked repo primitive (a state only a corrupted DB reaches). The winner
    // `/a` IS a contract: `area_folders_list_all` is `ORDER BY path ASC` and `find_owner` takes the first match.
    let b = boot().await;
    b.repo.area_folder_create(&b.area_id, "/a").await.unwrap();
    b.repo.area_folder_create(&b.area_id, "/a/b").await.unwrap();

    let (status, body) = get(b.app.clone(), "/api/areas/resolve?path=/a/b/c").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["folder_path"].as_str().unwrap(),
        "/a",
        "ORDER BY path ASC + first-match must resolve to the shortest claim"
    );
    assert_eq!(body["area_id"].as_str().unwrap(), b.area_id);
}

/// The conflict scan and the INSERT must share ONE `BEGIN IMMEDIATE` transaction: `UNIQUE(path)` rejects only
/// equal paths, never overlap. On-disk DB on purpose — shared-cache `sqlite::memory:` gives readers table-level
/// locks that would mask the difference.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn overlapping_claim_cannot_slip_between_scan_and_insert() {
    use calm_server::area_folder_claim::AreaFolderClaim;
    use calm_server::db::sqlite::begin_immediate_tx;

    let tmp = TempDir::new().expect("tempdir");
    let url = format!("sqlite://{}?mode=rwc", tmp.path().join("calm.db").display());
    let repo = Arc::new(SqlxRepo::open(&url).await.expect("open on-disk sqlite"));
    let area = repo
        .area_create(NewArea {
            name: "atomic-claim".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let area_id = area.id.to_string();

    let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
    sqlx::query("INSERT INTO area_folders (area_id, path, created_at) VALUES (?1, ?2, ?3)")
        .bind(&area_id)
        .bind("/a/b")
        .bind(0_i64)
        .execute(&mut *tx)
        .await
        .unwrap();

    let repo_b = repo.clone();
    let area_b = area_id.clone();
    let claim = tokio::spawn(async move { repo_b.area_folder_create_checked(&area_b, "/a").await });

    // The sleep is load-bearing only for a non-atomic mutant (scan on one connection, insert on another): it lets
    // the pre-commit snapshot land first. B blocks inside SQLite's own lock, which exposes no observable edge.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    tx.commit().await.unwrap();

    let outcome = claim.await.unwrap().expect("claim must not error");
    assert!(
        matches!(outcome, AreaFolderClaim::Conflict(_)),
        "claiming `/a` must see the just-committed `/a/b`; a scan on a \
         separate connection would have missed it and created the row"
    );

    let all = repo.area_folders_list_all().await.unwrap();
    let paths: Vec<&str> = all.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        vec!["/a/b"],
        "exactly one claim may survive; overlapping rows are the corrupt state"
    );
}

#[tokio::test]
async fn resolve_miss_returns_200_null() {
    let b = boot().await;
    let (status, body) = get(b.app.clone(), "/api/areas/resolve?path=/anywhere").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_null(), "expected null body, got {body}");
}

#[tokio::test]
async fn resolve_non_absolute_path_400() {
    let b = boot().await;
    let (status, body) = get(b.app.clone(), "/api/areas/resolve?path=relative").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"].as_str().unwrap(), "bad_request");
}

#[tokio::test]
async fn cascade_delete_area_drops_its_folders() {
    let b = boot().await;
    post(
        b.app.clone(),
        &format!("/api/areas/{}/folders", b.area_id),
        json!({"path": "/cascade-target"}),
    )
    .await;

    let pre = b.repo.area_folders_by_area(&b.area_id).await.unwrap();
    assert_eq!(pre.len(), 1);

    // area_folders rows ride the FK cascade.
    let status = delete(b.app.clone(), &format!("/api/areas/{}", b.area_id)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let post_drop = b.repo.area_folders_by_area(&b.area_id).await.unwrap();
    assert_eq!(
        post_drop.len(),
        0,
        "area_folders rows should cascade away with their area"
    );
}

#[tokio::test]
async fn post_to_unknown_area_returns_404() {
    let b = boot().await;
    // A well-formed UUID with no row: the repo surfaces NotFound instead of leaking the raw FK error.
    let bogus = "00000000-0000-0000-0000-000000000000";
    let (status, _) = post(
        b.app.clone(),
        &format!("/api/areas/{bogus}/folders"),
        json!({"path": "/x"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_returns_only_own_area_folders() {
    let b = boot().await;
    let area_b = b
        .repo
        .area_create(NewArea {
            name: "folders-test-b".into(),
            color: "#111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let area_b_id = area_b.id.to_string();

    let (sa, _) = post(
        b.app.clone(),
        &format!("/api/areas/{}/folders", b.area_id),
        json!({"path": "/path-a"}),
    )
    .await;
    assert_eq!(sa, StatusCode::CREATED);
    let (sb, _) = post(
        b.app.clone(),
        &format!("/api/areas/{area_b_id}/folders"),
        json!({"path": "/path-b"}),
    )
    .await;
    assert_eq!(sb, StatusCode::CREATED);

    let (status, list_a) = get(b.app.clone(), &format!("/api/areas/{}/folders", b.area_id)).await;
    assert_eq!(status, StatusCode::OK);
    let arr_a = list_a.as_array().unwrap();
    assert_eq!(arr_a.len(), 1);
    assert_eq!(arr_a[0]["path"].as_str().unwrap(), "/path-a");
    assert_eq!(arr_a[0]["area_id"].as_str().unwrap(), b.area_id);

    let (status, list_b) = get(b.app.clone(), &format!("/api/areas/{area_b_id}/folders")).await;
    assert_eq!(status, StatusCode::OK);
    let arr_b = list_b.as_array().unwrap();
    assert_eq!(arr_b.len(), 1);
    assert_eq!(arr_b[0]["path"].as_str().unwrap(), "/path-b");
    assert_eq!(arr_b[0]["area_id"].as_str().unwrap(), area_b_id);
}

#[tokio::test]
async fn cross_area_overlap_409_descendant() {
    let b = boot().await;
    let area_b = b
        .repo
        .area_create(NewArea {
            name: "folders-test-cross".into(),
            color: "#333".into(),
            sort: None,
        })
        .await
        .unwrap();
    let area_b_id = area_b.id.to_string();

    let (s1, _) = post(
        b.app.clone(),
        &format!("/api/areas/{}/folders", b.area_id),
        json!({"path": "/cross/parent"}),
    )
    .await;
    assert_eq!(s1, StatusCode::CREATED);

    let (s2, body) = post(
        b.app.clone(),
        &format!("/api/areas/{area_b_id}/folders"),
        json!({"path": "/cross/parent/child"}),
    )
    .await;
    assert_eq!(s2, StatusCode::CONFLICT);
    assert_eq!(body["conflict_kind"].as_str().unwrap(), "descendant");
    // The conflict body names the existing claim's area, not the caller's.
    assert_eq!(body["area_id"].as_str().unwrap(), b.area_id);
    assert_eq!(body["conflict_path"].as_str().unwrap(), "/cross/parent");

    let (s3, _) = post(
        b.app.clone(),
        &format!("/api/areas/{}/folders", b.area_id),
        json!({"path": "/cross/deep/inner"}),
    )
    .await;
    assert_eq!(s3, StatusCode::CREATED);
    let (s4, body) = post(
        b.app.clone(),
        &format!("/api/areas/{area_b_id}/folders"),
        json!({"path": "/cross/deep"}),
    )
    .await;
    assert_eq!(s4, StatusCode::CONFLICT);
    assert_eq!(body["conflict_kind"].as_str().unwrap(), "ancestor");
    assert_eq!(body["area_id"].as_str().unwrap(), b.area_id);
    assert_eq!(body["conflict_path"].as_str().unwrap(), "/cross/deep/inner");
}

#[tokio::test]
async fn delete_with_mismatched_area_id_returns_404() {
    let b = boot().await;
    let area_b = b
        .repo
        .area_create(NewArea {
            name: "folders-test-b".into(),
            color: "#222".into(),
            sort: None,
        })
        .await
        .unwrap();
    let area_b_id = area_b.id.to_string();

    let (_, body) = post(
        b.app.clone(),
        &format!("/api/areas/{}/folders", b.area_id),
        json!({"path": "/owned-by-a"}),
    )
    .await;
    let folder_id = body["id"].as_i64().unwrap();

    // An area_id mismatch surfaces as NotFound, intentionally not 403.
    let status = delete(
        b.app.clone(),
        &format!("/api/areas/{area_b_id}/folders/{folder_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (_, list) = get(b.app.clone(), &format!("/api/areas/{}/folders", b.area_id)).await;
    let arr = list.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"].as_i64().unwrap(), folder_id);
}
