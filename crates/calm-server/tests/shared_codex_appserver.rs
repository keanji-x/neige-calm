use std::sync::Arc;
use std::time::Duration;

use calm_server::config::Config;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::db::{Repo, RepoOutOfDomain, RepoSyncDomainRaw, SharedCodexDaemonUpdate};
use calm_server::model::{CardRole, NewCard, NewCove, NewWave};
use calm_server::routes::theme::RequestTheme;
use calm_server::shared_codex_appserver::{
    BackoffState, SharedCodexAppServer, SharedDaemonState, bounded_exponential_backoff,
};
use clap::Parser;
use serde_json::json;

fn fake_codex_bin() -> &'static str {
    env!("CARGO_BIN_EXE_osc-probe-child")
}

fn cfg(root: &tempfile::TempDir) -> Config {
    Config::parse_from([
        "calm-server",
        "--data-dir",
        root.path().to_str().unwrap(),
        "--codex-bin",
        fake_codex_bin(),
        "--shared-codex-appserver-restart-initial-delay-ms",
        "10",
        "--shared-codex-appserver-restart-max-delay-ms",
        "50",
    ])
}

async fn repo() -> Arc<SqlxRepo> {
    Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap())
}

async fn server(root: &tempfile::TempDir, repo: Arc<dyn Repo>) -> Arc<SharedCodexAppServer> {
    let cfg = cfg(root);
    let home = calm_server::shared_codex_home::SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
        cfg.data_dir_resolved().join("codex-homes"),
    );
    home.seed().unwrap();
    SharedCodexAppServer::new(&cfg, Arc::new(home), repo)
}

async fn seed_card(repo: &SqlxRepo, idx: usize) -> String {
    let cove = repo
        .cove_create(NewCove {
            name: format!("cove-{idx}"),
            color: "#abc".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: format!("wave-{idx}"),
            sort: None,
            cwd: "/tmp".into(),
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    repo.card_create(NewCard {
        wave_id: wave.id,
        kind: "terminal".into(),
        sort: None,
        payload: json!({}),
    })
    .await
    .unwrap()
    .id
    .to_string()
}

#[test]
fn state_machine_transitions_start_run_restart_failed() {
    let states = [
        SharedDaemonState::Idle,
        SharedDaemonState::Starting,
        SharedDaemonState::Running,
        SharedDaemonState::Restarting,
        SharedDaemonState::Failed,
    ];
    for state in states {
        assert_eq!(SharedDaemonState::from_db_str(state.as_db_str()), state);
    }
}

#[tokio::test]
async fn stale_daemon_detected_by_boot_id_mismatch() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    repo.shared_daemon_runtime_set(SharedCodexDaemonUpdate {
        state: "running".into(),
        pid: Some(999_999),
        pgid: Some(999_999),
        sock_path: Some(root.path().join("old.sock").display().to_string()),
        codex_home_path: Some(root.path().join("old-home").display().to_string()),
        process_start_time: Some(1),
        boot_id: Some("definitely-not-this-boot".into()),
        started_at: Some(1),
        last_error: None,
        increment_restart_count: false,
    })
    .await
    .unwrap();

    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    let snapshot = daemon.status_snapshot();
    assert_eq!(snapshot.state, SharedDaemonState::Running);
    assert_ne!(snapshot.runtime.unwrap().pid, 999_999);
}

#[tokio::test]
async fn stale_daemon_detected_by_start_time_mismatch() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let live_boot = calm_server::spec_appserver::read_boot_id().unwrap_or_default();
    repo.shared_daemon_runtime_set(SharedCodexDaemonUpdate {
        state: "running".into(),
        pid: Some(999_998),
        pgid: Some(999_998),
        sock_path: Some(root.path().join("old.sock").display().to_string()),
        codex_home_path: Some(root.path().join("old-home").display().to_string()),
        process_start_time: Some(1),
        boot_id: Some(live_boot),
        started_at: Some(1),
        last_error: None,
        increment_restart_count: false,
    })
    .await
    .unwrap();

    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    let snapshot = daemon.status_snapshot();
    assert_eq!(snapshot.state, SharedDaemonState::Running);
    assert_ne!(snapshot.runtime.unwrap().pid, 999_998);
}

#[tokio::test]
async fn takeover_rebuilds_thread_cache_from_db() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let mut pairs = Vec::new();
    for i in 0..3 {
        let card_id = seed_card(&repo, i).await;
        let thread_id = format!("thread-{i}");
        repo.card_codex_thread_upsert(&card_id, &thread_id, CardRole::Plain, None)
            .await
            .unwrap();
        pairs.push((thread_id, card_id));
    }

    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    for (thread_id, card_id) in pairs {
        assert_eq!(daemon.cached_card_for_thread(&thread_id), Some(card_id));
    }
}

#[tokio::test]
async fn restart_resumes_rollout_backed_threads() {
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = ENV_LOCK.lock().await;

    let root = tempfile::tempdir().unwrap();
    let capture = root.path().join("requests.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture);
    }

    let repo = repo().await;
    let card_id = seed_card(&repo, 1).await;
    repo.card_codex_thread_upsert(&card_id, "thread-resume", CardRole::Plain, None)
        .await
        .unwrap();

    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    unsafe {
        std::env::remove_var("FAKE_CODEX_CAPTURE_REQUESTS");
    }
    let requests = std::fs::read_to_string(capture).unwrap();
    assert!(requests.contains("\"method\":\"thread/resume\""));
}

#[test]
fn bounded_exponential_backoff_caps_at_max() {
    let initial = Duration::from_millis(250);
    let max = Duration::from_secs(10);
    assert_eq!(
        bounded_exponential_backoff(initial, max, 0),
        Duration::from_millis(250)
    );
    assert_eq!(bounded_exponential_backoff(initial, max, 99), max);

    let state = BackoffState::new(initial, max);
    let mut last = Duration::ZERO;
    for _ in 0..20 {
        last = state.next_delay();
    }
    assert_eq!(last, max);
}
