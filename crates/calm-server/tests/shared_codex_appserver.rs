use std::sync::Arc;
use std::time::Duration;
use std::{path::Path, process::Stdio};

use calm_server::codex_appserver::InputItem;
use calm_server::config::Config;
use calm_server::db::sqlite::{
    SqlxRepo, card_mcp_token_set_tx, session_mcp_token_set_tx, session_start_runtime_tx,
};
use calm_server::db::{
    Repo, RepoOutOfDomain, RepoRead, RepoSyncDomainRaw, SharedCodexDaemonUpdate,
};
use calm_server::mcp_server::{McpShimConfig, auth};
use calm_server::model::{CardRole, NewCard, NewCove, NewWave, new_id, now_ms};
use calm_server::proc_identity::{read_boot_id, read_proc_start_time};
use calm_server::routes::theme::RequestTheme;
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::{
    BackoffState, ReplaceOutcome, ReplacePrecondition, SPAWN_ENV_PASSTHROUGH, SharedCodexAppServer,
    SharedDaemonState, SharedThreadStartParams, ThreadConfig, bounded_exponential_backoff,
    drop_spawned_child_guard_for_test,
};
use clap::Parser;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// Serializes intra-binary tests that toggle `FAKE_CODEX_CAPTURE_REQUESTS`
/// (or any other process env read by the fake codex shim). Peer test
/// binaries keep their own `ENV_LOCK` because each test binary is a separate
/// process.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct EnvGuard(&'static str);

impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var(self.0);
        }
    }
}

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

    // #863: `env_clear()` subsumes the old per-key `env_remove`; the stale
    // per-card keys must be ABSENT from `get_envs()` (no explicit-removal
    // `Some(&None)` marker, no explicit set).
    for stale in [
        "NEIGE_CARD_ID",
        "NEIGE_HOOK_PROVIDER",
        "NEIGE_MCP_TOKEN",
        "NEIGE_HOOK_URL",
    ] {
        assert_eq!(
            env.get(stale),
            None,
            "{stale} must be absent from the explicit spawn env"
        );
    }
    assert_eq!(
        env.get("NEIGE_CALM_BASE_URL").cloned().flatten().as_deref(),
        Some("http://expected")
    );
}

/// #863 §5 — allow-list purity at the `get_envs()` seam: every explicitly-set
/// key is either a computed key, a `SPAWN_ENV_PASSTHROUGH` entry, or (in this
/// fixtures-enabled test build only) a fake-codex fixture-channel key.
#[tokio::test]
async fn spawn_env_explicit_keys_stay_within_allow_list_and_computed_keys() {
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

    let daemon = SharedCodexAppServer::new(&cfg, Arc::new(home), repo);
    let env = daemon.spawn_env_for_test().await.unwrap();

    let computed = [
        "CODEX_HOME",
        "NEIGE_CALM_BASE_URL",
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
    ];
    for key in env.keys() {
        let allowed = computed.contains(&key.as_str())
            || SPAWN_ENV_PASSTHROUGH.contains(&key.as_str())
            // Fixture channel: exists only because this test binary compiles
            // calm-server with the `fixtures` feature; NOT part of the prod const.
            || key.starts_with("FAKE_CODEX_")
            || key == "NEIGE_OSC_TRACE_PATH";
        assert!(
            allowed,
            "spawn env key {key} is outside the #863 allow-list; \
             the child env must be a pure function of config"
        );
    }
    assert!(env.contains_key("CODEX_HOME"));
    assert!(env.contains_key("NEIGE_CALM_BASE_URL"));
    assert_eq!(
        env.get("HTTP_PROXY").cloned().flatten().as_deref(),
        Some("http://proxy.local:3128"),
        "settings proxy must be an explicit computed key"
    );
}

/// #863 red repro (TEST A): the spawned shared app-server must not inherit
/// ambient parent env. `spawn_env_for_test` cannot catch this class of bug —
/// `Command::get_envs()` only reports explicitly-set keys, never implicit
/// inheritance — so this test boots the real spawn path
/// (`start_or_takeover` → `launch_spawned_process`) against the fake codex
/// binary and reads `/proc/<pid>/environ` of the live child directly.
///
/// RED today: `apply_spawn_env` never calls `env_clear()`, so parent-side
/// canaries leak into the codex child. Turns green once the child env is an
/// explicit allow-list (a pure function of config).
#[tokio::test]
async fn spawned_daemon_does_not_inherit_parent_canary_env() {
    let _guard = ENV_LOCK.lock().await;
    unsafe {
        std::env::set_var("NEIGE_TEST_CANARY_LEAK", "863");
        std::env::set_var("CLAUDE_CODE_EXECPATH", "/tmp/fake-claude-863");
    }
    let _canary = EnvGuard("NEIGE_TEST_CANARY_LEAK");
    let _execpath = EnvGuard("CLAUDE_CODE_EXECPATH");
    assert_eq!(
        std::env::var("NEIGE_TEST_CANARY_LEAK").as_deref(),
        Ok("863"),
        "positive control: canary must be set in the parent before spawn"
    );

    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let daemon = server(&root, repo).await;
    daemon.start_or_takeover().await.unwrap();

    // The fake app-server keeps serving its socket until the daemon drops
    // it (kill_on_drop), so /proc/<pid>/environ stays readable here.
    let pid = daemon
        .status_snapshot()
        .runtime
        .expect("running spawned daemon")
        .pid;
    let raw = std::fs::read(format!("/proc/{pid}/environ")).expect("read child /proc environ");
    let child_env = raw
        .split(|byte| *byte == 0)
        .filter(|chunk| !chunk.is_empty())
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect::<Vec<String>>();

    let expected_codex_home = format!(
        "CODEX_HOME={}",
        cfg(&root).data_dir_resolved().join("codex-home").display()
    );
    assert!(
        child_env.contains(&expected_codex_home),
        "sanity: child environ must carry the explicit CODEX_HOME pin; got {child_env:?}"
    );

    let leaks = child_env
        .iter()
        .filter(|kv| {
            kv.starts_with("NEIGE_TEST_CANARY_LEAK=") || kv.starts_with("CLAUDE_CODE_EXECPATH=")
        })
        .collect::<Vec<_>>();
    assert!(
        leaks.is_empty(),
        "spawned codex app-server must not inherit ambient parent env \
         (child env must be a pure function of config); leaked: {leaks:?}"
    );

    // #863 §5 allow-list purity: the child's key set must be a subset of
    // {computed keys} ∪ SPAWN_ENV_PASSTHROUGH (∪ the fixture channel, which
    // exists only in this fixtures-enabled build) — "no key outside the pure
    // function".
    let computed = [
        "CODEX_HOME",
        "NEIGE_CALM_BASE_URL",
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
    ];
    for kv in &child_env {
        let key = kv.split_once('=').map(|(k, _)| k).unwrap_or(kv.as_str());
        let allowed = computed.contains(&key)
            || SPAWN_ENV_PASSTHROUGH.contains(&key)
            || key.starts_with("FAKE_CODEX_")
            || key == "NEIGE_OSC_TRACE_PATH";
        assert!(
            allowed,
            "child environ key {key} is outside the #863 allow-list \
             (child env must be a pure function of config); full env: {child_env:?}"
        );
    }
    // Positive outage canaries: the allow-list must actually pass the
    // load-bearing vars through, not just drop everything.
    for canary in ["PATH=", "HOME="] {
        assert!(
            child_env.iter().any(|kv| kv.starts_with(canary)),
            "allow-list must pass {canary} through to the child; got {child_env:?}"
        );
    }
}

/// #863 red repro (TEST B): booting the shared app-server against a
/// CODEX_HOME whose config.toml carries an unexpected `[mcp_servers.*]`
/// entry must be refused at launch. `seed()` copies host `~/.codex/`
/// verbatim, so a host-level plugin registration lands in the shared home
/// exactly like the pollution written below.
///
/// RED today: launch never inspects config.toml and boots happily.
#[tokio::test]
async fn boot_rejects_codex_home_with_unexpected_mcp_server_entry() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let cfg = cfg(&root);
    let home = calm_server::shared_codex_home::SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
        cfg.data_dir_resolved().join("codex-homes"),
    );
    // seed_from(None): deterministic empty home, no host ~/.codex copy.
    home.seed_from(None).unwrap();
    // Legitimate `calm` entry, as boot wiring (state.rs) writes it.
    let shim = McpShimConfig {
        shim_bin: root.path().join("bin/neige-mcp-stdio-shim"),
        socket_path: root.path().join("mcp/kernel.sock"),
    };
    home.ensure_daemon_mcp_config(&shim, "daemon-token")
        .unwrap();
    // Pollution: an extra MCP server beyond the expected `calm` one.
    let cfg_path = home.path().join("config.toml");
    let mut polluted = std::fs::read_to_string(&cfg_path).unwrap();
    polluted.push_str("\n[mcp_servers.evil]\ncommand = \"/usr/bin/evil-mcp\"\n");
    std::fs::write(&cfg_path, &polluted).unwrap();

    let daemon = SharedCodexAppServer::new(&cfg, Arc::new(home), repo);
    let err = daemon.start_or_takeover().await.expect_err(
        "boot must refuse a CODEX_HOME polluted with an unexpected [mcp_servers.*] entry",
    );
    let msg = err.to_string();
    assert!(
        msg.contains("evil"),
        "launch refusal must name the unexpected mcp server entry; got: {msg}"
    );

    // #863 review F1 — the refusal must be visible on the status surface,
    // not only in the boot error: state=Failed with last_error naming the
    // offender.
    let status = daemon.status_snapshot();
    assert_eq!(
        status.state,
        SharedDaemonState::Failed,
        "guard refusal must surface state=Failed in status_snapshot()"
    );
    let last_error = status
        .last_error
        .expect("guard refusal must surface last_error in status_snapshot()");
    assert!(
        last_error.contains("evil"),
        "status last_error must name the unexpected mcp server entry; got: {last_error}"
    );
}

/// #863 review F2 — a `.env` dropped into the shared CODEX_HOME after boot
/// sanitize (e.g. while the daemon runs) would be injected into the daemon's
/// own process env by codex arg0 `load_dotenv` at the next launch, bypassing
/// the spawn allow-list. The launch guard treats it as derived-state
/// pollution and DELETES it (warn, no outage) before the spawn — boot must
/// succeed AND the file must be gone.
#[tokio::test]
async fn boot_deletes_leaked_codex_home_env_file_before_spawn() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let daemon = server(&root, repo).await;
    let env_path = daemon.codex_home_path().join(".env");
    std::fs::write(&env_path, "INJECTED_AFTER_BOOT_SANITIZE=863\n").unwrap();

    daemon
        .start_or_takeover()
        .await
        .expect("boot must converge by deleting the leaked .env, not refuse");

    assert!(
        !env_path.exists(),
        "launch guard must delete CODEX_HOME/.env before the daemon spawns"
    );
    assert_eq!(
        daemon.status_snapshot().state,
        SharedDaemonState::Running,
        "daemon must be running after the .env repair"
    );
}

/// Deterministic shared home whose config.toml carries an unexpected
/// `[mcp_servers.evil]` entry, so `start_or_takeover`'s boot guard refuses.
/// Mirrors the setup of `boot_rejects_codex_home_with_unexpected_mcp_server_entry`.
fn polluted_home(root: &tempfile::TempDir) -> calm_server::shared_codex_home::SharedCodexHome {
    let cfg = cfg(root);
    let home = calm_server::shared_codex_home::SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
        cfg.data_dir_resolved().join("codex-homes"),
    );
    home.seed_from(None).unwrap();
    let shim = McpShimConfig {
        shim_bin: root.path().join("bin/neige-mcp-stdio-shim"),
        socket_path: root.path().join("mcp/kernel.sock"),
    };
    home.ensure_daemon_mcp_config(&shim, "daemon-token")
        .unwrap();
    let cfg_path = home.path().join("config.toml");
    let mut polluted = std::fs::read_to_string(&cfg_path).unwrap();
    polluted.push_str("\n[mcp_servers.evil]\ncommand = \"/usr/bin/evil-mcp\"\n");
    std::fs::write(&cfg_path, &polluted).unwrap();
    home
}

/// #863 review R2-3(b) — a boot-guard refusal against a VERIFIED persisted
/// daemon must both reap the process and persist the reap: state=failed,
/// identity tuple (pid/pgid/start_time/boot_id) cleared, last_error naming
/// the offending guard error.
#[tokio::test]
async fn guard_refusal_reaps_verified_daemon_and_persists_failed_record() {
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
        .expect("spawn fake app-server for guard-refusal reap");
    let old_pid = i32::try_from(child.id().expect("fake app-server pid")).expect("pid fits i32");
    let process_start_time = wait_for_start_time_and_socket(old_pid, &sock).await;

    let repo = repo().await;
    persist_running_daemon(&repo, &root, old_pid, old_pid, &sock, process_start_time).await;

    let daemon =
        SharedCodexAppServer::new(&cfg(&root), Arc::new(polluted_home(&root)), repo.clone());
    let err = daemon
        .start_or_takeover()
        .await
        .expect_err("boot guard must refuse the polluted CODEX_HOME");
    assert!(
        err.to_string().contains("evil"),
        "refusal must name the offender; got: {err}"
    );

    tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("guard refusal must reap the verified persisted daemon")
        .expect("wait reaped fake app-server");

    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        SharedDaemonState::from_db_str(&record.state),
        SharedDaemonState::Failed,
        "guard-refusal reap must persist state=failed"
    );
    assert_eq!(record.pid, None, "pid must be cleared after the reap");
    assert_eq!(record.pgid, None, "pgid must be cleared after the reap");
    assert_eq!(
        record.process_start_time, None,
        "process_start_time must be cleared after the reap"
    );
    assert_eq!(
        record.boot_id, None,
        "boot_id must be cleared after the reap"
    );
    let last_error = record
        .last_error
        .expect("guard-refusal reap must persist last_error");
    assert!(
        last_error.contains("evil"),
        "persisted last_error must name the offending entry; got: {last_error}"
    );
}

/// #863 review R2-3(a) / F3a, reshaped by #953 §3 — a persisted record whose
/// pgid != pid is corrupt (spawn invariant: `process_group(0)` ⇒ pgid ==
/// pid); the guard refusal must NOT signal it (the launcher/native split
/// makes the liveness assertion meaningful: a regression that signals the
/// recorded pgid kills both). #953 change: the record is no longer left
/// untouched — the refusal persists `failed` RETAINING the identity tuple as
/// the durable unreconciled marker, with the `unreconciled:` prefix.
#[tokio::test]
async fn guard_refusal_pgid_mismatch_marks_unreconciled_retains_identity_and_never_signals() {
    let root = tempfile::tempdir().unwrap();
    let sock = root.path().join("run/codex-appserver.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();

    let (launcher, pgid, peer_pid) = spawn_launcher_with_fake_appserver(&sock, false, false).await;
    let process_start_time = read_proc_start_time(peer_pid).expect("fake app-server start time");

    let repo = repo().await;
    // Mismatched identity tuple: pid = native app-server, pgid = launcher group.
    persist_running_daemon(&repo, &root, peer_pid, pgid, &sock, process_start_time).await;

    let daemon =
        SharedCodexAppServer::new(&cfg(&root), Arc::new(polluted_home(&root)), repo.clone());
    let err = daemon
        .start_or_takeover()
        .await
        .expect_err("boot guard must still refuse the polluted CODEX_HOME");
    assert!(
        err.to_string().contains("evil"),
        "refusal must name the offender; got: {err}"
    );

    // Any (buggy) reap is fully awaited inside `start_or_takeover`; add a
    // short grace for signal delivery, then probe liveness.
    tokio::time::sleep(Duration::from_millis(200)).await;
    // SAFETY: signal 0 probes liveness without delivering a signal.
    let peer_alive = unsafe { libc::kill(peer_pid, 0) } == 0;
    let launcher_alive = unsafe { libc::kill(pgid, 0) } == 0;
    let record = repo.shared_daemon_runtime_get().await.unwrap();

    force_cleanup_process_group(launcher, pgid);

    assert!(
        peer_alive && launcher_alive,
        "guard refusal must NOT signal a corrupt (pgid != pid) persisted record; \
         peer_alive={peer_alive} launcher_alive={launcher_alive}"
    );
    // #953 §3 — identity RETAINED as-read (the durable unreconciled marker),
    // state now `failed` with the `unreconciled:` prefix.
    assert_eq!(
        record.pid,
        Some(peer_pid),
        "unreconciled refusal must retain the persisted pid as-read"
    );
    assert_eq!(
        record.pgid,
        Some(pgid),
        "unreconciled refusal must retain the persisted pgid as-read"
    );
    assert_eq!(
        record.process_start_time,
        Some(process_start_time),
        "unreconciled refusal must retain the persisted start_time as-read"
    );
    assert_eq!(
        SharedDaemonState::from_db_str(&record.state),
        SharedDaemonState::Failed,
        "unreconciled refusal must persist state=failed"
    );
    let last_error = record
        .last_error
        .expect("unreconciled refusal must persist last_error");
    assert!(
        last_error.starts_with("unreconciled: "),
        "unreconciled last_error must carry the durable prefix; got: {last_error}"
    );
    assert!(
        last_error.contains("evil"),
        "unreconciled last_error must name the offender; got: {last_error}"
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
        // These tests model daemon-group survival beyond the launcher's
        // death; the fixture's pdeathsig test-hygiene belt would kill the
        // child as soon as the launcher exits and make them vacuous.
        .env("FAKE_CODEX_NO_PDEATHSIG", "1")
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
            workflow_input: None,
            cove_id: cove.id,
            title: format!("wave-{idx}"),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    repo.card_create(NewCard {
        wave_id: wave.id,
        title: None,
        kind: "terminal".into(),
        sort: None,
        payload: json!({"codex_source": "shared"}),
    })
    .await
    .unwrap()
    .id
    .to_string()
}

async fn seed_runtime_thread(repo: &SqlxRepo, card_id: &str, thread_id: &str) -> String {
    seed_runtime_thread_with_kind(repo, card_id, thread_id, WorkerSessionKind::CodexCard).await
}

async fn seed_runtime_thread_with_kind(
    repo: &SqlxRepo,
    card_id: &str,
    thread_id: &str,
    kind: WorkerSessionKind,
) -> String {
    let runtime_id = new_id();
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: card_id.to_string(),
            kind,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Running,
            terminal_run_id: None,
            thread_id: Some(thread_id.to_string()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    runtime_id
}

async fn wait_for_requests(path: &Path, min_count: usize) -> Vec<Value> {
    for _ in 0..100 {
        if let Ok(raw) = std::fs::read_to_string(path) {
            let rows = raw
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect::<Vec<Value>>();
            if rows.len() >= min_count {
                return rows;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for captured fake app-server requests");
}

async fn card_mcp_hash(repo: &SqlxRepo, card_id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT hashed_token FROM card_mcp_tokens WHERE card_id = ?1")
        .bind(card_id)
        .fetch_optional(repo.pool())
        .await
        .unwrap()
}

async fn session_mcp_hash(repo: &SqlxRepo, runtime_id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id = ?1")
        .bind(runtime_id)
        .fetch_one(repo.pool())
        .await
        .unwrap()
}

fn thread_resume_token(req: &Value) -> &str {
    req.pointer("/params/config/shell_environment_policy/set/NEIGE_MCP_TOKEN")
        .and_then(Value::as_str)
        .expect("thread/resume config must carry NEIGE_MCP_TOKEN")
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
    let live_boot = calm_server::proc_identity::read_boot_id().unwrap_or_default();
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

/// #954 defect 3, design test 4 (rewrites the pre-#954
/// `takeover_respawns_when_env_signature_differs` — the semantics FLIP):
/// signature mismatch against a VERIFIED healthy daemon must ADOPT it and
/// mark `needs_respawn` (drain at the next thread-start boundary) instead of
/// executing it at boot — the boot-time reap is what killed prod on 7/12
/// (#863 salt ⇒ guaranteed mismatch on the first post-upgrade boot). The
/// re-stamp must keep the OLD persisted signature so the drain obligation
/// survives calm-server restarts.
#[tokio::test]
async fn takeover_adopts_on_env_signature_mismatch_and_marks_drain() {
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

    let snapshot = daemon.status_snapshot();
    assert_eq!(snapshot.state, SharedDaemonState::Running);
    assert_eq!(
        snapshot.runtime.as_ref().map(|runtime| runtime.pid),
        Some(old_pid),
        "the mismatched-but-healthy daemon must be ADOPTED, not reaped"
    );
    // SAFETY: signal 0 probes liveness without delivering a signal.
    assert_eq!(
        unsafe { libc::kill(old_pid, 0) },
        0,
        "the adopted daemon must still be alive — never executed at boot"
    );
    assert!(
        daemon.needs_respawn_on_next_thread_start_for_test(),
        "adoption of a mismatched signature must mark needs_respawn (drain)"
    );

    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        SharedDaemonState::from_db_str(&record.state),
        SharedDaemonState::Running,
        "adoption must re-stamp the row running"
    );
    assert_eq!(record.pid, Some(old_pid));
    assert_eq!(
        record.daemon_env_signature.as_deref(),
        Some("stale-env-signature"),
        "the re-stamp must keep the OLD signature — the drain obligation \
         must survive calm-server restarts (next boot re-detects + re-marks)"
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// #954 review D3 (failing-first) — adopt-and-drain marks `needs_respawn`,
/// but a crash-lane respawn already spawns with CURRENT settings and
/// persists the current signature: the pending drain is satisfied by that
/// very spawn and must be CLEARED, so the next mint does not pay a fully
/// redundant graceful replace (cold start, potentially minutes). Pre-fix
/// the stale flag survived the respawn and the mint replaced the fresh
/// daemon (pid/generation churn asserted below).
#[tokio::test]
async fn crash_respawn_with_current_settings_clears_pending_drain() {
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
        .expect("spawn stale-signature fake app-server for adoption");
    let adopted_pid =
        i32::try_from(child.id().expect("fake app-server pid")).expect("pid fits i32");
    let process_start_time = wait_for_start_time_and_socket(adopted_pid, &sock).await;

    let repo = repo().await;
    persist_running_daemon_with_signature(
        &repo,
        &root,
        adopted_pid,
        adopted_pid,
        &sock,
        process_start_time,
        Some("stale-env-signature".into()),
    )
    .await;

    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    assert!(
        daemon.needs_respawn_on_next_thread_start_for_test(),
        "adoption of a mismatched signature must arm the drain"
    );

    // Crash the adopted daemon: the crash-lane watcher respawns with
    // CURRENT settings and persists the current signature.
    child.kill().await.expect("kill adopted fake app-server");
    let _ = child.wait().await;
    let respawned = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let snapshot = daemon.status_snapshot();
            let pid = snapshot.runtime.as_ref().map(|runtime| runtime.pid);
            if snapshot.state == SharedDaemonState::Running && pid != Some(adopted_pid) {
                return snapshot;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("crashed adopted daemon must be respawned by the crash lane");
    let respawned_pid = respawned.runtime.expect("running").pid;

    assert!(
        !daemon.needs_respawn_on_next_thread_start_for_test(),
        "a successful respawn under current settings must clear the \
         now-satisfied drain flag"
    );

    // The mint must NOT graceful-replace the already-current daemon.
    let generation_before = daemon.generation_for_test().await;
    let card_id = seed_card(&repo, 1).await;
    let thread_id = daemon
        .thread_start_mint_for_card(
            &card_id,
            SharedThreadStartParams {
                cwd: "/tmp".into(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: None,
                config: ThreadConfig::NoMcp,
            },
        )
        .await
        .unwrap();
    assert_eq!(thread_id, "fake-thread-0001");
    let after_mint = daemon.status_snapshot();
    assert_eq!(
        after_mint.runtime.as_ref().map(|runtime| runtime.pid),
        Some(respawned_pid),
        "mint after a current-settings crash respawn must NOT pay a \
         redundant graceful replace"
    );
    assert_eq!(
        daemon.generation_for_test().await,
        generation_before,
        "no replace transition may have run at the mint boundary"
    );
}

/// #954 review D1 walk (r2 update) — the launcher (the persisted leader)
/// dies on the group SIGTERM but is held UNREAPED by this test (tokio
/// reaps only on `wait`/drop), so the reap helper deterministically
/// observes an exited ZOMBIE leader on the NON-owned (`VerifiedIdentity`)
/// path. Pre-r2 that arm sent a group-wide `kill(-pgid, SIGKILL)` — the
/// recycle-unsafe signal the r2 review flagged (an externally-reapable
/// zombie does not pin the pgid across the observation→signal interval).
/// Post-r2 the arm instead SWEEPS the remaining group members
/// individually (scan `/proc` for `pgrp == pgid`, re-verify each member's
/// `start_time`, SIGKILL by pid) — and the TERM-ignoring native
/// descendant still dies. This test is therefore the Z-observed cousin of
/// `reaped_leader_sweep_kills_verified_group_members` below: same
/// observable oracle (descendant dies) via the sweep instead of the group
/// signal; the absence of any group-wide signal on this arm is pinned by
/// construction (the `ExitedPinned`-non-owned arm no longer contains a
/// `scope.send(SIGKILL)` call), not by an observable — a recycled pgid
/// cannot be constructed deterministically in a test.
#[tokio::test]
async fn takeover_handshake_fail_kills_group_descendants_even_after_launcher_exits() {
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

/// #954 review r2 D1 (failing-first) — when the verified leader is FULLY
/// REAPED by its external parent (or observed as an externally-reapable
/// zombie) within the grace, the group identity is unverifiable and the
/// numeric pgid may already be recycled, so no group-wide
/// `kill(-pgid, SIGKILL)` may be sent. But simply SKIPPING (the r1 shape,
/// `fully_reaped_leader_skips_final_group_sigkill`) leaked TERM-ignoring
/// descendants — the r2 review's honesty note. r2 fix: the non-owned
/// Z/FullyGone arms SWEEP the remaining group members individually
/// (scan `/proc` for `pgrp == pgid`, capture each member's `start_time`,
/// re-verify it, SIGKILL by pid). Observable oracle: the TERM-ignoring
/// descendant left in the group now DIES — pre-r2-fix this construction
/// left it alive (asserted by this test's r1 predecessor), so this
/// assertion fails first on the pre-fix code. The recycled-pgid hazard
/// itself is not deterministically constructible in a test; its guard is
/// pinned by the per-member `start_time` re-verify (unit-tested in
/// `proc_identity`) plus code construction: neither non-owned arm
/// contains a group-wide signal anymore.
#[tokio::test]
async fn reaped_leader_sweep_kills_verified_group_members() {
    let root = tempfile::tempdir().unwrap();
    let leader_pid_file = root.path().join("leader.pid");
    let survivor_pid_file = root.path().join("survivor.pid");
    // Leader script, run as a fresh session/process-group leader: it traps
    // TERM → exits immediately, after spawning a TERM-ignoring survivor
    // inside its group. Its PARENT (the intermediate `sh` below) blocks in
    // the foreground and therefore reaps it the instant it dies — the
    // "external parent reaps the leader during the supervisor's grace
    // poll" shape.
    let leader_script = root.path().join("leader.sh");
    std::fs::write(
        &leader_script,
        r#"echo $$ > "$LEADER_PID_FILE"
( trap '' TERM; sleep 300 ) &
echo $! > "$SURVIVOR_PID_FILE"
trap 'exit 0' TERM
sleep 300 &
wait
"#,
    )
    .unwrap();
    // The trailing `; exit $?` keeps the intermediate sh from exec-ing
    // setsid: it must stay alive as the leader's reaping parent.
    let parent = Command::new("sh")
        .arg("-c")
        .arg(r#"setsid sh "$LEADER_SCRIPT"; exit $?"#)
        .env("LEADER_SCRIPT", &leader_script)
        .env("LEADER_PID_FILE", &leader_pid_file)
        .env("SURVIVOR_PID_FILE", &survivor_pid_file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn reaping parent for fully-gone leader");

    let read_pid_file = |path: std::path::PathBuf| async move {
        for _ in 0..100 {
            if let Ok(raw) = std::fs::read_to_string(&path)
                && let Ok(pid) = raw.trim().parse::<i32>()
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out reading pid file {}", path.display());
    };
    let leader_pid = read_pid_file(leader_pid_file.clone()).await;
    let survivor_pid = read_pid_file(survivor_pid_file.clone()).await;
    assert_eq!(
        unsafe { libc::getpgid(survivor_pid) },
        leader_pid,
        "survivor must live in the leader's fresh process group"
    );
    let process_start_time = read_proc_start_time(leader_pid).expect("leader start time");

    // Persist the leader as a verified running daemon whose socket has no
    // listener: the takeover handshake fails and the supervisor reaps the
    // verified group with the stop grace.
    let sock = root.path().join("run/codex-appserver.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
    let repo = repo().await;
    persist_running_daemon(
        &repo,
        &root,
        leader_pid,
        leader_pid,
        &sock,
        process_start_time,
    )
    .await;

    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();

    // The leader must be gone (TERM'd, then reaped by its parent) …
    assert!(
        wait_proc_gone(leader_pid).await,
        "leader must exit on SIGTERM and be reaped by its parent"
    );
    // … and the TERM-ignoring survivor must ALSO die: with the leader
    // reaped (or zombie) no group-wide SIGKILL is allowed, so the sweep
    // must have re-verified the survivor's identity and SIGKILLed it by
    // pid individually.
    let survivor_gone = wait_proc_gone(survivor_pid).await;
    // Cleanup before asserting so a failure can't leak the group.
    unsafe {
        libc::kill(-leader_pid, libc::SIGKILL);
    }
    drop(parent);
    assert!(
        survivor_gone,
        "a reaped/zombie leader must trigger the per-member identity \
         sweep; the TERM-ignoring survivor stayed alive, so the sweep \
         did not run (the r1 skip-only leak)"
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
async fn hot_takeover_plain_resumes_without_rotating_cached_thread_token() {
    let _guard = ENV_LOCK.lock().await;

    let root = tempfile::tempdir().unwrap();
    let sock = root.path().join("run/codex-appserver.sock");
    let capture = root.path().join("requests.ndjson");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();

    let mut child = Command::new(fake_codex_bin())
        .arg("app-server")
        .arg("--listen")
        .arg(format!("unix://{}", sock.display()))
        .env("FAKE_CODEX_CAPTURE_REQUESTS", &capture)
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
    let card_id = seed_card(&repo, 1).await;
    let runtime_id =
        seed_runtime_thread_with_kind(&repo, &card_id, "thread-hot", WorkerSessionKind::SharedSpec)
            .await;
    let old_hash = auth::hash_token("old-hot-token");
    let mut tx = repo.pool().begin().await.unwrap();
    card_mcp_token_set_tx(&mut tx, &card_id, &old_hash)
        .await
        .unwrap();
    session_mcp_token_set_tx(&mut tx, &runtime_id, &old_hash)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    persist_running_daemon(&repo, &root, old_pid, old_pid, &sock, process_start_time).await;

    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();

    let rows = wait_for_requests(&capture, 2).await;
    let resumes = rows
        .iter()
        .filter(|row| {
            row.get("method").and_then(Value::as_str) == Some("thread/resume")
                && row.pointer("/params/threadId").and_then(Value::as_str) == Some("thread-hot")
        })
        .collect::<Vec<_>>();
    assert_eq!(resumes.len(), 1);
    assert!(
        resumes[0].pointer("/params/config").is_none(),
        "hot takeover must plain-resume without config"
    );
    assert_eq!(
        card_mcp_hash(&repo, &card_id).await.as_deref(),
        Some(old_hash.as_str())
    );
    assert_eq!(
        session_mcp_hash(&repo, &runtime_id).await.as_deref(),
        Some(old_hash.as_str())
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
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
    let runtime_id = seed_runtime_thread_with_kind(
        &repo,
        &card_id,
        "thread-resume",
        WorkerSessionKind::SharedSpec,
    )
    .await;
    let old_hash = auth::hash_token("old-resume-token");
    let mut tx = repo.pool().begin().await.unwrap();
    card_mcp_token_set_tx(&mut tx, &card_id, &old_hash)
        .await
        .unwrap();
    session_mcp_token_set_tx(&mut tx, &runtime_id, &old_hash)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    let rows = wait_for_requests(&capture, 2).await;
    let resumes = rows
        .iter()
        .filter(|row| row.get("method").and_then(Value::as_str) == Some("thread/resume"))
        .collect::<Vec<_>>();
    assert_eq!(resumes.len(), 1);
    let first_resume_hash = card_mcp_hash(&repo, &card_id)
        .await
        .expect("initial resume remints card MCP hash");
    assert_ne!(first_resume_hash, old_hash);
    assert_eq!(
        auth::hash_token(thread_resume_token(resumes[0])),
        first_resume_hash
    );
    assert_eq!(
        session_mcp_hash(&repo, &runtime_id).await.as_deref(),
        Some(first_resume_hash.as_str())
    );

    let trigger_card_id = seed_card(&repo, 2).await;
    daemon.mark_needs_respawn();
    daemon
        .thread_start_mint_for_card(
            &trigger_card_id,
            SharedThreadStartParams {
                cwd: "/tmp".into(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: None,
                config: ThreadConfig::NoMcp,
            },
        )
        .await
        .unwrap();
    let rows = wait_for_requests(&capture, rows.len() + 3).await;
    let resumes = rows
        .iter()
        .filter(|row| row.get("method").and_then(Value::as_str) == Some("thread/resume"))
        .collect::<Vec<_>>();
    assert_eq!(resumes.len(), 2);
    let resumed = resumes.last().unwrap();
    let expected_socket = root
        .path()
        .join("mcp/kernel.sock")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        resumed
            .pointer("/params/config/shell_environment_policy/set/NEIGE_MCP_SOCKET")
            .and_then(Value::as_str),
        Some(expected_socket.as_str())
    );
    let respawn_hash = card_mcp_hash(&repo, &card_id)
        .await
        .expect("respawn resume remints card MCP hash");
    assert_ne!(respawn_hash, first_resume_hash);
    assert_eq!(auth::hash_token(thread_resume_token(resumed)), respawn_hash);
    assert_eq!(
        session_mcp_hash(&repo, &runtime_id).await.as_deref(),
        Some(respawn_hash.as_str())
    );

    unsafe {
        std::env::remove_var("FAKE_CODEX_CAPTURE_REQUESTS");
    }
}

#[tokio::test]
async fn cold_respawn_plain_resumes_stale_cache_entry_without_rotating_active_token() {
    let _guard = ENV_LOCK.lock().await;

    let root = tempfile::tempdir().unwrap();
    let capture = root.path().join("requests.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture);
    }
    let _env = EnvGuard("FAKE_CODEX_CAPTURE_REQUESTS");

    let repo = repo().await;
    let card_id = seed_card(&repo, 1).await;
    let runtime_id = seed_runtime_thread_with_kind(
        &repo,
        &card_id,
        "thread-active",
        WorkerSessionKind::SharedSpec,
    )
    .await;
    let old_hash = auth::hash_token("old-active-token");
    let mut tx = repo.pool().begin().await.unwrap();
    card_mcp_token_set_tx(&mut tx, &card_id, &old_hash)
        .await
        .unwrap();
    session_mcp_token_set_tx(&mut tx, &runtime_id, &old_hash)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    let rows = wait_for_requests(&capture, 2).await;
    let first_hash = card_mcp_hash(&repo, &card_id)
        .await
        .expect("initial cold resume remints active hash");
    assert_ne!(first_hash, old_hash);

    let stale_thread_id = daemon
        .thread_start_mint_for_card(
            &card_id,
            SharedThreadStartParams {
                cwd: "/tmp".into(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: None,
                config: ThreadConfig::NoMcp,
            },
        )
        .await
        .unwrap();
    assert_eq!(stale_thread_id, "fake-thread-0001");
    assert_ne!(stale_thread_id, "thread-active");
    let rows = wait_for_requests(&capture, rows.len() + 1).await;

    daemon.mark_needs_respawn();
    daemon.ensure_respawn_for_current_settings().await.unwrap();
    let rows = wait_for_requests(&capture, rows.len() + 3).await;
    let resumes = rows
        .iter()
        .filter(|row| row.get("method").and_then(Value::as_str) == Some("thread/resume"))
        .collect::<Vec<_>>();
    assert_eq!(resumes.len(), 3);
    let respawn_resumes = &resumes[1..];
    let active_resume = respawn_resumes
        .iter()
        .copied()
        .find(|row| {
            row.pointer("/params/threadId").and_then(Value::as_str) == Some("thread-active")
        })
        .expect("cold respawn must resume the active cached thread");
    let stale_resume = respawn_resumes
        .iter()
        .copied()
        .find(|row| {
            row.pointer("/params/threadId").and_then(Value::as_str)
                == Some(stale_thread_id.as_str())
        })
        .expect("cold respawn must plain-resume the stale cached thread");
    assert!(
        stale_resume.pointer("/params/config").is_none(),
        "stale cache entries must not receive reemitted MCP config"
    );
    let respawn_hash = auth::hash_token(thread_resume_token(active_resume));
    assert_ne!(respawn_hash, first_hash);
    assert_eq!(
        card_mcp_hash(&repo, &card_id).await.as_deref(),
        Some(respawn_hash.as_str())
    );
    assert_eq!(
        session_mcp_hash(&repo, &runtime_id).await.as_deref(),
        Some(respawn_hash.as_str())
    );
}

#[tokio::test]
async fn thread_start_mint_for_card_respects_needs_respawn_flag() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    let old_pid = daemon.status_snapshot().runtime.unwrap().pid;
    let card_id = seed_card(&repo, 1).await;

    daemon.mark_needs_respawn();
    assert!(daemon.needs_respawn_on_next_thread_start_for_test());
    let thread_id = daemon
        .thread_start_mint_for_card(
            &card_id,
            SharedThreadStartParams {
                cwd: "/tmp".into(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: None,
                config: ThreadConfig::NoMcp,
            },
        )
        .await
        .unwrap();

    assert_eq!(thread_id, "fake-thread-0001");
    assert!(!daemon.needs_respawn_on_next_thread_start_for_test());
    let new_pid = daemon.status_snapshot().runtime.unwrap().pid;
    assert_ne!(new_pid, old_pid);
    assert_eq!(
        daemon.cached_card_for_thread("fake-thread-0001"),
        Some(card_id.clone())
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
                CardRole::Worker,
                None,
                SharedThreadStartParams {
                    cwd: "/tmp".into(),
                    approval_policy: "never".into(),
                    sandbox_mode: "workspace-write".into(),
                    developer_instructions: None,
                    config: ThreadConfig::NoMcp,
                },
            )
            .await
    });

    let mut observed_respawn_in_progress = false;
    for _ in 0..200 {
        // #480 PR5b: respawn now transitions Restarting → Starting → Running
        // per §C.3. #954 review D3 — gate on the PERSISTED `starting` row:
        // it is written only AFTER the spawn's settings snapshot was read
        // (`load_spawn_env_snapshot` → `persist_runtime_starting`), so the
        // proxy upsert below deterministically postdates the in-flight
        // spawn's snapshot. The in-memory Starting state alone is flipped
        // BEFORE the snapshot read and would race the upsert.
        let record = repo.shared_daemon_runtime_get().await.unwrap();
        if SharedDaemonState::from_db_str(&record.state) == SharedDaemonState::Starting
            && !daemon.needs_respawn_on_next_thread_start_for_test()
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

    // #954 review D3 — model the production shape: the settings route
    // upserts the changed proxy BEFORE marking, so the concurrent mark
    // corresponds to a REAL signature change. The in-flight respawn's
    // snapshot predates the upsert (see the gate above), so the
    // spawn-success clear-if-current re-reads a DIFFERENT signature and
    // must preserve this mark.
    repo.settings_upsert("http_proxy", "http://drain-proxy.test:8080")
        .await
        .unwrap();
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
                CardRole::Worker,
                None,
                SharedThreadStartParams {
                    cwd: "/tmp".into(),
                    approval_policy: "never".into(),
                    sandbox_mode: "workspace-write".into(),
                    developer_instructions: None,
                    config: ThreadConfig::NoMcp,
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

/// #954 design test 12 (rewrites the pre-#954 `cleanup_guard_drop_kills_pgid`
/// — the belt semantics FLIP): `SpawnedChildGuard::drop` is a belt reachable
/// only via detached-task panic/teardown-abort; it must send SIGTERM ONLY —
/// no SIGKILL chaser (and no tokio `kill_on_drop`, which is removed). A
/// cooperative child that checkpoints for 300ms after SIGTERM must live long
/// enough to write its marker; the pre-#954 TERM+instant-SIGKILL belt (and
/// `kill_on_drop`) killed it before the write.
#[tokio::test]
async fn spawn_guard_drop_belt_sends_sigterm_only() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("sigterm-marker");
    let handler_ready = root.path().join("handler-ready-marker");
    let child = Command::new(fake_codex_bin())
        .arg("app-server")
        .arg("--listen")
        .arg(format!(
            "unix://{}",
            root.path().join("belt.sock").display()
        ))
        .env("FAKE_CODEX_SIGTERM_MARKER", &marker)
        .env("FAKE_CODEX_SIGTERM_EXIT_DELAY_MS", "300")
        .env("FAKE_CODEX_HANDLER_READY_MARKER", &handler_ready)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(false)
        .spawn()
        .expect("spawn guard test child");
    let pid = i32::try_from(child.id().expect("child pid")).expect("pid fits i32");
    // #954 review D4 — readiness handshake instead of a fixed sleep: the
    // fixture writes `handler_ready` only AFTER its SIGTERM handler (and
    // monitor thread) are armed. A fixed sleep on a loaded runner could
    // fire the belt TERM at a still-default-disposition process, killing
    // it before the cooperative marker write and hard-failing the oracle.
    let mut handler_armed = false;
    for _ in 0..500 {
        if handler_ready.exists() {
            handler_armed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        handler_armed,
        "fixture must report its SIGTERM handler armed before the belt fires"
    );

    drop_spawned_child_guard_for_test(child, pid);

    let mut marker_written = false;
    for _ in 0..100 {
        if marker.exists() {
            marker_written = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        marker_written,
        "the drop belt must be SIGTERM-only: the child's 300ms cooperative \
         shutdown must complete (an immediate SIGKILL chaser would kill it \
         before the marker write)"
    );
    assert!(
        waitpid_reaped(pid).await,
        "the TERM'd child must exit cleanly on its own"
    );
}

/// #954 design test 1 (failing-first) — settings-drain `transition_replace`
/// must reap the Running daemon with an exit-driven grace: a cooperative
/// daemon that checkpoints for 1.5s after SIGTERM writes its marker and
/// exits 0, and is never SIGKILLed. Pre-#954 the reap gave it a fixed 500ms
/// and then SIGKILLed — the marker never appeared (that instant SIGKILL is
/// what armed codex's 900s backfill lease in prod on 7/12).
#[tokio::test]
async fn settings_drain_reaps_running_daemon_gracefully() {
    let _guard = ENV_LOCK.lock().await;
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("graceful-term-marker");
    unsafe {
        std::env::set_var("FAKE_CODEX_SIGTERM_MARKER", &marker);
        std::env::set_var("FAKE_CODEX_SIGTERM_EXIT_DELAY_MS", "1500");
    }
    let _marker_env = EnvGuard("FAKE_CODEX_SIGTERM_MARKER");
    let _delay_env = EnvGuard("FAKE_CODEX_SIGTERM_EXIT_DELAY_MS");

    let repo = repo().await;
    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    let old_pid = daemon.status_snapshot().runtime.expect("running").pid;

    daemon.mark_needs_respawn();
    daemon.ensure_respawn_for_current_settings().await.unwrap();

    assert!(
        marker.exists(),
        "the old daemon must be given its cooperative 1.5s SIGTERM shutdown \
         (marker written before exit); a 500ms-then-SIGKILL reap kills it \
         before the write"
    );
    let snapshot = daemon.status_snapshot();
    assert_eq!(snapshot.state, SharedDaemonState::Running);
    assert_ne!(
        snapshot.runtime.as_ref().map(|runtime| runtime.pid),
        Some(old_pid),
        "the replacement daemon must be a fresh process"
    );
}

/// #954 design test 2 (failing-first) — the cold-start-deadline miss (the
/// mid-backfill child whose SIGKILL armed the production livelock) is an
/// ordinary error path and gets the FULL grace: deadline miss ⇒ explicit
/// graceful reap ⇒ the never-initialized child still gets its cooperative
/// 1.5s SIGTERM shutdown (marker written), never an instant SIGKILL.
/// Pre-#954 this failed twice over: the guard Drop's zero-wait SIGKILL AND
/// `kill_on_drop(true)`.
#[tokio::test]
async fn cold_start_deadline_miss_reaps_child_gracefully() {
    let _guard = ENV_LOCK.lock().await;
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("deadline-term-marker");
    unsafe {
        std::env::set_var("FAKE_CODEX_BIND_DELAY_MS", "60000");
        std::env::set_var("FAKE_CODEX_SIGTERM_MARKER", &marker);
        std::env::set_var("FAKE_CODEX_SIGTERM_EXIT_DELAY_MS", "1500");
    }
    let _bind_env = EnvGuard("FAKE_CODEX_BIND_DELAY_MS");
    let _marker_env = EnvGuard("FAKE_CODEX_SIGTERM_MARKER");
    let _delay_env = EnvGuard("FAKE_CODEX_SIGTERM_EXIT_DELAY_MS");

    let repo = repo().await;
    let mut cfg = cfg(&root);
    cfg.shared_codex_appserver_start_timeout_secs = 1;
    // Keep the armed heal loop quiet for the assertion window.
    cfg.shared_codex_appserver_restart_initial_delay_ms = 60_000;
    cfg.shared_codex_appserver_restart_max_delay_ms = 120_000;
    let home = calm_server::shared_codex_home::SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
        cfg.data_dir_resolved().join("codex-homes"),
    );
    home.seed().unwrap();
    let daemon = SharedCodexAppServer::new(&cfg, Arc::new(home), repo.clone());

    daemon
        .start_or_takeover()
        .await
        .expect_err("a never-binding child must fail at the configured deadline");

    // The graceful reap is awaited INSIDE the transition, so the marker must
    // already exist when the error surfaces.
    assert!(
        marker.exists(),
        "the deadline-missed child must get its cooperative 1.5s SIGTERM \
         shutdown (marker written) — never the pre-#954 zero-wait SIGKILL"
    );
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        SharedDaemonState::from_db_str(&record.state),
        SharedDaemonState::Failed,
        "the deadline failure must still terminalize the row"
    );
}

/// #954 design test 9(a) — the leak-audit contract: dropping every
/// supervisor Arc must leave the daemon RUNNING (no `impl Drop` SIGTERM, no
/// tokio `kill_on_drop` SIGKILL) — shutdown deliberately leaves the daemon
/// for the next boot's takeover, which this test then performs. Pre-#954
/// the Child drop SIGKILLed the daemon.
#[tokio::test]
async fn dropped_supervisor_leaves_daemon_running_for_next_boot_takeover() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    let pid = daemon.status_snapshot().runtime.expect("running").pid;

    drop(daemon);
    // Give any (buggy) drop-path signal time to land.
    tokio::time::sleep(Duration::from_millis(500)).await;
    // SAFETY: signal 0 probes liveness without delivering a signal.
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        0,
        "dropping the supervisor must leave the daemon running \
         (Drop impl deleted, kill_on_drop removed) — it is the next boot's \
         takeover target"
    );

    // Next boot takes over the surviving daemon (same pid, no respawn).
    let next_boot = server(&root, repo.clone()).await;
    next_boot.start_or_takeover().await.unwrap();
    let snapshot = next_boot.status_snapshot();
    assert_eq!(snapshot.state, SharedDaemonState::Running);
    assert_eq!(
        snapshot.runtime.as_ref().map(|runtime| runtime.pid),
        Some(pid),
        "the next boot must adopt the surviving daemon, not respawn"
    );

    // Explicit teardown (the whole point: nothing implicit reaps it).
    // SAFETY: SIGKILL to the fake daemon group this test spawned.
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
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
            CardRole::Worker,
            None,
            SharedThreadStartParams {
                cwd: "/tmp".into(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: None,
                config: ThreadConfig::NoMcp,
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
            CardRole::Worker,
            None,
            SharedThreadStartParams {
                cwd: "/tmp".into(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: None,
                config: ThreadConfig::NoMcp,
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
            CardRole::Worker,
            None,
            SharedThreadStartParams {
                cwd: "/tmp".into(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: None,
                config: ThreadConfig::NoMcp,
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

// ================= #949 cold-start deadline & child-liveness wait =================

/// `/proc/<pid>` liveness for reap assertions: gone entirely, or already
/// reaped into a zombie (state `Z`) — either way the process stopped working.
fn pid_gone_or_zombie(pid: i32) -> bool {
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(_) => return true,
    };
    // The state field is the first token after the `(comm)` field.
    stat.rsplit(") ")
        .next()
        .and_then(|rest| rest.chars().next())
        .map(|state| state == 'Z')
        .unwrap_or(false)
}

/// #949 acceptance (1)+(5) — repro of the production cold-start livelock:
/// codex may spend minutes rebuilding its state db (48.7s measured in
/// production) before it binds the listen socket at all. The fake delays
/// socket BIND past the old hardcoded 10s deadline; the start must still
/// succeed (default deadline 120s) and the child must NOT be reaped.
///
/// RED before #949: `poll_connect_initialized` gave up at a hardcoded 10s
/// and `SpawnedChildGuard` killed the legitimately-backfilling child.
#[tokio::test]
async fn cold_start_survives_socket_bind_slower_than_ten_seconds() {
    let _guard = ENV_LOCK.lock().await;
    unsafe {
        std::env::set_var("FAKE_CODEX_BIND_DELAY_MS", "11000");
    }
    let _delay = EnvGuard("FAKE_CODEX_BIND_DELAY_MS");

    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let daemon = server(&root, repo.clone()).await;
    let started = std::time::Instant::now();
    daemon
        .start_or_takeover()
        .await
        .expect("start must survive a >10s socket bind delay within the default 120s deadline");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs(10),
        "bind delay must actually outlive the old hardcoded 10s deadline (took {elapsed:?})"
    );

    let snapshot = daemon.status_snapshot();
    assert!(
        matches!(snapshot.state, SharedDaemonState::Running),
        "delayed-bind start must land Running, got {:?}",
        snapshot.state
    );
    let pid = snapshot.runtime.expect("running runtime has pid").pid;
    assert!(
        !pid_gone_or_zombie(pid),
        "child must not be reaped after a successful delayed start"
    );
}

/// #949 acceptance (2) — the child exits before the socket appears: the
/// cold-start poll must fail fast (well under the deadline) and surface the
/// child's exit status instead of blindly waiting out the timer.
///
/// RED before #949: the poll ignored child liveness and burned the full
/// deadline, then reported only the socket connect error.
#[tokio::test]
async fn cold_start_fails_fast_with_exit_status_when_child_dies_before_bind() {
    let _guard = ENV_LOCK.lock().await;
    unsafe {
        std::env::set_var("FAKE_CODEX_EXIT_BEFORE_BIND_CODE", "7");
    }
    let _code = EnvGuard("FAKE_CODEX_EXIT_BEFORE_BIND_CODE");

    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let daemon = server(&root, repo.clone()).await;
    let started = std::time::Instant::now();
    let err = daemon
        .start_or_takeover()
        .await
        .expect_err("start must fail when the child exits before binding");
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "child exit must fail the start fast, not wait out the deadline (took {elapsed:?})"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("exit status: 7"),
        "start error must include the child's exit status: {msg}"
    );
}

/// #949 acceptance (3) — the child stays alive but never binds: the
/// configured deadline (2s here) is the only kill switch. The start must
/// fail shortly after that deadline — not the old hardcoded 10s — and the
/// spawn guard must reap the child.
#[tokio::test]
async fn cold_start_reaps_never_binding_child_at_configured_deadline() {
    let _guard = ENV_LOCK.lock().await;
    unsafe {
        std::env::set_var("FAKE_CODEX_BIND_DELAY_MS", "8000");
    }
    let _delay = EnvGuard("FAKE_CODEX_BIND_DELAY_MS");

    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let mut cfg = cfg(&root);
    cfg.shared_codex_appserver_start_timeout_secs = 2;
    // #953 — keep the armed heal loop quiet for the assertion window so it
    // does not respawn (and re-persist `starting`) mid-test.
    cfg.shared_codex_appserver_restart_initial_delay_ms = 60_000;
    cfg.shared_codex_appserver_restart_max_delay_ms = 120_000;
    let home = calm_server::shared_codex_home::SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
        cfg.data_dir_resolved().join("codex-homes"),
    );
    home.seed().unwrap();
    let daemon = SharedCodexAppServer::new(&cfg, Arc::new(home), repo.clone());

    let started = std::time::Instant::now();
    let start_task = tokio::spawn({
        let daemon = daemon.clone();
        async move { daemon.start_or_takeover().await }
    });
    // #953 — the failed spawn persists `failed` with identity NULLed, so the
    // spawned pid must be captured from the transient `starting` row while
    // the spawn is still in flight.
    let pid = wait_for_starting_pid(&repo).await;
    let err = start_task
        .await
        .unwrap()
        .expect_err("a never-binding child must fail at the configured deadline");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs(2),
        "deadline must be honored, not undercut (took {elapsed:?})"
    );
    assert!(
        elapsed < Duration::from_secs(7),
        "the CONFIGURED 2s deadline must apply, not the old hardcoded 10s (took {elapsed:?})"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("deadline 2s"),
        "timeout error must report the configured deadline: {msg}"
    );

    // The spawn guard reaps the still-alive child on failure.
    let mut reaped = false;
    for _ in 0..60 {
        if pid_gone_or_zombie(pid) {
            reaped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        reaped,
        "never-binding child must be reaped after the deadline"
    );
    // #953 defect 1 — and the row must reflect the failure, not a stranded
    // `starting`.
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        SharedDaemonState::from_db_str(&record.state),
        SharedDaemonState::Failed,
        "deadline failure must persist state=failed; got {}",
        record.state
    );
    assert_eq!(record.pid, None, "proven-absent failure must NULL pid");
}

/// Poll for the transient `starting` row's pid while a spawn is in flight.
async fn wait_for_starting_pid(repo: &SqlxRepo) -> i32 {
    for _ in 0..200 {
        let record = repo.shared_daemon_runtime_get().await.unwrap();
        if SharedDaemonState::from_db_str(&record.state) == SharedDaemonState::Starting
            && let Some(pid) = record.pid
        {
            return pid;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("spawn never persisted a starting row with a pid");
}

/// #949 review-fix — the deadline is a TOTAL cap even across an in-flight
/// attempt: the child binds the socket immediately but never answers
/// `initialize` (delay >> deadline). Without `timeout_at` around the
/// attempt, the attempt's internal 10s request timeout would stretch the
/// configured 2s deadline to ~10s. The start must fail at ~2s and the spawn
/// guard must reap the child.
///
/// RED before the fix: elapsed was ~10s (attempt-internal request timeout),
/// violating the knob's documented total-time semantics.
#[tokio::test]
async fn cold_start_deadline_caps_in_flight_hanging_initialize() {
    let _guard = ENV_LOCK.lock().await;
    unsafe {
        // Socket binds immediately; only the initialize RESPONSE hangs, far
        // past both the 2s deadline and the attempt's 10s request timeout.
        std::env::set_var("FAKE_CODEX_INITIALIZE_DELAY_MS", "60000");
    }
    let _delay = EnvGuard("FAKE_CODEX_INITIALIZE_DELAY_MS");

    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let mut cfg = cfg(&root);
    cfg.shared_codex_appserver_start_timeout_secs = 2;
    // #953 — keep the armed heal loop quiet for the assertion window.
    cfg.shared_codex_appserver_restart_initial_delay_ms = 60_000;
    cfg.shared_codex_appserver_restart_max_delay_ms = 120_000;
    let home = calm_server::shared_codex_home::SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
        cfg.data_dir_resolved().join("codex-homes"),
    );
    home.seed().unwrap();
    let daemon = SharedCodexAppServer::new(&cfg, Arc::new(home), repo.clone());

    let started = std::time::Instant::now();
    let start_task = tokio::spawn({
        let daemon = daemon.clone();
        async move { daemon.start_or_takeover().await }
    });
    // #953 — capture the spawned pid from the transient `starting` row (the
    // failure now persists `failed` with identity NULLed).
    let pid = wait_for_starting_pid(&repo).await;
    let err = start_task
        .await
        .unwrap()
        .expect_err("a hanging initialize must fail at the configured deadline");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs(2),
        "deadline must be honored, not undercut (took {elapsed:?})"
    );
    assert!(
        elapsed < Duration::from_secs(7),
        "the 2s TOTAL deadline must cap the in-flight attempt, not the \
         attempt's internal 10s request timeout (took {elapsed:?})"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("deadline 2s"),
        "timeout error must report the configured deadline: {msg}"
    );

    // The spawn guard reaps the still-hanging child on failure.
    let mut reaped = false;
    for _ in 0..60 {
        if pid_gone_or_zombie(pid) {
            reaped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        reaped,
        "a child hanging on initialize must be reaped after the deadline"
    );
}

/// #949 acceptance (4) — the cold-start deadline knob: 120s default, and
/// flag + env overrides, mirroring the sibling restart-delay knobs.
#[tokio::test]
async fn cold_start_deadline_config_default_flag_and_env_override() {
    let _guard = ENV_LOCK.lock().await;
    // Ambient env from the invoking shell could already carry the override;
    // clear it so the default assertion is hermetic.
    unsafe {
        std::env::remove_var("CALM_SHARED_CODEX_APPSERVER_START_TIMEOUT_SECS");
    }
    assert_eq!(
        Config::parse_from(["calm-server"]).shared_codex_appserver_start_timeout_secs,
        120,
        "default must be 120s (#949: covers the measured 48.7s backfill with headroom)"
    );
    assert_eq!(
        Config::parse_from([
            "calm-server",
            "--shared-codex-appserver-start-timeout-secs",
            "7"
        ])
        .shared_codex_appserver_start_timeout_secs,
        7
    );
    unsafe {
        std::env::set_var("CALM_SHARED_CODEX_APPSERVER_START_TIMEOUT_SECS", "33");
    }
    let _env = EnvGuard("CALM_SHARED_CODEX_APPSERVER_START_TIMEOUT_SECS");
    assert_eq!(
        Config::parse_from(["calm-server"]).shared_codex_appserver_start_timeout_secs,
        33,
        "env override must win over the default"
    );
}

// ================= #953 supervisor self-heal =================

/// Build a daemon whose codex bin is a removable symlink, so tests can break
/// and repair the spawn path deterministically.
fn daemon_with_codex_symlink(
    root: &tempfile::TempDir,
    repo: Arc<SqlxRepo>,
) -> (Arc<SharedCodexAppServer>, std::path::PathBuf) {
    daemon_with_codex_symlink_cfg(root, repo, cfg(root))
}

fn daemon_with_codex_symlink_cfg(
    root: &tempfile::TempDir,
    repo: Arc<SqlxRepo>,
    mut cfg: Config,
) -> (Arc<SharedCodexAppServer>, std::path::PathBuf) {
    let bin_dir = root.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let codex_link = bin_dir.join("codex");
    std::os::unix::fs::symlink(fake_codex_bin(), &codex_link).unwrap();

    cfg.codex_bin = codex_link.display().to_string();
    let home = calm_server::shared_codex_home::SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
        cfg.data_dir_resolved().join("codex-homes"),
    );
    home.seed().unwrap();
    (
        SharedCodexAppServer::new(&cfg, Arc::new(home), repo),
        codex_link,
    )
}

async fn wait_for_state(daemon: &SharedCodexAppServer, want: SharedDaemonState, secs: u64) -> bool {
    tokio::time::timeout(Duration::from_secs(secs), async {
        loop {
            if daemon.status_snapshot().state == want {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .is_ok()
}

/// #953 defect 1 — failing-first repro (i), design test 1: a spawn failure
/// AFTER `persist_runtime_starting` (here: the child answers `initialize`
/// with an error until the 1s cold-start deadline) must persist a `failed`
/// row with the identity tuple NULLed and last_error set. It must never
/// strand the DB at `state='starting'` with a dead pid (production: 16 days).
#[tokio::test]
async fn spawn_failure_after_persist_starting_persists_failed_row() {
    let _guard = ENV_LOCK.lock().await;
    unsafe {
        std::env::set_var("FAKE_CODEX_FAIL_INITIALIZE", "1");
    }
    let _env = EnvGuard("FAKE_CODEX_FAIL_INITIALIZE");

    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let mut cfg = cfg(&root);
    cfg.shared_codex_appserver_start_timeout_secs = 1;
    // Keep the armed heal loop quiet for the assertion window: its retry
    // would legitimately re-persist `starting` while respawning.
    cfg.shared_codex_appserver_restart_initial_delay_ms = 60_000;
    cfg.shared_codex_appserver_restart_max_delay_ms = 120_000;
    let home = calm_server::shared_codex_home::SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
        cfg.data_dir_resolved().join("codex-homes"),
    );
    home.seed().unwrap();
    let daemon = SharedCodexAppServer::new(&cfg, Arc::new(home), repo.clone());

    daemon
        .start_or_takeover()
        .await
        .expect_err("failing initialize must fail the start at the deadline");
    assert!(
        daemon.heal_active_for_test(),
        "spawn failure must arm the background heal loop"
    );

    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        SharedDaemonState::from_db_str(&record.state),
        SharedDaemonState::Failed,
        "post-persist_runtime_starting spawn failure must persist state=failed, \
         never strand 'starting'; got state={}",
        record.state
    );
    assert_eq!(record.pid, None, "proven-absent failure must NULL pid");
    assert_eq!(record.pgid, None, "proven-absent failure must NULL pgid");
    assert_eq!(record.process_start_time, None);
    assert_eq!(record.boot_id, None);
    assert!(
        record.last_error.is_some(),
        "failed row must carry last_error"
    );
}

/// #953 defect 3 — failing-first repro (ii), design tests 2+4: once the
/// daemon is Failed (missing codex bin at boot), repairing the cause and
/// firing the settings-change nudge must bring the daemon back to Running
/// WITHOUT any manual respawn call and without restarting calm-server.
///
/// #953 review D5 — the heal delays are pinned FAR beyond the 15s wait
/// (60s/120s; the Persistent slow lane floors at the 120s max), so a pass
/// can only come from the nudge's immediate wake, never from a polling
/// round happening to land inside the window. This pins the design's
/// settings-nudge immediate-wake claim while keeping the test bounded.
#[tokio::test]
async fn failed_daemon_heals_in_background_without_server_restart() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let mut quiet_cfg = cfg(&root);
    quiet_cfg.shared_codex_appserver_restart_initial_delay_ms = 60_000;
    quiet_cfg.shared_codex_appserver_restart_max_delay_ms = 120_000;
    let (daemon, codex_link) = daemon_with_codex_symlink_cfg(&root, repo.clone(), quiet_cfg);

    std::fs::remove_file(&codex_link).unwrap();
    daemon
        .start_or_takeover()
        .await
        .expect_err("boot spawn must fail with a missing codex bin");
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        SharedDaemonState::from_db_str(&record.state),
        SharedDaemonState::Failed,
        "boot spawn failure must leave an accurate failed row; got state={}",
        record.state
    );

    // #953 defect 3/design test 4 — while Failed, the preflight message must
    // carry the live failure and the background-retry fact.
    assert!(
        !daemon.is_running(),
        "failed daemon must preflight as not running"
    );
    let message = daemon.not_running_message();
    assert!(
        message.contains("last error:") && message.contains("retry"),
        "not_running_message must carry last_error and the retry hint; got: {message}"
    );

    // Repair the cause, then fire ONLY the existing settings-change nudge.
    std::os::unix::fs::symlink(fake_codex_bin(), &codex_link).unwrap();
    daemon.mark_needs_respawn();

    assert!(
        wait_for_state(&daemon, SharedDaemonState::Running, 15).await,
        "background heal loop must revive a Failed daemon without a manual \
         respawn call or a calm-server restart; still {:?}",
        daemon.status_snapshot().state
    );
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        SharedDaemonState::from_db_str(&record.state),
        SharedDaemonState::Running,
        "heal success must persist the running row"
    );
    assert!(record.pid.is_some());
    // Lockout reversed: the user-path preflight passes again.
    assert!(
        daemon.is_running(),
        "preflighted user path must pass after the background heal"
    );
}

/// #953 PR2 §5 — the readiness watch is stamped `running: false` on a
/// terminal Failed (typestate error arm) and `running: true` with the
/// installed generation on every installed Running (typestate success arm).
/// This is the channel the deferred harness recovery consumes.
#[tokio::test]
async fn readiness_watch_tracks_failed_and_running_transitions() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let mut quiet_cfg = cfg(&root);
    quiet_cfg.shared_codex_appserver_restart_initial_delay_ms = 60_000;
    quiet_cfg.shared_codex_appserver_restart_max_delay_ms = 120_000;
    let (daemon, codex_link) = daemon_with_codex_symlink_cfg(&root, repo.clone(), quiet_cfg);
    let mut readiness = daemon.readiness_receiver();
    assert!(
        !readiness.borrow().running,
        "readiness starts not-running before any spawn"
    );

    std::fs::remove_file(&codex_link).unwrap();
    daemon
        .start_or_takeover()
        .await
        .expect_err("boot spawn must fail with a missing codex bin");
    let after_failure = *readiness.borrow_and_update();
    assert!(
        !after_failure.running,
        "terminal Failed must stamp running: false"
    );

    // Repair + the existing settings-change nudge: the heal loop's success
    // must stamp running: true with the installed generation.
    std::os::unix::fs::symlink(fake_codex_bin(), &codex_link).unwrap();
    daemon.mark_needs_respawn();
    let ready = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let current = *readiness.borrow_and_update();
            if current.running {
                break current;
            }
            readiness
                .changed()
                .await
                .expect("supervisor must outlive the wait");
        }
    })
    .await
    .expect("readiness must flip to running after the background heal");
    assert_eq!(
        ready.generation,
        daemon.generation_for_test().await,
        "readiness generation must be the installed Running incarnation's"
    );
    assert!(daemon.is_running());
}

/// #953 PR2 review D1(b) — entering a transition (leaving Running) must
/// invalidate readiness IMMEDIATELY: `transition_replace` publishes
/// `running: false` with the OUTGOING generation at transition entry, so a
/// claim-boundary consumer (deferred harness recovery) never accepts a
/// transitional Restarting/Starting daemon whose last terminal value was
/// still `running: true`. The Ok arm then re-publishes the terminal
/// `running: true` with the newly installed generation — no premature
/// `running: true` in between (the parked window only ever shows `false`).
#[tokio::test]
async fn transition_entry_invalidates_readiness_before_terminal_republish() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();

    let mut readiness = daemon.readiness_receiver();
    let before = *readiness.borrow_and_update();
    assert!(before.running, "daemon starts Running");
    let outgoing_generation = before.generation;

    // Park the transition inside the entry window (state left Running,
    // reap/start body not yet run).
    let gate = daemon.transition_entry_gate_for_test();
    let parked = gate.lock().await;
    let task = tokio::spawn({
        let daemon = daemon.clone();
        async move {
            daemon
                .transition_replace_for_test("settings changed", ReplacePrecondition::Always)
                .await
        }
    });

    // The ONLY publish that can wake this receiver while the transition is
    // parked is the entry-time invalidation.
    tokio::time::timeout(Duration::from_secs(5), readiness.changed())
        .await
        .expect("transition entry must publish a readiness invalidation")
        .unwrap();
    let mid = *readiness.borrow_and_update();
    assert!(
        !mid.running,
        "transition entry must invalidate readiness (running: false)"
    );
    assert_eq!(
        mid.generation, outgoing_generation,
        "the entry invalidation must carry the OUTGOING generation"
    );

    // Release: the transition completes and re-publishes the terminal value.
    drop(parked);
    let outcome = task.await.unwrap().unwrap();
    assert_eq!(outcome, ReplaceOutcome::Replaced);
    let after = *readiness.borrow_and_update();
    assert!(
        after.running,
        "the Ok arm must re-publish the terminal running: true"
    );
    assert_ne!(
        after.generation, outgoing_generation,
        "the terminal value must carry the newly installed generation"
    );
    assert_eq!(after.generation, daemon.generation_for_test().await);
}

/// Rewrite the polluted home's config.toml without the `evil` entry —
/// "polluted-then-repaired" (#953 design test 6).
fn repair_polluted_home(root: &tempfile::TempDir) {
    let cfg_path = root.path().join("codex-home/config.toml");
    let content = std::fs::read_to_string(&cfg_path).unwrap();
    let repaired = content.replace("[mcp_servers.evil]\ncommand = \"/usr/bin/evil-mcp\"\n", "");
    assert_ne!(content, repaired, "pollution must have been present");
    std::fs::write(&cfg_path, repaired).unwrap();
}

/// #953 design test 5 — the double-spawn race of the old split path,
/// demonstrated post-hoc via the precondition that replaced it (the split
/// path is deleted, so the race itself is no longer constructible): a
/// crash restart carrying a stale generation — its process was already
/// replaced by a settings respawn — must abort silently: no reap, no spawn,
/// a single live pid, consistent restart_count.
#[tokio::test]
async fn stale_generation_crash_restart_aborts_without_reap_or_spawn() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();
    let stale_generation = daemon.generation_for_test().await;

    // A settings respawn replaces the process: the generation moves on.
    daemon.mark_needs_respawn();
    daemon.ensure_respawn_for_current_settings().await.unwrap();
    let after_respawn = daemon.status_snapshot();
    assert_eq!(after_respawn.restart_count, 1);
    let replaced_pid = after_respawn.runtime.as_ref().unwrap().pid;
    assert_ne!(
        daemon.generation_for_test().await,
        stale_generation,
        "installing a new Running incarnation must bump the generation"
    );

    let outcome = daemon
        .transition_replace_for_test(
            "stale crash restart",
            ReplacePrecondition::GenerationIs(stale_generation),
        )
        .await
        .unwrap();
    assert_eq!(
        outcome,
        ReplaceOutcome::PreconditionFailed,
        "a stale-generation restart must abort at the precondition"
    );
    let after = daemon.status_snapshot();
    assert_eq!(after.state, SharedDaemonState::Running);
    assert_eq!(
        after.restart_count, 1,
        "the aborted restart must not bump restart_count"
    );
    assert_eq!(
        after.runtime.as_ref().map(|runtime| runtime.pid),
        Some(replaced_pid),
        "the aborted restart must not replace the live daemon"
    );
    assert!(
        !pid_gone_or_zombie(replaced_pid),
        "the aborted restart must not reap the live daemon"
    );
}

/// #953 review D2 — the generation bumps ONLY when a Running incarnation is
/// installed, so a crash restart made stale by an intervening FAILED
/// transition (here: a settings respawn that reaps the crashed process and
/// then fails to spawn) still sees its captured generation. The
/// `GenerationIs` precondition must additionally require the state to still
/// be the Running incarnation the crash watcher observed; against the
/// intervening Failed state the stale task must abort — never retry on the
/// crash lane, override the settings-failure classification, double-count
/// restarts, or consume the restored `needs_respawn` flag.
#[tokio::test]
async fn stale_crash_restart_after_failed_settings_respawn_aborts() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    // Quiet heal delays: the armed loop must not race the assertions below.
    let mut quiet_cfg = cfg(&root);
    quiet_cfg.shared_codex_appserver_restart_initial_delay_ms = 60_000;
    quiet_cfg.shared_codex_appserver_restart_max_delay_ms = 120_000;
    let (daemon, codex_link) = daemon_with_codex_symlink_cfg(&root, repo.clone(), quiet_cfg);
    daemon.start_or_takeover().await.unwrap();
    let stale_generation = daemon.generation_for_test().await;

    // Intervening settings respawn that FAILS: the bin disappears first.
    std::fs::remove_file(&codex_link).unwrap();
    daemon.mark_needs_respawn();
    daemon
        .ensure_respawn_for_current_settings()
        .await
        .expect_err("the settings respawn must fail with the bin gone");
    // #954 — the detached transition releases the serial before the caller
    // resumes, so the heal loop (nudged by mark_needs_respawn's stored
    // permit) may already be running its own — also failing — round when
    // the caller observes state. Wait for the terminal settle instead of
    // asserting the instantaneous state (the heal round consumes the nudge
    // permit and then sleeps on the quiet 120s slow lane, so the state is
    // stable afterwards).
    assert!(
        wait_for_state(&daemon, SharedDaemonState::Failed, 10).await,
        "the failed settings respawn must settle terminal Failed; still {:?}",
        daemon.status_snapshot().state
    );
    let after_failed_respawn = daemon.status_snapshot();
    assert_eq!(after_failed_respawn.state, SharedDaemonState::Failed);
    assert_eq!(
        daemon.generation_for_test().await,
        stale_generation,
        "a FAILED transition leaves the generation unchanged — equality \
         alone cannot prove the crashed incarnation is still installed"
    );
    let failed_error = after_failed_respawn
        .last_error
        .expect("failed settings respawn must record last_error");
    assert!(
        daemon.needs_respawn_on_next_thread_start_for_test(),
        "the failed respawn must restore the needs_respawn flag"
    );

    // The stale crash task (captured generation) now acquires the serial:
    // it must abort at the precondition, not retry on the crash lane.
    let outcome = daemon
        .transition_replace_for_test(
            "stale crash restart",
            ReplacePrecondition::GenerationIs(stale_generation),
        )
        .await
        .expect("the stale crash restart must abort cleanly, not respawn-fail");
    assert_eq!(
        outcome,
        ReplaceOutcome::PreconditionFailed,
        "an intervening failed transition must invalidate the crash task"
    );
    let after = daemon.status_snapshot();
    assert_eq!(
        after.state,
        SharedDaemonState::Failed,
        "the aborted stale task must leave the Failed state untouched"
    );
    assert_eq!(
        after.last_error.as_deref(),
        Some(failed_error.as_str()),
        "the settings-failure classification must not be overridden by the \
         stale crash lane"
    );
    assert_eq!(
        after.restart_count, after_failed_respawn.restart_count,
        "the aborted stale task must not double-count restarts"
    );
    assert!(
        daemon.needs_respawn_on_next_thread_start_for_test(),
        "the aborted stale task must not race away the restored flag"
    );
}

/// #953 design test 6 — fence-unreconciled: a corrupt pgid≠pid record plus a
/// polluted-then-repaired home. While the survivor lives, no round may
/// spawn (row keeps identity + `unreconciled:` prefix, survivor untouched);
/// once the survivor is gone, reconciliation proves absence, NULLs identity,
/// and the spawn proceeds.
#[tokio::test]
async fn unreconciled_record_fences_spawn_until_survivor_proven_gone() {
    let root = tempfile::tempdir().unwrap();
    let sock = root.path().join("run/codex-appserver.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();

    let (launcher, pgid, peer_pid) = spawn_launcher_with_fake_appserver(&sock, false, false).await;
    let process_start_time = read_proc_start_time(peer_pid).expect("fake app-server start time");

    let repo = repo().await;
    persist_running_daemon(&repo, &root, peer_pid, pgid, &sock, process_start_time).await;

    let daemon =
        SharedCodexAppServer::new(&cfg(&root), Arc::new(polluted_home(&root)), repo.clone());
    daemon
        .start_or_takeover()
        .await
        .expect_err("boot guard must refuse the polluted CODEX_HOME");

    repair_polluted_home(&root);

    // The home is clean now, but the unreconciled row still fences the spawn.
    let err = daemon
        .ensure_running()
        .await
        .expect_err("the unreconciled row must fence the spawn while the survivor lives");
    assert!(
        err.to_string().contains("unreconciled"),
        "fence error must say unreconciled; got: {err}"
    );
    // SAFETY: signal 0 probes liveness without delivering a signal.
    assert_eq!(
        unsafe { libc::kill(peer_pid, 0) },
        0,
        "the survivor must never be signaled while unreconciled"
    );
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        record.pid,
        Some(peer_pid),
        "the fenced row must retain the identity tuple"
    );
    assert!(
        record
            .last_error
            .as_deref()
            .unwrap_or_default()
            .starts_with("unreconciled: "),
        "fenced row must carry the durable prefix; got {:?}",
        record.last_error
    );
    assert_ne!(
        daemon.status_snapshot().state,
        SharedDaemonState::Running,
        "no spawn may happen while unreconciled"
    );

    // Survivor killed ⇒ absence provable ⇒ identity NULLed ⇒ spawn proceeds.
    force_cleanup_process_group(launcher, pgid);
    assert!(
        wait_proc_gone(peer_pid).await,
        "test survivor must terminate"
    );
    daemon
        .ensure_running()
        .await
        .expect("proven absence must reopen the spawn path");
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        SharedDaemonState::from_db_str(&record.state),
        SharedDaemonState::Running
    );
    assert_ne!(record.pid, Some(peer_pid), "a fresh daemon must be spawned");
    assert_eq!(daemon.status_snapshot().state, SharedDaemonState::Running);
}

/// #953 design test 11 — the unreconciled marker is durable: a NEW
/// supervisor instance over the same repo (calm-server restart) classifies
/// Unreconciled from the row alone (NOT SafeToRetry), spawns nothing while
/// the survivor lives, and recovers once the survivor is gone.
#[tokio::test]
async fn unreconciled_classification_survives_supervisor_restart() {
    let root = tempfile::tempdir().unwrap();
    let sock = root.path().join("run/codex-appserver.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();

    let (launcher, pgid, peer_pid) = spawn_launcher_with_fake_appserver(&sock, false, false).await;
    let process_start_time = read_proc_start_time(peer_pid).expect("fake app-server start time");

    let repo = repo().await;
    persist_running_daemon(&repo, &root, peer_pid, pgid, &sock, process_start_time).await;

    // Instance A refuses (polluted home) and persists the durable marker.
    // Its heal loop is quieted (long delays) so it cannot race instance B.
    let mut cfg_a = cfg(&root);
    cfg_a.shared_codex_appserver_restart_initial_delay_ms = 60_000;
    cfg_a.shared_codex_appserver_restart_max_delay_ms = 120_000;
    let instance_a =
        SharedCodexAppServer::new(&cfg_a, Arc::new(polluted_home(&root)), repo.clone());
    instance_a
        .start_or_takeover()
        .await
        .expect_err("boot guard must refuse the polluted CODEX_HOME");
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(record.pid, Some(peer_pid), "marker must retain identity");
    drop(instance_a);

    // The pollution is repaired; a NEW instance (fresh in-memory state) must
    // still classify the row Unreconciled and refuse to spawn.
    repair_polluted_home(&root);
    let home_b = calm_server::shared_codex_home::SharedCodexHome::new(
        root.path().join("codex-home"),
        root.path().join("codex-homes"),
    );
    let instance_b = SharedCodexAppServer::new(&cfg(&root), Arc::new(home_b), repo.clone());
    let err = instance_b
        .start_or_takeover()
        .await
        .expect_err("a new instance must classify the row Unreconciled, not SafeToRetry");
    assert!(
        err.to_string().contains("unreconciled"),
        "restart-surviving fence must say unreconciled; got: {err}"
    );
    // SAFETY: signal 0 probes liveness without delivering a signal.
    assert_eq!(
        unsafe { libc::kill(peer_pid, 0) },
        0,
        "the survivor must still be alive and unsignaled"
    );
    assert_ne!(
        instance_b.status_snapshot().state,
        SharedDaemonState::Running
    );

    // Kill the survivor ⇒ the next round proves absence, NULLs identity,
    // and spawns.
    force_cleanup_process_group(launcher, pgid);
    assert!(wait_proc_gone(peer_pid).await);
    instance_b
        .ensure_running()
        .await
        .expect("proven absence must reopen the spawn path after the restart");
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        SharedDaemonState::from_db_str(&record.state),
        SharedDaemonState::Running
    );
    assert_ne!(record.pid, Some(peer_pid));
}

/// #953 design test 7 (integration half) — identical consecutive failures
/// produce exactly one Failed DB write: `updated_at` stays put across
/// repeated failing rounds (the module unit test covers the forced-DB-
/// failure half of the dedup contract).
#[tokio::test]
async fn identical_consecutive_failures_dedup_to_one_db_write() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let (daemon, codex_link) = daemon_with_codex_symlink(&root, repo.clone());
    std::fs::remove_file(&codex_link).unwrap();

    daemon
        .start_or_takeover()
        .await
        .expect_err("missing codex bin must fail the start");
    let first = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        SharedDaemonState::from_db_str(&first.state),
        SharedDaemonState::Failed
    );

    // Give a differing updated_at time to become visible, then fail twice
    // more with the identical tuple.
    tokio::time::sleep(Duration::from_millis(50)).await;
    daemon
        .ensure_running()
        .await
        .expect_err("bin still missing");
    daemon
        .ensure_running()
        .await
        .expect_err("bin still missing");
    let second = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(second.last_error, first.last_error);
    assert_eq!(
        second.updated_at, first.updated_at,
        "identical consecutive Failed tuples must not be rewritten"
    );
}

/// #953 design test 9 — heal-task abort: the RAII guard clears the
/// singleton claim on abort, and re-scheduling can claim again.
#[tokio::test]
async fn heal_task_abort_clears_claim_via_raii_guard() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let (daemon, codex_link) = daemon_with_codex_symlink(&root, repo.clone());
    // Rounds will keep failing, so the loop stays alive until aborted.
    std::fs::remove_file(&codex_link).unwrap();

    assert!(!daemon.heal_active_for_test());
    let handle = daemon
        .schedule_heal_for_test()
        .expect("first claim must succeed");
    assert!(daemon.heal_active_for_test());
    assert!(
        daemon.schedule_heal_for_test().is_none(),
        "the heal task is a singleton while active"
    );

    handle.abort();
    let mut cleared = false;
    for _ in 0..100 {
        if !daemon.heal_active_for_test() {
            cleared = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        cleared,
        "abort must clear heal_active via the RAII guard drop"
    );

    let handle2 = daemon
        .schedule_heal_for_test()
        .expect("the claim must be reclaimable after an abort");
    assert!(daemon.heal_active_for_test());
    handle2.abort();
}

/// #953 review D1 — heal-claim release race: a heal round succeeds
/// (Running installed, serial released) but the task still holds the
/// singleton `heal_active` claim through the post-transition window
/// (production: `resume_cached_threads`, which can take seconds — modelled
/// here by the fixtures post-Ok gate). If the fresh daemon crashes and its
/// crash restart FAILS inside that window, the failure path's
/// `schedule_heal()` CAS silently loses against the held claim. The heal
/// task must therefore release the claim FIRST and then re-check the
/// terminal state, re-arming on Failed — otherwise the end state is
/// Failed + heal_active=false + no loop: the permanent lockout this PR
/// exists to remove.
#[tokio::test]
async fn heal_ok_claim_release_race_rearms_loop_for_failure_that_lost_cas() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let (daemon, codex_link) = daemon_with_codex_symlink(&root, repo.clone());

    // Boot failure arms the heal loop (claim held by the loop task).
    std::fs::remove_file(&codex_link).unwrap();
    daemon
        .start_or_takeover()
        .await
        .expect_err("boot spawn must fail with a missing codex bin");
    assert!(daemon.heal_active_for_test(), "boot failure must arm heal");

    // Park the NEXT successful round after ensure_running() Ok but before
    // the claim release (the resume_cached_threads window).
    let hold = daemon.heal_post_ok_gate_for_test().lock_owned().await;

    // Repair the bin: the heal loop heals to Running, then parks at the
    // gate STILL HOLDING the claim.
    std::os::unix::fs::symlink(fake_codex_bin(), &codex_link).unwrap();
    assert!(
        wait_for_state(&daemon, SharedDaemonState::Running, 30).await,
        "heal round must install Running before parking at the gate"
    );

    // Inside the window: crash the fresh daemon AND re-break the bin so
    // the crash restart fails; its schedule_heal() CAS loses against the
    // claim the parked task still holds.
    std::fs::remove_file(&codex_link).unwrap();
    let crashed_pid = daemon
        .status_snapshot()
        .runtime
        .expect("running daemon must expose its runtime")
        .pid;
    // SAFETY: SIGKILL to the daemon child this test's supervisor spawned.
    unsafe {
        libc::kill(crashed_pid, libc::SIGKILL);
    }
    assert!(
        wait_for_state(&daemon, SharedDaemonState::Failed, 30).await,
        "the failed crash restart must terminalize to Failed"
    );
    assert!(
        daemon.heal_active_for_test(),
        "the parked heal task must still hold the singleton claim"
    );

    // Repair again, then release the gate. The finishing heal task must
    // release its claim FIRST and re-arm against the Failed state it
    // re-reads (the crash path's schedule_heal already lost its CAS). The
    // nudge only wakes the re-armed loop promptly — with no loop armed
    // (the pre-fix lockout) it wakes nothing.
    std::os::unix::fs::symlink(fake_codex_bin(), &codex_link).unwrap();
    drop(hold);
    daemon.mark_needs_respawn();
    assert!(
        wait_for_state(&daemon, SharedDaemonState::Running, 60).await,
        "the released heal task must re-arm the loop for the failure that \
         lost its CAS; still {:?} (heal_active={})",
        daemon.status_snapshot().state,
        daemon.heal_active_for_test()
    );
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        SharedDaemonState::from_db_str(&record.state),
        SharedDaemonState::Running,
        "the re-armed heal round must persist the running row"
    );
}

/// #953 design test 10 — takeover success re-stamps the row: adopting a
/// daemon persisted as `starting` flips the row to `running` with the
/// adopted tuple.
#[tokio::test]
async fn takeover_restamps_starting_row_to_running() {
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
        .expect("spawn fake app-server for starting-row takeover");
    let old_pid = i32::try_from(child.id().expect("fake app-server pid")).expect("pid fits i32");
    let process_start_time = wait_for_start_time_and_socket(old_pid, &sock).await;

    let repo = repo().await;
    // Persist the daemon as it would look mid-launch: state='starting' with
    // a full identity tuple and the current env signature.
    repo.shared_daemon_runtime_set(SharedCodexDaemonUpdate {
        state: "starting".into(),
        pid: Some(old_pid),
        pgid: Some(old_pid),
        sock_path: Some(sock.display().to_string()),
        codex_home_path: Some(root.path().join("codex-home").display().to_string()),
        process_start_time: Some(process_start_time),
        boot_id: Some(read_boot_id().unwrap_or_default()),
        started_at: Some(now_ms()),
        last_error: None,
        increment_restart_count: false,
        daemon_env_signature: Some(effective_test_env_signature(
            &cfg(&root).codex_ingest_url_resolved(),
        )),
    })
    .await
    .unwrap();

    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();

    let snapshot = daemon.status_snapshot();
    assert_eq!(snapshot.state, SharedDaemonState::Running);
    assert_eq!(
        snapshot.runtime.as_ref().map(|runtime| runtime.pid),
        Some(old_pid),
        "the live daemon must be adopted, not respawned"
    );
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        SharedDaemonState::from_db_str(&record.state),
        SharedDaemonState::Running,
        "takeover success must re-stamp the row to running"
    );
    assert_eq!(record.pid, Some(old_pid));

    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// Spawn a fake app-server child directly from the test with a delayed
/// socket bind (models mid-backfill: alive, verified, not yet bound) and a
/// SIGTERM marker oracle. Returns (child, pid, process_start_time) once the
/// fixture reports its SIGTERM handler armed — before that, a reap would
/// hit a default-disposition process and the marker oracle would be vacuous.
async fn spawn_unbound_child_with_term_marker(
    sock: &Path,
    bind_delay_ms: u64,
    marker: &Path,
    handler_ready: &Path,
) -> (tokio::process::Child, i32, u64) {
    let child = Command::new(fake_codex_bin())
        .arg("app-server")
        .arg("--listen")
        .arg(format!("unix://{}", sock.display()))
        .env("FAKE_CODEX_BIND_DELAY_MS", bind_delay_ms.to_string())
        .env("FAKE_CODEX_SIGTERM_MARKER", marker)
        .env("FAKE_CODEX_HANDLER_READY_MARKER", handler_ready)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .expect("spawn unbound fake app-server child");
    let pid = i32::try_from(child.id().expect("child pid")).expect("pid fits i32");
    let mut handler_armed = false;
    for _ in 0..500 {
        if handler_ready.exists() {
            handler_armed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        handler_armed,
        "fixture must report its SIGTERM handler armed before the boot runs"
    );
    let process_start_time = read_proc_start_time(pid).expect("child start time");
    (child, pid, process_start_time)
}

async fn persist_starting_daemon(
    repo: &SqlxRepo,
    root: &tempfile::TempDir,
    pid: i32,
    sock: &Path,
    process_start_time: u64,
    started_at: i64,
) {
    repo.shared_daemon_runtime_set(SharedCodexDaemonUpdate {
        state: "starting".into(),
        pid: Some(pid),
        pgid: Some(pid),
        sock_path: Some(sock.display().to_string()),
        codex_home_path: Some(root.path().join("codex-home").display().to_string()),
        process_start_time: Some(process_start_time),
        boot_id: Some(read_boot_id().unwrap_or_default()),
        started_at: Some(started_at),
        last_error: None,
        increment_restart_count: false,
        daemon_env_signature: Some(effective_test_env_signature(
            &cfg(root).codex_ingest_url_resolved(),
        )),
    })
    .await
    .unwrap();
}

/// #954 defect 2, design test 10 (failing-first) — a persisted `starting`
/// row naming a VERIFIED alive child that has not yet bound its socket
/// (mid-backfill) must be given the remaining readiness window budgeted by
/// the persisted `started_at`, and ADOPTED when it binds: same pid, never
/// signaled (no marker), restart_count unchanged, backfill progress
/// preserved. Pre-#954-PR2 the takeover probe made a single
/// `connect_initialized` attempt that failed instantly on the unbound
/// socket → handshake-failure reap → a fresh spawn restarting backfill from
/// scratch on every boot.
///
/// #953 interaction: the adoption funnels through the SAME
/// `start_new_process_typestate` seam as a fresh spawn, so it must publish
/// readiness `running: true` with the bumped installed generation (the
/// claim boundary deferred harness recovery consumes).
#[tokio::test]
async fn boot_adopts_starting_child_within_readiness_window() {
    let root = tempfile::tempdir().unwrap();
    let sock = root.path().join("run/codex-appserver.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
    let marker = root.path().join("window-term-marker");
    let handler_ready = root.path().join("window-handler-ready");

    let (mut child, old_pid, process_start_time) =
        spawn_unbound_child_with_term_marker(&sock, 2_000, &marker, &handler_ready).await;
    let repo = repo().await;
    persist_starting_daemon(&repo, &root, old_pid, &sock, process_start_time, now_ms()).await;

    let daemon = server(&root, repo.clone()).await;
    let mut readiness = daemon.readiness_receiver();
    daemon.start_or_takeover().await.unwrap();

    let snapshot = daemon.status_snapshot();
    assert_eq!(snapshot.state, SharedDaemonState::Running);
    assert_eq!(
        snapshot.runtime.as_ref().map(|runtime| runtime.pid),
        Some(old_pid),
        "the starting child must be ADOPTED when it binds within the \
         remaining readiness window, not reaped for a fresh spawn"
    );
    assert!(
        !marker.exists(),
        "adoption must never signal the child (no SIGTERM marker) — \
         backfill progress is preserved"
    );
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        SharedDaemonState::from_db_str(&record.state),
        SharedDaemonState::Running,
        "adoption must re-stamp the starting row to running"
    );
    assert_eq!(record.pid, Some(old_pid));
    assert_eq!(
        record.restart_count, 0,
        "adoption is not a restart — restart_count must be unchanged"
    );
    let ready = *readiness.borrow_and_update();
    assert!(
        ready.running,
        "adoption must publish readiness running: true (same typestate \
         seam as spawn — #953 claim boundary)"
    );
    assert_eq!(
        ready.generation,
        daemon.generation_for_test().await,
        "readiness generation must be the adopted incarnation's bumped generation"
    );
    assert!(
        ready.generation >= 1,
        "installing the adopted Running incarnation must bump the generation"
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// #954 defect 2, design test 11 — window lapse: the persisted `started_at`
/// is backdated beyond the start timeout, so the remaining window is zero.
/// The alive-but-unbound child is reaped GRACEFULLY (cooperative SIGTERM
/// marker written — the defect-1 helper, not an instant SIGKILL) and a
/// fresh spawn replaces it.
#[tokio::test]
async fn readiness_window_lapse_reaps_gracefully_then_spawns_fresh() {
    let root = tempfile::tempdir().unwrap();
    let sock = root.path().join("run/codex-appserver.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
    let marker = root.path().join("lapse-term-marker");
    let handler_ready = root.path().join("lapse-handler-ready");

    let (child, old_pid, process_start_time) =
        spawn_unbound_child_with_term_marker(&sock, 60_000, &marker, &handler_ready).await;
    let repo = repo().await;
    // Backdated far beyond the 120s default start timeout ⇒ zero window.
    persist_starting_daemon(
        &repo,
        &root,
        old_pid,
        &sock,
        process_start_time,
        now_ms() - 600_000,
    )
    .await;

    let daemon = server(&root, repo.clone()).await;
    daemon.start_or_takeover().await.unwrap();

    assert!(
        marker.exists(),
        "the lapsed child must be reaped GRACEFULLY (cooperative SIGTERM \
         marker written), never an instant SIGKILL"
    );
    assert!(
        waitpid_reaped(old_pid).await,
        "the lapsed child must be gone after the graceful reap"
    );
    let snapshot = daemon.status_snapshot();
    assert_eq!(snapshot.state, SharedDaemonState::Running);
    let new_pid = snapshot.runtime.as_ref().map(|runtime| runtime.pid);
    assert_ne!(
        new_pid,
        Some(old_pid),
        "a fresh spawn must replace the lapsed child"
    );
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(record.pid, new_pid, "the row must name the fresh spawn");
    drop(child);
}

/// #954 defect 2, design test 11 (exit arm) — a child that EXITS during the
/// readiness window is detected by the liveness probe (`verify_owned_pid`
/// polling) and replaced by an immediate fresh spawn — the boot never sits
/// out the remaining window against a dead child.
#[tokio::test]
async fn child_exit_during_readiness_window_spawns_immediately() {
    let root = tempfile::tempdir().unwrap();
    let sock = root.path().join("run/codex-appserver.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
    let marker = root.path().join("exit-term-marker");
    let handler_ready = root.path().join("exit-handler-ready");

    let (child, old_pid, process_start_time) =
        spawn_unbound_child_with_term_marker(&sock, 60_000, &marker, &handler_ready).await;
    let repo = repo().await;
    // Fresh started_at ⇒ near-full 120s window; the test kills the child
    // 500ms into it, so only prompt exit-detection can finish in time.
    persist_starting_daemon(&repo, &root, old_pid, &sock, process_start_time, now_ms()).await;

    let daemon = server(&root, repo.clone()).await;
    let boot = tokio::spawn({
        let daemon = daemon.clone();
        async move { daemon.start_or_takeover().await }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    // SAFETY: SIGKILL to the fake child this test spawned (models a child
    // that dies mid-backfill; SIGKILL so no marker is written).
    unsafe {
        libc::kill(old_pid, libc::SIGKILL);
    }
    tokio::time::timeout(Duration::from_secs(30), boot)
        .await
        .expect("boot must not sit out the remaining window against a dead child")
        .expect("boot task")
        .expect("boot must recover by spawning fresh");

    let snapshot = daemon.status_snapshot();
    assert_eq!(snapshot.state, SharedDaemonState::Running);
    assert_ne!(
        snapshot.runtime.as_ref().map(|runtime| runtime.pid),
        Some(old_pid),
        "a fresh spawn must replace the exited child"
    );
    drop(child);
}

/// #954 defect 2 — the readiness window NEVER re-arms across repeated boots
/// for the same child: it is budgeted by the PERSISTED `started_at` (which
/// adoption/re-stamp preserves), so a second boot sees only the leftover
/// budget. Constructed via a backdated `started_at` anchored to a
/// timestamp captured BEFORE the spawn: with an 8s start timeout and 6s
/// already consumed, the lapse deadline is `started_at + 8s` = anchor + 2s,
/// while the child binds no earlier than anchor + 5s — a ≥3s margin that
/// setup latency (repo init, handler-ready wait) cannot erode, because
/// extra latency only shrinks the leftover window further. The lapse
/// happens even though a (wrongly) re-armed fresh 8s window would have
/// adopted the 5s bind.
#[tokio::test]
async fn readiness_window_never_rearms_for_same_child() {
    let root = tempfile::tempdir().unwrap();
    let sock = root.path().join("run/codex-appserver.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
    let marker = root.path().join("rearm-term-marker");
    let handler_ready = root.path().join("rearm-handler-ready");

    let spawn_anchor = now_ms();
    let (child, old_pid, process_start_time) =
        spawn_unbound_child_with_term_marker(&sock, 5_000, &marker, &handler_ready).await;
    let repo = repo().await;
    persist_starting_daemon(
        &repo,
        &root,
        old_pid,
        &sock,
        process_start_time,
        spawn_anchor - 6_000,
    )
    .await;

    let mut cfg = cfg(&root);
    cfg.shared_codex_appserver_start_timeout_secs = 8;
    let home = calm_server::shared_codex_home::SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
        cfg.data_dir_resolved().join("codex-homes"),
    );
    home.seed().unwrap();
    let daemon = SharedCodexAppServer::new(&cfg, Arc::new(home), repo.clone());
    daemon.start_or_takeover().await.unwrap();

    assert!(
        marker.exists(),
        "the leftover window (≤2s) must lapse before the 5s bind: a fresh \
         re-armed 8s window would have adopted the child instead of reaping"
    );
    assert!(
        waitpid_reaped(old_pid).await,
        "the lapsed child must be gone after the graceful reap"
    );
    let snapshot = daemon.status_snapshot();
    assert_eq!(snapshot.state, SharedDaemonState::Running);
    assert_ne!(
        snapshot.runtime.as_ref().map(|runtime| runtime.pid),
        Some(old_pid),
        "the second boot gets only the leftover budget — the same child is \
         replaced, not re-granted a full window"
    );
    drop(child);
}

/// #953 design test 12 — the settings PUT path (mark + nudge) never takes
/// the transition serial: it must return promptly even while a stalled
/// spawn holds the serial.
#[tokio::test]
async fn settings_nudge_returns_promptly_during_stalled_spawn() {
    let _guard = ENV_LOCK.lock().await;
    unsafe {
        std::env::set_var("FAKE_CODEX_INITIALIZE_DELAY_MS", "1500");
    }
    let _env = EnvGuard("FAKE_CODEX_INITIALIZE_DELAY_MS");

    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let daemon = server(&root, repo.clone()).await;
    let start_task = tokio::spawn({
        let daemon = daemon.clone();
        async move { daemon.start_or_takeover().await }
    });
    let mut stalled = false;
    for _ in 0..200 {
        if daemon.status_snapshot().state == SharedDaemonState::Starting {
            stalled = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(stalled, "spawn must be observably in flight");

    let nudge_started = std::time::Instant::now();
    daemon.mark_needs_respawn();
    let nudge_elapsed = nudge_started.elapsed();
    assert!(
        nudge_elapsed < Duration::from_millis(100),
        "settings PUT (mark + nudge) must not block on the held transition \
         serial (took {nudge_elapsed:?})"
    );

    start_task
        .await
        .unwrap()
        .expect("delayed initialize within the default deadline must succeed");
}

/// #953 design test 13(a) — pid-partial shape: a failed row naming only a
/// pid (verification pair incomplete). While `/proc/<pid>` exists the shape
/// stays unreconciled and the process is NEVER signaled (ownership
/// unprovable — a bare-pid kill could hit an unrelated process); once it
/// exits, the next round proves absence and the spawn proceeds.
#[tokio::test]
async fn partial_identity_pid_only_never_signals_and_recovers_on_exit() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let mut survivor = Command::new("sleep")
        .arg("120")
        .kill_on_drop(true)
        .spawn()
        .expect("spawn unrelated survivor process");
    let survivor_pid = i32::try_from(survivor.id().expect("survivor pid")).expect("pid fits i32");

    repo.shared_daemon_runtime_set(SharedCodexDaemonUpdate {
        state: "failed".into(),
        pid: Some(survivor_pid),
        pgid: None,
        sock_path: None,
        codex_home_path: None,
        process_start_time: None,
        boot_id: None,
        started_at: None,
        last_error: Some("crash".into()),
        increment_restart_count: false,
        daemon_env_signature: None,
    })
    .await
    .unwrap();

    let daemon = server(&root, repo.clone()).await;
    daemon
        .ensure_running()
        .await
        .expect_err("pid-partial shape with a live pid must stay unreconciled");
    // A `sleep` dies on any signal, so liveness proves nothing was sent.
    // SAFETY: signal 0 probes liveness without delivering a signal.
    assert_eq!(
        unsafe { libc::kill(survivor_pid, 0) },
        0,
        "the pid-partial survivor must never be signaled"
    );
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(record.pid, Some(survivor_pid), "identity must be retained");
    assert!(
        record
            .last_error
            .as_deref()
            .unwrap_or_default()
            .starts_with("unreconciled: "),
        "row must carry the unreconciled prefix; got {:?}",
        record.last_error
    );

    // Rounds only re-probe: a second round changes nothing and stays fenced.
    daemon
        .ensure_running()
        .await
        .expect_err("still fenced while the pid is live");
    assert_eq!(unsafe { libc::kill(survivor_pid, 0) }, 0);

    // Process exits ⇒ next round NULLs identity ⇒ SafeToRetry ⇒ spawn.
    survivor.kill().await.expect("stop test survivor");
    let _ = survivor.wait().await;
    daemon
        .ensure_running()
        .await
        .expect("proven absence must reopen the spawn path");
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        SharedDaemonState::from_db_str(&record.state),
        SharedDaemonState::Running
    );
    assert_ne!(record.pid, Some(survivor_pid));
}

/// #953 design test 13(b) — the pid-NULL operator shape: identity fragments
/// without a pid name no process at all. Rounds only re-read; the row and
/// `status_snapshot().last_error` carry `unreconciled-needs-operator:`;
/// remediation is the operator clearing the identity columns.
#[tokio::test]
async fn partial_identity_pid_null_operator_shape_requires_manual_clear() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    repo.shared_daemon_runtime_set(SharedCodexDaemonUpdate {
        state: "failed".into(),
        pid: None,
        pgid: None,
        sock_path: None,
        codex_home_path: None,
        process_start_time: Some(12345),
        boot_id: Some("some-old-boot".into()),
        started_at: None,
        last_error: Some("crash".into()),
        increment_restart_count: false,
        daemon_env_signature: None,
    })
    .await
    .unwrap();

    let daemon = server(&root, repo.clone()).await;
    let err = daemon
        .ensure_running()
        .await
        .expect_err("the operator shape must never spawn");
    assert!(
        err.to_string().contains("unreconciled-needs-operator"),
        "operator shape must be labeled; got: {err}"
    );
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert!(
        record
            .last_error
            .as_deref()
            .unwrap_or_default()
            .starts_with("unreconciled-needs-operator: "),
        "row must carry the operator prefix; got {:?}",
        record.last_error
    );
    assert_eq!(
        record.process_start_time,
        Some(12345),
        "identity fragments must be retained for the operator"
    );
    // Surfaced via the existing daemon status API (no new UI).
    assert!(
        daemon
            .status_snapshot()
            .last_error
            .unwrap_or_default()
            .contains("unreconciled-needs-operator"),
        "status_snapshot().last_error must surface the operator state"
    );

    // Rounds only re-read: identical tuple, no rewrite.
    tokio::time::sleep(Duration::from_millis(50)).await;
    daemon
        .ensure_running()
        .await
        .expect_err("still the operator shape");
    let after_round = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        after_round.updated_at, record.updated_at,
        "operator-shape rounds must not rewrite the row"
    );

    // Operator remediation: clear the identity columns (manual DB fix).
    repo.shared_daemon_runtime_set(SharedCodexDaemonUpdate {
        state: "failed".into(),
        pid: None,
        pgid: None,
        sock_path: None,
        codex_home_path: None,
        process_start_time: None,
        boot_id: None,
        started_at: None,
        last_error: Some("operator cleared identity".into()),
        increment_restart_count: false,
        daemon_env_signature: None,
    })
    .await
    .unwrap();
    daemon
        .ensure_running()
        .await
        .expect("cleared identity must classify SafeToRetry and spawn");
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        SharedDaemonState::from_db_str(&record.state),
        SharedDaemonState::Running
    );
}

/// #953 design test 13(c) — triple complete (pid/start_time/boot_id) with
/// pgid NULL: verifiable but not group-reapable — there is no valid pgid to
/// target and a bare-pid signal is not a group reap. Stays unreconciled and
/// unsignaled while alive; verify-false after exit proves absence.
#[tokio::test]
async fn partial_identity_triple_complete_pgid_null_never_signals_and_recovers() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let mut survivor = Command::new("sleep")
        .arg("120")
        .kill_on_drop(true)
        .spawn()
        .expect("spawn survivor process");
    let survivor_pid = i32::try_from(survivor.id().expect("survivor pid")).expect("pid fits i32");
    let start_time = read_proc_start_time(survivor_pid).expect("survivor start time");
    let boot_id = read_boot_id().unwrap_or_default();

    repo.shared_daemon_runtime_set(SharedCodexDaemonUpdate {
        state: "failed".into(),
        pid: Some(survivor_pid),
        pgid: None,
        sock_path: None,
        codex_home_path: None,
        process_start_time: Some(start_time),
        boot_id: Some(boot_id),
        started_at: None,
        last_error: Some("crash".into()),
        increment_restart_count: false,
        daemon_env_signature: None,
    })
    .await
    .unwrap();

    let daemon = server(&root, repo.clone()).await;
    daemon
        .ensure_running()
        .await
        .expect_err("triple-complete pgid-NULL shape must stay unreconciled while alive");
    // A `sleep` dies on any signal — liveness proves no signal was sent.
    // SAFETY: signal 0 probes liveness without delivering a signal.
    assert_eq!(
        unsafe { libc::kill(survivor_pid, 0) },
        0,
        "the pgid-NULL survivor must never be signaled"
    );
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(record.pid, Some(survivor_pid));
    assert!(
        record
            .last_error
            .as_deref()
            .unwrap_or_default()
            .starts_with("unreconciled: "),
        "row must carry the unreconciled prefix; got {:?}",
        record.last_error
    );

    // Process exits ⇒ verify-false ⇒ identity NULLed ⇒ spawn.
    survivor.kill().await.expect("stop test survivor");
    let _ = survivor.wait().await;
    daemon
        .ensure_running()
        .await
        .expect("verify-false after exit must reopen the spawn path");
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        SharedDaemonState::from_db_str(&record.state),
        SharedDaemonState::Running
    );
    assert_ne!(record.pid, Some(survivor_pid));
}

/// `/proc/<pid>/stat` state field (first token after the `(comm)` field).
fn proc_stat_state(pid: i32) -> Option<char> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit(") ").next()?.chars().next()
}

/// Wait for `pid` to become a zombie (its tokio `Child` is deliberately
/// held un-`wait()`ed by the caller, so the kernel keeps the entry).
async fn wait_until_zombie(pid: i32) -> bool {
    for _ in 0..250 {
        if proc_stat_state(pid) == Some('Z') {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// #953 review D4(a) — a zombie group LEADER is not proof that its process
/// group is dead: live descendants may remain in the group. For a full
/// verified pid==pgid tuple, reconciliation must still run the group reap
/// (verification permits signaling the known pgid; the leader being a
/// zombie is irrelevant to the members) instead of classifying ProvenAbsent
/// on the leader's zombie state alone — which would clear the durable fence
/// and spawn a replacement while the old group still contains live
/// processes.
///
/// Construction: a `sh` group leader backgrounds a `sleep` in its own pgid,
/// prints the member pid, and exits; the test holds the tokio `Child`
/// without `wait()`ing, so the leader stays a zombie while the member lives.
#[tokio::test]
async fn zombie_leader_with_live_group_member_is_group_reaped_not_proven_absent() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;

    let mut leader = Command::new("sh")
        .arg("-c")
        .arg("sleep 120 & echo $!; exit 0")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("spawn zombie-leader group");
    let leader_pid = i32::try_from(leader.id().expect("leader pid")).expect("pid fits i32");
    let member_pid = read_pid_line(leader.stdout.take().expect("leader stdout piped")).await;
    assert!(
        wait_until_zombie(leader_pid).await,
        "the exited-but-unreaped leader must show as a zombie"
    );
    // The zombie's /proc entry still carries its identity: the persisted
    // tuple verifies against it.
    let start_time = read_proc_start_time(leader_pid).expect("zombie leader keeps /proc stat");
    // SAFETY: signal 0 probes liveness without delivering a signal.
    assert_eq!(
        unsafe { libc::kill(member_pid, 0) },
        0,
        "the group member must be alive behind the zombie leader"
    );
    assert_eq!(
        unsafe { libc::getpgid(member_pid) },
        leader_pid,
        "the live member must sit in the zombie leader's process group"
    );

    repo.shared_daemon_runtime_set(SharedCodexDaemonUpdate {
        state: "failed".into(),
        pid: Some(leader_pid),
        pgid: Some(leader_pid),
        sock_path: None,
        codex_home_path: None,
        process_start_time: Some(start_time),
        boot_id: Some(read_boot_id().unwrap_or_default()),
        started_at: None,
        last_error: Some("crash".into()),
        increment_restart_count: false,
        daemon_env_signature: None,
    })
    .await
    .unwrap();

    let daemon = server(&root, repo.clone()).await;
    daemon
        .ensure_running()
        .await
        .expect("group reap of the verified tuple must prove absence and reopen the spawn path");

    // The core assertion: absence was proven by SIGNALING the known pgid,
    // never silently from the leader's zombie state — the live member must
    // be gone (SIGKILL to the group; init reaps it once its dead parent's
    // slot is cleared). Pre-fix, the member survived unsignaled.
    let mut member_gone = false;
    for _ in 0..100 {
        // SAFETY: signal 0 probes liveness without delivering a signal.
        if unsafe { libc::kill(member_pid, 0) } != 0 {
            member_gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        member_gone,
        "the live group member must have been reaped with the group; \
         a zombie leader must never yield silent ProvenAbsent without signaling"
    );
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        SharedDaemonState::from_db_str(&record.state),
        SharedDaemonState::Running,
        "post-reap proven absence must reopen the spawn path"
    );
    assert_ne!(
        record.pid,
        Some(leader_pid),
        "a fresh daemon must be spawned"
    );

    // Reap the zombie leader (test-side wait) for hygiene.
    let _ = leader.wait().await;
}

/// #953 review D4(b) — the pid-partial shape (a): a ZOMBIE pid is still a
/// present /proc entry, and group absence is NOT provable from it (the
/// unverifiable record may name a group whose members outlive the zombie
/// leader; a bare pid must never be signaled). The shape must stay
/// unreconciled while the zombie exists and recover only once the entry is
/// truly gone (parent reaps it).
#[tokio::test]
async fn partial_identity_pid_only_zombie_stays_unreconciled_until_reaped() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let mut child = Command::new("true")
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn short-lived child");
    let zombie_pid = i32::try_from(child.id().expect("child pid")).expect("pid fits i32");
    assert!(
        wait_until_zombie(zombie_pid).await,
        "the exited-but-unreaped child must show as a zombie"
    );

    repo.shared_daemon_runtime_set(SharedCodexDaemonUpdate {
        state: "failed".into(),
        pid: Some(zombie_pid),
        pgid: None,
        sock_path: None,
        codex_home_path: None,
        process_start_time: None,
        boot_id: None,
        started_at: None,
        last_error: Some("crash".into()),
        increment_restart_count: false,
        daemon_env_signature: None,
    })
    .await
    .unwrap();

    let daemon = server(&root, repo.clone()).await;
    let err = daemon
        .ensure_running()
        .await
        .expect_err("a zombie pid is a present /proc entry: the shape must stay unreconciled");
    assert!(
        err.to_string().contains("unreconciled"),
        "fence error must say unreconciled; got: {err}"
    );
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        record.pid,
        Some(zombie_pid),
        "the fenced row must retain the identity tuple"
    );
    assert_ne!(
        daemon.status_snapshot().state,
        SharedDaemonState::Running,
        "no spawn may happen while the zombie entry exists"
    );

    // Parent reaps the zombie ⇒ /proc entry gone ⇒ absence provable ⇒ the
    // next round NULLs identity and spawns.
    let _ = child.wait().await;
    daemon
        .ensure_running()
        .await
        .expect("a truly-gone /proc entry must reopen the spawn path");
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        SharedDaemonState::from_db_str(&record.state),
        SharedDaemonState::Running
    );
    assert_ne!(record.pid, Some(zombie_pid));
}

// ================= #954 graceful replacement & shutdown (PR1) =================

/// #954 design test 3 — the grace is a CEILING: a wedged daemon that ignores
/// SIGTERM pays the full (test-shortened, 1s) grace and is then SIGKILLed;
/// the transition still completes with a fresh daemon.
#[tokio::test]
async fn grace_ceiling_escalates_to_sigkill_for_sigterm_ignoring_daemon() {
    let _guard = ENV_LOCK.lock().await;
    unsafe {
        std::env::set_var("FAKE_CODEX_IGNORE_SIGTERM", "1");
    }
    let _ignore_env = EnvGuard("FAKE_CODEX_IGNORE_SIGTERM");

    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let mut cfg = cfg(&root);
    cfg.shared_codex_appserver_stop_grace_secs = 1;
    let home = calm_server::shared_codex_home::SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
        cfg.data_dir_resolved().join("codex-homes"),
    );
    home.seed().unwrap();
    let daemon = SharedCodexAppServer::new(&cfg, Arc::new(home), repo.clone());
    daemon.start_or_takeover().await.unwrap();
    let old_pid = daemon.status_snapshot().runtime.expect("running").pid;

    let started = std::time::Instant::now();
    daemon.mark_needs_respawn();
    daemon.ensure_respawn_for_current_settings().await.unwrap();
    let elapsed = started.elapsed();

    assert!(
        elapsed >= Duration::from_secs(1),
        "a SIGTERM-ignoring daemon must be given the full 1s grace ceiling \
         before SIGKILL (took {elapsed:?})"
    );
    assert!(
        pid_gone_or_zombie(old_pid),
        "the ceiling SIGKILL must actually end the wedged daemon"
    );
    let snapshot = daemon.status_snapshot();
    assert_eq!(snapshot.state, SharedDaemonState::Running);
    assert_ne!(
        snapshot.runtime.as_ref().map(|runtime| runtime.pid),
        Some(old_pid)
    );
}

/// #954 design test 5 — the drain boundary, continuation of the adopt test:
/// after adopt-on-mismatch, the FIRST thread start triggers the replace —
/// the old daemon exits via SIGTERM (cooperative marker), a fresh daemon
/// with the CURRENT signature is persisted, and the thread is minted on it.
#[tokio::test]
async fn adopted_mismatched_daemon_drains_at_first_thread_start() {
    let root = tempfile::tempdir().unwrap();
    let sock = root.path().join("run/codex-appserver.sock");
    let marker = root.path().join("drain-term-marker");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();

    let mut child = Command::new(fake_codex_bin())
        .arg("app-server")
        .arg("--listen")
        .arg(format!("unix://{}", sock.display()))
        .env("FAKE_CODEX_SIGTERM_MARKER", &marker)
        .env("FAKE_CODEX_SIGTERM_EXIT_DELAY_MS", "500")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .expect("spawn stale-signature fake app-server for drain test");
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
    assert_eq!(
        daemon.status_snapshot().runtime.map(|runtime| runtime.pid),
        Some(old_pid),
        "precondition: the mismatched daemon is adopted"
    );
    assert!(daemon.needs_respawn_on_next_thread_start_for_test());

    // The drain boundary: the first thread start replaces the daemon.
    let card_id = seed_card(&repo, 1).await;
    let thread_id = daemon
        .thread_start_mint_for_card(
            &card_id,
            SharedThreadStartParams {
                cwd: "/tmp".into(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: None,
                config: ThreadConfig::NoMcp,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        thread_id, "fake-thread-0001",
        "the thread must be minted on the fresh daemon"
    );
    assert!(
        marker.exists(),
        "the drained old daemon must exit via its cooperative SIGTERM path"
    );
    let snapshot = daemon.status_snapshot();
    assert_eq!(snapshot.state, SharedDaemonState::Running);
    let new_pid = snapshot.runtime.map(|runtime| runtime.pid);
    assert_ne!(new_pid, Some(old_pid), "drain must replace the process");
    assert!(!daemon.needs_respawn_on_next_thread_start_for_test());

    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(record.pid, new_pid);
    let expected_signature = effective_test_env_signature(&cfg(&root).codex_ingest_url_resolved());
    assert_eq!(
        record.daemon_env_signature.as_deref(),
        Some(expected_signature.as_str()),
        "the drain replace must persist the FRESH signature"
    );

    let _ = child.wait().await;
}

/// #954 design test 6 — drain durability across supervisor loss: the drain
/// obligation lives in the persisted OLD signature, nothing else. Destroying
/// the supervisor (its in-memory needs_respawn flag dies with it) and
/// constructing a new one over the same repo must re-detect the mismatch,
/// re-adopt, and re-mark.
#[tokio::test]
async fn adopt_drain_obligation_survives_supervisor_loss() {
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
        .expect("spawn stale-signature fake app-server for durability test");
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

    let instance_a = server(&root, repo.clone()).await;
    instance_a.start_or_takeover().await.unwrap();
    assert!(instance_a.needs_respawn_on_next_thread_start_for_test());
    drop(instance_a);
    tokio::time::sleep(Duration::from_millis(200)).await;
    // SAFETY: signal 0 probes liveness without delivering a signal.
    assert_eq!(
        unsafe { libc::kill(old_pid, 0) },
        0,
        "the adopted daemon must survive supervisor loss"
    );

    let instance_b = server(&root, repo.clone()).await;
    instance_b.start_or_takeover().await.unwrap();
    assert_eq!(
        instance_b
            .status_snapshot()
            .runtime
            .map(|runtime| runtime.pid),
        Some(old_pid),
        "the fresh supervisor must RE-ADOPT the mismatched daemon"
    );
    assert!(
        instance_b.needs_respawn_on_next_thread_start_for_test(),
        "the fresh supervisor must RE-MARK the drain from the durable \
         old-signature mismatch"
    );
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        record.daemon_env_signature.as_deref(),
        Some("stale-env-signature"),
        "the re-stamp must keep the old signature on every adoption"
    );

    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// #954 design test 8 — re-stamp failure keeps the mismatch durably
/// detectable: the failing write is the ONLY write on the adoption path, so
/// the pre-existing row (old state, old signature) stays untouched; the
/// current process still drains (in-memory flag), and a fresh supervisor
/// still re-detects and re-marks.
#[tokio::test]
async fn adopt_restamp_failure_keeps_mismatch_durably_detectable() {
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
        .expect("spawn stale-signature fake app-server for re-stamp failure");
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
    // Force the re-stamp (the sole `running` write on the adoption path)
    // to fail.
    sqlx::query(
        "CREATE TRIGGER fail_running_stamp BEFORE UPDATE ON shared_codex_daemon \
         WHEN NEW.state = 'running' BEGIN SELECT RAISE(ABORT, 'forced re-stamp failure'); END",
    )
    .execute(repo.pool())
    .await
    .unwrap();

    let instance_a = server(&root, repo.clone()).await;
    instance_a
        .start_or_takeover()
        .await
        .expect("adoption must succeed despite the re-stamp failure (warn only)");
    assert_eq!(
        instance_a
            .status_snapshot()
            .runtime
            .map(|runtime| runtime.pid),
        Some(old_pid)
    );
    assert!(
        instance_a.needs_respawn_on_next_thread_start_for_test(),
        "the in-memory drain must still be armed on re-stamp failure"
    );
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        record.daemon_env_signature.as_deref(),
        Some("stale-env-signature"),
        "the failed write must leave the old row untouched — the mismatch \
         stays durably detectable"
    );

    // Supervisor loss + repaired DB: a fresh supervisor re-detects from the
    // untouched row, re-adopts, re-marks.
    drop(instance_a);
    sqlx::query("DROP TRIGGER fail_running_stamp")
        .execute(repo.pool())
        .await
        .unwrap();
    let instance_b = server(&root, repo.clone()).await;
    instance_b.start_or_takeover().await.unwrap();
    assert_eq!(
        instance_b
            .status_snapshot()
            .runtime
            .map(|runtime| runtime.pid),
        Some(old_pid)
    );
    assert!(instance_b.needs_respawn_on_next_thread_start_for_test());

    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// #954 design test 7 — the cancellation belt, post-persist: aborting the
/// DETACHED transition task itself (test seam; models runtime teardown or an
/// in-task panic — caller cancellation can no longer reach the guard) mid-
/// readiness-poll must fire the belt: the child receives SIGTERM ONLY (its
/// cooperative marker proves no SIGKILL chaser — also proving kill_on_drop
/// is gone), the row remains `starting` with the full tuple, and the serial
/// is released so the next transition recovers through the normal walk.
#[tokio::test]
async fn aborted_detached_transition_belt_terms_child_and_leaves_starting_row() {
    let _guard = ENV_LOCK.lock().await;
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("belt-term-marker");
    unsafe {
        std::env::set_var("FAKE_CODEX_INITIALIZE_DELAY_MS", "30000");
        std::env::set_var("FAKE_CODEX_SIGTERM_MARKER", &marker);
        std::env::set_var("FAKE_CODEX_SIGTERM_EXIT_DELAY_MS", "300");
    }
    let _init_env = EnvGuard("FAKE_CODEX_INITIALIZE_DELAY_MS");
    let _marker_env = EnvGuard("FAKE_CODEX_SIGTERM_MARKER");
    let _delay_env = EnvGuard("FAKE_CODEX_SIGTERM_EXIT_DELAY_MS");

    let repo = repo().await;
    let daemon = server(&root, repo.clone()).await;
    let caller = tokio::spawn({
        let daemon = daemon.clone();
        async move { daemon.start_or_takeover().await }
    });
    let pid = wait_for_starting_pid(&repo).await;

    assert!(
        daemon.abort_detached_spawn_transition_for_test(),
        "the detached transition task handle must be present mid-poll"
    );
    let caller_result = caller.await.unwrap();
    assert!(
        caller_result.is_err(),
        "the observing caller must surface the aborted task"
    );

    let mut marker_written = false;
    for _ in 0..150 {
        if marker.exists() {
            marker_written = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        marker_written,
        "the belt must SIGTERM only: the child's 300ms cooperative shutdown \
         must complete (kill_on_drop/SIGKILL would kill it first)"
    );
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        SharedDaemonState::from_db_str(&record.state),
        SharedDaemonState::Starting,
        "a post-persist belt drop leaves the committed starting row"
    );
    assert_eq!(record.pid, Some(pid), "the starting tuple must be intact");
    assert!(record.pgid.is_some() && record.boot_id.is_some() && record.started_at.is_some());

    // Serial released by the aborted task; the next transition reaches the
    // row through the normal walk (verify-false ⇒ stale-socket reap ⇒
    // fresh spawn).
    unsafe {
        std::env::remove_var("FAKE_CODEX_INITIALIZE_DELAY_MS");
    }
    daemon.ensure_running().await.unwrap();
    let snapshot = daemon.status_snapshot();
    assert_eq!(snapshot.state, SharedDaemonState::Running);
    assert_ne!(snapshot.runtime.map(|runtime| runtime.pid), Some(pid));
}

/// #954 design test 14(a) — caller cancelled mid-transition: the detached
/// task still finishes the transition to a terminal state (Running
/// installed) — never a stuck Starting in memory, never a row-less live
/// child.
#[tokio::test]
async fn caller_cancelled_mid_transition_still_reaches_terminal_running() {
    let _guard = ENV_LOCK.lock().await;
    unsafe {
        std::env::set_var("FAKE_CODEX_INITIALIZE_DELAY_MS", "1000");
    }
    let _env = EnvGuard("FAKE_CODEX_INITIALIZE_DELAY_MS");

    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let daemon = server(&root, repo.clone()).await;
    let caller = tokio::spawn({
        let daemon = daemon.clone();
        async move { daemon.start_or_takeover().await }
    });
    let pid = wait_for_starting_pid(&repo).await;
    caller.abort();

    assert!(
        wait_for_state(&daemon, SharedDaemonState::Running, 20).await,
        "the detached task must finish the transition despite the cancelled \
         caller; still {:?}",
        daemon.status_snapshot().state
    );
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        SharedDaemonState::from_db_str(&record.state),
        SharedDaemonState::Running
    );
    assert_eq!(record.pid, Some(pid), "the spawned child owns the row");
}

/// #954 design test 14(b) — serial retained by the detached task: a
/// concurrent `transition_replace` attempted while the cancelled caller's
/// task still runs BLOCKS until that task's terminal write, then proceeds —
/// the first task's persist never overwrites the second transition's row.
#[tokio::test]
async fn concurrent_transition_blocks_until_cancelled_callers_task_terminal() {
    let _guard = ENV_LOCK.lock().await;
    unsafe {
        std::env::set_var("FAKE_CODEX_INITIALIZE_DELAY_MS", "1500");
    }

    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let daemon = server(&root, repo.clone()).await;
    let caller = tokio::spawn({
        let daemon = daemon.clone();
        async move { daemon.start_or_takeover().await }
    });
    let first_pid = wait_for_starting_pid(&repo).await;
    caller.abort();
    // The second transition's child must spawn fast.
    unsafe {
        std::env::remove_var("FAKE_CODEX_INITIALIZE_DELAY_MS");
    }

    let mut second = tokio::spawn({
        let daemon = daemon.clone();
        async move {
            daemon
                .transition_replace_for_test("second transition", ReplacePrecondition::Always)
                .await
        }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(300), &mut second)
            .await
            .is_err(),
        "the concurrent transition must BLOCK on the serial the detached \
         task still owns"
    );

    let outcome = second.await.unwrap().unwrap();
    assert_eq!(outcome, ReplaceOutcome::Replaced);
    let snapshot = daemon.status_snapshot();
    assert_eq!(snapshot.state, SharedDaemonState::Running);
    let second_pid = snapshot.runtime.map(|runtime| runtime.pid);
    assert_ne!(second_pid, Some(first_pid));
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        record.pid, second_pid,
        "the row must end as the SECOND transition's write — the first \
         task's persist happened strictly before (serial ordering)"
    );
}

/// #954 design test 14(c) — caller cancelled AFTER the completion Result was
/// sent: nothing is pending, the state is already terminal, and dropping the
/// queued Result carries no obligations (the r4 race) — the serial is free
/// for the next transition.
#[tokio::test]
async fn caller_cancelled_after_result_sent_leaves_terminal_state_and_free_serial() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let daemon = server(&root, repo.clone()).await;

    let serial = daemon.lock_transition_serial_for_test().await;
    let mut caller_future = Box::pin(daemon.detached_spawn_transition_future_for_test(serial));
    assert!(
        futures::poll!(caller_future.as_mut()).is_pending(),
        "first poll parks the caller at the receiver await"
    );
    // Let the detached task run the whole transition and SEND its result.
    assert!(
        wait_for_state(&daemon, SharedDaemonState::Running, 20).await,
        "the detached task must reach terminal Running"
    );
    // Cancel the caller post-send: the queued Result is dropped.
    drop(caller_future);

    // Nothing pending: the state is terminal and the serial is free — a
    // fresh transition acquires it promptly and completes.
    let outcome = tokio::time::timeout(Duration::from_secs(30), async {
        daemon
            .transition_replace_for_test("post-cancel transition", ReplacePrecondition::Always)
            .await
    })
    .await
    .expect("the serial must be free after the post-send cancellation")
    .unwrap();
    assert_eq!(outcome, ReplaceOutcome::Replaced);
    assert_eq!(daemon.status_snapshot().state, SharedDaemonState::Running);
}

/// #954 design test 14(d) — forced persist-Err on the Running-row write: the
/// detached task's typestate Err arm reaps the child GRACEFULLY (cooperative
/// marker), persists Failed (identity NULLed — proven absent), and arms heal
/// before releasing the serial.
#[tokio::test]
async fn forced_running_persist_error_reaps_gracefully_and_terminalizes_failed() {
    let _guard = ENV_LOCK.lock().await;
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("persist-err-term-marker");
    unsafe {
        std::env::set_var("FAKE_CODEX_SIGTERM_MARKER", &marker);
        std::env::set_var("FAKE_CODEX_SIGTERM_EXIT_DELAY_MS", "300");
    }
    let _marker_env = EnvGuard("FAKE_CODEX_SIGTERM_MARKER");
    let _delay_env = EnvGuard("FAKE_CODEX_SIGTERM_EXIT_DELAY_MS");

    let repo = repo().await;
    let mut cfg = cfg(&root);
    // Keep the armed heal loop quiet for the assertion window.
    cfg.shared_codex_appserver_restart_initial_delay_ms = 60_000;
    cfg.shared_codex_appserver_restart_max_delay_ms = 120_000;
    let home = calm_server::shared_codex_home::SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
        cfg.data_dir_resolved().join("codex-homes"),
    );
    home.seed().unwrap();
    let daemon = SharedCodexAppServer::new(&cfg, Arc::new(home), repo.clone());

    // The `starting` INSERT passes; only the Running upsert (an UPDATE on
    // the existing row) trips the trigger.
    sqlx::query(
        "CREATE TRIGGER fail_running_stamp BEFORE UPDATE ON shared_codex_daemon \
         WHEN NEW.state = 'running' BEGIN SELECT RAISE(ABORT, 'forced running persist failure'); END",
    )
    .execute(repo.pool())
    .await
    .unwrap();

    let err = daemon
        .start_or_takeover()
        .await
        .expect_err("the forced Running-row write failure must fail the transition");
    assert!(
        err.to_string().contains("forced running persist failure"),
        "the caller must observe the persist failure; got: {err}"
    );
    assert!(
        marker.exists(),
        "the Err arm must reap the child gracefully (cooperative marker) \
         before the error surfaces"
    );
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        SharedDaemonState::from_db_str(&record.state),
        SharedDaemonState::Failed,
        "the typestate Err arm must persist Failed"
    );
    assert_eq!(record.pid, None, "proven-absent failure must NULL identity");
    assert!(
        daemon.heal_active_for_test(),
        "heal must be armed before the serial releases"
    );
}

/// #954 r5 ordering pin — `tokio::spawn` is synchronous: the detached
/// transition task is created BEFORE the caller's first await of the
/// receiver. Poll the raw transition future exactly ONCE (which runs it
/// synchronously up to and through `tokio::spawn`, parking at the receiver),
/// then DROP it — the caller is cancelled at its very first await — and the
/// already-created task must still complete the transition.
#[tokio::test]
async fn spawn_transition_task_outlives_caller_cancelled_at_first_await() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo().await;
    let daemon = server(&root, repo.clone()).await;

    let serial = daemon.lock_transition_serial_for_test().await;
    let mut caller_future = Box::pin(daemon.detached_spawn_transition_future_for_test(serial));
    assert!(
        futures::poll!(caller_future.as_mut()).is_pending(),
        "the first poll must reach (and park at) the receiver await"
    );
    drop(caller_future);

    assert!(
        wait_for_state(&daemon, SharedDaemonState::Running, 20).await,
        "the task created synchronously before the first receiver await \
         must complete the transition; still {:?}",
        daemon.status_snapshot().state
    );
    let record = repo.shared_daemon_runtime_get().await.unwrap();
    assert_eq!(
        SharedDaemonState::from_db_str(&record.state),
        SharedDaemonState::Running
    );
}

/// #954 — the stop-grace knob: default 60 (codex upstream STOP_GRACE_PERIOD),
/// flag + env overrides mirroring the sibling knobs, and validation 1..=600
/// (0 would silently restore the instant-SIGKILL defect).
#[tokio::test]
async fn stop_grace_config_default_flag_env_and_validation() {
    let _guard = ENV_LOCK.lock().await;
    unsafe {
        std::env::remove_var("CALM_SHARED_CODEX_APPSERVER_STOP_GRACE_SECS");
    }
    assert_eq!(
        Config::parse_from(["calm-server"]).shared_codex_appserver_stop_grace_secs,
        60,
        "default must be 60s, matching codex's own supervisor grace"
    );
    assert_eq!(
        Config::parse_from([
            "calm-server",
            "--shared-codex-appserver-stop-grace-secs",
            "7"
        ])
        .shared_codex_appserver_stop_grace_secs,
        7
    );
    unsafe {
        std::env::set_var("CALM_SHARED_CODEX_APPSERVER_STOP_GRACE_SECS", "33");
    }
    let _env = EnvGuard("CALM_SHARED_CODEX_APPSERVER_STOP_GRACE_SECS");
    assert_eq!(
        Config::parse_from(["calm-server"]).shared_codex_appserver_stop_grace_secs,
        33,
        "env override must win over the default"
    );
    unsafe {
        std::env::remove_var("CALM_SHARED_CODEX_APPSERVER_STOP_GRACE_SECS");
    }
    assert!(
        Config::try_parse_from([
            "calm-server",
            "--shared-codex-appserver-stop-grace-secs",
            "0"
        ])
        .is_err(),
        "0 must be rejected: it silently restores the instant-SIGKILL defect"
    );
    assert!(
        Config::try_parse_from([
            "calm-server",
            "--shared-codex-appserver-stop-grace-secs",
            "601"
        ])
        .is_err(),
        "values beyond the 600s sanity cap must be rejected"
    );
}
