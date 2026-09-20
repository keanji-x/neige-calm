//! Integration tests for per-kind payload validators wired into the `cards` and `overlays` route layer:
//! a minimal Axum app with a stub-only AppState, driven through `tower::ServiceExt::oneshot`.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{BroadcastEnvelope, Event, EventBus, EventScope};
use calm_server::ids::ActorId;
use calm_server::model::{NewArea, NewCard, NewOverlay, NewTrack, TrackLifecycle, TrackPatch};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::routes::tracks::{
    TrackLifecyclePatchRaceHook, install_track_lifecycle_patch_race_hook_for_test,
};
use calm_server::state::{AppState, DaemonClient};
use calm_server::validation::SERVER_OWNED_TERMINAL_PAYLOAD_KEYS;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// Minimal AppState + one seeded area + track; returns the track id.
async fn boot() -> (AppState, String) {
    let (state, track_id, _repo) = boot_with_repo().await;
    (state, track_id)
}

/// [`boot`] variant that also hands back the full-capability repo: `AppState.repo` is the narrower `RouteRepo` with no `sqlite_pool` escape hatch.
async fn boot_with_repo() -> (AppState, String, Arc<dyn Repo>) {
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite repo"),
    );
    let area = repo
        .area_create(NewArea {
            name: "demo".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "demo".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let state = AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::track_area_cache::TrackAreaCache::new(),
            ),
        )),
        Arc::new(calm_server::state::CodexClient::new_stub()),
        None,
        None,
    );
    (state, track.id.to_string(), repo)
}

fn app(state: AppState) -> axum::Router {
    // The cards / overlays handlers extract `Actor` from request extensions, so the middleware that populates it must be layered on, mirroring main.rs.
    axum::Router::new()
        .merge(routes::cards::router())
        .merge(routes::overlays::router())
        .merge(routes::tracks::router())
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state)
}

async fn body_to_json(resp: axum::http::Response<Body>) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn collect_envelopes(events: EventBus, n: usize) -> Vec<BroadcastEnvelope> {
    let mut rx = events.subscribe();
    let mut out = Vec::with_capacity(n);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while out.len() < n {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!("expected {n} envelopes; got {}", out.len());
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(env)) => out.push(env),
            Ok(Err(err)) => panic!("broadcast recv error: {err:?}"),
            Err(_) => continue,
        }
    }
    out
}

async fn post_card(app: axum::Router, track_id: &str, body: Value) -> axum::http::Response<Body> {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri(format!("/api/tracks/{track_id}/cards"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn patch_card(app: axum::Router, card_id: &str, body: Value) -> axum::http::Response<Body> {
    app.oneshot(
        Request::builder()
            .method("PATCH")
            .uri(format!("/api/cards/{card_id}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn post_overlay(app: axum::Router, body: Value) -> axum::http::Response<Body> {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/api/overlays")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn get_overlays(
    app: axum::Router,
    entity_kind: &str,
    entity_id: Option<&str>,
) -> axum::http::Response<Body> {
    let uri = match entity_id {
        Some(eid) => format!("/api/overlays?entity_kind={entity_kind}&entity_id={eid}"),
        None => format!("/api/overlays?entity_kind={entity_kind}"),
    };
    app.oneshot(
        Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn post_terminal_card_with_bad_payload_returns_400() {
    let (state, track_id) = boot().await;
    let resp = post_card(
        app(state),
        &track_id,
        json!({
            "kind": "terminal",
            "payload": { "terminal_id": 42 }
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_to_json(resp).await;
    assert_eq!(body["code"], "bad_request");
    assert!(
        body["error"].as_str().unwrap().contains("terminal"),
        "error message should mention terminal: {body:?}"
    );
}

#[tokio::test]
async fn post_terminal_card_with_valid_payload_creates() {
    let (state, track_id) = boot().await;
    let resp = post_card(
        app(state),
        &track_id,
        json!({
            "kind": "terminal",
            "payload": { "terminal_id": "t1" }
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn post_terminal_card_with_no_payload_is_accepted() {
    // Payload defaults to null on the wire; freshly-created terminal cards have no PTY yet.
    let (state, track_id) = boot().await;
    let resp = post_card(app(state), &track_id, json!({ "kind": "terminal" })).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn post_ui_kind_card_with_junk_payload_is_accepted() {
    let (state, track_id) = boot().await;
    let resp = post_card(
        app(state),
        &track_id,
        json!({
            "kind": "ui://example/view",
            "payload": { "junk": "ok", "any": [1, 2, 3] }
        }),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "ui:// kind must be opaque"
    );
}

#[tokio::test]
async fn patch_terminal_card_with_bad_payload_returns_400() {
    let (state, track_id) = boot().await;
    let seeded = state
        .raw_repo()
        .card_create(NewCard {
            track_id: track_id.clone().into(),
            title: None,
            kind: "terminal".into(),
            sort: None,
            payload: json!({ "terminal_id": "t1" }),
        })
        .await
        .unwrap();

    let resp = patch_card(
        app(state),
        seeded.id.as_str(),
        json!({ "payload": { "terminal_id": 99 } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_to_json(resp).await;
    assert_eq!(body["code"], "bad_request");
}

/// Client-supplied values a server-owned key is refused with: the minted shapes, a wrong-typed one, an empty object and null.
fn server_owned_probe_values() -> [Value; 5] {
    [
        json!(true),
        json!(false),
        json!({}),
        json!("track_policy"),
        Value::Null,
    ]
}

/// `terminal_signals`, `claude_permissions` and `claude_permissions_source` are stamped by the kernel on Planner-opened
/// terminals; no client may write any of them, for any kind (the hook route reads the marker from the payload, not the kind).
#[tokio::test]
async fn post_card_with_a_server_owned_key_is_rejected_for_every_kind() {
    let (state, track_id, repo) = boot_with_repo().await;
    assert_eq!(
        SERVER_OWNED_TERMINAL_PAYLOAD_KEYS,
        [
            "terminal_signals",
            "claude_permissions",
            "claude_permissions_source"
        ]
    );
    for key in SERVER_OWNED_TERMINAL_PAYLOAD_KEYS {
        for (kind, value) in [
            ("terminal", json!(true)),
            ("codex", json!(true)),
            ("claude", json!(true)),
            // Kind-agnostic: opaque kinds do not get to smuggle it either.
            ("ui://example/view", json!(true)),
        ]
        .into_iter()
        .chain(
            server_owned_probe_values()
                .into_iter()
                .map(|value| ("terminal", value)),
        ) {
            let mut payload = json!({ "schemaVersion": 1 });
            payload[key] = value;
            let resp = post_card(
                app(state.clone()),
                &track_id,
                json!({ "kind": kind, "payload": payload }),
            )
            .await;
            assert_eq!(
                resp.status(),
                StatusCode::BAD_REQUEST,
                "kind={kind} key={key} payload={payload}"
            );
            let body = body_to_json(resp).await;
            assert_eq!(body["code"], "bad_request", "kind={kind} key={key}");
            let error = body["error"].as_str().unwrap();
            assert!(
                error.contains(key) && error.contains("server-owned"),
                "kind={kind} key={key}: {body:?}"
            );
        }
    }
    assert!(
        repo.cards_by_track(&track_id).await.unwrap().is_empty(),
        "nothing was written"
    );
}

#[tokio::test]
async fn patch_card_with_a_server_owned_key_is_rejected() {
    let (state, track_id, repo) = boot_with_repo().await;
    let seeded = repo
        .card_create(NewCard {
            track_id: track_id.clone().into(),
            title: None,
            kind: "terminal".into(),
            sort: None,
            payload: json!({ "schemaVersion": 1, "terminal_id": "t1" }),
        })
        .await
        .unwrap();
    for key in SERVER_OWNED_TERMINAL_PAYLOAD_KEYS {
        for value in server_owned_probe_values() {
            let mut payload = json!({ "schemaVersion": 1, "terminal_id": "t1" });
            payload[key] = value;
            let resp = patch_card(
                app(state.clone()),
                seeded.id.as_str(),
                json!({ "payload": payload }),
            )
            .await;
            assert_eq!(
                resp.status(),
                StatusCode::BAD_REQUEST,
                "key={key} payload={payload}"
            );
            let body = body_to_json(resp).await;
            assert_eq!(body["code"], "bad_request", "key={key}");
            let error = body["error"].as_str().unwrap();
            assert!(
                error.contains(key) && error.contains("server-owned"),
                "key={key}: {body:?}"
            );
        }
    }
    let stored = repo.card_get(seeded.id.as_str()).await.unwrap().unwrap();
    assert_eq!(
        stored.payload, seeded.payload,
        "the rejected PATCHes wrote nothing"
    );
}

/// The kernel re-stamps `terminal_signals: true` on a whole-payload replacement; a card without the marker never gains it.
#[tokio::test]
async fn patch_replacing_payload_keeps_the_planner_terminal_marker() {
    let (state, track_id, repo) = boot_with_repo().await;
    // Seeded through the repo, the only writer allowed to mint the marker.
    let marked = repo
        .card_create(NewCard {
            track_id: track_id.clone().into(),
            title: None,
            kind: "terminal".into(),
            sort: None,
            payload: json!({ "schemaVersion": 1, "terminal_signals": true }),
        })
        .await
        .unwrap();
    let plain = repo
        .card_create(NewCard {
            track_id: track_id.clone().into(),
            title: None,
            kind: "terminal".into(),
            sort: None,
            payload: json!({ "schemaVersion": 1 }),
        })
        .await
        .unwrap();

    let resp = patch_card(
        app(state.clone()),
        marked.id.as_str(),
        json!({ "payload": { "schemaVersion": 1, "terminal_id": "replaced" } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_to_json(resp).await;
    assert_eq!(body["payload"]["terminal_id"], "replaced");
    assert_eq!(body["payload"]["terminal_signals"], true, "{body:?}");
    let stored = repo.card_get(marked.id.as_str()).await.unwrap().unwrap();
    assert_eq!(
        stored.payload["terminal_signals"], true,
        "{}",
        stored.payload
    );
    assert_eq!(stored.payload["terminal_id"], "replaced");

    let resp = patch_card(
        app(state),
        plain.id.as_str(),
        json!({ "payload": { "schemaVersion": 1, "terminal_id": "replaced" } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let stored = repo.card_get(plain.id.as_str()).await.unwrap().unwrap();
    assert!(
        stored.payload.get("terminal_signals").is_none(),
        "a PATCH never mints the marker: {}",
        stored.payload
    );
}

#[tokio::test]
async fn patch_ui_card_with_junk_payload_is_accepted() {
    let (state, track_id) = boot().await;
    let seeded = state
        .raw_repo()
        .card_create(NewCard {
            track_id: track_id.clone().into(),
            title: None,
            kind: "ui://example/view".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();

    let resp = patch_card(
        app(state),
        seeded.id.as_str(),
        json!({ "payload": { "garbage": true } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn post_status_overlay_with_bad_payload_returns_400() {
    let (state, track_id) = boot().await;
    let resp = post_overlay(
        app(state),
        json!({
            "plugin_id": "p1",
            "entity_kind": "track",
            "entity_id": track_id,
            "kind": "status",
            "payload": {} // missing required `state` field
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_to_json(resp).await;
    assert_eq!(body["code"], "bad_request");
}

#[tokio::test]
async fn post_status_overlay_with_valid_payload_returns_200() {
    let (state, track_id) = boot().await;
    let resp = post_overlay(
        app(state),
        json!({
            "plugin_id": "p1",
            "entity_kind": "track",
            "entity_id": track_id,
            "kind": "status",
            "payload": { "state": "running" }
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

/// The carrier is `layout`: its first payload below is valid and the rest are not, indistinguishable from
/// outside because the gate runs before the validator. A refused write must also not land.
#[tokio::test]
async fn post_reserved_view_overlay_is_forbidden_regardless_of_payload_validity() {
    let (state, track_id, repo) = boot_with_repo().await;
    for payload in [
        json!({ "positions": { "c": { "x": 0, "y": 0, "w": 4, "h": 3 } } }),
        json!({}),
        json!({ "positions": { "c": { "x": 0, "y": 0, "w": 4, "h": 3 } }, "extra": true }),
        json!({ "schemaVersion": 99, "positions": {} }),
    ] {
        let resp = post_overlay(
            app(state.clone()),
            json!({
                "plugin_id": "kernel",
                "entity_kind": "view",
                "entity_id": track_id,
                "kind": "layout",
                "payload": payload
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "payload={payload:?}");
        let body = body_to_json(resp).await;
        assert_eq!(body["code"], "forbidden", "payload={payload:?}");
    }
    // `Repo::track_create` writes no overlays at all, so "nothing under `view`" is exact.
    let overlays = repo
        .overlays_for("view", &track_id)
        .await
        .expect("overlays for the seeded track");
    assert!(
        overlays.is_empty(),
        "refused writes must not land: {overlays:?}"
    );
}

#[tokio::test]
async fn post_overlay_reserved_namespaces_are_independently_enforced() {
    let (state, track_id) = boot().await;
    let cases = [
        ("p1", "view", "layout", json!({ "positions": {} })),
        ("p1", "system", "status", json!({ "state": "ok" })),
        ("kernel", "track", "status", json!({ "state": "ok" })),
    ];
    for (plugin_id, entity_kind, kind, payload) in cases {
        let resp = post_overlay(
            app(state.clone()),
            json!({
                "plugin_id": plugin_id,
                "entity_kind": entity_kind,
                "entity_id": track_id,
                "kind": kind,
                "payload": payload
            }),
        )
        .await;
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "{plugin_id}/{entity_kind}/{kind}"
        );
    }
}

#[tokio::test]
async fn post_overlay_still_accepts_non_reserved_plugin_and_entity_kind() {
    let (state, track_id) = boot().await;
    let resp = post_overlay(
        app(state),
        json!({
            "plugin_id": "p1",
            "entity_kind": "track",
            "entity_id": track_id,
            "kind": "plugin-private-kind",
            "payload": { "anything": true }
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn delete_overlay_rejects_reserved_namespaces() {
    let (state, track_id) = boot().await;
    for (plugin_id, entity_kind) in [("kernel", "view"), ("p1", "view"), ("kernel", "track")] {
        let resp = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/overlays/delete")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "plugin_id": plugin_id,
                            "entity_kind": entity_kind,
                            "entity_id": track_id,
                            "kind": "layout"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "{plugin_id}/{entity_kind}"
        );
    }
}

#[tokio::test]
async fn post_overlay_routes_registered_entity_kinds_to_expected_scope() {
    let (state, track_id) = boot().await;
    let track = state
        .raw_repo()
        .track_get(&track_id)
        .await
        .unwrap()
        .expect("seeded track");
    let card = state
        .raw_repo()
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "terminal".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();

    let cases = [
        (
            "card",
            card.id.as_str(),
            EventScope::Card {
                card: card.id.clone(),
                track: track.id.clone(),
                area: track.area_id.clone(),
            },
        ),
        (
            "track",
            track.id.as_str(),
            EventScope::Track {
                track: track.id.clone(),
                area: track.area_id.clone(),
            },
        ),
        // `view` / `system` are kernel-reserved and cannot be reached through this route.
    ];

    for (entity_kind, entity_id, expected_scope) in cases {
        let events = state.events.clone();
        let subscription = tokio::spawn(async move { collect_envelopes(events, 1).await });
        tokio::time::sleep(Duration::from_millis(20)).await;

        let resp = post_overlay(
            app(state.clone()),
            json!({
                "plugin_id": "p1",
                "entity_kind": entity_kind,
                "entity_id": entity_id,
                "kind": "status",
                "payload": { "state": "running" }
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK, "{entity_kind} should write");

        let envelopes = subscription.await.unwrap();
        assert_eq!(envelopes.len(), 1);
        assert_eq!(envelopes[0].scope, expected_scope);
        assert!(
            matches!(envelopes[0].event, Event::OverlaySet(_)),
            "expected OverlaySet event for {entity_kind}"
        );
    }
}

#[tokio::test]
async fn post_progress_overlay_with_string_value_returns_400() {
    let (state, track_id) = boot().await;
    let resp = post_overlay(
        app(state),
        json!({
            "plugin_id": "p1",
            "entity_kind": "track",
            "entity_id": track_id,
            "kind": "progress",
            "payload": { "value": "fast" }
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn post_unknown_overlay_kind_with_arbitrary_payload_returns_200() {
    let (state, track_id) = boot().await;
    let resp = post_overlay(
        app(state),
        json!({
            "plugin_id": "p1",
            "entity_kind": "track",
            "entity_id": track_id,
            "kind": "my-plugin-badge",
            "payload": { "anything": [1, 2, 3], "nested": { "ok": true } }
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

/// Seed an overlay row directly via `raw_repo`, bypassing `validate_overlay_payload`, to simulate a future-version row a newer kernel binary left in the DB.
async fn seed_overlay(
    state: &AppState,
    plugin_id: &str,
    entity_kind: &str,
    entity_id: &str,
    kind: &str,
    payload: Value,
) {
    state
        .raw_repo()
        .overlay_upsert(NewOverlay {
            plugin_id: plugin_id.into(),
            entity_kind: entity_kind.into(),
            entity_id: entity_id.into(),
            kind: kind.into(),
            payload,
        })
        .await
        .expect("seed overlay");
}

#[tokio::test]
async fn list_overlays_filters_kernel_owned_future_schema_version() {
    let (state, track_id) = boot().await;
    seed_overlay(
        &state,
        "kernel",
        "track",
        &track_id,
        "status",
        json!({ "schemaVersion": 999, "state": "running" }),
    )
    .await;

    let resp = get_overlays(app(state), "track", Some(&track_id)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_to_json(resp).await;
    let arr = body.as_array().expect("array body");
    assert!(
        arr.is_empty(),
        "future-version kernel-owned overlay must be filtered, got {arr:?}"
    );
}

#[tokio::test]
async fn list_overlays_keeps_kernel_owned_supported_schema_version() {
    let (state, track_id) = boot().await;
    seed_overlay(
        &state,
        "kernel",
        "track",
        &track_id,
        "status",
        json!({ "schemaVersion": 1, "state": "running" }),
    )
    .await;

    let resp = get_overlays(app(state), "track", Some(&track_id)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_to_json(resp).await;
    let arr = body.as_array().expect("array body");
    assert_eq!(arr.len(), 1, "supported-version overlay must pass through");
    assert_eq!(arr[0]["kind"], "status");
}

#[tokio::test]
async fn list_overlays_keeps_kernel_owned_missing_schema_version() {
    // `payload_schema_version` defaults absent to `1`, so historical rows without a stamp still surface.
    let (state, track_id) = boot().await;
    seed_overlay(
        &state,
        "kernel",
        "track",
        &track_id,
        "status",
        json!({ "state": "idle" }),
    )
    .await;

    let resp = get_overlays(app(state), "track", Some(&track_id)).await;
    let body = body_to_json(resp).await;
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1, "row without schemaVersion must pass through");
}

#[tokio::test]
async fn list_overlays_passes_through_plugin_kind_with_future_schema_version() {
    // Plugin-defined overlay kinds are opaque: the kernel has no version policy for them.
    let (state, track_id) = boot().await;
    seed_overlay(
        &state,
        "p1",
        "track",
        &track_id,
        "my-plugin-badge",
        json!({ "schemaVersion": 9999, "anything": true }),
    )
    .await;

    let resp = get_overlays(app(state), "track", Some(&track_id)).await;
    let body = body_to_json(resp).await;
    let arr = body.as_array().unwrap();
    assert_eq!(
        arr.len(),
        1,
        "plugin-defined overlay must not be touched by the kernel read guard"
    );
    assert_eq!(arr[0]["kind"], "my-plugin-badge");
}

#[tokio::test]
async fn list_overlays_filters_mixed_kernel_and_plugin_rows() {
    let (state, track_id) = boot().await;
    seed_overlay(
        &state,
        "kernel",
        "track",
        &track_id,
        "progress",
        json!({ "schemaVersion": 42, "value": 0.5 }),
    )
    .await;
    seed_overlay(
        &state,
        "kernel",
        "track",
        &track_id,
        "eta",
        json!({ "schemaVersion": 1, "text": "5m" }),
    )
    .await;
    seed_overlay(
        &state,
        "p1",
        "track",
        &track_id,
        "plugin-thing",
        json!({ "schemaVersion": 7, "any": "thing" }),
    )
    .await;

    let resp = get_overlays(app(state), "track", Some(&track_id)).await;
    let body = body_to_json(resp).await;
    let arr = body.as_array().unwrap();
    let kinds: Vec<&str> = arr.iter().map(|o| o["kind"].as_str().unwrap()).collect();
    assert!(
        kinds.contains(&"eta"),
        "supported kernel kind should pass: {kinds:?}"
    );
    assert!(
        kinds.contains(&"plugin-thing"),
        "plugin kind should pass: {kinds:?}"
    );
    assert!(
        !kinds.contains(&"progress"),
        "future-version kernel kind should be filtered: {kinds:?}"
    );
    assert_eq!(arr.len(), 2);
}

#[tokio::test]
async fn list_overlays_by_kind_also_filters_future_versions() {
    // The `entity_id`-omitted branch (`overlays_by_kind`) shares the same guard.
    let (state, track_id) = boot().await;
    seed_overlay(
        &state,
        "kernel",
        "track",
        &track_id,
        "status",
        json!({ "schemaVersion": 100, "state": "running" }),
    )
    .await;

    let resp = get_overlays(app(state), "track", None).await;
    let body = body_to_json(resp).await;
    let arr = body.as_array().unwrap();
    let has_status = arr.iter().any(|o| o["kind"] == "status");
    assert!(
        !has_status,
        "future-version row must be filtered on the no-entity_id read path too, got {arr:?}"
    );
}

async fn get_track_detail(app: axum::Router, track_id: &str) -> axum::http::Response<Body> {
    app.oneshot(
        Request::builder()
            .method("GET")
            .uri(format!("/api/tracks/{track_id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn track_detail_filters_kernel_owned_future_schema_version() {
    let (state, track_id) = boot().await;
    seed_overlay(
        &state,
        "kernel",
        "track",
        &track_id,
        "status",
        json!({ "schemaVersion": 999, "state": "running" }),
    )
    .await;

    let resp = get_track_detail(app(state), &track_id).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_to_json(resp).await;
    let overlays = body["overlays"].as_array().expect("overlays array");
    assert!(
        overlays.is_empty(),
        "future-version kernel-owned overlay must be filtered from track detail, got {overlays:?}"
    );
}

#[tokio::test]
async fn track_detail_keeps_kernel_owned_supported_schema_version() {
    let (state, track_id) = boot().await;
    seed_overlay(
        &state,
        "kernel",
        "track",
        &track_id,
        "status",
        json!({ "schemaVersion": 1, "state": "running" }),
    )
    .await;

    let resp = get_track_detail(app(state), &track_id).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_to_json(resp).await;
    let overlays = body["overlays"].as_array().expect("overlays array");
    assert_eq!(
        overlays.len(),
        1,
        "supported-version overlay must pass through track detail"
    );
    assert_eq!(overlays[0]["kind"], "status");
}

#[tokio::test]
async fn track_detail_exposes_resume_when_transition_is_structurally_allowed() {
    let (state, track_id, repo) = boot_with_repo().await;
    repo.track_update(
        &track_id,
        TrackPatch {
            lifecycle: Some(TrackLifecycle::Done),
            ..TrackPatch::default()
        },
    )
    .await
    .expect("seed root Done lifecycle");

    let response = get_track_detail(app(state.clone()), &track_id).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_to_json(response).await["can_resume"], true);

    repo.track_update(
        &track_id,
        TrackPatch {
            lifecycle: Some(TrackLifecycle::Reviewing),
            ..TrackPatch::default()
        },
    )
    .await
    .expect("reopen root before attaching it to a parent task");

    let child = state.repo.track_get(&track_id).await.unwrap().unwrap();
    let parent = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: child.area_id,
            title: "parent".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("seed parent track");
    sqlx::query("UPDATE tracks SET parent_track_id = ?1 WHERE id = ?2")
        .bind(parent.id.as_str())
        .bind(&track_id)
        .execute(&repo.sqlite_pool().expect("sqlite-backed fixture"))
        .await
        .expect("attach child to parent");
    sqlx::query(
        "INSERT INTO tasks(\
            id, track_id, key, kind, goal, context_json, status, child_track_id, \
            created_at_ms, updated_at_ms\
         ) VALUES(\
            'parent:resume-capability', ?1, 'resume-capability', 'codex', 'g', '{}', \
            'running', ?2, 1, 1\
         )",
    )
    .bind(parent.id.as_str())
    .bind(&track_id)
    .execute(&repo.sqlite_pool().expect("sqlite-backed fixture"))
    .await
    .expect("bind child to parent task");

    let response = get_track_detail(app(state.clone()), &track_id).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_to_json(response).await["can_resume"],
        true,
        "a non-terminal child has no structural reopen restriction"
    );

    repo.track_update(
        &track_id,
        TrackPatch {
            lifecycle: Some(TrackLifecycle::Done),
            ..TrackPatch::default()
        },
    )
    .await
    .expect("finish child track");

    let response = get_track_detail(app(state), &track_id).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_to_json(response).await["can_resume"], false);
}

#[tokio::test]
async fn track_detail_withholds_resume_from_area_chat_tracks() {
    let (state, track_id, repo) = boot_with_repo().await;
    repo.track_update(
        &track_id,
        TrackPatch {
            lifecycle: Some(TrackLifecycle::Done),
            ..TrackPatch::default()
        },
    )
    .await
    .expect("seed Done lifecycle");
    sqlx::query("UPDATE tracks SET purpose = 'area-chat' WHERE id = ?1")
        .bind(&track_id)
        .execute(&repo.sqlite_pool().expect("sqlite-backed fixture"))
        .await
        .expect("mark retired area-chat track");

    let response = get_track_detail(app(state.clone()), &track_id).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_to_json(response).await["can_resume"],
        false,
        "the capability must include the route's area-chat authority fence"
    );

    let response = patch_track(app(state), &track_id, json!({"lifecycle": "working"})).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

async fn patch_track(app: axum::Router, track_id: &str, body: Value) -> axum::http::Response<Body> {
    app.oneshot(
        Request::builder()
            .method("PATCH")
            .uri(format!("/api/tracks/{track_id}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn track_patch_same_state_lifecycle_is_idempotent_no_event() {
    let (state, track_id) = boot().await;

    // Subscribe BEFORE the patch so we don't race the bus.
    let mut rx = state.events.subscribe();

    let pre = state
        .repo
        .track_get(&track_id)
        .await
        .unwrap()
        .expect("seeded track exists");
    assert_eq!(
        pre.lifecycle,
        calm_server::model::TrackLifecycle::Draft,
        "boot fixture lands in Draft",
    );

    // No `X-Calm-Actor` header → "user", an authorized actor for lifecycle.
    let resp = patch_track(app(state.clone()), &track_id, json!({"lifecycle": "draft"})).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_to_json(resp).await;
    assert_eq!(body["lifecycle"], "draft");

    let bus = tokio::time::timeout(std::time::Duration::from_millis(150), rx.recv()).await;
    assert!(
        bus.is_err(),
        "no event should fire for same-state lifecycle PATCH (got {bus:?})",
    );

    let post = state.repo.track_get(&track_id).await.unwrap().unwrap();
    assert_eq!(post.lifecycle, calm_server::model::TrackLifecycle::Draft);
    assert_eq!(
        post.updated_at, pre.updated_at,
        "updated_at must not advance on a lifecycle-only no-op",
    );
}

#[tokio::test]
async fn track_patch_user_resume_done_to_working_clears_terminal_at_and_emits() {
    let (state, track_id, repo) = boot_with_repo().await;
    let done = repo
        .track_update(
            &track_id,
            TrackPatch {
                lifecycle: Some(TrackLifecycle::Done),
                ..TrackPatch::default()
            },
        )
        .await
        .expect("seed Done lifecycle");
    assert!(
        done.terminal_at.is_some(),
        "Done fixture must carry terminal_at"
    );

    let mut rx = state.events.subscribe();
    let resp = patch_track(
        app(state.clone()),
        &track_id,
        json!({ "lifecycle": "working" }),
    )
    .await;
    let status = resp.status();
    let body = body_to_json(resp).await;
    assert_eq!(status, StatusCode::OK, "resume response: {body}");
    assert_eq!(body["lifecycle"], "working");
    assert_eq!(body["terminal_at"], Value::Null);

    let lifecycle = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("lifecycle event arrives")
        .expect("event bus remains open");
    assert_eq!(lifecycle.actor, ActorId::User);
    assert!(
        matches!(
            lifecycle.event,
            Event::TrackLifecycleChanged {
                from: TrackLifecycle::Done,
                to: TrackLifecycle::Working,
                ..
            }
        ),
        "first event must be Done -> Working, got {:?}",
        lifecycle.event,
    );

    let updated = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("track update event arrives")
        .expect("event bus remains open");
    assert_eq!(updated.actor, ActorId::User);
    match updated.event {
        Event::TrackUpdated(payload) => {
            assert_eq!(payload.lifecycle, TrackLifecycle::Working);
            assert_eq!(payload.terminal_at, None);
        }
        other => panic!("second event must be TrackUpdated, got {other:?}"),
    }
}

#[tokio::test]
async fn racing_rest_lifecycle_patch_rechecks_the_snapshot_before_writing_or_emitting() {
    let (state, track_id, repo) = boot_with_repo().await;
    repo.track_update(
        &track_id,
        TrackPatch {
            lifecycle: Some(TrackLifecycle::Done),
            ..TrackPatch::default()
        },
    )
    .await
    .expect("seed Done before the REST pre-read");

    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    install_track_lifecycle_patch_race_hook_for_test(
        &track_id,
        TrackLifecyclePatchRaceHook {
            entered: entered.clone(),
            release: release.clone(),
        },
    );
    let request_track_id = track_id.clone();
    let request_app = app(state.clone());
    let request = tokio::spawn(async move {
        patch_track(
            request_app,
            &request_track_id,
            json!({"lifecycle": "working"}),
        )
        .await
    });

    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("REST PATCH reaches the post-preflight race hook");
    repo.track_update(
        &track_id,
        TrackPatch {
            lifecycle: Some(TrackLifecycle::Planning),
            ..TrackPatch::default()
        },
    )
    .await
    .expect("commit newer lifecycle before the route transaction");
    release.notify_one();

    let response = request.await.expect("REST PATCH task joins");
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = body_to_json(response).await;
    assert_eq!(body["code"], "conflict");

    let current = state.repo.track_get(&track_id).await.unwrap().unwrap();
    assert_eq!(current.lifecycle, TrackLifecycle::Planning);
    let events = repo.events_since(0, i64::MAX).await.expect("event log");
    assert!(
        events.is_empty(),
        "stale snapshot must emit nothing: {events:?}"
    );
}

#[tokio::test]
async fn track_patch_same_state_lifecycle_with_title_still_writes_title() {
    use calm_server::event::Event;
    let (state, track_id) = boot().await;
    let mut rx = state.events.subscribe();

    let resp = patch_track(
        app(state.clone()),
        &track_id,
        json!({"lifecycle": "draft", "title": "renamed-via-rest"}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_to_json(resp).await;
    assert_eq!(body["title"], "renamed-via-rest");
    assert_eq!(body["lifecycle"], "draft");

    let env = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("bus delivers")
        .expect("bus open");
    assert!(
        matches!(env.event, Event::TrackUpdated(_)),
        "first envelope is TrackUpdated, got {:?}",
        env.event,
    );

    let bus = tokio::time::timeout(std::time::Duration::from_millis(150), rx.recv()).await;
    assert!(
        bus.is_err(),
        "no TrackLifecycleChanged should be emitted for same-state lifecycle (got {bus:?})",
    );
}

async fn track_policy_columns(repo: &Arc<dyn Repo>, track_id: &str) -> (Option<i64>, i64) {
    let pool = repo.sqlite_pool().expect("sqlite pool");
    let (budget, require_gates): (Option<i64>, i64) =
        sqlx::query_as("SELECT task_budget, require_task_gates FROM tracks WHERE id = ?1")
            .bind(track_id)
            .fetch_one(&pool)
            .await
            .expect("read track policy columns");
    (budget, require_gates)
}

#[tokio::test]
async fn track_patch_task_budget_and_require_task_gates_persist() {
    let (state, track_id, repo) = boot_with_repo().await;

    let resp = patch_track(
        app(state.clone()),
        &track_id,
        json!({"task_budget": 3, "require_task_gates": false}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let (budget, require_gates) = track_policy_columns(&repo, &track_id).await;
    assert_eq!(budget, Some(3));
    assert_eq!(require_gates, 0);

    let resp = patch_track(app(state.clone()), &track_id, json!({"task_budget": null})).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let (budget, require_gates) = track_policy_columns(&repo, &track_id).await;
    assert_eq!(budget, None);
    assert_eq!(require_gates, 0, "untouched by the budget-only patch");
}

#[tokio::test]
async fn track_patch_negative_task_budget_rejected_with_400() {
    let (state, track_id, repo) = boot_with_repo().await;

    let resp = patch_track(app(state.clone()), &track_id, json!({"task_budget": -1})).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_to_json(resp).await;
    assert_eq!(body["code"], "bad_request");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("task_budget must be >= 0"),
        "error message should explain the bound: {body:?}"
    );

    let (budget, require_gates) = track_policy_columns(&repo, &track_id).await;
    assert_eq!(budget, None);
    assert_eq!(
        require_gates, 1,
        "post-migration default untouched by the rejected patch"
    );
}
