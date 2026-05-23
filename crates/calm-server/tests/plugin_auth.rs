//! Slice H integration tests — per-plugin auth tokens.
//!
//! Post-M5 (m3-mcp-apps) the iframe-cookie half of Slice H is gone — the
//! `IframeCookieCache` and `iframe-write` REST route were deleted when
//! AppBridge took over the iframe ↔ host channel (see migration doc §3.3).
//! What remains covers the process token surface:
//!
//!   1. Process token mint reuses + rotates (host-level).
//!   2. Auth-mismatch kills the plugin and does NOT respawn (host-level).
//!   3. `experimental.dev.neige/kernel-callbacks` capability gate (M1).
//!
//! Test #2 reuses the echo stub with `STUB_ECHO_OVERRIDE` env to force a
//! wrong echo response — that env-driven branch lives in the echo stub's
//! `main.rs` so we don't need a fourth fixture binary.

#![cfg(unix)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::plugin_host::{
    Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus, hash_token, verify_token,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::{Instant, sleep};

const ECHO_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-echo");
const CALLER_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-caller");

// ---------------------------------------------------------------------------
// Host fixtures (mirror plugin_host_smoke.rs but parameterized for env)
// ---------------------------------------------------------------------------

async fn boot_host(
    plugin_id: &str,
    extra_env: &[(&str, &str)],
) -> (Arc<PluginHost>, Arc<dyn Repo>, TempDir, EventBus) {
    let tmp = tempfile::tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    let install_dir = plugins_dir.join(plugin_id);
    let bin_dir = install_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&plugins_data_dir).unwrap();
    std::os::unix::fs::symlink(Path::new(ECHO_BIN), bin_dir.join("stub")).unwrap();

    let env_map: serde_json::Map<String, Value> = extra_env
        .iter()
        .map(|(k, v)| ((*k).to_string(), Value::String((*v).to_string())))
        .collect();
    let manifest_json = json!({
        "manifest_version": 1,
        "id": plugin_id,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": "Auth Stub",
        "entrypoint": { "command": "bin/stub", "env": env_map }, "theme": {"fg": [216,219,226], "bg": [15,20,24]} });
    let manifest: Manifest = Manifest::parse(&manifest_json.to_string()).expect("manifest parses");

    let registry = PluginRegistry::empty();
    registry.insert(manifest, Some(install_dir.clone()));
    let events = EventBus::new();
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite repo"),
    );
    // Production flow installs the plugin row (via the REST install handler)
    // before `spawn` runs; these tests bypass the REST surface and call
    // `host.spawn` directly. With MockRepo this happened to work because the
    // mock didn't enforce FKs — SqlxRepo's `plugin_tokens.plugin_id` FK
    // requires a real plugins row. Seed it here.
    repo.plugin_install(calm_server::model::NewPlugin {
        id: plugin_id.into(),
        version: "0.1.0".into(),
        install_path: install_dir.display().to_string(),
        manifest: json!({}),
        enabled: true,
        user_config: json!({}),
    })
    .await
    .expect("seed plugin row");
    let host = Arc::new(PluginHost::new_full(
        Arc::new(registry),
        repo.clone(),
        plugins_dir,
        plugins_data_dir,
        Vec::new(),
        events.clone(),
        calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::wave_cove_cache::WaveCoveCache::new(),
    ));
    (host, repo, tmp, events)
}

async fn wait_for_status(
    host: &Arc<PluginHost>,
    id: &str,
    pred: impl Fn(&PluginRuntimeStatus) -> bool,
    timeout: Duration,
) -> Option<PluginRuntimeStatus> {
    let start = Instant::now();
    loop {
        if let Some(snap) = host.status(id).await
            && pred(&snap.status)
        {
            return Some(snap.status);
        }
        if start.elapsed() > timeout {
            return host.status(id).await.map(|s| s.status);
        }
        sleep(Duration::from_millis(25)).await;
    }
}

// ===========================================================================
// 1. Process token mint reuses + rotates
// ===========================================================================
//
// `ensure_plugin_token` is documented to mint fresh every call (raw not
// recoverable from hash). We assert the *hash* gets persisted on spawn, and
// that rotate_plugin_token both kills + respawns + swaps the hash.

#[tokio::test]
async fn process_token_persists_hash_on_spawn() {
    let (host, repo, _tmp, _events) = boot_host("test.tok1", &[]).await;
    host.spawn("test.tok1").await.expect("spawn");
    wait_for_status(
        &host,
        "test.tok1",
        |s| matches!(s, PluginRuntimeStatus::Running),
        Duration::from_secs(3),
    )
    .await
    .unwrap();

    // The repo should now have a token row whose hash is NOT the raw value
    // (it's a SHA-256 hex). We can't recover the raw from the row, but we
    // can assert the hash length is 64 hex chars.
    let row = repo
        .plugin_token_get("test.tok1")
        .await
        .unwrap()
        .expect("token row");
    assert_eq!(row.0.len(), 64, "hashed token should be 64 hex chars");
    assert!(row.0.chars().all(|c| c.is_ascii_hexdigit()));

    host.stop("test.tok1").await.unwrap();
}

#[tokio::test]
async fn rotate_plugin_token_swaps_hash_and_restarts() {
    let (host, repo, _tmp, _events) = boot_host("test.tok2", &[]).await;
    host.spawn("test.tok2").await.unwrap();
    wait_for_status(
        &host,
        "test.tok2",
        |s| matches!(s, PluginRuntimeStatus::Running),
        Duration::from_secs(3),
    )
    .await
    .unwrap();
    let before = repo.plugin_token_get("test.tok2").await.unwrap().unwrap().0;

    host.rotate_plugin_token("test.tok2")
        .await
        .expect("rotate ok");
    wait_for_status(
        &host,
        "test.tok2",
        |s| matches!(s, PluginRuntimeStatus::Running),
        Duration::from_secs(3),
    )
    .await
    .unwrap();

    let after = repo.plugin_token_get("test.tok2").await.unwrap().unwrap().0;
    assert_ne!(before, after, "rotate must change the stored hash");

    host.stop("test.tok2").await.unwrap();
}

// ===========================================================================
// 2. Auth mismatch → Crashed, no respawn
// ===========================================================================
//
// We point the echo stub at `STUB_ECHO_OVERRIDE=bogus` so it returns a wrong
// echoed_token on initialize. The kernel must:
//   * kill the child,
//   * surface an AuthMismatch error from `spawn`,
//   * NOT install a supervisor task (so no respawn fires).

#[tokio::test]
async fn auth_mismatch_kills_and_does_not_respawn() {
    let (host, _repo, _tmp, _events) = boot_host(
        "test.badauth",
        &[("STUB_ECHO_OVERRIDE", "definitely-not-the-real-token")],
    )
    .await;

    let err = host
        .spawn("test.badauth")
        .await
        .expect_err("spawn should fail");
    // The kernel surfaces AuthMismatch (distinct from InitializeRejected).
    assert!(
        matches!(err, calm_server::plugin_host::HostError::AuthMismatch(_)),
        "expected AuthMismatch, got {err:?}",
    );

    // Wait a moment to confirm no respawn supervisor fires. The host should
    // have no entry for this plugin in processes either way.
    sleep(Duration::from_millis(300)).await;
    let snap = host.status("test.badauth").await;
    assert!(
        snap.is_none(),
        "no supervisor → no processes-map entry; got {:?}",
        snap.map(|s| s.status)
    );
}

// ===========================================================================
// 3. Iframe cookie round-trip via REST — REMOVED in M3 (mcp-apps migration)
// ===========================================================================
//
// Pre-M3 the iframe cookie was minted by `GET /api/plugins/:id/views/:view_id`
// (the legacy iframe-HTML route). M3 deletes that route in favor of MCP
// `resources/read` over postMessage (see `plugin_host::resources` and its
// own tests). M5 will introduce the AppBridge → kernel transport, at which
// point the cookie (if it survives the redesign) gets minted on a different
// path. Until then we have no REST-level cookie minting path to round-trip,
// so the original REST round-trip + revoke tests have been removed.
//
// Cache-level cookie behaviour stays covered by:
//   * `iframe_cookie_expires` (this file, test #4)
//   * the in-module tests in `auth.rs`

// (M3) The boot_app / install_and_enable fixtures that previously drove the
// `view_html` GET → cookie mint flow have been removed alongside the route.
// If a future test needs a full REST app fixture, plugin_routes.rs's
// boot_state + app helpers cover the same surface.

// ===========================================================================
// 4 + 5. Iframe cookie tests — deleted in M5.
// ===========================================================================
//
// Pre-M5 the cache-level mint/verify/expire/revoke tests guarded the second
// auth surface that backed `iframe-write`. Both the route and the cache went
// away in M5 (migration doc §3.3); the trust boundary now lives in the
// `tool-call` route's `neige.*` prefix gate (`plugin_routes_m5.rs`) and the
// CORS allowlist on `main.rs`. Nothing to assert at the cache level.

// ===========================================================================
// Sanity: the auth helpers we re-export from plugin_host are wired correctly.
// (Round-trip is also covered by the in-module tests in `auth.rs`; this is
// the "make sure they're public surface" guard.)
// ===========================================================================

#[test]
fn auth_helpers_reachable_from_public_surface() {
    let raw = "deadbeef".repeat(8); // 64 chars to mirror a real token shape
    let h = hash_token(&raw);
    assert!(verify_token(&raw, &h));
    assert!(!verify_token("nope", &h));
}

// ===========================================================================
// 6. M1: no kernel-callbacks capability → MethodNotFound on neige.*
// ===========================================================================
//
// Spec contract (migration doc §6/M1): a plugin opts into the `neige.*`
// host-callback namespace by echoing `experimental.dev.neige/kernel-callbacks`
// back in its `initialize` response. If absent, the kernel installs a
// MethodNotFound drainer in place of the real dispatcher — so the caller-stub
// firing `neige.overlay.set` should leave the kernel's repo state empty
// (overlay never written, kv never written, no card rows) and the call should
// be answered with -32601.
//
// We can't peek the wire directly (the kernel synthesizes the error frame),
// so the test asserts on the *effect*: with the dispatcher installed, the
// caller-stub's six pipelined neige.* calls touch kv + overlays + cards;
// with the drainer installed, none of those should land.

#[tokio::test]
async fn no_kernel_callbacks_capability_installs_method_not_found_drainer() {
    use calm_server::model::{NewCove, NewWave};

    let plugin_id = "test.nocaps";

    let tmp = tempfile::tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    let install_dir = plugins_dir.join(plugin_id);
    let bin_dir = install_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&plugins_data_dir).unwrap();
    std::os::unix::fs::symlink(Path::new(CALLER_BIN), bin_dir.join("stub")).unwrap();

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
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    // STUB_OMIT_CAPABILITY=1 → the caller-stub's initialize response carries
    // an empty `capabilities` object. The kernel should then install the
    // MethodNotFound drainer.
    let manifest_json = json!({
        "manifest_version": 1,
        "id": plugin_id,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": "No-caps caller",
        "entrypoint": {
            "command": "bin/stub",
            "env": {
                "NEIGE_DEMO_WAVE": wave.id.clone(),
                "STUB_OMIT_CAPABILITY": "1"
            }
        },
        // Permissions are generous on paper, but the dispatcher is never
        // installed so they don't matter — the drainer answers MethodNotFound
        // before perms are consulted.
        "permissions": {
            "overlays_write": ["wave", "card"],
            "cards_create": true,
            "kv_quota_bytes": 1048576
        }, "theme": {"fg": [216,219,226], "bg": [15,20,24]} });
    let manifest = Manifest::parse(&manifest_json.to_string()).expect("manifest parses");
    let registry = PluginRegistry::empty();
    registry.insert(manifest, Some(install_dir.clone()));
    let events = EventBus::new();
    // Seed the plugins row before spawn (FK for plugin_tokens).
    repo.plugin_install(calm_server::model::NewPlugin {
        id: plugin_id.into(),
        version: "0.1.0".into(),
        install_path: install_dir.display().to_string(),
        manifest: json!({}),
        enabled: true,
        user_config: json!({}),
    })
    .await
    .expect("seed plugin row");
    let host = Arc::new(PluginHost::new_full(
        Arc::new(registry),
        // method-call clone is a coercion site for the `Arc<dyn Repo>` →
        // `Arc<dyn RouteRepo>` upcast (PR #41 — kernel-narrow).
        repo.clone(),
        plugins_dir,
        plugins_data_dir,
        Vec::new(),
        events,
        calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::wave_cove_cache::WaveCoveCache::new(),
    ));

    host.spawn(plugin_id).await.expect("spawn no-caps stub");
    wait_for_status(
        &host,
        plugin_id,
        |s| matches!(s, PluginRuntimeStatus::Running),
        Duration::from_secs(3),
    )
    .await
    .expect("plugin running");

    // Give the stub time to pipeline all six callbacks and the kernel time to
    // synthesize MethodNotFound responses for each. 300 ms is conservative
    // vs the ~1 ms per round-trip we see on the dispatcher path.
    sleep(Duration::from_millis(300)).await;

    // With the drainer installed, none of the writes the dispatcher does
    // should have landed:
    let kv = repo.plugin_kv_get(plugin_id, "answer").await.unwrap();
    assert!(
        kv.is_none(),
        "neige.kv.set must NOT touch kv when capability is absent; got {kv:?}"
    );

    let cards = repo.cards_by_wave(wave.id.as_str()).await.unwrap();
    assert!(
        cards.is_empty(),
        "neige.card.create must NOT create cards when capability is absent; got {} cards",
        cards.len()
    );

    let overlays = repo.overlays_for("wave", wave.id.as_str()).await.unwrap();
    assert!(
        overlays.is_empty(),
        "neige.overlay.set must NOT write overlays when capability is absent; got {} overlays",
        overlays.len()
    );

    host.stop(plugin_id).await.expect("stop");
}
