//! Integration tests for D4 per-kind payload validators wired into the
//! `cards` and `overlays` route layer.
//!
//! Boots a minimal Axum app with the cards + overlays routers + a stub-only
//! AppState (in-memory SqlxRepo, EventBus, stub DaemonClient, stub PluginHost),
//! then POSTs payloads through `tower::ServiceExt::oneshot` to verify HTTP-level
//! behavior:
//!
//!   * Bad terminal Card payload → 400 with a clear `bad_request` error code.
//!   * `ui://` Card with arbitrary garbage payload → 201 (opaque path works).
//!   * Bad `status` Overlay payload → 400.
//!   * Good `status` Overlay payload → 200.
//!   * Card `PATCH` with bad payload for an existing `terminal` card → 400.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{BroadcastEnvelope, Event, EventBus, EventScope};
use calm_server::model::{NewCard, NewCove, NewOverlay, NewWave};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// Build a minimal AppState + seed one cove + wave + (optional) card. Returns
/// the wave id (and an optional card id) the test will hit.
async fn boot() -> (AppState, String) {
    let (state, wave_id, _repo) = boot_with_repo().await;
    (state, wave_id)
}

/// [`boot`] variant that also hands back the full-capability repo —
/// `AppState.repo` is the narrower `RouteRepo`, which deliberately has
/// no `sqlite_pool` escape hatch, but the #644 WavePatch tests need raw
/// column reads (the `Wave` row struct doesn't carry the new columns).
async fn boot_with_repo() -> (AppState, String, Arc<dyn Repo>) {
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite repo"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "demo".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "demo".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
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
                calm_server::wave_cove_cache::WaveCoveCache::new(),
            ),
        )),
        Arc::new(calm_server::state::CodexClient::new_stub()),
        None,
        None,
    );
    (state, wave.id.to_string(), repo)
}

fn app(state: AppState) -> axum::Router {
    // Scope G: the cards / overlays handlers extract `Actor` from request
    // extensions, which means the middleware that populates it must be
    // present. Mirror main.rs by layering it on the REST router.
    //
    // `waves::router` is also merged here so the PR #214 follow-up tests
    // can exercise `GET /api/waves/{id}` for the wave-detail read-side
    // schemaVersion guard. Adding the router is a no-op for the existing
    // card/overlay tests; we only ever hit the wave route from the tests
    // that explicitly construct that URI.
    axum::Router::new()
        .merge(routes::cards::router())
        .merge(routes::overlays::router())
        .merge(routes::waves::router())
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

async fn post_card(app: axum::Router, wave_id: &str, body: Value) -> axum::http::Response<Body> {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri(format!("/api/waves/{wave_id}/cards"))
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

// --------------------------------------------------------------------------
// Cards
// --------------------------------------------------------------------------

#[tokio::test]
async fn post_terminal_card_with_bad_payload_returns_400() {
    let (state, wave_id) = boot().await;
    let resp = post_card(
        app(state),
        &wave_id,
        json!({
            "kind": "terminal",
            // terminal_id must be a string when present.
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
    let (state, wave_id) = boot().await;
    let resp = post_card(
        app(state),
        &wave_id,
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
    // Payload defaults to null on the wire — validator must accept that
    // because freshly-created terminal cards have no PTY yet.
    let (state, wave_id) = boot().await;
    let resp = post_card(app(state), &wave_id, json!({ "kind": "terminal" })).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn post_ui_kind_card_with_junk_payload_is_accepted() {
    // D4 acceptance criterion: `ui://*` cards stay opaque — a junk payload
    // must NOT be rejected. Proves the plugin-defined opt-out works.
    let (state, wave_id) = boot().await;
    let resp = post_card(
        app(state),
        &wave_id,
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
    // Seed a terminal card directly via the repo so we can patch it.
    let (state, wave_id) = boot().await;
    let seeded = state
        .raw_repo()
        .card_create(NewCard {
            wave_id: wave_id.clone().into(),
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

#[tokio::test]
async fn patch_ui_card_with_junk_payload_is_accepted() {
    // Patching a ui://* card must remain opaque too.
    let (state, wave_id) = boot().await;
    let seeded = state
        .raw_repo()
        .card_create(NewCard {
            wave_id: wave_id.clone().into(),
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

// --------------------------------------------------------------------------
// Overlays
// --------------------------------------------------------------------------

#[tokio::test]
async fn post_status_overlay_with_bad_payload_returns_400() {
    let (state, wave_id) = boot().await;
    let resp = post_overlay(
        app(state),
        json!({
            "plugin_id": "p1",
            "entity_kind": "wave",
            "entity_id": wave_id,
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
    let (state, wave_id) = boot().await;
    let resp = post_overlay(
        app(state),
        json!({
            "plugin_id": "p1",
            "entity_kind": "wave",
            "entity_id": wave_id,
            "kind": "status",
            "payload": { "state": "running" }
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn post_overlay_routes_registered_entity_kinds_to_expected_scope() {
    let (state, wave_id) = boot().await;
    let wave = state
        .raw_repo()
        .wave_get(&wave_id)
        .await
        .unwrap()
        .expect("seeded wave");
    let card = state
        .raw_repo()
        .card_create(NewCard {
            wave_id: wave.id.clone(),
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
                wave: wave.id.clone(),
                cove: wave.cove_id.clone(),
            },
        ),
        (
            "wave",
            wave.id.as_str(),
            EventScope::Wave {
                wave: wave.id.clone(),
                cove: wave.cove_id.clone(),
            },
        ),
        ("view", "main", EventScope::System),
        ("system", "global", EventScope::System),
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
    let (state, wave_id) = boot().await;
    let resp = post_overlay(
        app(state),
        json!({
            "plugin_id": "p1",
            "entity_kind": "wave",
            "entity_id": wave_id,
            "kind": "progress",
            "payload": { "value": "fast" }
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn post_unknown_overlay_kind_with_arbitrary_payload_returns_200() {
    // Plugin-defined overlay kinds remain opaque.
    let (state, wave_id) = boot().await;
    let resp = post_overlay(
        app(state),
        json!({
            "plugin_id": "p1",
            "entity_kind": "wave",
            "entity_id": wave_id,
            "kind": "my-plugin-badge",
            "payload": { "anything": [1, 2, 3], "nested": { "ok": true } }
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

// --------------------------------------------------------------------------
// Overlay schemaVersion read-side guard (issue #198 concern 4)
//
// These tests bypass the write-side validator by seeding via `raw_repo()`,
// then call the read route and assert future-version kernel-owned overlays
// are filtered out while plugin-defined kinds (and supported kernel rows)
// pass through.
// --------------------------------------------------------------------------

/// Seed an overlay row directly via `raw_repo`, bypassing
/// `validate_overlay_payload` so we can simulate a future-version row that
/// a newer kernel binary left in the DB.
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
    // A kernel-owned overlay with `schemaVersion = MAX + 1` (simulating a
    // row written by a newer binary) must not appear in the read response.
    let (state, wave_id) = boot().await;
    seed_overlay(
        &state,
        "kernel",
        "wave",
        &wave_id,
        "status",
        json!({ "schemaVersion": 999, "state": "running" }),
    )
    .await;

    let resp = get_overlays(app(state), "wave", Some(&wave_id)).await;
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
    // Sanity check: a row at the supported version still comes through.
    let (state, wave_id) = boot().await;
    seed_overlay(
        &state,
        "kernel",
        "wave",
        &wave_id,
        "status",
        json!({ "schemaVersion": 1, "state": "running" }),
    )
    .await;

    let resp = get_overlays(app(state), "wave", Some(&wave_id)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_to_json(resp).await;
    let arr = body.as_array().expect("array body");
    assert_eq!(arr.len(), 1, "supported-version overlay must pass through");
    assert_eq!(arr[0]["kind"], "status");
}

#[tokio::test]
async fn list_overlays_keeps_kernel_owned_missing_schema_version() {
    // Historical rows written before `schemaVersion` was stamped should
    // still surface — `payload_schema_version` defaults absent to `1`,
    // which is `<= MAX` for every kernel-owned kind today.
    let (state, wave_id) = boot().await;
    seed_overlay(
        &state,
        "kernel",
        "wave",
        &wave_id,
        "status",
        json!({ "state": "idle" }),
    )
    .await;

    let resp = get_overlays(app(state), "wave", Some(&wave_id)).await;
    let body = body_to_json(resp).await;
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1, "row without schemaVersion must pass through");
}

#[tokio::test]
async fn list_overlays_passes_through_plugin_kind_with_future_schema_version() {
    // Plugin-defined overlay kinds are opaque — the kernel has no version
    // policy for them, so a "future" value on the schemaVersion field must
    // not cause the read guard to drop the row.
    let (state, wave_id) = boot().await;
    seed_overlay(
        &state,
        "p1",
        "wave",
        &wave_id,
        "my-plugin-badge",
        json!({ "schemaVersion": 9999, "anything": true }),
    )
    .await;

    let resp = get_overlays(app(state), "wave", Some(&wave_id)).await;
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
    // Mixed scenario: one kernel-owned overlay with a future schemaVersion,
    // one kernel-owned overlay at the supported version, one plugin-owned
    // overlay with an arbitrary schemaVersion. Only the future kernel row
    // is filtered.
    let (state, wave_id) = boot().await;
    seed_overlay(
        &state,
        "kernel",
        "wave",
        &wave_id,
        "progress",
        json!({ "schemaVersion": 42, "value": 0.5 }),
    )
    .await;
    seed_overlay(
        &state,
        "kernel",
        "wave",
        &wave_id,
        "eta",
        json!({ "schemaVersion": 1, "text": "5m" }),
    )
    .await;
    seed_overlay(
        &state,
        "p1",
        "wave",
        &wave_id,
        "plugin-thing",
        json!({ "schemaVersion": 7, "any": "thing" }),
    )
    .await;

    let resp = get_overlays(app(state), "wave", Some(&wave_id)).await;
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
    // The `entity_id`-omitted branch (`overlays_by_kind`) shares the same
    // guard — sidebar fetches go through this code path.
    let (state, wave_id) = boot().await;
    seed_overlay(
        &state,
        "kernel",
        "wave",
        &wave_id,
        "status",
        json!({ "schemaVersion": 100, "state": "running" }),
    )
    .await;

    let resp = get_overlays(app(state), "wave", None).await;
    let body = body_to_json(resp).await;
    let arr = body.as_array().unwrap();
    let has_status = arr.iter().any(|o| o["kind"] == "status");
    assert!(
        !has_status,
        "future-version row must be filtered on the no-entity_id read path too, got {arr:?}"
    );
}

// --------------------------------------------------------------------------
// Wave detail read-side guard (PR #214 review follow-up, issue #198 concern 4)
//
// `GET /api/waves/{id}` returns `WaveDetail { wave, cards, overlays }` and is
// the primary read path the frontend uses to render status/progress/eta/now
// overlays on a wave's detail view. The PR #214 reviewer flagged that the
// initial fix only guarded `GET /api/overlays` — a future-`schemaVersion`
// row would still sail through the wave-detail route. These tests assert
// the same filter applies there.
// --------------------------------------------------------------------------

async fn get_wave_detail(app: axum::Router, wave_id: &str) -> axum::http::Response<Body> {
    app.oneshot(
        Request::builder()
            .method("GET")
            .uri(format!("/api/waves/{wave_id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn wave_detail_filters_kernel_owned_future_schema_version() {
    // Seed a kernel-owned status overlay with `schemaVersion = MAX + 1`
    // (simulating a row a newer kernel binary left in the DB), then hit
    // `GET /api/waves/{id}` and assert the future-version row is filtered
    // out of `WaveDetail.overlays`. The frontend's `adaptWave` consumes
    // exactly this field — without this guard a future row would defeat
    // the PR #214 read-side check for the primary wave-rendering path.
    let (state, wave_id) = boot().await;
    seed_overlay(
        &state,
        "kernel",
        "wave",
        &wave_id,
        "status",
        json!({ "schemaVersion": 999, "state": "running" }),
    )
    .await;

    let resp = get_wave_detail(app(state), &wave_id).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_to_json(resp).await;
    let overlays = body["overlays"].as_array().expect("overlays array");
    assert!(
        overlays.is_empty(),
        "future-version kernel-owned overlay must be filtered from wave detail, got {overlays:?}"
    );
}

#[tokio::test]
async fn wave_detail_keeps_kernel_owned_supported_schema_version() {
    // Paired sanity check: a kernel-owned overlay at the supported version
    // still surfaces through `GET /api/waves/{id}`, so the guard is not
    // accidentally dropping everything.
    let (state, wave_id) = boot().await;
    seed_overlay(
        &state,
        "kernel",
        "wave",
        &wave_id,
        "status",
        json!({ "schemaVersion": 1, "state": "running" }),
    )
    .await;

    let resp = get_wave_detail(app(state), &wave_id).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_to_json(resp).await;
    let overlays = body["overlays"].as_array().expect("overlays array");
    assert_eq!(
        overlays.len(),
        1,
        "supported-version overlay must pass through wave detail"
    );
    assert_eq!(overlays[0]["kind"], "status");
}

// --------------------------------------------------------------------------
// Wave lifecycle PATCH — issue #145 followup, idempotent same-state semantics
//
// `PATCH /api/waves/{id}` with `{"lifecycle": "<current>"}` from an
// authorized actor (default = user via missing header) must succeed
// silently: HTTP 200, no `WaveLifecycleChanged` event, no `WaveUpdated`
// event (lifecycle was the only field), and the row's `updated_at`
// stays put. This pins the idempotent contract on the REST surface so
// a client retry doesn't pollute the event log.
// --------------------------------------------------------------------------

async fn patch_wave(app: axum::Router, wave_id: &str, body: Value) -> axum::http::Response<Body> {
    app.oneshot(
        Request::builder()
            .method("PATCH")
            .uri(format!("/api/waves/{wave_id}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn wave_patch_same_state_lifecycle_is_idempotent_no_event() {
    let (state, wave_id) = boot().await;

    // Subscribe BEFORE the patch so we don't race the bus.
    let mut rx = state.events.subscribe();

    let pre = state
        .repo
        .wave_get(&wave_id)
        .await
        .unwrap()
        .expect("seeded wave exists");
    assert_eq!(
        pre.lifecycle,
        calm_server::model::WaveLifecycle::Draft,
        "boot fixture lands in Draft",
    );

    // Default actor (no `X-Calm-Actor` header → "user"). User is
    // an authorized actor for lifecycle, so a same-state request
    // takes the idempotent path.
    let resp = patch_wave(app(state.clone()), &wave_id, json!({"lifecycle": "draft"})).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_to_json(resp).await;
    assert_eq!(body["lifecycle"], "draft");

    // No bus envelope at all.
    let bus = tokio::time::timeout(std::time::Duration::from_millis(150), rx.recv()).await;
    assert!(
        bus.is_err(),
        "no event should fire for same-state lifecycle PATCH (got {bus:?})",
    );

    // Row untouched.
    let post = state.repo.wave_get(&wave_id).await.unwrap().unwrap();
    assert_eq!(post.lifecycle, calm_server::model::WaveLifecycle::Draft);
    assert_eq!(
        post.updated_at, pre.updated_at,
        "updated_at must not advance on a lifecycle-only no-op",
    );
}

#[tokio::test]
async fn wave_patch_same_state_lifecycle_with_title_still_writes_title() {
    // Companion: lifecycle is a no-op but `title` legitimately changes
    // — we still bump the row, emit `WaveUpdated`, but NOT
    // `WaveLifecycleChanged`. Verifies the strip-and-continue path in
    // `routes::waves::update_wave`.
    use calm_server::event::Event;
    let (state, wave_id) = boot().await;
    let mut rx = state.events.subscribe();

    let resp = patch_wave(
        app(state.clone()),
        &wave_id,
        json!({"lifecycle": "draft", "title": "renamed-via-rest"}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_to_json(resp).await;
    assert_eq!(body["title"], "renamed-via-rest");
    assert_eq!(body["lifecycle"], "draft");

    // Exactly one envelope: WaveUpdated. The lifecycle envelope is
    // suppressed because from == to.
    let env = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("bus delivers")
        .expect("bus open");
    assert!(
        matches!(env.event, Event::WaveUpdated(_)),
        "first envelope is WaveUpdated, got {:?}",
        env.event,
    );

    // No follow-up envelope.
    let bus = tokio::time::timeout(std::time::Duration::from_millis(150), rx.recv()).await;
    assert!(
        bus.is_err(),
        "no WaveLifecycleChanged should be emitted for same-state lifecycle (got {bus:?})",
    );
}

// --------------------------------------------------------------------------
// Wave scheduler-policy PATCH — issue #644 `task_budget` / `require_task_gates`
//
// Route-level coverage for the new WavePatch fields: a valid patch lands in
// the DB columns (the `Wave` row struct doesn't carry them, so persistence is
// asserted against the table), and a negative budget is rejected with 400
// before anything is written.
// --------------------------------------------------------------------------

async fn wave_policy_columns(repo: &Arc<dyn Repo>, wave_id: &str) -> (Option<i64>, i64) {
    let pool = repo.sqlite_pool().expect("sqlite pool");
    let (budget, require_gates): (Option<i64>, i64) =
        sqlx::query_as("SELECT task_budget, require_task_gates FROM waves WHERE id = ?1")
            .bind(wave_id)
            .fetch_one(&pool)
            .await
            .expect("read wave policy columns");
    (budget, require_gates)
}

#[tokio::test]
async fn wave_patch_task_budget_and_require_task_gates_persist() {
    let (state, wave_id, repo) = boot_with_repo().await;

    let resp = patch_wave(
        app(state.clone()),
        &wave_id,
        json!({"task_budget": 3, "require_task_gates": false}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let (budget, require_gates) = wave_policy_columns(&repo, &wave_id).await;
    assert_eq!(budget, Some(3));
    assert_eq!(require_gates, 0);

    // `task_budget: null` clears back to the kernel default; the other
    // column is left alone.
    let resp = patch_wave(app(state.clone()), &wave_id, json!({"task_budget": null})).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let (budget, require_gates) = wave_policy_columns(&repo, &wave_id).await;
    assert_eq!(budget, None);
    assert_eq!(require_gates, 0, "untouched by the budget-only patch");
}

#[tokio::test]
async fn wave_patch_negative_task_budget_rejected_with_400() {
    let (state, wave_id, repo) = boot_with_repo().await;

    let resp = patch_wave(app(state.clone()), &wave_id, json!({"task_budget": -1})).await;
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

    // Nothing was written.
    let (budget, require_gates) = wave_policy_columns(&repo, &wave_id).await;
    assert_eq!(budget, None);
    assert_eq!(
        require_gates, 1,
        "post-migration default untouched by the rejected patch"
    );
}
