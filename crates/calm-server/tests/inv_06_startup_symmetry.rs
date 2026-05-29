//! # INV-6 — startup-entry symmetry (create-wave vs takeover)
//!
//! **Bug**: R3-B2 (from #318)
//! **Encoded contract**: the two paths that park a `SpecPushHandle` in
//! `SpecPushRegistry` — `routes::waves::spawn_push_appserver` (fresh
//! wave) and `lib::register_and_catch_up` (boot takeover) — must run
//! the SAME init hook. Concretely: every parked handle must have a
//! `WatermarkSink` installed before it can be reached by a push;
//! without it, queued-then-flushed observations silently fail to
//! advance the durable `push_watermark`, and boot recovery
//! double-pushes those envelopes forever.
//!
//! ## v4 encoding — CI-runnable behavioral guard via fake codex
//!
//! v3 shipped the behavioral both-paths test gated on `codex-e2e`
//! (needs a real `codex` binary) plus a seam-smoke that only proves
//! the [`SpecPushRegistry::has_watermark_sink`] wiring works on an
//! unregistered wave. The user pushed back: the gated test isn't a
//! CI invariant. v4 lifts the gate by pointing the spawn at the
//! [`common::fake_codex_client`] fixture — the same `osc-probe-child`
//! fake `theme_osc_roundtrip` and the `wave_create_with_theme`
//! integration tests already use — which answers exactly the
//! `initialize` / `thread/start` / `thread/resume` / `turn/start`
//! handshake both production paths drive (see
//! `tests/fixtures/osc-probe-child/appserver.rs`). The
//! sister real-codex regression test
//! (`spec_push_boot_recovery_e2e.rs::boot_takeover_resumes_…`) keeps
//! covering the same scenario end-to-end under `--features codex-e2e`.
//!
//! ## What this file ships
//!
//! 1. **`inv6_seam_returns_none_for_unregistered_wave`** (active,
//!    passes on main): proves the observability seam
//!    [`SpecPushRegistry::has_watermark_sink`] wires through correctly
//!    — `None` for an unregistered wave. Without this smoke test, a
//!    refactor breaking the seam's `WaveId`-lookup would silently
//!    poison the behavioral test below.
//!
//! 2. **`inv6_both_init_paths_install_sink`** (active in CI, passes on
//!    main): the regression guard. Drives `POST /api/waves` (Path A,
//!    `spawn_push_appserver`) then a simulated kernel restart +
//!    `takeover_spec_appservers_on_boot` (Path B,
//!    `register_and_catch_up`) — asserts via the seam that both leave
//!    the sink installed. Both paths spawn the
//!    `osc-probe-child` fake app-server (no real codex needed) so it
//!    runs in default CI. Source check: both
//!    `register_and_catch_up` (`lib.rs::~514` install →
//!    `lib.rs::~559` registry insert) and `spawn_push_appserver`
//!    (`routes/waves.rs::~983` install → `routes/waves.rs::~994`
//!    registry insert) install the sink before parking. The day a
//!    future entry-point (or a refactor of either current path) parks
//!    a handle without running `install_watermark_sink`, this test
//!    fails.
//!
//! See: `src/lib.rs::register_and_catch_up` (install +
//! `debug_assert!`), `src/routes/waves.rs::spawn_push_appserver`
//! (install + `debug_assert!`),
//! `src/spec_appserver.rs::SpecPushRegistry::has_watermark_sink`
//! (the observability seam).

use calm_server::ids::WaveId;
use calm_server::spec_appserver::SpecPushRegistry;

#[cfg(unix)]
mod common;

/// INV-6 (a) — **seam smoke test, active, passes on main**: confirms
/// the observability seam ([`SpecPushRegistry::has_watermark_sink`])
/// wires through correctly. An unregistered wave returns `None` (no
/// handle); the `Some(true)` path is covered by the behavioral test
/// below. A refactor that breaks the seam (e.g. accidentally swaps
/// the `WaveId` lookup, or returns `Some(false)` for missing keys)
/// fails this test immediately.
#[tokio::test]
async fn inv6_seam_returns_none_for_unregistered_wave() {
    let reg = SpecPushRegistry::new();
    let unknown = WaveId::from("wave-not-registered");
    let result = reg.has_watermark_sink(&unknown).await;
    assert_eq!(
        result, None,
        "INV-6 seam: SpecPushRegistry::has_watermark_sink for an unregistered \
         wave must return None (sentinel for 'no handle here'). Got {result:?} \
         — the seam wiring is broken; INV-6 behavioral test will report false \
         symmetry failures."
    );
}

// ---------------------------------------------------------------------------
// INV-6 (b) — default-active behavioral both-paths regression guard.
// ---------------------------------------------------------------------------

/// **Behavioral INV-6 regression guard (default-active CI test)**: runs
/// BOTH production init paths against the `osc-probe-child` fake codex
/// app-server (no real codex binary required) and asserts each leaves
/// the parked handle's [`WatermarkSink`] installed (observed via the
/// [`SpecPushRegistry::has_watermark_sink`] seam). PASSES on main —
/// would FAIL the day a future entry-point (or a refactor of either
/// current path) parks a handle without running
/// `install_watermark_sink`.
///
/// The fake app-server (see `tests/fixtures/osc-probe-child/appserver.rs`)
/// answers the `initialize` / `thread/start` / `thread/resume` /
/// `turn/start` handshake both production paths drive, which is
/// everything `spawn_spec_appserver` (create-wave) and
/// `resume_spec_appserver` (boot takeover) need to construct a parked
/// `SpecPushHandle`. The sister `spec_push_boot_recovery_e2e.rs::\
/// boot_takeover_resumes_…` test exercises the same scenario against a
/// real codex under `--features codex-e2e`.
#[cfg(unix)]
mod behavioral {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use calm_server::card_role_cache::CardRoleCache;
    use calm_server::db::prelude::Repo;
    use calm_server::db::sqlite::SqlxRepo;
    use calm_server::event::EventBus;
    use calm_server::ids::WaveId;
    use calm_server::model::NewCove;
    use calm_server::plugin_host::{PluginHost, PluginRegistry};
    use calm_server::routes;
    use calm_server::spec_appserver::SpecPushRegistry;
    use calm_server::state::{AppState, DaemonClient};
    use calm_server::wave_cove_cache::WaveCoveCache;
    use http_body_util::BodyExt;
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use tower::ServiceExt;

    use super::common;

    /// `argv-recorder-daemon` test-binary path — Cargo drops it next to
    /// the integration-test bin. Used as the spec card's session daemon
    /// (it just records its argv and binds the unix socket the kernel
    /// polls; the takeover path doesn't touch this anyway, but a wave
    /// create needs a daemon that succeeds).
    fn locate_recorder_bin() -> PathBuf {
        PathBuf::from(env!("CARGO_BIN_EXE_argv-recorder-daemon"))
    }

    /// Build a fresh `AppState` backed by the SAME on-disk sqlite file
    /// (so a takeover restart in the second build sees the prior
    /// build's persisted spec card). `db_url` is reused across builds;
    /// `tmp` provides the data dir for sockets / terminals.
    ///
    /// `codex_bin` points at the `osc-probe-child` fake app-server (see
    /// `common::fake_codex_client`), so both `POST /api/waves` (Path A)
    /// and `takeover_spec_appservers_on_boot` (Path B) drive a real
    /// JSON-RPC handshake without needing a real codex binary.
    async fn build_state(tmp: &Path, db_url: &str) -> (AppState, Arc<dyn Repo>) {
        let repo: Arc<dyn Repo> = Arc::new(
            SqlxRepo::open(db_url)
                .await
                .expect("open sqlite db for inv6 behavioral test"),
        );
        let daemon = Arc::new(DaemonClient {
            data_dir: tmp.join("terminals"),
            session_daemon_bin: locate_recorder_bin(),
            proc_supervisor_sock: None,
        });
        let codex = common::fake_codex_client();
        let card_role_cache = CardRoleCache::new();
        let wave_cove_cache = WaveCoveCache::new();
        let state = AppState::from_parts(
            repo.clone(),
            EventBus::new(),
            daemon,
            Arc::new(PluginHost::new_full(
                Arc::new(PluginRegistry::empty()),
                repo.clone(),
                PathBuf::new(),
                std::env::temp_dir().join("calm-plugins-data-inv6-behavioral"),
                Vec::new(),
                EventBus::new(),
                card_role_cache.clone(),
                wave_cove_cache.clone(),
            )),
            Arc::new(codex),
            Some(card_role_cache),
            Some(wave_cove_cache),
        );
        (state, repo)
    }

    async fn post_json(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
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

    /// Assert the sink-installed contract for `wave_id` on `registry`.
    async fn assert_sink_installed(
        registry: &SpecPushRegistry,
        wave_id: &WaveId,
        path_label: &str,
    ) {
        let probe = registry.has_watermark_sink(wave_id).await;
        assert!(
            matches!(probe, Some(true)),
            "INV-6 violated ({path_label} path, R3-B2): SpecPushRegistry::\
             has_watermark_sink({wave_id}) = {probe:?}; expected Some(true). \
             The {path_label} init path parked a handle WITHOUT installing the \
             WatermarkSink — queued-then-flushed envelopes from this handle \
             will silently fail to persist their watermark, causing boot \
             catch-up to double-push them on the next restart. The two \
             production install sites are lib.rs::register_and_catch_up (boot \
             takeover) and routes/waves.rs::spawn_push_appserver (create-wave) \
             — verify both still call `handle.install_watermark_sink(\
             state.dispatcher.watermark_sink_for(card_key))` before the \
             registry insert."
        );
    }

    #[tokio::test]
    async fn inv6_both_init_paths_install_sink() {
        let tmp = TempDir::new().expect("tempdir");
        // Use a file-backed sqlite URL so the second `build_state`
        // sees the spec card the first build persisted. Path B's
        // `register_and_catch_up` (via `takeover_spec_appservers_on_boot`)
        // needs the persisted `codex_thread_id` + `appserver_pgid` +
        // `appserver_sock`.
        let db_path = tmp.path().join("inv6.sqlite");
        let db_url = format!("sqlite://{}?mode=rwc", db_path.display());

        // --- Path A: create-wave -> spawn_push_appserver ---
        let (state_a, repo_a) = build_state(tmp.path(), &db_url).await;
        let cove = repo_a
            .cove_create(NewCove {
                name: "inv6".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let app_a = routes::router()
            .layer(axum::middleware::from_fn(
                calm_server::actor::actor_middleware,
            ))
            .with_state(state_a.clone());
        let (status, body) = post_json(
            app_a,
            "/api/waves",
            json!({
                "cove_id": cove.id,
                "title": "inv6 symmetry wave",
                "cwd": "/tmp/inv6-behavioral",
                "attach_folder": true,
                "theme": {"fg": [216,219,226], "bg": [15,20,24]}
            }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "POST /api/waves must succeed with the fake codex fixture; body={body}"
        );
        let wave_id = WaveId::from(body.get("id").and_then(Value::as_str).unwrap().to_string());
        eprintln!("[inv6-behavioral] Path A: wave created: {wave_id:?}");

        // Path A assertion: create-wave installed the sink.
        assert_sink_installed(&state_a.spec_push, &wave_id, "create-wave (Path A)").await;
        eprintln!("[inv6-behavioral] Path A: sink installed PASS");

        // --- Simulated kernel exit ---
        // Reap the in-memory handle from the registry so the next
        // build's takeover sees a cold registry. Uses the reap path
        // (SIGTERM → SIGKILL → socket cleanup) to ensure the fake
        // app-server child is gone before respawn.
        calm_server::terminal_sweeper::reap_spec_push(&state_a, &wave_id).await;
        // Drop the AppState entirely — mirrors a kernel process exit.
        drop(state_a);
        tokio::time::sleep(Duration::from_millis(200)).await;

        // --- Path B: rebuild -> takeover_spec_appservers_on_boot -> register_and_catch_up ---
        let (state_b, _repo_b) = build_state(tmp.path(), &db_url).await;
        calm_server::takeover_spec_appservers_on_boot(&state_b).await;

        assert!(
            state_b.spec_push.contains(&wave_id),
            "Path B: takeover did not re-register the spec push handle for \
             {wave_id:?} — the fake app-server should answer `thread/resume` \
             successfully, so the only way this fails is if `resume_spec_appserver` \
             (lib.rs::try_takeover_one_wave) regressed. Without a parked handle, \
             the INV-6 takeover-path assertion below is vacuous."
        );

        // Path B assertion: takeover installed the sink on the
        // re-registered handle.
        assert_sink_installed(&state_b.spec_push, &wave_id, "boot-takeover (Path B)").await;
        eprintln!("[inv6-behavioral] Path B: sink installed PASS");

        // Teardown.
        calm_server::terminal_sweeper::reap_spec_push(&state_b, &wave_id).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        eprintln!("[inv6-behavioral] ALL PASS");
    }
}
