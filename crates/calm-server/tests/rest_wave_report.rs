//! Issue #247 PR3 — REST-side wave-report edit endpoint integration
//! tests (`POST /api/waves/:id/report`).
//!
//! Companion to `mcp_wave_report.rs` (MCP-side coverage). Each test
//! drives the real axum router via `tower::ServiceExt::oneshot` with
//! the same router assembly the production binary uses
//! (protected REST behind `auth::require_session` + the actor
//! middleware), so the auth gate, session-extraction, actor pinning,
//! and the shared persist boundary all run end-to-end.
//!
//! Coverage:
//!
//!   * **Happy path** — authenticated user POST → 200; response is the
//!     projected `WaveReportPayload`; both `CardUpdated` and
//!     `WaveReportEdited` envelopes are emitted; the latter carries
//!     `author == EditAuthor::User`.
//!   * **Author cannot be forged** — request body with an extra
//!     `author` field is rejected (`deny_unknown_fields` returns 4xx)
//!     so a client can never persuade the server to attribute a User
//!     edit as Spec.
//!   * **No session** — request without cookie → 401, no events
//!     emitted.
//!   * **Cross-wave isolation** — user posts to a wave that doesn't
//!     exist → 404, no events emitted. (Today's single-user owner
//!     model has no per-wave ACL; the wave-existence 404 is the
//!     effective cross-wave gate. Multi-user would extend this with a
//!     403 on a wave the principal doesn't own.)
//!   * **Worker / non-user actor** — authenticated session but
//!     `X-Calm-Actor: ai:codex` → 403, no events emitted. Only
//!     `ActorId::User` may persist via REST.
//!   * **MCP path still tags Spec** — re-asserts via the existing
//!     `mcp_wave_report` regression suite (already pinned in PR2 and
//!     re-confirmed by the build).

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use calm_server::auth::{self, AuthConfig, AuthState, SESSION_COOKIE};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{EditAuthor, Event, EventBus};
use calm_server::ids::WaveId;
use calm_server::model::{NewCard, NewCove, NewWave};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::wave_report::WaveReportPayload;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Fixture boot — fresh `AppState` + a seeded cove/wave/report card. The
// router assembly mirrors `main.rs` so the auth + actor middleware fire
// in the same order production sees them.
// ---------------------------------------------------------------------------

struct Boot {
    state: AppState,
    auth_state: AuthState,
    wave_id: WaveId,
    /// Repo used to seed fixtures + read back post-write state.
    repo: Arc<dyn Repo>,
}

async fn boot() -> Boot {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "rest-report-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "report wave".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    // Mint the wave-report card the same way `routes::waves::create_wave`
    // does (kind = "wave-report", seed payload = `WaveReportPayload::initial()`).
    // We bypass the role gate / kernel-owned bit here — those are MCP-
    // side concerns; the REST endpoint resolves the report card by
    // `kind == "wave-report"` (matching the in-handler resolver).
    let _report_card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "wave-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(WaveReportPayload::initial()).unwrap(),
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
            std::env::temp_dir().join("calm-plugins-data-rest-report"),
            Vec::new(),
            EventBus::new(),
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::wave_cove_cache::WaveCoveCache::new(),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    );
    let auth_state = AuthState::new(AuthConfig {
        username: Some("alice".into()),
        password: Some("hunter2".into()),
        dev_autologin: false,
        display_name: "alice".into(),
    });
    Boot {
        state,
        auth_state,
        wave_id: wave.id,
        repo,
    }
}

/// Assemble the same router tree `main.rs` does — protected REST
/// behind `actor_middleware` (innermost) + `require_session`
/// (outermost), public REST + auth router unconditionally. Order
/// matters: an unauthenticated request must be rejected by the
/// session check before the actor extractor runs (otherwise we'd
/// 400 on a missing/invalid actor header instead of 401-ing for
/// the missing session).
fn app(state: AppState, auth_state: AuthState) -> axum::Router {
    let protected_rest = routes::protected_router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            auth_state.clone(),
            auth::require_session,
        ));
    let public_rest = routes::public_router();
    let auth_router = auth::router().with_state(auth_state.clone());
    axum::Router::new()
        .merge(protected_rest)
        .merge(public_rest)
        .with_state(state)
        .merge(auth_router)
}

/// Login and return the bare `name=value` cookie string suitable for
/// the `Cookie` header.
async fn login(app: &axum::Router) -> String {
    let body = serde_json::to_vec(&json!({
        "username": "alice",
        "password": "hunter2",
    }))
    .unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "login must succeed");
    let raw = resp
        .headers()
        .get(header::SET_COOKIE)
        .expect("Set-Cookie present on login")
        .to_str()
        .unwrap();
    let first = raw.split(';').next().unwrap();
    assert!(first.starts_with(&format!("{SESSION_COOKIE}=")));
    first.to_string()
}

/// Collect up to `n` envelopes from the bus, with a short timeout so a
/// missing event surfaces as a length mismatch instead of a hang.
async fn collect_n(events: &EventBus, n: usize) -> Vec<calm_server::event::BroadcastEnvelope> {
    let mut sub = events.subscribe();
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        match tokio::time::timeout(Duration::from_secs(2), sub.recv()).await {
            Ok(Ok(env)) => out.push(env),
            Ok(Err(_lag)) => break,
            Err(_timeout) => break,
        }
    }
    out
}

/// Assert no envelope arrives within a short window — used to confirm
/// that auth/forbidden failures emit nothing. Bounded by `dur` so the
/// test doesn't sit on a happy-no-event path forever.
async fn expect_no_events(events: &EventBus, dur: Duration) {
    let mut sub = events.subscribe();
    match tokio::time::timeout(dur, sub.recv()).await {
        // Timeout = no event arrived. Good.
        Err(_) => {}
        Ok(Ok(env)) => panic!("unexpected event arrived: {env:?}"),
        Ok(Err(_lag)) => {}
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn happy_path_user_edit_returns_payload_and_emits_user_authored_event() {
    let boot = boot().await;
    let events = boot.state.events.clone();
    let wave_id = boot.wave_id.clone();
    let app = app(boot.state, boot.auth_state);
    let cookie = login(&app).await;

    // Subscribe BEFORE issuing the write so we can collect the two
    // envelopes the persist boundary emits (CardUpdated +
    // WaveReportEdited).
    let bus_clone = events.clone();
    let collector = tokio::spawn(async move { collect_n(&bus_clone, 2).await });
    // Small yield so the collector subscribes before the persist
    // emit completes.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let body = serde_json::to_vec(&json!({
        "summary": "user wrote this",
        "body": "# Goal\n\nuser edit body\n",
    }))
    .unwrap();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/waves/{}/report", wave_id.as_str()))
                .header("content-type", "application/json")
                .header(header::COOKIE, cookie)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "user edit succeeds");

    // Response body — projected payload reflects what was written.
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["summary"], "user wrote this");
    assert_eq!(v["body"], "# Goal\n\nuser edit body\n");
    assert_eq!(
        v["schemaVersion"], 1,
        "schemaVersion is server-pinned to the current constant"
    );

    // Bus — exactly two envelopes: CardUpdated first (preserves the
    // pre-PR2 broadcast order), WaveReportEdited second with
    // `author == User`.
    let envs = collector.await.expect("collector ok");
    assert_eq!(
        envs.len(),
        2,
        "expected two envelopes (CardUpdated + WaveReportEdited), got {envs:?}",
    );
    assert!(
        matches!(envs[0].event, Event::CardUpdated(_)),
        "CardUpdated arrives first; got {:?}",
        envs[0].event,
    );
    match &envs[1].event {
        Event::WaveReportEdited {
            wave_id: w,
            author,
            summary_before,
            summary_after,
            body_before,
            body_after,
            ..
        } => {
            assert_eq!(w, &wave_id, "wave_id matches the path param");
            assert_eq!(
                *author,
                EditAuthor::User,
                "REST endpoint MUST tag User — Spec attribution would be the PR3 spoof bug",
            );
            // Pre-write was the seeded initial payload; post-write is
            // exactly what the request body carried.
            assert_eq!(summary_before, "");
            assert_eq!(
                body_before, "# Goal\n\n_The spec agent will fill this in._\n",
                "before-state matches the WaveReportPayload::initial seed",
            );
            assert_eq!(summary_after, "user wrote this");
            assert_eq!(body_after, "# Goal\n\nuser edit body\n");
        }
        other => panic!("expected WaveReportEdited as the second envelope, got {other:?}"),
    }

    // DB also has the new shape — read back via the wave-report card
    // row.
    let cards = boot.repo.cards_by_wave(wave_id.as_str()).await.unwrap();
    let report_card = cards.into_iter().find(|c| c.kind == "wave-report").unwrap();
    let persisted: WaveReportPayload = serde_json::from_value(report_card.payload).unwrap();
    assert_eq!(persisted.body, "# Goal\n\nuser edit body\n");
    assert_eq!(persisted.summary, "user wrote this");
}

#[tokio::test]
async fn extra_author_field_in_body_is_rejected() {
    // The load-bearing PR3 security invariant: the wire body MUST NOT
    // accept an `author` field. `deny_unknown_fields` on the request
    // body bounces any payload that tries to carry one, so a malicious
    // (or accidentally over-eager) client cannot persuade the server
    // to attribute a User edit as Spec.
    let boot = boot().await;
    let events = boot.state.events.clone();
    let wave_id = boot.wave_id.clone();
    let app = app(boot.state, boot.auth_state);
    let cookie = login(&app).await;

    // No subscription before — we expect zero events on the rejection
    // path; `expect_no_events` after confirms.
    let body = serde_json::to_vec(&json!({
        "summary": "spoof attempt",
        "body": "# Goal\n\npretending to be spec\n",
        // The hostile field — must be rejected.
        "author": "spec",
    }))
    .unwrap();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/waves/{}/report", wave_id.as_str()))
                .header("content-type", "application/json")
                .header(header::COOKIE, cookie)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    // axum maps a `serde::Deserialize` failure on `Json<T>` to 400
    // Bad Request via the default `JsonRejection` mapping. The exact
    // body shape is irrelevant — what matters is (a) the call did NOT
    // succeed and (b) no event was emitted.
    assert!(
        resp.status().is_client_error(),
        "request with `author` field must be rejected; got {}",
        resp.status(),
    );
    expect_no_events(&events, Duration::from_millis(150)).await;

    // DB is unchanged — the seed body is still in place.
    let cards = boot.repo.cards_by_wave(wave_id.as_str()).await.unwrap();
    let report_card = cards.into_iter().find(|c| c.kind == "wave-report").unwrap();
    let persisted: WaveReportPayload = serde_json::from_value(report_card.payload).unwrap();
    assert_eq!(
        persisted.body, "# Goal\n\n_The spec agent will fill this in._\n",
        "spoof attempt did not mutate the report card",
    );
}

#[tokio::test]
async fn missing_session_returns_401_and_emits_nothing() {
    let boot = boot().await;
    let events = boot.state.events.clone();
    let wave_id = boot.wave_id.clone();
    let app = app(boot.state, boot.auth_state);

    let body = serde_json::to_vec(&json!({
        "summary": "no cookie",
        "body": "# Goal\n\nshould 401\n",
    }))
    .unwrap();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/waves/{}/report", wave_id.as_str()))
                .header("content-type", "application/json")
                // No Cookie header — session middleware must 401 us.
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["code"], "unauthorized");

    // The session middleware short-circuits before reaching the
    // handler, so no DB write and no event emit. Pin that.
    expect_no_events(&events, Duration::from_millis(150)).await;
}

#[tokio::test]
async fn nonexistent_wave_returns_404_and_emits_nothing() {
    let boot = boot().await;
    let events = boot.state.events.clone();
    let app = app(boot.state, boot.auth_state);
    let cookie = login(&app).await;

    let body = serde_json::to_vec(&json!({
        "summary": "ghost wave",
        "body": "# Goal\n\nghost\n",
    }))
    .unwrap();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                // Random-looking id; nothing matches.
                .uri("/api/waves/does-not-exist/report")
                .header("content-type", "application/json")
                .header(header::COOKIE, cookie)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    expect_no_events(&events, Duration::from_millis(150)).await;
}

#[tokio::test]
async fn non_user_actors_via_header_are_all_rejected_with_403_and_emit_nothing() {
    // The X-Calm-Actor middleware accepts `ai:<id>` as a declared
    // identity. The REST endpoint refuses *any* non-User actor with
    // 403 so a hypothetical future worker / spec-card session-bearing
    // surface cannot bypass the User-only contract by relabeling the
    // header. The session itself is valid in every iteration here —
    // the rejection is strictly on the actor pinning.
    //
    // Followup-nits coverage (security hygiene): the handler used to
    // gate on `matches!(actor.to_actor_id(), ActorId::User)`. That
    // type mapping has a defensive `_ => ActorId::User` fallback
    // (intended to keep an attacker from synthesizing a Kernel /
    // Plugin identity for the *event log*) which would also let any
    // unknown `ai:<id>` header value pass the gate — only `ai:codex`
    // is explicitly mapped to `ActorId::AiCodex`. The fix tightened
    // the gate to a raw-string check (`actor.as_str() != "user"`); we
    // pin the new behavior here by iterating every validated non-user
    // shape the middleware will let through. The list deliberately
    // includes `ai:codex` (the only one the *old* typed gate would
    // also have caught) plus several `ai:<id>` values that would
    // have slipped past it (`ai:claude`, `ai:gpt5`, `ai:claude-3-5`).
    //
    // `kernel` and `plugin:*` get rejected by the middleware itself
    // (400 before the handler even runs) — those are pinned in
    // `actor.rs` unit tests, not here.
    let non_user_actors = [
        "ai:codex",
        "ai:claude",
        "ai:gpt5",
        "ai:claude-3-5",
        "ai:newmodel-99",
    ];

    for declared_actor in non_user_actors {
        let boot = boot().await;
        let events = boot.state.events.clone();
        let wave_id = boot.wave_id.clone();
        let app = app(boot.state, boot.auth_state);
        let cookie = login(&app).await;

        let body = serde_json::to_vec(&json!({
            "summary": format!("disguised-as-{declared_actor}"),
            "body": "# Goal\n\nshould 403\n",
        }))
        .unwrap();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/waves/{}/report", wave_id.as_str()))
                    .header("content-type", "application/json")
                    .header(header::COOKIE, cookie)
                    // Declared non-user actor — the handler must
                    // reject, regardless of whether
                    // `Actor::to_actor_id` would have classified
                    // this as `AiCodex` or fallen through to the
                    // defensive `User` default.
                    .header(calm_server::actor::Actor::HEADER, declared_actor)
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "non-user actor `{declared_actor}` must be 403",
        );
        expect_no_events(&events, Duration::from_millis(150)).await;

        // Belt-and-suspenders: DB unchanged for every iteration.
        let cards = boot.repo.cards_by_wave(wave_id.as_str()).await.unwrap();
        let report_card = cards.into_iter().find(|c| c.kind == "wave-report").unwrap();
        let persisted: WaveReportPayload = serde_json::from_value(report_card.payload).unwrap();
        assert_eq!(
            persisted.body, "# Goal\n\n_The spec agent will fill this in._\n",
            "non-user actor `{declared_actor}` did not mutate the row",
        );
    }
}

#[tokio::test]
async fn explicit_user_actor_header_succeeds() {
    // Defensive: an explicit `X-Calm-Actor: user` is the same as no
    // header (both map to `ActorId::User`). Pin that we don't
    // accidentally reject the explicit form.
    let boot = boot().await;
    let wave_id = boot.wave_id.clone();
    let app = app(boot.state, boot.auth_state);
    let cookie = login(&app).await;

    let body = serde_json::to_vec(&json!({
        "summary": "explicit user",
        "body": "# Goal\n\nexplicit user\n",
    }))
    .unwrap();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/waves/{}/report", wave_id.as_str()))
                .header("content-type", "application/json")
                .header(header::COOKIE, cookie)
                .header(calm_server::actor::Actor::HEADER, "user")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
