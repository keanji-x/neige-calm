//! Stub hook sidecars and shared CODEX_HOME must be scoped to a per-instance tempdir that disappears on drop;
//! a shared global path once accumulated 134 GB of per-card codex state across test runs.

use std::path::PathBuf;
use std::sync::Arc;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::state::{AppState, CodexClient, DaemonClient};

fn sidecar_root(codex: &CodexClient) -> PathBuf {
    codex
        .claude_settings_dir
        .parent()
        .expect("sidecar root")
        .to_path_buf()
}

/// The old hardcoded shared path; a refactor that reverts to it shows up here.
fn old_shared_path() -> PathBuf {
    std::env::temp_dir().join("neige-codex-homes-stub")
}

#[tokio::test]
async fn sidecar_root_cleanup_new_stub_sidecar_root_is_per_instance() {
    let a = CodexClient::new_stub();
    let b = CodexClient::new_stub();
    assert_ne!(
        sidecar_root(&a),
        sidecar_root(&b),
        "two `new_stub()` calls must produce distinct sidecar_root paths \
         (otherwise we're back to the shared-global-dir leak from #267)",
    );
    assert_ne!(
        sidecar_root(&a),
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
            sidecar_root(&a)
                .parent()
                .expect("stub sidecar_root has temp root parent")
        ),
        "shared CODEX_HOME must live under the same temp root as sidecar_root"
    );
}

#[tokio::test]
async fn sidecar_root_cleanup_new_stub_sidecar_root_exists_until_drop() {
    let codex = CodexClient::new_stub();
    let path = sidecar_root(&codex);
    let shared_path = codex.codex_home_dir().to_path_buf();
    assert!(
        path.exists(),
        "`new_stub()` must create the tempdir eagerly so hook settings paths can immediately `mkdir <path>/<card_id>` \
         without checking; got non-existent {}",
        path.display(),
    );

    // Seed a hook sidecar directory with a sentinel file.
    let card_id = uuid::Uuid::new_v4().to_string();
    let card_home = codex.claude_settings_dir.join(&card_id);
    std::fs::create_dir_all(&card_home).expect("seed hook sidecar directory");
    std::fs::write(card_home.join("config.toml"), b"# stub\n").expect("seed sidecar sentinel");
    assert!(card_home.join("config.toml").exists());
    codex
        .shared_codex_home
        .seed_from(None)
        .expect("seed stub shared CODEX_HOME");
    assert!(shared_path.exists());

    drop(codex);
    assert!(
        !path.exists(),
        "dropping `CodexClient` must remove its sidecar_root tempdir; \
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
async fn sidecar_root_cleanup_appstate_sidecar_is_under_per_test_tempdir() {
    // Asserts only that the per-card sidecar lives under the per-test tempdir; drop-then-assert is covered by the other two tests.
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let codex = Arc::new(CodexClient::new_stub());
    let sidecar_root = sidecar_root(&codex);
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

    // Seed a per-card hook sidecar under the stub sidecar root.
    let card_id = uuid::Uuid::new_v4().to_string();
    let card_home = state.codex.claude_settings_dir.join(&card_id);
    std::fs::create_dir_all(&card_home).expect("seed hook sidecar directory");
    std::fs::write(card_home.join("history"), vec![0u8; 4096])
        .expect("seed multi-byte sidecar sentinel");
    assert!(card_home.exists());

    let tmp_root = std::env::temp_dir();
    assert!(
        sidecar_root.starts_with(&tmp_root),
        "sidecar_root must live under temp_dir() so OS / TempDir \
         drop can reap it; got {}",
        sidecar_root.display(),
    );
    let pre_267_global = tmp_root.join("neige-codex-homes-stub");
    assert_ne!(
        sidecar_root,
        pre_267_global,
        "regression: `new_stub()` returned the pre-#267 global path \
         (`{}`) — the leak fix in `state.rs::CodexClient::new_stub` was \
         reverted",
        pre_267_global.display(),
    );
    assert!(
        card_home.starts_with(&sidecar_root),
        "per-card sidecar must live under the per-test sidecar_root; \
         got {} (sidecar_root = {})",
        card_home.display(),
        sidecar_root.display(),
    );
    assert!(
        shared_codex_home.starts_with(
            sidecar_root
                .parent()
                .expect("stub sidecar_root has temp root parent")
        ),
        "shared CODEX_HOME must live under the same per-test temp root; \
         got {} (sidecar_root = {})",
        shared_codex_home.display(),
        sidecar_root.display(),
    );

    drop(state);
}

/// The dispatcher holds a `Weak<CodexClient>`; a strong ref would cycle with the broadcast bus and keep the
/// `TempDir` alive until process exit.
#[tokio::test]
async fn sidecar_root_cleanup_appstate_drop_removes_sidecar_root_on_disk() {
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let codex = Arc::new(CodexClient::new_stub());
    let sidecar_root = sidecar_root(&codex);
    let shared_codex_home = codex.codex_home_dir().to_path_buf();
    assert!(
        sidecar_root.exists(),
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
    let card_home = state.codex.claude_settings_dir.join(&card_id);
    std::fs::create_dir_all(&card_home).expect("seed hook sidecar directory");
    std::fs::write(card_home.join("history"), vec![0u8; 4096]).expect("seed sidecar sentinel");
    assert!(card_home.exists());

    // `state.codex` is the last strong ref; dropping it removes the tree.
    drop(state);

    assert!(
        !sidecar_root.exists(),
        "post-#272 N3: dropping AppState must remove its sidecar_root tempdir; \
         {} still exists after drop — dispatcher Arc cycle has been resurrected, \
         per-test cleanup is back to process-exit only (the leak PR #271 punted on)",
        sidecar_root.display(),
    );
    assert!(
        !card_home.exists(),
        "per-card sidecar under sidecar_root survived AppState drop — \
         tempdir reap regression"
    );
    assert!(
        !shared_codex_home.exists(),
        "shared CODEX_HOME survived AppState drop — tempdir reap regression"
    );
}
