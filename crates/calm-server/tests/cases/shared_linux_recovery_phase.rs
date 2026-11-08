//! Linux must reject unsupported durable phases before process or endpoint effects.
//! These bodies compile on the host too; only Linux can execute the regressions.
use super::*;
use calm_server::agent_backend_pool::{BackendId, identity::BackendIsolation, paths::BackendPaths};
use calm_server::db::SharedCodexDaemonRecord;
use calm_server::error::CalmError;
use calm_truth::db::{DaemonRecovery, RecoveryId, RecoveryWrite};
use sqlx::Connection;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug)]
enum Phase {
    Legacy,
    Pending,
    Clear,
    Recorded,
}

#[derive(Clone, Copy, Debug)]
enum Entry {
    Start,
    Ensure,
    Detached,
    Replace,
}

impl Entry {
    async fn call(self, daemon: &Arc<SharedCodexAppServer>) -> Result<(), CalmError> {
        match self {
            Self::Start => daemon.start_or_takeover().await,
            Self::Ensure => daemon.ensure_running().await,
            Self::Detached => {
                let serial = daemon.lock_transition_serial_for_test().await;
                daemon
                    .detached_spawn_transition_future_for_test(serial)
                    .await
            }
            Self::Replace => daemon
                .transition_replace_for_test("phase regression", ReplacePrecondition::Always)
                .await
                .map(|_| ()),
        }
    }
}

#[path = "shared_linux_recovery_phase/witness.rs"]
mod witness;
use witness::{ExecWitness, LiveEndpoint, optional_text};

#[path = "shared_linux_recovery_phase/admission_errors.rs"]
mod admission_errors;

#[path = "shared_linux_recovery_phase/post_consumption.rs"]
mod post_consumption;

#[path = "shared_linux_recovery_phase/cold_start.rs"]
mod cold_start;

#[path = "shared_linux_recovery_phase/boot_preparation.rs"]
mod boot_preparation;

struct Fixture {
    // Drop captured children before deleting their files.
    exec: ExecWitness,
    root: tempfile::TempDir,
    url: String,
    repo: Arc<SqlxRepo>,
    config: Config,
    backend: BackendId,
    paths: Arc<BackendPaths>,
}

impl Fixture {
    async fn new(multiple: bool, backend: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let exec = ExecWitness::new(root.path());
        let mut config = if multiple {
            agent_backend_upgrade::multi_backend_cfg(&root)
        } else {
            quiet_heal_cfg(&root)
        };
        config.codex_bin = exec.binary.to_str().unwrap().into();
        config.shared_codex_appserver_restart_initial_delay_ms = 60_000;
        config.shared_codex_appserver_restart_max_delay_ms = 120_000;
        config.shared_codex_appserver_stop_grace_secs = 1;
        config.shared_codex_appserver_start_timeout_secs = 3;
        let url = format!(
            "sqlite://{}?mode=rwc",
            root.path().join("phase.db").display()
        );
        let repo = Arc::new(SqlxRepo::open(&url).await.unwrap());
        let erased: Arc<dyn Repo> = repo.clone();
        let isolation = BackendIsolation::prepare(&config, &erased).await.unwrap();
        let backend = BackendId::try_from(backend.to_owned()).unwrap();
        let paths = isolation.paths(&backend).clone();
        let home = calm_server::shared_codex_home::SharedCodexHome::new(paths.home.clone());
        home.seed_from(None).unwrap();
        home.ensure_config(None).unwrap();
        Self {
            exec,
            root,
            url,
            repo,
            config,
            backend,
            paths,
        }
    }

    fn update(&self, recovery: RecoveryWrite, state: SharedDaemonState) -> SharedCodexDaemonUpdate {
        SharedCodexDaemonUpdate {
            recovery,
            state,
            pid: None,
            pgid: None,
            process_start_time: None,
            boot_id: None,
            started_at: None,
            paths_are_legacy_display: false,
            sock_path: Some(self.paths.socket.as_os_str().as_bytes().to_vec()),
            codex_home_path: Some(self.paths.home.as_os_str().as_bytes().to_vec()),
            last_error: None,
            increment_restart_count: false,
            daemon_env_signature: None,
        }
    }

    async fn write(&self, update: SharedCodexDaemonUpdate) {
        self.repo
            .shared_daemon_runtime_set(self.backend.as_str(), update)
            .await
            .unwrap();
    }

    async fn row(&self) -> SharedCodexDaemonRecord {
        self.repo
            .shared_daemon_runtime_get(self.backend.as_str())
            .await
            .unwrap()
    }

    async fn seed_phase(
        &mut self,
        phase: Phase,
        state: SharedDaemonState,
        live: Option<&LiveEndpoint>,
    ) {
        let id = RecoveryId::fresh();
        let write = if matches!(phase, Phase::Legacy) {
            RecoveryWrite::Legacy
        } else {
            RecoveryWrite::Begin { id }
        };
        self.write(self.update(write, state)).await;
        match phase {
            Phase::Legacy | Phase::Pending => {}
            Phase::Clear => {
                self.write(self.update(RecoveryWrite::CancelReserved { id }, state))
                    .await
            }
            Phase::Recorded => {
                let live = live.expect("Recorded uses a real process identity");
                let mut update = self.update(RecoveryWrite::Publish { id }, state);
                update.pid = Some(live.pid);
                update.pgid = Some(live.pid);
                update.process_start_time = Some(live.birth);
                update.boot_id = Some(live.boot.clone());
                update.started_at = Some(now_ms());
                self.write(update).await;
            }
        }
        let before = self.row().await;
        self.repo.db().close().await;
        self.repo = Arc::new(SqlxRepo::open(&self.url).await.unwrap());
        assert_same_row(&before, &self.row().await);
    }

    async fn daemon(&self) -> Arc<SharedCodexAppServer> {
        let erased: Arc<dyn Repo> = self.repo.clone();
        let isolation = BackendIsolation::prepare(&self.config, &erased)
            .await
            .unwrap();
        // Explicitly prove that both isolation consumers accept the matching paths.
        isolation
            .verify_backend(erased.as_ref(), &self.backend)
            .await
            .unwrap();
        if self.backend.as_str() == "default" {
            SharedCodexAppServer::new_with_isolation(&self.config, erased, None, isolation)
        } else {
            let pending = Arc::new(
                calm_server::pending_codex_threads::PendingThreadStartRegistry::new(
                    erased.clone(),
                    calm_server::event::EventBus::new(),
                ),
            );
            calm_server::agent_backend_pool::codex::CodexBackends::build(
                &self.config,
                erased,
                pending,
                isolation,
            )
            .instance(&self.backend)
            .unwrap()
            .clone()
        }
    }

    fn pollute(&self) {
        let path = self.paths.home.join("config.toml");
        let mut content = std::fs::read_to_string(&path).unwrap();
        content.push_str("\n[mcp_servers.evil]\ncommand='/missing'\n");
        std::fs::write(path, content).unwrap();
    }

    async fn invoke(
        &self,
        daemon: &Arc<SharedCodexAppServer>,
        entry: Entry,
    ) -> Result<(), CalmError> {
        let previous = self.exec.pids().len();
        let mut writer = sqlx::SqliteConnection::connect(&self.url).await.unwrap();
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut writer)
            .await
            .unwrap();
        let call = entry.call(daemon);
        tokio::pin!(call);
        // Do not assert while holding the barrier: even a broken entry must be
        // allowed to finish its real production cleanup before test teardown.
        let (early, stalled) = tokio::select! {
            result = &mut call => (Some(result), false),
            () = self.exec.capture_new_exec(previous) => (None, false),
            () = tokio::time::sleep(Duration::from_secs(10)) => (None, true),
        };
        sqlx::query("ROLLBACK").execute(&mut writer).await.unwrap();
        let result = match early {
            Some(result) => result,
            None => tokio::time::timeout(Duration::from_secs(15), &mut call)
                .await
                .expect("production entry must finish after releasing writer barrier"),
        };
        assert!(
            !stalled,
            "neither refusal nor exec witness completed at the writer barrier"
        );
        result
    }

    // Change only durable phase via the typed claim. Keeping this owner alive is
    // essential: reopening it would turn its installed Child into an external PID.
    async fn claim_installed_child(&self) {
        let row = self.row().await;
        assert_eq!(row.recovery, DaemonRecovery::LegacyUntracked);
        assert_eq!(row.state, SharedDaemonState::Running);
        let mut update = self.update(
            RecoveryWrite::ClaimLegacy {
                id: RecoveryId::fresh(),
            },
            row.state,
        );
        update.pid = row.pid;
        update.pgid = row.pgid;
        update.process_start_time = row.process_start_time;
        update.boot_id = row.boot_id;
        update.started_at = row.started_at;
        update.daemon_env_signature = row.daemon_env_signature;
        self.write(update).await;
    }
}

fn assert_same_row(before: &SharedCodexDaemonRecord, after: &SharedCodexDaemonRecord) {
    assert_eq!(after.recovery, before.recovery, "durable scope");
    assert_eq!(after.state, before.state, "durable lifecycle");
    assert_eq!(
        (after.pid, after.pgid, after.process_start_time),
        (before.pid, before.pgid, before.process_start_time)
    );
    assert_eq!(after.boot_id, before.boot_id);
    assert_eq!(after.started_at, before.started_at);
    assert_eq!(after.sock_path, before.sock_path);
    assert_eq!(after.codex_home_path, before.codex_home_path);
    assert_eq!(
        after.paths_are_legacy_display,
        before.paths_are_legacy_display
    );
    assert_eq!(after.restart_count, before.restart_count);
    assert_eq!(after.daemon_env_signature, before.daemon_env_signature);
    assert_eq!(after.last_error, before.last_error);
    assert_eq!(after.updated_at, before.updated_at);
}

async fn refused_case(
    phase: Phase,
    state: SharedDaemonState,
    entry: Entry,
    live: bool,
    polluted: bool,
    multiple: bool,
    backend: &str,
) {
    let mut fixture = Fixture::new(multiple, backend).await;
    let mut endpoint = if live {
        Some(LiveEndpoint::new(fixture.root.path(), &fixture.paths.socket).await)
    } else {
        None
    };
    fixture.seed_phase(phase, state, endpoint.as_ref()).await;
    let daemon = fixture.daemon().await;
    assert_eq!(daemon.sock_path(), fixture.paths.socket);
    if polluted {
        fixture.pollute();
    }
    let before = fixture.row().await;
    // Guard and Recorded paths can legitimately reach a row write on the
    // unfixed consumer. Observe those without introducing writer contention.
    // The exec barrier is needed for the empty-identity spawn counterexamples.
    let result = if polluted || matches!(phase, Phase::Recorded) {
        tokio::time::timeout(Duration::from_secs(15), entry.call(&daemon))
            .await
            .expect("production entry must complete")
    } else {
        fixture.invoke(&daemon, entry).await
    };
    let after = fixture.row().await;
    let endpoint_effects = match &mut endpoint {
        Some(endpoint) => Some(endpoint.effects().await),
        None => None,
    };
    let pids = fixture.exec.pids();
    let evidence = format!(
        "{phase:?}/{state:?}/{entry:?}: result={result:?}, execs={pids:?}, endpoint={endpoint_effects:?}, before={before:?}, after={after:?}"
    );
    assert!(
        pids.is_empty(),
        "must refuse before actual exec: {evidence}"
    );
    if let Some(effects) = endpoint_effects {
        assert!(
            effects.alive && effects.no_term,
            "must not signal independent child: {evidence}"
        );
        assert!(
            effects.same_inode && effects.connectable,
            "must not unlink independent endpoint: {evidence}"
        );
        assert!(
            effects.no_requests,
            "must refuse before protocol entry: {evidence}"
        );
    } else {
        assert!(
            !fixture.paths.socket.exists(),
            "cold refusal must not create an endpoint: {evidence}"
        );
    }
    assert!(
        result.is_err(),
        "unsupported phase must be refused: {evidence}"
    );
    assert_same_row(&before, &after);
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn legacy_start_and_ensure_execute_the_real_fake() {
    let _env = ENV_LOCK.lock().await;
    for entry in [Entry::Start, Entry::Ensure] {
        let mut fixture = Fixture::new(false, "default").await;
        fixture
            .seed_phase(Phase::Legacy, SharedDaemonState::Failed, None)
            .await;
        let daemon = fixture.daemon().await;
        fixture.invoke(&daemon, entry).await.unwrap();
        let row = fixture.row().await;
        assert_eq!(row.recovery, DaemonRecovery::LegacyUntracked);
        assert_eq!(row.state, SharedDaemonState::Running);
        assert_eq!(fixture.exec.pids(), vec![row.pid.unwrap()]);
        assert_eq!(
            daemon.status_snapshot().runtime.unwrap().pid,
            row.pid.unwrap()
        );
        assert!(
            tokio::net::UnixStream::connect(&fixture.paths.socket)
                .await
                .is_ok()
        );
    }
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn pending_starting_cold_start_refuses_before_exec() {
    let _env = ENV_LOCK.lock().await;
    refused_case(
        Phase::Pending,
        SharedDaemonState::Starting,
        Entry::Start,
        false,
        false,
        false,
        "default",
    )
    .await;
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn pending_failed_cold_ensure_refuses_before_exec() {
    let _env = ENV_LOCK.lock().await;
    refused_case(
        Phase::Pending,
        SharedDaemonState::Failed,
        Entry::Ensure,
        false,
        false,
        false,
        "default",
    )
    .await;
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn pending_starting_live_start_preserves_independent_endpoint() {
    let _env = ENV_LOCK.lock().await;
    refused_case(
        Phase::Pending,
        SharedDaemonState::Starting,
        Entry::Start,
        true,
        false,
        false,
        "default",
    )
    .await;
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn pending_failed_live_ensure_preserves_independent_endpoint() {
    let _env = ENV_LOCK.lock().await;
    refused_case(
        Phase::Pending,
        SharedDaemonState::Failed,
        Entry::Ensure,
        true,
        false,
        false,
        "default",
    )
    .await;
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn clear_cold_start_refuses_before_exec() {
    let _env = ENV_LOCK.lock().await;
    refused_case(
        Phase::Clear,
        SharedDaemonState::Starting,
        Entry::Start,
        false,
        false,
        false,
        "default",
    )
    .await;
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn recorded_starting_start_refuses_before_takeover() {
    let _env = ENV_LOCK.lock().await;
    refused_case(
        Phase::Recorded,
        SharedDaemonState::Starting,
        Entry::Start,
        true,
        false,
        false,
        "default",
    )
    .await;
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn recorded_failed_ensure_preserves_independent_endpoint() {
    let _env = ENV_LOCK.lock().await;
    refused_case(
        Phase::Recorded,
        SharedDaemonState::Failed,
        Entry::Ensure,
        true,
        false,
        false,
        "default",
    )
    .await;
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn pending_polluted_home_preserves_starting_row() {
    let _env = ENV_LOCK.lock().await;
    refused_case(
        Phase::Pending,
        SharedDaemonState::Starting,
        Entry::Start,
        true,
        true,
        false,
        "default",
    )
    .await;
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn clear_polluted_home_preserves_row_without_exec() {
    let _env = ENV_LOCK.lock().await;
    refused_case(
        Phase::Clear,
        SharedDaemonState::Failed,
        Entry::Ensure,
        false,
        true,
        false,
        "default",
    )
    .await;
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn recorded_polluted_home_preserves_independent_endpoint() {
    let _env = ENV_LOCK.lock().await;
    refused_case(
        Phase::Recorded,
        SharedDaemonState::Starting,
        Entry::Start,
        true,
        true,
        false,
        "default",
    )
    .await;
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn pending_detached_entry_refuses_before_exec() {
    let _env = ENV_LOCK.lock().await;
    refused_case(
        Phase::Pending,
        SharedDaemonState::Failed,
        Entry::Detached,
        false,
        false,
        false,
        "default",
    )
    .await;
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn clear_detached_entry_refuses_before_exec() {
    let _env = ENV_LOCK.lock().await;
    refused_case(
        Phase::Clear,
        SharedDaemonState::Failed,
        Entry::Detached,
        false,
        false,
        false,
        "default",
    )
    .await;
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn recorded_detached_entry_preserves_independent_endpoint() {
    let _env = ENV_LOCK.lock().await;
    refused_case(
        Phase::Recorded,
        SharedDaemonState::Failed,
        Entry::Detached,
        true,
        false,
        false,
        "default",
    )
    .await;
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn pending_multi_default_reaches_phase_boundary() {
    let _env = ENV_LOCK.lock().await;
    refused_case(
        Phase::Pending,
        SharedDaemonState::Failed,
        Entry::Ensure,
        false,
        false,
        true,
        "default",
    )
    .await;
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn pending_multi_other_reaches_phase_boundary() {
    let _env = ENV_LOCK.lock().await;
    refused_case(
        Phase::Pending,
        SharedDaemonState::Starting,
        Entry::Start,
        false,
        false,
        true,
        "other",
    )
    .await;
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn recorded_owned_replacement_refuses_before_term_or_unlink() {
    let _env = ENV_LOCK.lock().await;
    let fixture = Fixture::new(false, "default").await;
    let daemon = fixture.daemon().await;
    fixture.invoke(&daemon, Entry::Start).await.unwrap();
    fixture.claim_installed_child().await;
    let before = fixture.row().await;
    let pid = before.pid.unwrap();
    let metadata = std::fs::metadata(&fixture.paths.socket).unwrap();
    let result = fixture.invoke(&daemon, Entry::Replace).await;
    assert_eq!(
        fixture.exec.pids(),
        vec![pid],
        "replacement must not exec again: {result:?}"
    );
    assert_eq!(
        read_proc_start_time(pid),
        before.process_start_time,
        "installed child must survive refusal"
    );
    assert!(!PathBuf::from(format!("{}.term.{pid}", fixture.exec.prefix.display())).exists());
    let after = std::fs::metadata(&fixture.paths.socket).expect("original endpoint retained");
    assert_eq!((after.dev(), after.ino()), (metadata.dev(), metadata.ino()));
    assert!(
        tokio::net::UnixStream::connect(&fixture.paths.socket)
            .await
            .is_ok()
    );
    assert!(result.is_err());
    assert_same_row(&before, &fixture.row().await);
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn recorded_connection_recovery_refuses_before_initialize() {
    let _env = ENV_LOCK.lock().await;
    let fixture = Fixture::new(false, "default").await;
    let daemon = fixture.daemon().await;
    fixture.invoke(&daemon, Entry::Start).await.unwrap();
    let socket = &fixture.paths.socket;
    std::fs::write(socket.with_extension("disconnect-once"), "model/list").unwrap();
    assert!(
        daemon
            .model_list(tokio::time::Instant::now() + Duration::from_secs(2))
            .await
            .is_err()
    );
    assert!(!socket.with_extension("disconnect-once").exists());
    assert!(
        !daemon.has_connection().await,
        "control must close the real transport"
    );
    fixture.claim_installed_child().await;
    let before = fixture.row().await;
    let metadata = std::fs::metadata(socket).unwrap();
    let methods = optional_text(&socket.with_extension("methods"));
    let result = daemon
        .model_list(tokio::time::Instant::now() + Duration::from_secs(2))
        .await;
    assert_eq!(
        optional_text(&socket.with_extension("methods")),
        methods,
        "unsupported phase must refuse before reconnect initialize/model request: {result:?}"
    );
    assert!(result.is_err());
    assert_eq!(fixture.exec.pids(), vec![before.pid.unwrap()]);
    assert_eq!(
        read_proc_start_time(before.pid.unwrap()),
        before.process_start_time
    );
    assert!(
        !PathBuf::from(format!(
            "{}.term.{}",
            fixture.exec.prefix.display(),
            before.pid.unwrap()
        ))
        .exists()
    );
    let after = std::fs::metadata(socket).expect("connection refusal retains endpoint");
    assert_eq!((after.dev(), after.ino()), (metadata.dev(), metadata.ino()));
    assert!(tokio::net::UnixStream::connect(socket).await.is_ok());
    assert_same_row(&before, &fixture.row().await);
}
