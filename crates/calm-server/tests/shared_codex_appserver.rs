use std::sync::Arc;
use std::time::Duration;
use std::{path::Path, process::Stdio};

use calm_server::codex_appserver::InputItem;
use calm_server::config::Config;
use calm_server::db::sqlite::{SqlxRepo, runtime_start_tx};
use calm_server::db::{
    Repo, RepoOutOfDomain, RepoRead, RepoSyncDomainRaw, SharedCodexDaemonUpdate,
};
use calm_server::model::{CardRole, NewCard, NewCove, NewWave, new_id, now_ms};
use calm_server::routes::theme::RequestTheme;
use calm_server::runtime_repo::{AgentProvider, RunStatus, RuntimeInit, RuntimeKind};
use calm_server::shared_codex_appserver::{
    BackoffState, SharedCodexAppServer, SharedDaemonState, SharedThreadStartParams,
    bounded_exponential_backoff, drop_spawned_child_guard_for_test,
};
use calm_server::spec_appserver::{read_boot_id, read_proc_start_time};
use clap::Parser;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

fn effective_test_env_signature(ingest_url: &str) -> String {
    let http_proxy = SharedCodexAppServer::effective_proxy_env(None, &["HTTP_PROXY", "http_proxy"]);
    let https_proxy =
        SharedCodexAppServer::effective_proxy_env(None, &["HTTPS_PROXY", "https_proxy"]);
    SharedCodexAppServer::compute_env_signature(
        ingest_url,
        http_proxy.as_deref(),
        https_proxy.as_deref(),
    )
}

fn inherited_http_proxy(value: &'static str) -> impl Fn(&str) -> Option<String> {
    move |key| (key == "HTTP_PROXY").then(|| value.into())
}

#[tokio::test]
async fn start_new_process_passes_ingest_url_and_proxy_env_without_card_id() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    repo.settings_upsert("http_proxy", "http://proxy.local:3128")
        .await
        .unwrap();
    repo.settings_upsert("https_proxy", "http://secure-proxy.local:3129")
        .await
        .unwrap();

    let mut cfg = cfg(&root);
    cfg.codex_ingest_url = Some("http://127.0.0.1:8765".into());
    let home = calm_server::shared_codex_home::SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
        cfg.data_dir_resolved().join("codex-homes"),
    );
    home.seed().unwrap();

    let daemon = SharedCodexAppServer::new(&cfg, Arc::new(home), repo.clone());
    let env = daemon.spawn_env_for_test().await.unwrap();

    let get = |key: &str| env.get(key).and_then(|v| v.as_deref());
    let expected_codex_home = cfg
        .data_dir_resolved()
        .join("codex-home")
        .to_string_lossy()
        .into_owned();
    assert_eq!(get("CODEX_HOME"), Some(expected_codex_home.as_str()));
    assert_eq!(get("NEIGE_CALM_BASE_URL"), Some("http://127.0.0.1:8765"));
    assert_eq!(get("HTTP_PROXY"), Some("http://proxy.local:3128"));
    assert_eq!(get("http_proxy"), Some("http://proxy.local:3128"));
    assert_eq!(get("HTTPS_PROXY"), Some("http://secure-proxy.local:3129"));
    assert_eq!(get("https_proxy"), Some("http://secure-proxy.local:3129"));
}

#[tokio::test]
async fn start_new_process_strips_per_card_env_keys() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let mut cfg = cfg(&root);
    cfg.codex_ingest_url = Some("http://expected".into());
    let home = calm_server::shared_codex_home::SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
        cfg.data_dir_resolved().join("codex-homes"),
    );
    home.seed().unwrap();

    let daemon = SharedCodexAppServer::new(&cfg, Arc::new(home), repo);
    let env = daemon.spawn_env_for_test().await.unwrap();

    assert_eq!(env.get("NEIGE_CARD_ID"), Some(&None));
    assert_eq!(env.get("NEIGE_HOOK_PROVIDER"), Some(&None));
    assert_eq!(env.get("NEIGE_MCP_TOKEN"), Some(&None));
    assert_eq!(env.get("NEIGE_HOOK_URL"), Some(&None));
    assert_eq!(
        env.get("NEIGE_CALM_BASE_URL").cloned().flatten().as_deref(),
        Some("http://expected")
    );
}

async fn persist_running_daemon(
    repo: &SqlxRepo,
    root: &tempfile::TempDir,
    pid: i32,
    pgid: i32,
    sock: &Path,
    process_start_time: u64,
) {
    persist_running_daemon_with_signature(
        repo,
        root,
        pid,
        pgid,
        sock,
        process_start_time,
        Some(effective_test_env_signature(
            &cfg(root).codex_ingest_url_resolved(),
        )),
    )
    .await;
}

async fn persist_running_daemon_with_signature(
    repo: &SqlxRepo,
    root: &tempfile::TempDir,
    pid: i32,
    pgid: i32,
    sock: &Path,
    process_start_time: u64,
    daemon_env_signature: Option<String>,
) {
    repo.shared_daemon_runtime_set(SharedCodexDaemonUpdate {
        state: "running".into(),
        pid: Some(pid),
        pgid: Some(pgid),
        sock_path: Some(sock.display().to_string()),
        codex_home_path: Some(root.path().join("codex-home").display().to_string()),
        process_start_time: Some(process_start_time),
        boot_id: Some(read_boot_id().unwrap_or_default()),
        started_at: Some(now_ms()),
        last_error: None,
        increment_restart_count: false,
        daemon_env_signature,
    })
    .await
    .unwrap();
}

async fn wait_for_start_time_and_socket(pid: i32, sock: &std::path::Path) -> u64 {
    let mut process_start_time = None;
    for _ in 0..40 {
        process_start_time = read_proc_start_time(pid);
        if process_start_time.is_some() && sock.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(sock.exists(), "fake app-server must bind takeover socket");
    process_start_time.expect("fake app-server start time")
}

async fn waitpid_reaped(pid: i32) -> bool {
    for _ in 0..50 {
        let mut status = 0;
        // SAFETY: waitpid is called for a direct child pid spawned by this test.
        let rc = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if rc == pid {
            return true;
        }
        if rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD) {
            return true;
        }
        // SAFETY: signal 0 probes liveness without delivering a signal.
        if unsafe { libc::kill(pid, 0) } != 0 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

async fn wait_proc_gone(pid: i32) -> bool {
    for _ in 0..80 {
        if unsafe { libc::getpgid(pid) } < 0 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

async fn wait_for_active_turn(
    daemon: &SharedCodexAppServer,
    thread_id: &str,
    expected: Option<&str>,
) {
    for _ in 0..80 {
        if daemon.active_turn_for_test(thread_id).as_deref() == expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "timed out waiting for active turn {expected:?}; got {:?}",
        daemon.active_turn_for_test(thread_id)
    );
}

async fn read_pid_line(out: tokio::process::ChildStdout) -> i32 {
    let mut line = String::new();
    let n = tokio::time::timeout(
        Duration::from_secs(5),
        BufReader::new(out).read_line(&mut line),
    )
    .await
    .expect("timed out reading child pid")
    .expect("read child pid");
    assert!(n != 0, "launcher exited before printing child pid");
    line.trim().parse::<i32>().expect("child pid int")
}

async fn spawn_launcher_with_fake_appserver(
    sock: &Path,
    ignore_child_sigterm: bool,
    fail_initialize: bool,
) -> (tokio::process::Child, i32, i32) {
    let trap = if ignore_child_sigterm {
        r#"trap "" TERM; "#
    } else {
        ""
    };
    let script = format!(
        r#"sh -c '{trap}exec "$FAKE_CODEX_BIN" app-server --listen "unix://$FAKE_CODEX_SOCK"' & echo $!; wait"#
    );
    let mut launcher = Command::new("sh")
        .arg("-c")
        .arg(script)
        .env("FAKE_CODEX_BIN", fake_codex_bin())
        .env("FAKE_CODEX_SOCK", sock)
        .env(
            "FAKE_CODEX_FAIL_INITIALIZE",
            if fail_initialize { "1" } else { "0" },
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .expect("spawn launcher with fake app-server child");
    let pgid = i32::try_from(launcher.id().expect("launcher pid")).expect("pid fits i32");
    let peer_pid = read_pid_line(launcher.stdout.take().expect("launcher stdout piped")).await;

    for _ in 0..40 {
        if sock.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(sock.exists(), "fake app-server child must bind socket");
    assert_ne!(peer_pid, pgid, "test must model launcher/native split");
    assert_eq!(
        unsafe { libc::getpgid(peer_pid) },
        pgid,
        "fake app-server child must share launcher pgid"
    );

    (launcher, pgid, peer_pid)
}

fn force_cleanup_process_group(child: tokio::process::Child, pgid: i32) {
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
    drop(child);
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
        payload: json!({"codex_source": "shared"}),
    })
    .await
    .unwrap()
    .id
    .to_string()
}

async fn seed_runtime_thread(repo: &SqlxRepo, card_id: &str, thread_id: &str) {
    seed_runtime_thread_with_kind(repo, card_id, thread_id, RuntimeKind::CodexCard).await;
}

async fn seed_runtime_thread_with_kind(
    repo: &SqlxRepo,
    card_id: &str,
    thread_id: &str,
    kind: RuntimeKind,
) {
    let mut tx = repo.pool().begin().await.unwrap();
    runtime_start_tx(
        &mut tx,
        RuntimeInit {
            id: new_id(),
            card_id: card_id.to_string(),
            kind,
            agent_provider: Some(AgentProvider::Codex),
            status: RunStatus::Running,
            terminal_run_id: None,
            thread_id: Some(thread_id.to_string()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            lease_owner: None,
            lease_until_ms: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
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
        daemon_env_signature: None,
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
        daemon_env_signature: None,
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
async fn takeover_handshake_failure_reaps_verified_daemon_before_relaunch() {
    let root = tempfile::tempdir().unwrap();
    let sock = root.path().join("run/codex-appserver.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();

    let mut child = Command::new(fake_codex_bin())
        .arg("app-server")
        .arg("--listen")
        .arg(format!("unix://{}", sock.display()))
        .env("FAKE_CODEX_FAIL_INITIALIZE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .expect("spawn handshake-failing fake app-server for takeover");
    let old_pid = i32::try_from(child.id().expect("fake app-server pid")).expect("pid fits i32");
    let process_start_time = wait_for_start_time_and_socket(old_pid, &sock).await;

    let repo = repo().await;
    persist_running_daemon(&repo, &root, old_pid, old_pid, &sock, process_start_time).await;

    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();

    tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("verified handshake-failing daemon should be reaped before relaunch")
        .expect("wait old fake app-server");

    let snapshot = daemon.status_snapshot();
    assert_eq!(snapshot.state, SharedDaemonState::Running);
    let new_pid = snapshot
        .runtime
        .as_ref()
        .map(|runtime| runtime.pid)
        .unwrap();
    assert_ne!(new_pid, old_pid);

    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(record.pid, Some(new_pid));
    assert_eq!(record.pgid, Some(new_pid));
}

#[tokio::test]
async fn takeover_respawns_when_env_signature_differs() {
    let root = tempfile::tempdir().unwrap();
    let sock = root.path().join("run/codex-appserver.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();

    let mut child = Command::new(fake_codex_bin())
        .arg("app-server")
        .arg("--listen")
        .arg(format!("unix://{}", sock.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .expect("spawn stale-signature fake app-server for takeover");
    let old_pid = i32::try_from(child.id().expect("fake app-server pid")).expect("pid fits i32");
    let process_start_time = wait_for_start_time_and_socket(old_pid, &sock).await;

    let repo = repo().await;
    persist_running_daemon_with_signature(
        &repo,
        &root,
        old_pid,
        old_pid,
        &sock,
        process_start_time,
        Some("stale-env-signature".into()),
    )
    .await;

    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();

    tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("stale-signature daemon should be reaped before relaunch")
        .expect("wait old fake app-server");

    let snapshot = daemon.status_snapshot();
    assert_eq!(snapshot.state, SharedDaemonState::Running);
    let new_pid = snapshot
        .runtime
        .as_ref()
        .map(|runtime| runtime.pid)
        .unwrap();
    assert_ne!(new_pid, old_pid);

    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(record.pid, Some(new_pid));
    let expected_signature = effective_test_env_signature(&cfg(&root).codex_ingest_url_resolved());
    assert_eq!(
        record.daemon_env_signature.as_deref(),
        Some(expected_signature.as_str())
    );
}

#[tokio::test]
async fn takeover_handshake_fail_sigkills_pgid_even_after_launcher_exits() {
    let root = tempfile::tempdir().unwrap();
    let sock = root.path().join("run/codex-appserver.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();

    let (launcher, pgid, native_pid) = spawn_launcher_with_fake_appserver(&sock, true, true).await;
    let process_start_time = read_proc_start_time(pgid).expect("launcher start time");

    let repo = repo().await;
    persist_running_daemon(&repo, &root, pgid, pgid, &sock, process_start_time).await;

    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();

    let native_gone = wait_proc_gone(native_pid).await;
    force_cleanup_process_group(launcher, pgid);
    assert!(
        native_gone,
        "takeover reap must SIGKILL the pgid even after launcher pid exits"
    );
}

#[tokio::test]
async fn stale_socket_with_live_listener_reaped_before_relaunch() {
    let root = tempfile::tempdir().unwrap();
    let sock = root.path().join("run/codex-appserver.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();

    let mut child = Command::new(fake_codex_bin())
        .arg("app-server")
        .arg("--listen")
        .arg(format!("unix://{}", sock.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .expect("spawn orphaned fake app-server listener");
    let old_pid = i32::try_from(child.id().expect("fake app-server pid")).expect("pid fits i32");
    let _ = wait_for_start_time_and_socket(old_pid, &sock).await;

    let repo = repo().await;
    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();

    tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("live listener on stale socket should be reaped before relaunch")
        .expect("wait old fake app-server");

    let snapshot = daemon.status_snapshot();
    assert_eq!(snapshot.state, SharedDaemonState::Running);
    let new_pid = snapshot
        .runtime
        .as_ref()
        .map(|runtime| runtime.pid)
        .unwrap();
    assert_ne!(new_pid, old_pid);
}

#[tokio::test]
async fn reap_listener_uses_getpgid_to_derive_pgid_from_peer_pid() {
    let root = tempfile::tempdir().unwrap();
    let sock = root.path().join("run/codex-appserver.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();

    let (launcher, pgid, peer_pid) = spawn_launcher_with_fake_appserver(&sock, false, false).await;

    let repo = repo().await;
    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();

    let peer_gone = wait_proc_gone(peer_pid).await;
    force_cleanup_process_group(launcher, pgid);
    assert!(
        peer_gone,
        "stale listener reap must kill peer_pid's real pgid, not kill(-peer_pid)"
    );
}

#[tokio::test]
async fn takeover_rebuilds_thread_cache_from_db() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let mut pairs = Vec::new();
    for i in 0..3 {
        let card_id = seed_card(&repo, i).await;
        let thread_id = format!("thread-{i}");
        seed_runtime_thread(&repo, &card_id, &thread_id).await;
        pairs.push((thread_id, card_id));
    }

    let daemon = server(&root, repo.clone()).await;
    daemon.set_active_turn_for_test("stale-thread", "stale-turn");
    daemon.start_or_takeover().await.unwrap();
    for (thread_id, card_id) in pairs {
        assert_eq!(daemon.cached_card_for_thread(&thread_id), Some(card_id));
    }
    assert_eq!(daemon.active_turn_for_test("stale-thread"), None);
}

#[tokio::test]
async fn thread_cache_rebuild_legacy_wins_on_conflict() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let card_id = seed_card(&repo, 1).await;
    seed_runtime_thread_with_kind(&repo, &card_id, "thread-old", RuntimeKind::SharedSpec).await;
    repo.card_codex_thread_upsert(&card_id, "thread-new", CardRole::Plain, None)
        .await
        .unwrap();

    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();

    assert_eq!(
        daemon.cached_card_for_thread("thread-new"),
        Some(card_id.clone())
    );
    assert_eq!(daemon.cached_card_for_thread("thread-old"), None);
}

#[tokio::test]
async fn thread_cache_rebuild_merges_runtime_only_cards() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let runtime_card_id = seed_card(&repo, 1).await;
    seed_runtime_thread_with_kind(
        &repo,
        &runtime_card_id,
        "thread-runtime",
        RuntimeKind::SharedSpec,
    )
    .await;
    let legacy_card_id = seed_card(&repo, 2).await;
    repo.card_codex_thread_upsert(&legacy_card_id, "thread-legacy", CardRole::Plain, None)
        .await
        .unwrap();

    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();

    assert_eq!(
        daemon.cached_card_for_thread("thread-runtime"),
        Some(runtime_card_id)
    );
    assert_eq!(
        daemon.cached_card_for_thread("thread-legacy"),
        Some(legacy_card_id)
    );
}

#[tokio::test]
async fn restart_resumes_rollout_backed_threads() {
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

#[tokio::test]
async fn thread_start_for_card_respects_needs_respawn_flag() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    let old_pid = daemon.status_snapshot().runtime.unwrap().pid;
    let card_id = seed_card(&repo, 1).await;

    daemon.mark_needs_respawn();
    assert!(daemon.needs_respawn_on_next_thread_start_for_test());
    let thread_id = daemon
        .thread_start_for_card(
            &card_id,
            CardRole::Plain,
            None,
            SharedThreadStartParams {
                cwd: "/tmp".into(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(thread_id, "fake-thread-0001");
    assert!(!daemon.needs_respawn_on_next_thread_start_for_test());
    let new_pid = daemon.status_snapshot().runtime.unwrap().pid;
    assert_ne!(new_pid, old_pid);
    assert_eq!(
        repo.card_codex_thread_get_by_card(&card_id)
            .await
            .unwrap()
            .unwrap()
            .thread_id,
        "fake-thread-0001"
    );
}

#[tokio::test]
async fn concurrent_mark_during_respawn_is_preserved() {
    let _guard = ENV_LOCK.lock().await;

    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    let card_id = seed_card(&repo, 1).await;

    unsafe {
        std::env::set_var("FAKE_CODEX_INITIALIZE_DELAY_MS", "500");
    }
    daemon.mark_needs_respawn();
    let respawning_daemon = daemon.clone();
    let respawning_card_id = card_id.clone();
    let respawn_task = tokio::spawn(async move {
        respawning_daemon
            .thread_start_for_card(
                &respawning_card_id,
                CardRole::Plain,
                None,
                SharedThreadStartParams {
                    cwd: "/tmp".into(),
                    approval_policy: "never".into(),
                    sandbox_mode: "workspace-write".into(),
                    developer_instructions: None,
                },
            )
            .await
    });

    let mut observed_respawn_in_progress = false;
    for _ in 0..100 {
        let snapshot = daemon.status_snapshot();
        // #480 PR5b: respawn now transitions Restarting → Starting → Running
        // per §C.3, so "respawn in progress" is either Restarting (reap) or
        // Starting (spawn callback inside start_new_process_typestate).
        if matches!(
            snapshot.state,
            SharedDaemonState::Restarting | SharedDaemonState::Starting
        ) && !daemon.needs_respawn_on_next_thread_start_for_test()
        {
            observed_respawn_in_progress = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        observed_respawn_in_progress,
        "respawn did not reach test window"
    );

    daemon.mark_needs_respawn();
    unsafe {
        std::env::remove_var("FAKE_CODEX_INITIALIZE_DELAY_MS");
    }

    assert_eq!(respawn_task.await.unwrap().unwrap(), "fake-thread-0001");
    assert!(
        daemon.needs_respawn_on_next_thread_start_for_test(),
        "mark made during respawn must survive the completed respawn"
    );
    let after_first = daemon.status_snapshot();
    assert_eq!(after_first.restart_count, 1);
    let after_first_pid = after_first.runtime.as_ref().map(|runtime| runtime.pid);

    assert_eq!(
        daemon
            .thread_start_for_card(
                &card_id,
                CardRole::Plain,
                None,
                SharedThreadStartParams {
                    cwd: "/tmp".into(),
                    approval_policy: "never".into(),
                    sandbox_mode: "workspace-write".into(),
                    developer_instructions: None,
                },
            )
            .await
            .unwrap(),
        "fake-thread-0001"
    );
    let after_second = daemon.status_snapshot();
    assert_eq!(after_second.restart_count, 2);
    assert_ne!(
        after_second.runtime.as_ref().map(|runtime| runtime.pid),
        after_first_pid,
        "second preserved mark must trigger the next respawn"
    );
    assert!(!daemon.needs_respawn_on_next_thread_start_for_test());
}

#[tokio::test]
async fn respawn_failure_then_retry_succeeds() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let bin_dir = root.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let codex_link = bin_dir.join("codex");
    std::os::unix::fs::symlink(fake_codex_bin(), &codex_link).unwrap();

    let mut cfg = cfg(&root);
    cfg.codex_bin = codex_link.display().to_string();
    let home = calm_server::shared_codex_home::SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
        cfg.data_dir_resolved().join("codex-homes"),
    );
    home.seed().unwrap();
    let daemon = SharedCodexAppServer::new(&cfg, Arc::new(home), repo);

    daemon.start_or_takeover().await.unwrap();
    let old_pid = daemon.status_snapshot().runtime.unwrap().pid;

    std::fs::remove_file(&codex_link).unwrap();
    daemon.mark_needs_respawn();
    assert!(daemon.ensure_respawn_for_current_settings().await.is_err());
    let failed = daemon.status_snapshot();
    assert!(
        failed.runtime.is_none(),
        "failed respawn must leave no installed runtime"
    );
    assert!(
        daemon.needs_respawn_on_next_thread_start_for_test(),
        "failed respawn must stay retryable"
    );

    std::os::unix::fs::symlink(fake_codex_bin(), &codex_link).unwrap();
    daemon.ensure_respawn_for_current_settings().await.unwrap();
    let recovered = daemon.status_snapshot();
    assert_eq!(recovered.state, SharedDaemonState::Running);
    assert!(!daemon.needs_respawn_on_next_thread_start_for_test());
    assert_ne!(
        recovered.runtime.as_ref().map(|runtime| runtime.pid),
        Some(old_pid)
    );
    assert!(recovered.runtime.is_some());
}

#[tokio::test]
async fn manual_respawn_aborts_taken_over_pid_watcher() {
    let root = tempfile::tempdir().unwrap();
    let sock = root.path().join("run/codex-appserver.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();

    let mut child = Command::new(fake_codex_bin())
        .arg("app-server")
        .arg("--listen")
        .arg(format!("unix://{}", sock.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .expect("spawn fake app-server for takeover");
    let old_pid = i32::try_from(child.id().expect("fake app-server pid")).expect("pid fits i32");
    let process_start_time = wait_for_start_time_and_socket(old_pid, &sock).await;

    let repo = repo().await;
    persist_running_daemon(&repo, &root, old_pid, old_pid, &sock, process_start_time).await;

    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    assert!(
        daemon.taken_over_pid_watcher_active_for_test().await,
        "takeover path must install a pid watcher"
    );

    daemon.mark_needs_respawn();
    daemon.ensure_respawn_for_current_settings().await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;

    assert!(
        !daemon.taken_over_pid_watcher_active_for_test().await,
        "manual reap must clear the takeover watcher slot"
    );
    assert!(!daemon.needs_respawn_on_next_thread_start_for_test());
    let after_manual = daemon.status_snapshot();
    assert_eq!(after_manual.state, SharedDaemonState::Running);
    assert_eq!(after_manual.restart_count, 1);
    assert_ne!(
        after_manual.runtime.as_ref().map(|runtime| runtime.pid),
        Some(old_pid)
    );

    tokio::time::sleep(Duration::from_millis(900)).await;
    let stable = daemon.status_snapshot();
    assert_eq!(
        stable.restart_count, 1,
        "aborted takeover watcher must not race in a second crash restart"
    );
    assert_eq!(stable.state, SharedDaemonState::Running);
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

#[test]
fn current_env_signature_changes_with_ingest_url_and_proxy() {
    let s1 = SharedCodexAppServer::compute_env_signature("u1", None, None);
    let s2 = SharedCodexAppServer::compute_env_signature("u2", None, None);
    assert_ne!(s1, s2);

    let s3 = SharedCodexAppServer::compute_env_signature("u1", Some("p"), None);
    assert_ne!(s1, s3);

    let s4 = SharedCodexAppServer::compute_env_signature("u1", None, Some("p"));
    assert_ne!(s1, s4);
    assert_eq!(s1.len(), 16);
}

#[test]
fn current_env_signature_reads_inherited_proxy_when_settings_absent() {
    let proxy = SharedCodexAppServer::effective_proxy_env_from(
        None,
        &["HTTP_PROXY", "http_proxy"],
        inherited_http_proxy("http://from-env"),
    );
    let sig_with_env = SharedCodexAppServer::compute_env_signature("u1", proxy.as_deref(), None);

    let other_proxy = SharedCodexAppServer::effective_proxy_env_from(
        None,
        &["HTTP_PROXY", "http_proxy"],
        inherited_http_proxy("http://other"),
    );
    let sig_with_other_env =
        SharedCodexAppServer::compute_env_signature("u1", other_proxy.as_deref(), None);

    assert_ne!(
        sig_with_env, sig_with_other_env,
        "signature must reflect inherited env, not just settings"
    );
}

#[test]
fn current_env_signature_prefers_settings_over_inherited_env() {
    let proxy = SharedCodexAppServer::effective_proxy_env_from(
        Some("http://from-settings"),
        &["HTTP_PROXY", "http_proxy"],
        inherited_http_proxy("http://from-env"),
    );
    let sig = SharedCodexAppServer::compute_env_signature("u1", proxy.as_deref(), None);

    let proxy_no_env = SharedCodexAppServer::effective_proxy_env_from(
        Some("http://from-settings"),
        &["HTTP_PROXY", "http_proxy"],
        |_| None,
    );
    let sig_no_env =
        SharedCodexAppServer::compute_env_signature("u1", proxy_no_env.as_deref(), None);

    assert_eq!(proxy.as_deref(), Some("http://from-settings"));
    assert_eq!(sig, sig_no_env, "settings override must take precedence");
}

#[test]
fn backoff_does_not_reset_within_stable_window() {
    let state = BackoffState::new(Duration::from_millis(250), Duration::from_secs(10));

    let d1 = state.next_delay();
    state.note_relaunch_now();
    let d2_no_stable = state.next_delay();

    assert!(
        d2_no_stable > d1,
        "backoff must grow without stable window: {d1:?} -> {d2_no_stable:?}"
    );
}

#[test]
fn backoff_resets_after_stable_window() {
    let state = BackoffState::new(Duration::from_millis(250), Duration::from_secs(10));
    let _ = state.next_delay();
    let _ = state.next_delay();
    let _ = state.next_delay();
    state.note_relaunch_now();
    state.simulate_stable_run_for(Duration::from_secs(61));

    let d_after_stable = state.next_delay();

    assert_eq!(
        d_after_stable,
        Duration::from_millis(250),
        "backoff must reset to initial after stable window: got {d_after_stable:?}"
    );
}

#[tokio::test]
async fn taken_over_daemon_exit_triggers_restart() {
    let root = tempfile::tempdir().unwrap();
    let sock = root.path().join("run/codex-appserver.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();

    let mut child = Command::new(fake_codex_bin())
        .arg("app-server")
        .arg("--listen")
        .arg(format!("unix://{}", sock.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .expect("spawn fake app-server for takeover");
    let pid = i32::try_from(child.id().expect("fake app-server pid")).expect("pid fits i32");

    let mut process_start_time = None;
    for _ in 0..40 {
        process_start_time = read_proc_start_time(pid);
        if process_start_time.is_some() && sock.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let process_start_time = process_start_time.expect("fake app-server start time");
    assert!(sock.exists(), "fake app-server must bind takeover socket");

    let repo = repo().await;
    persist_running_daemon(&repo, &root, pid, pid, &sock, process_start_time).await;

    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    let snapshot = daemon.status_snapshot();
    assert_eq!(snapshot.state, SharedDaemonState::Running);
    assert_eq!(
        snapshot.runtime.as_ref().map(|runtime| runtime.pid),
        Some(pid)
    );

    child.kill().await.expect("kill taken-over fake app-server");
    let _ = child.wait().await;

    let restarted = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let snapshot = daemon.status_snapshot();
            let restarted_pid = snapshot.runtime.as_ref().map(|runtime| runtime.pid);
            if snapshot.state == SharedDaemonState::Running
                && snapshot.restart_count >= 1
                && restarted_pid != Some(pid)
            {
                return snapshot;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("taken-over daemon exit should trigger restart");

    assert_eq!(restarted.restart_count, 1);
}

#[tokio::test]
async fn cleanup_guard_drop_kills_pgid() {
    let child = Command::new("sleep")
        .arg("120")
        .process_group(0)
        .kill_on_drop(false)
        .spawn()
        .expect("spawn guard test child");
    let pid = i32::try_from(child.id().expect("child pid")).expect("pid fits i32");

    drop_spawned_child_guard_for_test(child, pid);

    assert!(
        waitpid_reaped(pid).await,
        "SpawnedChildGuard drop must reap the child process group"
    );
}

#[tokio::test]
async fn interrupt_active_turn_is_noop_when_no_active_turn() {
    let repo = repo().await;
    let daemon = SharedCodexAppServer::new_stub(repo);

    daemon
        .interrupt_active_turn("thread-without-active-turn")
        .await
        .expect("missing active turn should be a no-op");
}

#[tokio::test]
async fn turn_start_seeds_active_turns_synchronously() {
    let _guard = ENV_LOCK.lock().await;
    unsafe {
        std::env::set_var("FAKE_CODEX_SKIP_TURN_STARTED", "1");
    }

    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let card_id = seed_card(&repo, 1).await;
    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    let thread_id = daemon
        .thread_start_for_card(
            &card_id,
            CardRole::Plain,
            None,
            SharedThreadStartParams {
                cwd: "/tmp".into(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: None,
            },
        )
        .await
        .unwrap();
    let turn_id = daemon
        .turn_start(&thread_id, vec![InputItem::text("seed active turn")])
        .await
        .unwrap();

    unsafe {
        std::env::remove_var("FAKE_CODEX_SKIP_TURN_STARTED");
    }

    assert_eq!(
        daemon.active_turn_for_test(&thread_id).as_deref(),
        Some(turn_id.as_str())
    );
}

#[tokio::test]
async fn interrupt_active_turn_immediately_after_turn_start_succeeds() {
    let _guard = ENV_LOCK.lock().await;
    let root = tempfile::tempdir().unwrap();
    let interrupt_marker = root.path().join("interrupt-marker");
    unsafe {
        std::env::set_var("FAKE_CODEX_SKIP_TURN_STARTED", "1");
        std::env::set_var("FAKE_CODEX_INTERRUPT_MARKER", &interrupt_marker);
    }

    let repo = repo().await;
    let card_id = seed_card(&repo, 1).await;
    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    let thread_id = daemon
        .thread_start_for_card(
            &card_id,
            CardRole::Plain,
            None,
            SharedThreadStartParams {
                cwd: "/tmp".into(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: None,
            },
        )
        .await
        .unwrap();
    daemon
        .turn_start(&thread_id, vec![InputItem::text("interrupt active turn")])
        .await
        .unwrap();
    daemon.interrupt_active_turn(&thread_id).await.unwrap();

    unsafe {
        std::env::remove_var("FAKE_CODEX_SKIP_TURN_STARTED");
        std::env::remove_var("FAKE_CODEX_INTERRUPT_MARKER");
    }

    assert_eq!(std::fs::read_to_string(interrupt_marker).unwrap(), "1");
}

#[tokio::test]
async fn active_turns_map_tracks_turn_started_and_completed() {
    let _guard = ENV_LOCK.lock().await;
    unsafe {
        std::env::set_var("FAKE_CODEX_TURN_COMPLETED_DELAY_MS", "250");
    }

    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let card_id = seed_card(&repo, 1).await;
    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    let thread_id = daemon
        .thread_start_for_card(
            &card_id,
            CardRole::Plain,
            None,
            SharedThreadStartParams {
                cwd: "/tmp".into(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: None,
            },
        )
        .await
        .unwrap();
    let turn_id = daemon
        .turn_start(&thread_id, vec![InputItem::text("track active turn")])
        .await
        .unwrap();

    wait_for_active_turn(&daemon, &thread_id, Some(&turn_id)).await;
    wait_for_active_turn(&daemon, &thread_id, None).await;

    unsafe {
        std::env::remove_var("FAKE_CODEX_TURN_COMPLETED_DELAY_MS");
    }
}
