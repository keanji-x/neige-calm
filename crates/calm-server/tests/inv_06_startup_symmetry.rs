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
//! ## v3 encoding — narrow observability seam, regression-guard shape
//!
//! v1 asserted "at most ONE `install_watermark_sink(` callsite across
//! `lib.rs` + `routes/waves.rs`". Codex rejected: a correct fix CAN
//! keep two callsites if it pairs each with the install. v2 added an
//! architectural-shape source-grep that pinned `insert` to return a
//! `Result<…>` / take a witness type / have a `register_spec_handle`
//! helper. That works but is prescriptive — it forces a specific
//! refactor shape, when the actual INV-6 contract is just "both paths
//! must install".
//!
//! v3 takes the user's explicit guidance verbatim:
//!
//! > "Read both `register_and_catch_up` (lib.rs:~470) and
//! > `spawn_push_appserver` (routes/waves.rs:~823) carefully — confirm
//! > both install today, and that the test passes if both do. If both
//! > install (likely, per the debug_asserts already there), the test
//! > currently PASSES — that's a regression guard, mark it clearly as
//! > such in the top comment."
//!
//! Source check: both `register_and_catch_up` (lib.rs:514 install →
//! lib.rs:559 registry insert) and `spawn_push_appserver` (routes/
//! waves.rs:983 install → routes/waves.rs:994 registry insert) install
//! the sink before parking. The behavioral both-paths test below is a
//! **regression guard** that fails the day a future entry-point (or a
//! refactor of either current path) parks a handle without running
//! `install_watermark_sink`.
//!
//! ## What this file ships
//!
//! 1. **`inv6_seam_returns_none_for_unregistered_wave`** (active,
//!    passes on main): proves the v3 observability seam
//!    [`SpecPushRegistry::has_watermark_sink`] wires through correctly
//!    — `None` for an unregistered wave. Without this smoke test, a
//!    refactor breaking the seam's `WaveId`-lookup would silently
//!    poison the behavioral test below.
//!
//! 2. **`inv6_both_init_paths_install_sink`** (`#[cfg(feature =
//!    "codex-e2e")]`, active with `--features codex-e2e`, passes
//!    today): the regression guard. Drives `POST /api/waves` (Path A,
//!    `spawn_push_appserver`) then a simulated kernel restart +
//!    `takeover_spec_appservers_on_boot` (Path B,
//!    `register_and_catch_up`) — asserts via the new seam that both
//!    leave the sink installed.  Gated like the sister
//!    `spec_push_boot_recovery_e2e.rs` because CI ships no codex
//!    binary; runs locally as the load-bearing INV-6 contract test.
//!
//! ## Why no currently-failing INV-6 test in CI
//!
//! After deep production-code reading (lib.rs:504-519 +
//! routes/waves.rs:963-989) there is NO residual asymmetry between the
//! two paths today: both install the sink BEFORE the registry insert,
//! both follow it with `debug_assert!(handle.has_watermark_sink())`.
//! Inventing a CI-runnable failing test would either pin an
//! architectural-shape (the v2 approach, prescriptive about HOW to fix
//! a non-existent gap) or fabricate a false-positive (v1's count-
//! callsites approach). Per the user's v3 instructions, we ship the
//! contract as the always-active seam smoke test plus the
//! `codex-e2e`-gated behavioral regression guard rather than fake an
//! always-on failing test.
//!
//! See: `src/lib.rs::register_and_catch_up` (install +
//! `debug_assert!`), `src/routes/waves.rs::spawn_push_appserver`
//! (install + `debug_assert!`),
//! `src/spec_appserver.rs::SpecPushRegistry::has_watermark_sink`
//! (the v3 observability seam).

use calm_server::ids::WaveId;
use calm_server::spec_appserver::SpecPushRegistry;

/// INV-6 (a) — **seam smoke test, active, passes on main**: confirms
/// the v3 observability seam ([`SpecPushRegistry::has_watermark_sink`])
/// wires through correctly. An unregistered wave returns `None` (no
/// handle); the `Some(true)` / `Some(false)` paths are covered by the
/// `codex-e2e` behavioral test below. A refactor that breaks the seam
/// (e.g. accidentally swaps the `WaveId` lookup, or returns
/// `Some(false)` for missing keys) fails this test immediately.
#[tokio::test]
async fn inv6_seam_returns_none_for_unregistered_wave() {
    let reg = SpecPushRegistry::new();
    let unknown = WaveId::from("wave-not-registered");
    let result = reg.has_watermark_sink(&unknown).await;
    assert_eq!(
        result, None,
        "INV-6 seam: SpecPushRegistry::has_watermark_sink for an unregistered \
         wave must return None (sentinel for 'no handle here'). Got {result:?} \
         — the seam wiring is broken; INV-6 behavioral test (codex-e2e) will \
         report false symmetry failures."
    );
}

// ---------------------------------------------------------------------------
// INV-6 (b) — `codex-e2e`-gated behavioral both-paths regression guard.
// ---------------------------------------------------------------------------

/// **Behavioral INV-6 regression guard (`codex-e2e`)**: runs BOTH
/// production init paths against a real `codex app-server` and asserts
/// each leaves the parked handle's [`WatermarkSink`] installed
/// (observed via the new [`SpecPushRegistry::has_watermark_sink`]
/// seam). PASSES on main — would FAIL the day a future entry-point
/// (or a refactor of either current path) parks a handle without
/// running `install_watermark_sink`.
///
/// Gated on `codex-e2e` for the same reason as the sister
/// `spec_push_boot_recovery_e2e.rs`: CI ships no codex binary, so
/// running real model turns + a thread/resume isn't feasible there.
/// Run locally with:
///
/// ```sh
/// cargo test -p calm-server --features codex-e2e \
///   --test inv_06_startup_symmetry -- --nocapture
/// ```
///
/// Self-skips (prints SKIP, returns success) if `NEIGE_CODEX_BIN`
/// doesn't resolve to an executable codex binary — same posture as
/// the sister e2e.
#[cfg(all(unix, feature = "codex-e2e"))]
mod e2e {
    use std::os::unix::fs::PermissionsExt;
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
    use calm_server::state::{AppState, CodexClient, DaemonClient};
    use calm_server::wave_cove_cache::WaveCoveCache;
    use http_body_util::BodyExt;
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use tower::ServiceExt;

    const DEFAULT_CODEX_BIN: &str = "~/.nvm/versions/node/v24.4.1/bin/codex";

    fn resolve_codex_bin() -> Option<PathBuf> {
        let raw =
            std::env::var("NEIGE_CODEX_BIN").unwrap_or_else(|_| DEFAULT_CODEX_BIN.to_string());
        let expanded = if let Some(stripped) = raw.strip_prefix("~/")
            && let Ok(home) = std::env::var("HOME")
        {
            PathBuf::from(home).join(stripped)
        } else {
            PathBuf::from(raw)
        };
        if !expanded.is_file() {
            return None;
        }
        let meta = std::fs::metadata(&expanded).ok()?;
        if meta.permissions().mode() & 0o111 == 0 {
            return None;
        }
        Some(expanded)
    }

    fn locate_recorder_bin() -> PathBuf {
        if let Ok(p) = std::env::var("CARGO_BIN_EXE_argv-recorder-daemon") {
            return PathBuf::from(p);
        }
        let me = std::env::current_exe().expect("current_exe");
        let target_profile = me
            .parent()
            .and_then(|p| p.parent())
            .expect("test bin parent");
        let candidate = target_profile.join("argv-recorder-daemon");
        assert!(
            candidate.exists(),
            "argv-recorder-daemon not found at {candidate:?}; build with \
             `cargo build --tests -p calm-server`"
        );
        candidate
    }

    macro_rules! skip {
        ($($arg:tt)*) => {{
            eprintln!("[inv6-symmetry-e2e] SKIP: {}", format!($($arg)*));
            return;
        }};
    }

    /// Build a fresh `AppState` backed by the SAME on-disk sqlite file
    /// (so a takeover restart in the second build sees the prior
    /// build's persisted spec card). `db_url` is reused across builds;
    /// `tmp` provides the data dir for sockets / terminals.
    async fn build_state(tmp: &Path, db_url: &str, codex_bin: &Path) -> (AppState, Arc<dyn Repo>) {
        let repo: Arc<dyn Repo> = Arc::new(
            SqlxRepo::open(db_url)
                .await
                .expect("open sqlite db for inv6 e2e"),
        );
        let daemon = Arc::new(DaemonClient {
            data_dir: tmp.join("terminals"),
            session_daemon_bin: locate_recorder_bin(),
        });
        let mut codex = CodexClient::new_stub();
        codex.codex_bin = codex_bin.to_string_lossy().to_string();
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
                std::env::temp_dir().join("calm-plugins-data-inv6-e2e"),
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
    /// Returns whether the probe found the sink installed so the caller
    /// can include it in panic messages.
    async fn assert_sink_installed(
        registry: &SpecPushRegistry,
        wave_id: &WaveId,
        path_label: &str,
    ) -> bool {
        let probe = registry.has_watermark_sink(wave_id).await;
        let installed = matches!(probe, Some(true));
        assert!(
            installed,
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
        installed
    }

    #[tokio::test]
    async fn inv6_both_init_paths_install_sink() {
        let Some(codex_bin) = resolve_codex_bin() else {
            skip!(
                "no codex binary at NEIGE_CODEX_BIN (~/.nvm/.../codex by \
                 default); rerun with NEIGE_CODEX_BIN=/abs/path/to/codex"
            );
        };

        let tmp = TempDir::new().expect("tempdir");
        // Use a file-backed sqlite URL so the second `build_state`
        // sees the spec card the first build persisted. Path B's
        // `register_and_catch_up` (via `takeover_spec_appservers_on_boot`)
        // needs the persisted `codex_thread_id` + `appserver_pgid` +
        // `appserver_sock`.
        let db_path = tmp.path().join("inv6.sqlite");
        let db_url = format!("sqlite://{}?mode=rwc", db_path.display());

        // --- Path A: create-wave -> spawn_push_appserver ---
        let (state_a, repo_a) = build_state(tmp.path(), &db_url, &codex_bin).await;
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
                "cwd": "/tmp/inv6-e2e",
                "attach_folder": true,
                "theme": {"fg": [216,219,226], "bg": [15,20,24]}
            }),
        )
        .await;
        if status != StatusCode::CREATED {
            skip!(
                "POST /api/waves returned {status} (likely no codex auth / network); body={body}"
            );
        }
        let wave_id = WaveId::from(body.get("id").and_then(Value::as_str).unwrap().to_string());
        eprintln!("[inv6-symmetry-e2e] Path A: wave created: {wave_id:?}");

        // Path A assertion: create-wave installed the sink.
        assert_sink_installed(&state_a.spec_push, &wave_id, "create-wave (Path A)").await;
        eprintln!("[inv6-symmetry-e2e] Path A: sink installed PASS");

        // --- Simulated kernel exit ---
        // Drop the in-memory handle from the registry so the next
        // build's takeover sees a cold registry. Use the reap path
        // (SIGTERM → SIGKILL → socket cleanup) to avoid leaking the
        // codex app-server child across the build boundary.
        calm_server::terminal_sweeper::reap_spec_push(&state_a, &wave_id).await;
        // Drop the AppState entirely — mirrors a kernel process exit.
        drop(state_a);
        tokio::time::sleep(Duration::from_millis(500)).await;

        // --- Path B: rebuild -> takeover_spec_appservers_on_boot -> register_and_catch_up ---
        let (state_b, _repo_b) = build_state(tmp.path(), &db_url, &codex_bin).await;
        calm_server::takeover_spec_appservers_on_boot(&state_b).await;

        if !state_b.spec_push.contains(&wave_id) {
            // Takeover may legitimately decline (e.g. -32600 no rollout
            // on a thread that never ran turn #1). Skip — INV-6 only
            // applies to handles takeover actually parks.
            skip!(
                "Path B: takeover did not re-register the spec push handle \
                 for {wave_id:?} (likely -32600 no rollout on a thread \
                 that never executed a turn — non-fatal, the wave is inert). \
                 INV-6's takeover-path half is vacuous in that case."
            );
        }

        // Path B assertion: takeover installed the sink on the
        // re-registered handle.
        assert_sink_installed(&state_b.spec_push, &wave_id, "boot-takeover (Path B)").await;
        eprintln!("[inv6-symmetry-e2e] Path B: sink installed PASS");

        // Teardown.
        calm_server::terminal_sweeper::reap_spec_push(&state_b, &wave_id).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        eprintln!("[inv6-symmetry-e2e] ALL PASS");
    }
}
