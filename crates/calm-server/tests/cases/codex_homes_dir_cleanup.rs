//! `CodexClient::new_stub()` must scope `codex_homes_dir` to a per-instance tempdir that disappears on drop;
//! a shared global path once accumulated 134 GB of per-card codex state across test runs.

use std::path::PathBuf;
use std::sync::Arc;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::state::{AppState, CodexClient, DaemonClient};

/// The old hardcoded shared path; a refactor that reverts to it shows up here.
fn old_shared_path() -> PathBuf {
    std::env::temp_dir().join("neige-codex-homes-stub")
}

#[tokio::test]
async fn codex_homes_dir_cleanup_new_stub_codex_homes_dir_is_per_instance() {
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
    assert_ne!(
        a.codex_home_dir(),
        b.codex_home_dir(),
        "two `new_stub()` calls must also produce distinct shared CODEX_HOME paths"
    );
    assert!(
        a.codex_home_dir().starts_with(
            a.codex_homes_dir
                .parent()
                .expect("stub codex_homes_dir has temp root parent")
        ),
        "shared CODEX_HOME must live under the same temp root as codex_homes_dir"
    );
}

#[tokio::test]
async fn codex_homes_dir_cleanup_new_stub_codex_homes_dir_exists_until_drop() {
    let codex = CodexClient::new_stub();
    let path = codex.codex_homes_dir.clone();
    let shared_path = codex.codex_home_dir().to_path_buf();
    assert!(
        path.exists(),
        "`new_stub()` must create the tempdir eagerly so track-create / \
         planner-card spawn paths can immediately `mkdir <path>/<card_id>` \
         without checking; got non-existent {}",
        path.display(),
    );

    // Simulate the planner-card spawn: a UUID-named per-card subdir with a sentinel file.
    let card_id = uuid::Uuid::new_v4().to_string();
    let card_home = path.join(&card_id);
    std::fs::create_dir_all(&card_home).expect("seed per-card codex home");
    std::fs::write(card_home.join("config.toml"), b"# stub\n").expect("seed config.toml");
    assert!(card_home.join("config.toml").exists());
    codex
        .shared_codex_home
        .seed_from(None)
        .expect("seed stub shared CODEX_HOME");
    assert!(shared_path.exists());

    drop(codex);
    assert!(
        !path.exists(),
        "dropping `CodexClient` must remove its codex_homes_dir tempdir; \
         {} still exists after drop — leak regression",
        path.display(),
    );
    assert!(
        !shared_path.exists(),
        "dropping `CodexClient` must remove its shared CODEX_HOME tempdir; \
         {} still exists after drop — leak regression",
        shared_path.display(),
    );
}

#[tokio::test]
async fn codex_homes_dir_cleanup_appstate_track_create_subdir_is_under_per_test_tempdir() {
    // Asserts only that the per-card subdir lives under the per-test tempdir; drop-then-assert is covered by the other two tests.
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let codex = Arc::new(CodexClient::new_stub());
    let codex_homes_dir = codex.codex_homes_dir.clone();
    let shared_codex_home = codex.codex_home_dir().to_path_buf();

    let daemon = Arc::new(DaemonClient::new_stub());
    let card_role_cache = CardRoleCache::new();
    let track_area_cache = calm_server::track_area_cache::TrackAreaCache::new();

    let plugin_data_root = tempfile::tempdir().expect("plugin data tempdir");
    let plugin = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        repo.clone(),
        PathBuf::new(),
        plugin_data_root.path().to_path_buf(),
        Vec::new(),
        EventBus::new(),
        calm_server::state::WriteContext::new(card_role_cache.clone(), track_area_cache.clone()),
    ));

    let state = AppState::from_parts(
        repo,
        EventBus::new(),
        daemon,
        plugin,
        codex,
        Some(card_role_cache),
        Some(track_area_cache),
    );

    // Simulate a track-create minting `<codex_homes_dir>/<card_id>/`.
    let card_id = uuid::Uuid::new_v4().to_string();
    let card_home = state.codex.codex_homes_dir.join(&card_id);
    std::fs::create_dir_all(&card_home).expect("seed per-card codex home");
    std::fs::write(card_home.join("history"), vec![0u8; 4096])
        .expect("seed multi-byte fake codex state file");
    assert!(card_home.exists());

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
    assert!(
        shared_codex_home.starts_with(
            codex_homes_dir
                .parent()
                .expect("stub codex_homes_dir has temp root parent")
        ),
        "shared CODEX_HOME must live under the same per-test temp root; \
         got {} (codex_homes_dir = {})",
        shared_codex_home.display(),
        codex_homes_dir.display(),
    );

    drop(state);
}

/// The dispatcher holds a `Weak<CodexClient>`; a strong ref would cycle with the broadcast bus and keep the
/// `TempDir` alive until process exit.
#[tokio::test]
async fn codex_homes_dir_cleanup_appstate_drop_removes_codex_homes_dir_on_disk() {
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let codex = Arc::new(CodexClient::new_stub());
    let codex_homes_dir = codex.codex_homes_dir.clone();
    let shared_codex_home = codex.codex_home_dir().to_path_buf();
    assert!(
        codex_homes_dir.exists(),
        "precondition: per-test tempdir must exist before AppState construction"
    );
    codex
        .shared_codex_home
        .seed_from(None)
        .expect("seed stub shared CODEX_HOME");
    assert!(
        shared_codex_home.exists(),
        "precondition: shared CODEX_HOME tempdir must exist before AppState construction"
    );

    let daemon = Arc::new(DaemonClient::new_stub());
    let card_role_cache = CardRoleCache::new();
    let track_area_cache = calm_server::track_area_cache::TrackAreaCache::new();
    let plugin_data_root = tempfile::tempdir().expect("plugin data tempdir");
    let plugin = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        repo.clone(),
        PathBuf::new(),
        plugin_data_root.path().to_path_buf(),
        Vec::new(),
        EventBus::new(),
        calm_server::state::WriteContext::new(card_role_cache.clone(), track_area_cache.clone()),
    ));

    let state = AppState::from_parts(
        repo,
        EventBus::new(),
        daemon,
        plugin,
        codex, // moved into state.codex
        Some(card_role_cache),
        Some(track_area_cache),
    );

    // Seed bytes on disk so the assertion is not just an empty dir.
    let card_id = uuid::Uuid::new_v4().to_string();
    let card_home = state.codex.codex_homes_dir.join(&card_id);
    std::fs::create_dir_all(&card_home).expect("seed per-card codex home");
    std::fs::write(card_home.join("history"), vec![0u8; 4096]).expect("seed fake codex state file");
    assert!(card_home.exists());

    // `state.codex` is the last strong ref; dropping it removes the tree.
    drop(state);

    assert!(
        !codex_homes_dir.exists(),
        "post-#272 N3: dropping AppState must remove its codex_homes_dir tempdir; \
         {} still exists after drop — dispatcher Arc cycle has been resurrected, \
         per-test cleanup is back to process-exit only (the leak PR #271 punted on)",
        codex_homes_dir.display(),
    );
    assert!(
        !card_home.exists(),
        "per-card subdir under codex_homes_dir survived AppState drop — \
         tempdir reap regression"
    );
    assert!(
        !shared_codex_home.exists(),
        "shared CODEX_HOME survived AppState drop — tempdir reap regression"
    );
}
