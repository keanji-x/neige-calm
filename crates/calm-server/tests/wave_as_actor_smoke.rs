//! PR6 (#136) — wave-as-actor end-to-end smoke test.
//!
//! Proves PR1-PR6 actually compose:
//!
//!   1. POST /api/coves + POST /api/waves → spec card lands with
//!      `CardRole::Spec` in the cache, two events emit
//!      (WaveUpdated + CardAdded).
//!   2. Hand-emit `Event::CodexJobRequested` with
//!      `actor = ActorId::AiSpec(spec_card_id)` and `scope = Wave`.
//!   3. The PR3 role gate permits the emit (Spec role + WaveUpdated
//!      rule + AiSpec).
//!   4. The PR5 dispatcher picks it up, mints a worker card with
//!      `CardRole::Worker`.
//!   5. PR6's dispatcher activation attempts the daemon spawn (stub
//!      bin → fails, but the worker card row + terminal row land
//!      and a `Event::TaskFailed` follows for PR8 to consume).
//!
//! This is the first end-to-end "wave-as-actor" assertion in the
//! suite. Any future regression that breaks (a) the role-cache
//! write-through, (b) the role gate's `AiSpec(Spec) → WaveUpdated`
//! pathway, or (c) the dispatcher's worker mint will surface here
//! as a single failed test instead of a constellation of
//! unit-test regressions.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::{ActorId, CardId, CoveId, WaveId};
use calm_server::model::{CardRole, NewCove};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    cove_id: String,
    events: EventBus,
    repo: Arc<dyn Repo>,
    card_role_cache: CardRoleCache,
    wave_cove_cache: calm_server::wave_cove_cache::WaveCoveCache,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir for daemon sockets");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "wave-as-actor-smoke".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();

    // Stub daemon bin — spec card daemon spawn will fail at the
    // post-commit phase (which is fine; we test the *event /
    // role-gate / dispatcher* composition, not the daemon path).
    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        session_daemon_bin: PathBuf::from("/nonexistent-daemon-bin-smoke-test"),
    });
    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-smoke"),
            Vec::new(),
            EventBus::new(),
            card_role_cache.clone(),
            wave_cove_cache.clone(),
        )),
        {
            // Deterministically-broken codex bin: an absolute path that
            // does not exist, so the spec-push app-server boot fails fast
            // (`spawn codex app-server: No such file or directory`)
            // regardless of whether a real `codex` happens to be on the
            // test process's PATH. Wave create is now tolerant of this
            // (issue #293 / PR #311): it returns 201 with an inert wave.
            let mut codex = CodexClient::new_stub();
            codex.codex_bin = "/nonexistent-codex-bin-wave-as-actor-smoke".into();
            Arc::new(codex)
        },
        Some(card_role_cache.clone()),
        Some(wave_cove_cache.clone()),
    );

    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);

    Boot {
        app,
        cove_id: cove.id.to_string(),
        events,
        repo,
        card_role_cache,
        wave_cove_cache,
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
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// Poll a predicate every 20ms until it returns Some(...) or the
/// timeout elapses.
async fn wait_for<T, F, Fut>(timeout: Duration, mut f: F) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(v) = f().await {
            return Some(v);
        }
        if tokio::time::Instant::now() > deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn wave_as_actor_smoke_spec_dispatches_worker_via_kernel() {
    let boot = boot().await;

    // 1. POST /api/waves — atomically mints wave + spec card. Issue
    //    #293 / PR #311: the spec-push app-server boot is now NON-FATAL,
    //    so a broken codex bin surfaces as **201** (inert wave) rather
    //    than 500; the wave + spec card + terminal rows still persist and
    //    the WaveUpdated + CardAdded events still emit (events broadcast
    //    at tx commit, before the boot attempt). The test digs into the
    //    persisted state to find the spec card id.
    let (status, _body) = post(
        boot.app.clone(),
        "/api/waves",
        json!({"cove_id": boot.cove_id, "title": "smoke wave", "cwd": "/tmp/issue-250-pr2-test", "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "wave create returns 201 even when the spec app-server boot fails (issue #293 / PR #311 — boot is non-fatal); rows + events still persisted (got: {status:?})",
    );

    // 2. Find the spec card the route minted under the cove.
    let waves = boot
        .repo
        .waves_by_cove(&boot.cove_id)
        .await
        .expect("waves_by_cove");
    assert_eq!(waves.len(), 1, "exactly one wave under the cove");
    let wave = waves.into_iter().next().unwrap();
    let cards = boot
        .repo
        .cards_by_wave(wave.id.as_str())
        .await
        .expect("cards_by_wave");
    // Issue #229 PR B — wave create mints two kernel-owned cards: the
    // spec card (PR6) and the wave-report card (PR B).
    assert_eq!(cards.len(), 2, "wave create mints spec + wave-report cards");
    let spec_card_id = cards
        .iter()
        .find(|c| c.kind == "codex")
        .expect("spec card present")
        .id
        .clone();
    assert_eq!(
        boot.card_role_cache.get(&spec_card_id),
        Some(CardRole::Spec),
        "spec card's role lives in the cache after wave create",
    );

    // 3. Hand-emit a `CodexJobRequested` envelope with
    //    `actor = AiSpec(spec_card_id)` under the wave scope. The
    //    role gate must permit this (PR3 invariant).
    //    `log_pure_event` runs `enforce_role` internally; a violation
    //    surfaces as `CalmError::Forbidden`, so an Ok() return
    //    confirms the gate accepted the AiSpec actor.
    let idem = "smoke-job-1";
    let job = Event::CodexJobRequested {
        idempotency_key: idem.into(),
        goal: "do the smoke test thing".into(),
        context: serde_json::Value::Null,
        acceptance_criteria: None,
    };
    let scope = EventScope::Wave {
        wave: WaveId::from(wave.id.as_str()),
        cove: CoveId::from(boot.cove_id.as_str()),
    };
    let event_id = boot
        .repo
        .log_pure_event(
            ActorId::AiSpec(CardId::from(spec_card_id.as_str())),
            scope,
            None,
            &boot.events,
            &boot.card_role_cache,
            &boot.wave_cove_cache,
            job,
        )
        .await
        .expect("AiSpec(Spec) emit of CodexJobRequested must pass enforce_role");
    assert!(event_id > 0, "events.id stamped");

    // 4. The dispatcher (spawned in AppState::from_parts) consumes
    //    the envelope and mints a worker card. Poll for the worker
    //    card by `idempotency_key`.
    let worker = wait_for(Duration::from_secs(3), || async {
        let cards = boot.repo.cards_by_wave(wave.id.as_str()).await.unwrap();
        cards
            .into_iter()
            .find(|c| c.payload.get("idempotency_key").and_then(|v| v.as_str()) == Some(idem))
    })
    .await
    .expect("worker card minted by the dispatcher within 3s");

    assert_eq!(worker.kind, "codex");
    assert_eq!(
        worker.payload.get("role_request").and_then(|v| v.as_str()),
        Some("codex"),
        "worker payload carries the role_request discriminator",
    );
    assert_eq!(
        boot.card_role_cache.get(&worker.id),
        Some(CardRole::Worker),
        "worker card lands with CardRole::Worker in the cache",
    );
    assert_ne!(
        worker.id, spec_card_id,
        "worker card is distinct from the spec card"
    );
}
