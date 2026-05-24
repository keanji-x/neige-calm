//! #267 E2E — `CodexClient::new_stub()` MUST scope its `codex_homes_dir`
//! to a per-instance tempdir that disappears when the stub drops.
//!
//! The incident this guards against: prior to #267 the stub default was
//! `std::env::temp_dir().join("neige-codex-homes-stub")` — a single
//! global path every test instance wrote into and nobody cleaned up.
//! Across enough test runs the dir accumulated codex's full per-card
//! session state (`logs_*.sqlite`, `history`, the seeded `~/.codex`
//! copy), eventually 134 GB observed in one incident, until the /tmp
//! partition filled. The fix puts a `tempfile::TempDir` inside the
//! `CodexClient` struct so when the test drops its `Arc<CodexClient>`
//! (via `AppState`) the directory and everything under it goes away.
//!
//! This test exercises the property end-to-end against a real
//! `AppState`-shaped construction (i.e. the same shape every other
//! integration test uses), drops the state, and asserts the path is
//! gone. Skipping the assertion would let a regression that resurrected
//! the hardcoded path silently revive the leak.

use std::path::PathBuf;
use std::sync::Arc;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::state::{AppState, CodexClient, DaemonClient};

/// The pre-#267 hardcoded path. If a future refactor accidentally
/// reverts the fix this constant will be the giveaway.
fn old_shared_path() -> PathBuf {
    std::env::temp_dir().join("neige-codex-homes-stub")
}

#[tokio::test]
async fn new_stub_codex_homes_dir_is_per_instance() {
    let a = CodexClient::new_stub();
    let b = CodexClient::new_stub();
    assert_ne!(
        a.codex_homes_dir, b.codex_homes_dir,
        "two `new_stub()` calls must produce distinct codex_homes_dir paths \
         (otherwise we're back to the shared-global-dir leak from #267)",
    );
    assert_ne!(
        a.codex_homes_dir,
        old_shared_path(),
        "regression: `new_stub()` returned the pre-#267 hardcoded shared path \
         (`{}`); the fix in `state.rs::CodexClient::new_stub` was reverted",
        old_shared_path().display(),
    );
}

#[tokio::test]
async fn new_stub_codex_homes_dir_exists_until_drop() {
    let codex = CodexClient::new_stub();
    let path = codex.codex_homes_dir.clone();
    assert!(
        path.exists(),
        "`new_stub()` must create the tempdir eagerly so wave-create / \
         spec-card spawn paths can immediately `mkdir <path>/<card_id>` \
         without checking; got non-existent {}",
        path.display(),
    );

    // Simulate the real wave-create / spec-card spawn: create a UUID
    // named per-card subdir and a sentinel file inside it. This is the
    // exact shape `spec_card.rs:230` / `codex_cards.rs:178` write.
    let card_id = uuid::Uuid::new_v4().to_string();
    let card_home = path.join(&card_id);
    std::fs::create_dir_all(&card_home).expect("seed per-card codex home");
    std::fs::write(card_home.join("config.toml"), b"# stub\n").expect("seed config.toml");
    assert!(card_home.join("config.toml").exists());

    // Drop the stub — the wrapped `tempfile::TempDir` removes the entire
    // tree, including our per-card subdir.
    drop(codex);
    assert!(
        !path.exists(),
        "dropping `CodexClient` must remove its codex_homes_dir tempdir; \
         {} still exists after drop — leak regression",
        path.display(),
    );
}

#[tokio::test]
async fn appstate_wave_create_subdir_is_under_per_test_tempdir() {
    // End-to-end: build a full `AppState` the way integration tests do
    // (this is the construction shape `cargo test -p calm-server --test
    // wave_create_with_theme` — the test that triggered the #267
    // incident — uses), simulate a wave-create that mints a per-card
    // codex home, and assert that subdir lives under a per-instance
    // tempdir (i.e. NOT the pre-#267 hardcoded
    // `temp_dir().join("neige-codex-homes-stub")`).
    //
    // Why not "drop `state` and assert the dir is gone": `AppState`
    // shares its `Arc<CodexClient>` with the long-lived `Dispatcher`
    // background task (`Dispatcher::spawn` → `Inner.codex`). That task
    // only winds down when the broadcast bus closes, which requires
    // the inner's `EventBus` sender to drop — and the inner is held by
    // the task. The cycle resolves at test-process exit (when the
    // tokio runtime tears down) but not on a synchronous `drop(state)`
    // inside one test, so a "drop + assert" assertion would be racy.
    // The leak we're closing isn't lifetime-too-long, it's
    // lifetime-unbounded-across-test-processes — and a per-instance
    // `TempDir` fixes that even if cleanup waits for process exit.
    // The `new_stub_codex_homes_dir_exists_until_drop` test above
    // covers the direct drop semantics in isolation.
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let codex = Arc::new(CodexClient::new_stub());
    let codex_homes_dir = codex.codex_homes_dir.clone();

    let daemon = Arc::new(DaemonClient::new_stub());
    let card_role_cache = CardRoleCache::new();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();

    let plugin_data_root = tempfile::tempdir().expect("plugin data tempdir");
    let plugin = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        repo.clone(),
        PathBuf::new(),
        plugin_data_root.path().to_path_buf(),
        Vec::new(),
        EventBus::new(),
        card_role_cache.clone(),
        wave_cove_cache.clone(),
    ));

    let state = AppState::from_parts(
        repo,
        EventBus::new(),
        daemon,
        plugin,
        codex,
        Some(card_role_cache),
        Some(wave_cove_cache),
    );

    // Simulate a wave-create that mints a per-card codex home — exactly
    // what the real handlers do via `<codex_homes_dir>/<card_id>/`
    // (see `spec_card.rs:230` and `codex_cards.rs:178`).
    let card_id = uuid::Uuid::new_v4().to_string();
    let card_home = state.codex.codex_homes_dir.join(&card_id);
    std::fs::create_dir_all(&card_home).expect("seed per-card codex home");
    std::fs::write(card_home.join("history"), vec![0u8; 4096])
        .expect("seed multi-byte fake codex state file");
    assert!(card_home.exists());

    // The per-card subdir is under the per-test tempdir, not the
    // pre-#267 global path. This is the property that closes the
    // 134 GB-per-day leak: two separate test invocations get two
    // separate tempdirs, neither one stomps the other, and the OS
    // reaps both on process teardown.
    let tmp_root = std::env::temp_dir();
    assert!(
        codex_homes_dir.starts_with(&tmp_root),
        "codex_homes_dir must live under temp_dir() so OS / TempDir \
         drop can reap it; got {}",
        codex_homes_dir.display(),
    );
    let pre_267_global = tmp_root.join("neige-codex-homes-stub");
    assert_ne!(
        codex_homes_dir,
        pre_267_global,
        "regression: `new_stub()` returned the pre-#267 global path \
         (`{}`) — the leak fix in `state.rs::CodexClient::new_stub` was \
         reverted",
        pre_267_global.display(),
    );
    assert!(
        card_home.starts_with(&codex_homes_dir),
        "per-card subdir must live under the per-test codex_homes_dir; \
         got {} (codex_homes_dir = {})",
        card_home.display(),
        codex_homes_dir.display(),
    );

    drop(state);
}
