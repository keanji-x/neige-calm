//! The `deletable` card bit: repo round-trip, migration backfill, the REST DELETE guard, the track-delete
//! cascade, and PATCH rejection.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{CardRole, NewArea, NewCard, NewOverlay, NewTrack};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::common;
use crate::support::git_helpers::attached_repo_fixture;
struct Boot {
    app: axum::Router,
    area_id: String,
    repo: Arc<dyn Repo>,
    tmp: TempDir,
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
            name: "deletable-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();

    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: None,
    });
    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    let track_area_cache = calm_server::track_area_cache::TrackAreaCache::new();
    repo.seed_track_area_cache(&track_area_cache).await.unwrap();
    let state = AppState::from_parts(
        repo.clone(),
        events,
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-deletable-test"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                card_role_cache.clone(),
                track_area_cache.clone(),
            ),
        )),
        Arc::new(common::fake_codex_client()),
        Some(card_role_cache.clone()),
        Some(track_area_cache.clone()),
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
        tmp,
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

async fn delete(app: axum::Router, uri: &str) -> StatusCode {
    delete_with_body(app, uri).await.0
}

async fn delete_with_body(app: axum::Router, uri: &str) -> (StatusCode, Value) {
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
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

async fn patch(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
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

async fn insert_held_workspace_lease(
    boot: &Boot,
    lease_id: &str,
    card_id: &str,
    track_id: &str,
) -> String {
    let lease_path = boot
        .tmp
        .path()
        .join("workspace-leases")
        .join(track_id)
        .join(card_id);
    std::fs::create_dir_all(&lease_path).unwrap();
    let lease_path = lease_path.to_str().unwrap().to_string();
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    sqlx::query(
        r#"INSERT INTO workspace_leases (
               lease_id, card_id, track_id, path, state, lease_owner,
               lease_until_ms, boot_id, created_at_ms, updated_at_ms
           )
           VALUES (?1, ?2, ?3, ?4, 'held', ?5, ?6, NULL, ?7, ?7)"#,
    )
    .bind(lease_id)
    .bind(card_id)
    .bind(track_id)
    .bind(&lease_path)
    .bind("owner-delete-test")
    .bind(60_000_i64)
    .bind(1_i64)
    .execute(&pool)
    .await
    .unwrap();
    lease_path
}

#[tokio::test]
async fn card_create_with_id_tx_round_trips_deletable_bit() {
    let repo = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open in-memory sqlite");
    let area = repo
        .area_create(NewArea {
            name: "c".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "w".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let cache = CardRoleCache::new();

    let mut tx = repo.pool().begin().await.unwrap();
    let deletable_card = calm_server::db::sqlite::card_create_with_id_tx(
        &mut tx,
        calm_server::model::new_id(),
        NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "terminal".into(),
            sort: None,
            payload: json!({}),
        },
        CardRole::Worker,
        true,
        &cache,
    )
    .await
    .unwrap();

    // Role is Worker here to isolate the `deletable` axis from the role axis.
    let undeletable_card = calm_server::db::sqlite::card_create_with_id_tx(
        &mut tx,
        calm_server::model::new_id(),
        NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "terminal".into(),
            sort: None,
            payload: json!({}),
        },
        CardRole::Worker,
        false,
        &cache,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    assert!(deletable_card.deletable);
    assert!(!undeletable_card.deletable);

    let got_deletable = repo
        .card_get(deletable_card.id.as_str())
        .await
        .unwrap()
        .expect("deletable card");
    assert!(got_deletable.deletable);
    let got_undeletable = repo
        .card_get(undeletable_card.id.as_str())
        .await
        .unwrap()
        .expect("undeletable card");
    assert!(!got_undeletable.deletable);

    let listed = repo.cards_by_track(track.id.as_str()).await.unwrap();
    assert_eq!(listed.len(), 2);
    let by_id: std::collections::HashMap<_, _> = listed
        .iter()
        .map(|c| (c.id.as_str().to_string(), c))
        .collect();
    assert!(by_id.get(deletable_card.id.as_str()).unwrap().deletable);
    assert!(!by_id.get(undeletable_card.id.as_str()).unwrap().deletable);
}

#[tokio::test]
async fn planner_card_minted_by_track_create_is_undeletable() {
    let boot = boot().await;
    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({"planner_provider": "codex", "area_id": boot.area_id, "title": "w", "cwd": attached_repo_fixture("issue-250-pr2-test"), "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "track create returned: {body}");
    let track_id = body
        .get("id")
        .and_then(Value::as_str)
        .expect("track id in response")
        .to_string();

    let cards = boot.repo.cards_by_track(&track_id).await.unwrap();
    // Track create mints two kernel-owned cards in one tx; the report card sorts ahead (`sort = -1.0`).
    assert_eq!(
        cards.len(),
        2,
        "track create mints planner + track-report; got {} cards",
        cards.len(),
    );
    assert!(
        cards.iter().all(|c| !c.deletable),
        "both planner and track-report cards must be undeletable; got: {:?}",
        cards
            .iter()
            .map(|c| (c.kind.clone(), c.deletable))
            .collect::<Vec<_>>(),
    );
    let kinds: Vec<&str> = cards.iter().map(|c| c.kind.as_str()).collect();
    assert!(
        kinds.contains(&"codex"),
        "planner card kind is codex; got {kinds:?}"
    );
    assert!(
        kinds.contains(&"track-report"),
        "track-report card present; got {kinds:?}"
    );
}

#[tokio::test]
async fn delete_card_returns_403_for_undeletable_planner_card() {
    let boot = boot().await;
    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({"planner_provider": "codex", "area_id": boot.area_id, "title": "w", "cwd": attached_repo_fixture("issue-250-pr2-test"), "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "track create body: {body}");
    let track_id = body["id"].as_str().unwrap().to_string();
    let cards = boot.repo.cards_by_track(&track_id).await.unwrap();
    let planner_card = cards
        .iter()
        .find(|c| c.kind == "codex")
        .expect("planner card present");
    let planner_card_id = planner_card.id.as_str().to_string();
    assert!(!planner_card.deletable);

    let status = delete(boot.app.clone(), &format!("/api/cards/{planner_card_id}")).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "planner card delete must be refused with 403"
    );

    let after = boot.repo.card_get(&planner_card_id).await.unwrap();
    assert!(
        after.is_some(),
        "planner card row must survive the refused delete"
    );
}

#[tokio::test]
async fn delete_card_returns_204_for_deletable_worker_card() {
    let boot = boot().await;
    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({"planner_provider": "codex", "area_id": boot.area_id, "title": "w", "cwd": attached_repo_fixture("issue-250-pr2-test"), "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "track create body: {body}");
    let track_id = body["id"].as_str().unwrap().to_string();

    let (status, body) = post(
        boot.app.clone(),
        &format!("/api/tracks/{track_id}/cards"),
        json!({"kind": "plugin:t:v"}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "worker card create body: {body}"
    );
    let worker_card_id = body["id"].as_str().unwrap().to_string();

    let status = delete(boot.app.clone(), &format!("/api/cards/{worker_card_id}")).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "worker user-deletable card delete returns 204"
    );

    let after = boot.repo.card_get(&worker_card_id).await.unwrap();
    assert!(after.is_none(), "worker card row removed");
}

#[tokio::test]
async fn delete_card_releases_active_workspace_lease_row_before_card_row_delete() {
    let boot = boot().await;
    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({"planner_provider": "codex", "area_id": boot.area_id, "title": "w", "cwd": attached_repo_fixture("issue-760-card-delete-lease"), "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "track create body: {body}");
    let track_id = body["id"].as_str().unwrap().to_string();

    let (status, body) = post(
        boot.app.clone(),
        &format!("/api/tracks/{track_id}/cards"),
        json!({"kind": "plugin:t:v"}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "worker card create body: {body}"
    );
    let card_id = body["id"].as_str().unwrap().to_string();
    let lease_id = format!("lease-{card_id}");
    let lease_path = insert_held_workspace_lease(&boot, &lease_id, &card_id, &track_id).await;
    assert!(std::path::Path::new(&lease_path).is_dir());

    let status = delete(boot.app.clone(), &format!("/api/cards/{card_id}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    assert!(
        std::path::Path::new(&lease_path).is_dir(),
        "card delete releases the row without removing lease artifacts"
    );
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    let state: String =
        sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
            .bind(&lease_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "released");
    let released_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'workspace.released'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(released_events, 1);
}

#[tokio::test]
async fn track_delete_cascades_to_undeletable_planner_card() {
    let boot = boot().await;
    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({"planner_provider": "codex", "area_id": boot.area_id, "title": "w", "cwd": attached_repo_fixture("issue-250-pr2-test"), "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "track create body: {body}");
    let track_id = body["id"].as_str().unwrap().to_string();
    let cards = boot.repo.cards_by_track(&track_id).await.unwrap();
    let planner_card = cards
        .iter()
        .find(|c| c.kind == "codex")
        .expect("planner card present");
    let planner_card_id = planner_card.id.as_str().to_string();
    assert!(!planner_card.deletable);

    // `terminals.card_id` is ON DELETE RESTRICT; the route's terminal-reap step handles that.
    let status = delete(boot.app.clone(), &format!("/api/tracks/{track_id}")).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "track delete must succeed even with an undeletable child card"
    );

    let after_track = boot.repo.track_get(&track_id).await.unwrap();
    assert!(after_track.is_none());
    let after_card = boot.repo.card_get(&planner_card_id).await.unwrap();
    assert!(
        after_card.is_none(),
        "planner card cascade-deleted with track"
    );
}

#[tokio::test]
async fn acceptance_20_track_delete_route_refuses_descendant_and_names_child() {
    let boot = boot().await;
    let parent = boot
        .repo
        .track_create(NewTrack {
            area_id: boot.area_id.clone().into(),
            title: "parent".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let child = boot
        .repo
        .track_create(NewTrack {
            area_id: boot.area_id.clone().into(),
            title: "child".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET parent_track_id=?1 WHERE id=?2")
        .bind(parent.id.as_str())
        .bind(child.id.as_str())
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();

    let (status, body) =
        delete_with_body(boot.app.clone(), &format!("/api/tracks/{}", parent.id)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.to_string().contains(child.id.as_str()), "{body}");
    assert!(
        boot.repo
            .track_get(parent.id.as_str())
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn rest_track_create_cannot_set_parent_track_id() {
    let boot = boot().await;
    let (status, body) = post(
        boot.app,
        "/api/tracks",
        json!({
            "planner_provider": "codex",
            "area_id": boot.area_id,
            "title": "forged child",
            "cwd": attached_repo_fixture("forged-child"),
            "attach_folder": true,
            "theme": {"fg": [216,219,226], "bg": [15,20,24]},
            "parent_track_id": "track-forged-parent"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM tracks")
        .fetch_one(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn track_delete_releases_active_workspace_lease_rows_before_cascade() {
    let boot = boot().await;
    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({"planner_provider": "codex", "area_id": boot.area_id, "title": "w", "cwd": attached_repo_fixture("issue-760-track-delete-lease"), "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "track create body: {body}");
    let track_id = body["id"].as_str().unwrap().to_string();
    let cards = boot.repo.cards_by_track(&track_id).await.unwrap();
    let card_id = cards[0].id.as_str().to_string();
    let lease_id = format!("lease-{card_id}");
    let lease_path = insert_held_workspace_lease(&boot, &lease_id, &card_id, &track_id).await;
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    assert!(std::path::Path::new(&lease_path).is_dir());

    let status = delete(boot.app.clone(), &format!("/api/tracks/{track_id}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    assert!(
        std::path::Path::new(&lease_path).is_dir(),
        "track delete does not remove non-track-root lease artifacts"
    );
    let remaining: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM workspace_leases WHERE track_id = ?1")
            .bind(&track_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(remaining, 0, "track cascade removes released lease rows");
    let released_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'workspace.released'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(released_events, 1);
}

#[tokio::test]
async fn area_delete_releases_track_workspace_lease_rows_before_cascade() {
    let boot = boot().await;
    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({"planner_provider": "codex", "area_id": boot.area_id, "title": "w", "cwd": attached_repo_fixture("issue-760-area-delete-lease"), "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "track create body: {body}");
    let track_id = body["id"].as_str().unwrap().to_string();
    let cards = boot.repo.cards_by_track(&track_id).await.unwrap();
    let card_id = cards[0].id.as_str().to_string();
    let lease_id = format!("lease-{card_id}");
    let lease_path = insert_held_workspace_lease(&boot, &lease_id, &card_id, &track_id).await;
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    assert!(std::path::Path::new(&lease_path).is_dir());

    let (status, body) =
        delete_with_body(boot.app.clone(), &format!("/api/areas/{}", boot.area_id)).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "delete body: {body}");

    assert!(
        std::path::Path::new(&lease_path).is_dir(),
        "area delete does not remove non-track-root lease artifacts"
    );
    let remaining: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM workspace_leases WHERE lease_id = ?1")
            .bind(&lease_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(remaining, 0, "area cascade removes released lease row");
    let released_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'workspace.released'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(released_events, 1);
}

#[tokio::test]
async fn track_delete_route_sweeps_card_track_and_view_overlays() {
    let boot = boot().await;
    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({"planner_provider": "codex", "area_id": boot.area_id, "title": "w", "cwd": attached_repo_fixture("issue-454-route-overlay-test"), "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "track create body: {body}");
    let track_id = body["id"].as_str().unwrap().to_string();
    let card = boot
        .repo
        .card_create(NewCard {
            track_id: track_id.clone().into(),
            title: None,
            kind: "terminal".into(),
            sort: None,
            payload: json!({"title": "worker"}),
        })
        .await
        .unwrap();

    for (entity_kind, entity_id) in [
        ("card", card.id.as_str()),
        ("track", track_id.as_str()),
        ("view", track_id.as_str()),
    ] {
        boot.repo
            .overlay_upsert(NewOverlay {
                plugin_id: "route-test".into(),
                entity_kind: entity_kind.into(),
                entity_id: entity_id.into(),
                kind: "status".into(),
                payload: json!({"schemaVersion": 1, "state": "idle"}),
            })
            .await
            .unwrap();
    }

    let status = delete(boot.app.clone(), &format!("/api/tracks/{track_id}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    assert!(
        boot.repo
            .overlays_for("card", card.id.as_str())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        boot.repo
            .overlays_for("track", &track_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        boot.repo
            .overlays_for("view", &track_id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn patch_card_with_deletable_returns_400() {
    let boot = boot().await;
    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({"planner_provider": "codex", "area_id": boot.area_id, "title": "w", "cwd": attached_repo_fixture("issue-250-pr2-test"), "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "track create body: {body}");
    let track_id = body["id"].as_str().unwrap().to_string();
    let (status, body) = post(
        boot.app.clone(),
        &format!("/api/tracks/{track_id}/cards"),
        json!({"kind": "plugin:t:v"}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "worker card create body: {body}"
    );
    let card_id = body["id"].as_str().unwrap().to_string();

    // Rejected even when the value matches the current row — the field is kernel-managed, not "stable-write-allowed".
    let (status, body) = patch(
        boot.app.clone(),
        &format!("/api/cards/{card_id}"),
        json!({"deletable": false}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "patching deletable must 400; body={body}",
    );
}
