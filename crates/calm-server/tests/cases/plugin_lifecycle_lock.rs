//! Acceptance suite for the per-plugin lifecycle lock. `try_lock_lifecycle` is
//! non-blocking: a refused caller is finished and must retry explicitly.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::error::CalmError;
use calm_server::event::EventBus;
use calm_server::plugin_host::lifecycle::LifecycleDb;
use calm_server::plugin_host::{
    HostError, Manifest, PluginHost, PluginListDb, PluginRegistry, PluginRuntimeStatus,
};
use serde_json::json;
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::time::{Instant, sleep};

const ECHO_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-echo");
const CRASH_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-crash");

const ID: &str = "test.lock";

struct Fx {
    host: Arc<PluginHost>,
    repo: Arc<dyn Repo>,
    plugins_dir: PathBuf,
    _tmp: TempDir,
}

fn write_plugin_with_args(plugins_dir: &Path, id: &str, stub_bin: &str, args: &[&str]) -> PathBuf {
    let dir = plugins_dir.join(id);
    let bin_dir = dir.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let link = bin_dir.join("stub");
    if !link.exists() {
        std::os::unix::fs::symlink(Path::new(stub_bin), &link).unwrap();
    }
    std::fs::write(
        dir.join("manifest.json"),
        json!({
            "manifest_version": 1,
            "id": id,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Lock Stub",
            "entrypoint": { "command": "bin/stub", "args": args },
        })
        .to_string(),
    )
    .unwrap();
    dir
}

struct BootOpts {
    stub: &'static str,
    stub_args: Vec<&'static str>,
    /// Seed the `plugins` row (and load the registry) for `ID`.
    seed: bool,
    enabled: bool,
    backoff: Option<(Vec<u64>, Duration, u32)>,
    lifecycle_db: Option<Arc<dyn LifecycleDb>>,
    /// Narrow repo read used by boot autospawn's initial plugin enumeration.
    plugin_list_db: Option<Arc<dyn PluginListDb>>,
    plugin_list_wall: Option<Duration>,
    /// Pre-built repo, for tests that construct their [`LifecycleDb`] fake around the same handle.
    repo: Option<Arc<dyn Repo>>,
    /// `config.plugins_disabled`, the operator's kill switch.
    plugins_disabled: Vec<String>,
    app_wall: Option<Duration>,
}

impl Default for BootOpts {
    fn default() -> Self {
        Self {
            stub: ECHO_BIN,
            stub_args: Vec::new(),
            seed: true,
            enabled: true,
            backoff: None,
            lifecycle_db: None,
            plugin_list_db: None,
            plugin_list_wall: None,
            repo: None,
            plugins_disabled: Vec::new(),
            app_wall: None,
        }
    }
}

async fn boot_with(opts: BootOpts) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    std::fs::create_dir_all(&plugins_data_dir).unwrap();
    let dir = write_plugin_with_args(&plugins_dir, ID, opts.stub, &opts.stub_args);

    let repo: Arc<dyn Repo> = match opts.repo.clone() {
        Some(r) => r,
        None => Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("in-memory sqlite"),
        ),
    };
    if opts.seed {
        repo.plugin_install(calm_server::model::NewPlugin {
            id: ID.into(),
            version: "0.1.0".into(),
            install_path: dir.display().to_string(),
            manifest: json!({}),
            enabled: opts.enabled,
            user_config: json!({}),
        })
        .await
        .expect("seed plugin row");
    }

    let (registry, report) = PluginRegistry::load_from_dir(&plugins_dir).unwrap();
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);

    let mut host = PluginHost::new_full(
        Arc::new(registry),
        repo.clone(),
        plugins_dir.clone(),
        plugins_data_dir,
        opts.plugins_disabled.clone(),
        EventBus::new(),
        calm_server::state::WriteContext::new(
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::track_area_cache::TrackAreaCache::new(),
        ),
    );
    if let Some((schedule, window, limit)) = opts.backoff {
        host = host.with_backoff_schedule(schedule, window, limit);
    }
    if let Some(db) = opts.lifecycle_db {
        host = host.with_lifecycle_db(db);
    }
    if let Some(db) = opts.plugin_list_db {
        host = host.with_plugin_list_db(db);
    }
    if let Some(wall) = opts.plugin_list_wall {
        host = host.with_plugin_list_wall(wall);
    }
    if let Some(wall) = opts.app_wall {
        host = host.with_app_autospawn_wall(wall);
    }
    Fx {
        host: Arc::new(host),
        repo,
        plugins_dir,
        _tmp: tmp,
    }
}

async fn boot() -> Fx {
    boot_with(BootOpts::default()).await
}

/// Holds a `BEGIN IMMEDIATE` transaction open, so every write to the database parks.
/// This blocks the whole database, not one plugin's rows; on `sqlite::memory:` readers get
/// no snapshot isolation, so it cannot serve as a window barrier.
struct DbBarrier {
    release: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl DbBarrier {
    async fn hold(repo: &Arc<dyn Repo>) -> Self {
        let (held_tx, held_rx) = oneshot::channel::<()>();
        let (release, rx) = oneshot::channel::<()>();
        let repo = repo.clone();
        let task = tokio::spawn(async move {
            let _ = repo
                .write_in_tx(Box::new(move |_tx| {
                    Box::pin(async move {
                        let _ = held_tx.send(());
                        let _ = rx.await;
                        Ok(())
                    })
                }))
                .await;
        });
        held_rx.await.expect("write tx never opened");
        Self {
            release: Some(release),
            task,
        }
    }

    async fn release(mut self) {
        let _ = self.release.take().unwrap().send(());
        let _ = self.task.await;
    }
}

/// Captures `tracing` events into a buffer for the duration of one test. Thread-local
/// (`set_default`): every test here runs on a current-thread runtime, so no other test is affected.
struct LogCapture {
    buf: SharedBuf,
    _guard: tracing::subscriber::DefaultGuard,
}

#[derive(Clone)]
struct SharedBuf(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for SharedBuf {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SharedBuf {
    type Writer = SharedBuf;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl LogCapture {
    fn install() -> Self {
        let buf = SharedBuf(Arc::new(std::sync::Mutex::new(Vec::new())));
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .with_writer(buf.clone())
            .finish();
        Self {
            buf,
            _guard: tracing::subscriber::set_default(subscriber),
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.buf.0.lock().unwrap()).into_owned()
    }

    /// Block until `needle` appears in the captured log, or fail loud.
    async fn wait_for(&self, needle: &str, timeout: Duration, why: &str) {
        let deadline = Instant::now() + timeout;
        loop {
            if self.text().contains(needle) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "never saw `{needle}` within {timeout:?} — {why}\n--- captured log ---\n{}",
                self.text()
            );
            sleep(Duration::from_millis(10)).await;
        }
    }
}

/// Block until `id`'s lifecycle lock is held by somebody else.
async fn wait_until_locked(host: &Arc<PluginHost>, id: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if host.try_lock_lifecycle(id).is_err() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "nobody ever took {id}'s lifecycle lock"
        );
        sleep(Duration::from_millis(2)).await;
    }
}

async fn wait_for_status(
    host: &Arc<PluginHost>,
    id: &str,
    pred: impl Fn(Option<&PluginRuntimeStatus>) -> bool,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    loop {
        let st = host.status(id).await.map(|s| s.status);
        if pred(st.as_ref()) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting on {id}; last status {st:?}"
        );
        sleep(Duration::from_millis(10)).await;
    }
}

/// Everything a refused operation must have left alone.
#[derive(Debug, PartialEq)]
struct Snapshot {
    row: Option<(bool, String, String)>,
    in_registry: bool,
    live: Option<String>,
    has_token: bool,
}

async fn snapshot(fx: &Fx, id: &str) -> Snapshot {
    Snapshot {
        row: fx
            .repo
            .plugin_get_by_id(id)
            .await
            .unwrap()
            .map(|p| (p.enabled, p.version, p.install_path)),
        in_registry: fx.host.registry().get(id).is_some(),
        live: fx
            .host
            .status(id)
            .await
            .map(|s| s.status.wire_name().to_string()),
        has_token: fx.repo.plugin_token_get(id).await.unwrap().is_some(),
    }
}

/// Run a lifecycle call that is expected to be refused at the entry, under a hard time bound:
/// a refusal answers immediately, whereas a call that got into the critical section parks on the winner's barrier.
async fn refused<T>(what: &str, fut: impl std::future::Future<Output = T>) -> T {
    match tokio::time::timeout(Duration::from_secs(5), fut).await {
        Ok(v) => v,
        Err(_) => panic!(
            "{what} did not answer within 5 s: it must be refused at the entry, \
             not admitted into the winner's critical section"
        ),
    }
}

fn assert_busy_calm(err: &CalmError, what: &str) {
    assert_eq!(
        err.code(),
        "plugin_busy",
        "{what}: the refusal must be distinguishable from `plugin_conflict`, got {err:?}"
    );
    assert_eq!(err.status(), axum::http::StatusCode::CONFLICT, "{what}");
}

fn assert_busy_host(err: &HostError, what: &str) {
    assert!(
        matches!(err, HostError::LifecycleBusy(_)),
        "{what}: expected LifecycleBusy, got {err:?}"
    );
}

#[tokio::test]
async fn a1_spawn_is_refused_while_the_lock_is_held() {
    let fx = boot().await;
    let before = snapshot(&fx, ID).await;
    let held = fx.host.try_lock_lifecycle(ID).expect("lock is free");

    let err = fx.host.spawn(ID).await.expect_err("spawn must be refused");
    assert_busy_host(&err, "spawn");
    assert_eq!(snapshot(&fx, ID).await, before, "a refused spawn did work");

    drop(held);
    fx.host.spawn(ID).await.expect("spawn succeeds on retry");
    assert!(matches!(
        fx.host.status(ID).await.map(|s| s.status),
        Some(PluginRuntimeStatus::Running)
    ));
    fx.host.stop(ID).await.unwrap();
}

#[tokio::test]
async fn a1_stop_is_refused_while_the_lock_is_held() {
    let fx = boot().await;
    fx.host.spawn(ID).await.unwrap();
    let before = snapshot(&fx, ID).await;
    let held = fx.host.try_lock_lifecycle(ID).expect("lock is free");

    let err = fx.host.stop(ID).await.expect_err("stop must be refused");
    assert_busy_host(&err, "stop");
    assert_eq!(snapshot(&fx, ID).await, before, "a refused stop did work");

    drop(held);
    fx.host.stop(ID).await.expect("stop succeeds on retry");
    assert!(fx.host.status(ID).await.is_none());
}

#[tokio::test]
async fn a1_restart_is_refused_while_the_lock_is_held() {
    let fx = boot().await;
    fx.host.spawn(ID).await.unwrap();
    let pid_before = fx.host.status(ID).await.unwrap().pid;
    let before = snapshot(&fx, ID).await;
    let held = fx.host.try_lock_lifecycle(ID).expect("lock is free");

    let err = fx
        .host
        .restart(ID)
        .await
        .expect_err("restart must be refused");
    assert_busy_host(&err, "restart");
    assert_eq!(snapshot(&fx, ID).await, before);
    assert_eq!(
        fx.host.status(ID).await.unwrap().pid,
        pid_before,
        "a refused restart must not have replaced the process"
    );

    drop(held);
    fx.host
        .restart(ID)
        .await
        .expect("restart succeeds on retry");
    assert_ne!(
        fx.host.status(ID).await.unwrap().pid,
        pid_before,
        "the retry must really have restarted the child"
    );
    fx.host.stop(ID).await.unwrap();
}

#[tokio::test]
async fn a1_rotate_token_is_refused_while_the_lock_is_held() {
    let fx = boot().await;
    fx.host.spawn(ID).await.unwrap();
    let token_before = fx.repo.plugin_token_get(ID).await.unwrap();
    assert!(token_before.is_some(), "spawn mints a token");
    let held = fx.host.try_lock_lifecycle(ID).expect("lock is free");

    let err = fx
        .host
        .rotate_plugin_token(ID)
        .await
        .expect_err("rotation must be refused");
    assert_busy_host(&err, "rotate_plugin_token");
    assert_eq!(
        fx.repo.plugin_token_get(ID).await.unwrap(),
        token_before,
        "a refused rotation must not have deleted the token row"
    );

    drop(held);
    fx.host
        .rotate_plugin_token(ID)
        .await
        .expect("rotation succeeds on retry");
    assert_ne!(
        fx.repo.plugin_token_get(ID).await.unwrap(),
        token_before,
        "the retry must really have minted a new token"
    );
    fx.host.stop(ID).await.unwrap();
}

#[tokio::test]
async fn a1_install_is_refused_while_the_lock_is_held() {
    // Nothing installed yet: the guard is taken on an id that is about to exist.
    let fx = boot_with(BootOpts {
        seed: false,
        ..Default::default()
    })
    .await;
    let src = fx.plugins_dir.join(ID);
    let manifest =
        Manifest::parse(&std::fs::read_to_string(src.join("manifest.json")).unwrap()).unwrap();
    let held = fx.host.try_lock_lifecycle(ID).expect("lock is free");

    let err = fx
        .host
        .install(manifest.clone(), &src)
        .await
        .expect_err("install must be refused");
    assert_busy_calm(&err, "install");
    assert!(
        fx.repo.plugin_get_by_id(ID).await.unwrap().is_none(),
        "a refused install must not have written the row"
    );

    drop(held);
    let plug = fx
        .host
        .install(manifest, &src)
        .await
        .expect("install succeeds on retry");
    assert_eq!(plug.id, ID);
    assert!(!plug.enabled, "install leaves the plugin disabled");
}

/// The guard sits after the read-only min-kernel check, before the duplicate probe.
#[tokio::test]
async fn a1_install_reports_kernel_too_old_even_when_the_id_is_busy() {
    let fx = boot_with(BootOpts {
        seed: false,
        ..Default::default()
    })
    .await;
    let src = fx.plugins_dir.join(ID);
    let mut manifest =
        Manifest::parse(&std::fs::read_to_string(src.join("manifest.json")).unwrap()).unwrap();
    manifest.min_kernel_version = "99.0.0".into();

    let _held = fx.host.try_lock_lifecycle(ID).expect("lock is free");
    let err = fx.host.install(manifest, &src).await.expect_err("refused");
    assert_eq!(
        err.code(),
        "plugin_kernel_too_old",
        "the min-kernel verdict must not be masked by the lock: {err:?}"
    );
}

#[tokio::test]
async fn a1_enable_is_refused_while_the_lock_is_held() {
    let fx = boot_with(BootOpts {
        enabled: false,
        ..Default::default()
    })
    .await;
    let before = snapshot(&fx, ID).await;
    let held = fx.host.try_lock_lifecycle(ID).expect("lock is free");

    let err = fx
        .host
        .enable(ID)
        .await
        .expect_err("enable must be refused");
    assert_busy_calm(&err, "enable");
    assert_eq!(snapshot(&fx, ID).await, before, "a refused enable did work");

    drop(held);
    let plug = fx.host.enable(ID).await.expect("enable succeeds on retry");
    assert!(plug.enabled);
    assert!(matches!(
        fx.host.status(ID).await.map(|s| s.status),
        Some(PluginRuntimeStatus::Running)
    ));
    fx.host.stop(ID).await.unwrap();
}

#[tokio::test]
async fn a1_disable_is_refused_while_the_lock_is_held() {
    let fx = boot().await;
    fx.host.spawn(ID).await.unwrap();
    let before = snapshot(&fx, ID).await;
    let held = fx.host.try_lock_lifecycle(ID).expect("lock is free");

    let err = fx
        .host
        .disable(ID)
        .await
        .expect_err("disable must be refused");
    assert_busy_calm(&err, "disable");
    assert_eq!(
        snapshot(&fx, ID).await,
        before,
        "a refused disable did work"
    );

    drop(held);
    let plug = fx
        .host
        .disable(ID)
        .await
        .expect("disable succeeds on retry");
    assert!(!plug.enabled);
    assert!(fx.host.status(ID).await.is_none());
}

#[tokio::test]
async fn a1_a11_uninstall_is_refused_fail_closed_while_the_lock_is_held() {
    let fx = boot().await;
    fx.host.spawn(ID).await.unwrap();
    let before = snapshot(&fx, ID).await;
    assert!(before.has_token && before.in_registry && before.row.is_some());
    let held = fx.host.try_lock_lifecycle(ID).expect("lock is free");

    let err = fx
        .host
        .uninstall(ID)
        .await
        .expect_err("uninstall must be refused");
    assert_busy_calm(&err, "uninstall");
    assert_eq!(
        snapshot(&fx, ID).await,
        before,
        "a refused uninstall must leave row, registry, token and live entry intact"
    );

    drop(held);
    fx.host.uninstall(ID).await.expect("uninstall on retry");
    let after = snapshot(&fx, ID).await;
    assert_eq!(after.row, None);
    assert!(!after.in_registry);
    assert!(!after.has_token);
    assert_eq!(after.live, None);
}

#[tokio::test]
async fn a1_reload_is_refused_while_the_lock_is_held() {
    let fx = boot().await;
    fx.host.spawn(ID).await.unwrap();
    let before = snapshot(&fx, ID).await;
    let held = fx.host.try_lock_lifecycle(ID).expect("lock is free");

    let err = fx
        .host
        .reload(ID)
        .await
        .expect_err("reload must be refused");
    assert_busy_calm(&err, "reload");
    assert_eq!(snapshot(&fx, ID).await, before, "a refused reload did work");

    drop(held);
    fx.host.reload(ID).await.expect("reload succeeds on retry");
    assert!(matches!(
        fx.host.status(ID).await.map(|s| s.status),
        Some(PluginRuntimeStatus::Running)
    ));
    fx.host.stop(ID).await.unwrap();
}

/// The re-entrancy gate: under non-blocking semantics a `*_under` body that re-entered a
/// lock-taking wrapper returns a silent 409 instead of deadlocking.
#[tokio::test]
async fn a12b_no_entry_point_returns_busy_without_contention() {
    let fx = boot_with(BootOpts {
        enabled: false,
        ..Default::default()
    })
    .await;

    macro_rules! not_busy {
        ($what:literal, $e:expr) => {
            match $e {
                Err(e) => assert_ne!(
                    e.code(),
                    "plugin_busy",
                    concat!($what, " answered plugin_busy with no competitor")
                ),
                Ok(_) => {}
            }
        };
    }

    not_busy!("enable", fx.host.enable(ID).await);
    not_busy!("reload", fx.host.reload(ID).await);
    not_busy!("disable", fx.host.disable(ID).await);

    for (what, res) in [
        ("spawn", fx.host.spawn(ID).await),
        ("restart", fx.host.restart(ID).await),
        ("rotate_plugin_token", fx.host.rotate_plugin_token(ID).await),
        ("stop", fx.host.stop(ID).await),
    ] {
        if let Err(e) = res {
            assert!(
                !matches!(e, HostError::LifecycleBusy(_)),
                "{what} answered LifecycleBusy with no competitor"
            );
        }
    }

    not_busy!("uninstall", fx.host.uninstall(ID).await);
}

#[tokio::test]
async fn a12a_every_pair_of_entry_points_settles() {
    #[derive(Clone, Copy, Debug)]
    enum Op {
        Spawn,
        Stop,
        Restart,
        Rotate,
        Enable,
        Disable,
        Reload,
        Uninstall,
    }
    const ALL: [Op; 8] = [
        Op::Spawn,
        Op::Stop,
        Op::Restart,
        Op::Rotate,
        Op::Enable,
        Op::Disable,
        Op::Reload,
        Op::Uninstall,
    ];

    async fn run(host: Arc<PluginHost>, op: Op) -> Result<(), String> {
        match op {
            Op::Spawn => host.spawn(ID).await.map_err(|e| e.to_string()),
            Op::Stop => host.stop(ID).await.map_err(|e| e.to_string()),
            Op::Restart => host.restart(ID).await.map_err(|e| e.to_string()),
            Op::Rotate => host
                .rotate_plugin_token(ID)
                .await
                .map_err(|e| e.to_string()),
            Op::Enable => host.enable(ID).await.map(|_| ()).map_err(|e| e.to_string()),
            Op::Disable => host
                .disable(ID)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string()),
            Op::Reload => host.reload(ID).await.map(|_| ()).map_err(|e| e.to_string()),
            Op::Uninstall => host.uninstall(ID).await.map_err(|e| e.to_string()),
        }
    }

    for a in ALL {
        for b in ALL {
            let fx = boot().await;
            fx.host.spawn(ID).await.unwrap();
            let (ha, hb) = (Arc::clone(&fx.host), Arc::clone(&fx.host));
            let ta = tokio::spawn(async move { run(ha, a).await });
            let tb = tokio::spawn(async move { run(hb, b).await });
            let both = async { (ta.await.unwrap(), tb.await.unwrap()) };
            let (ra, rb) = tokio::time::timeout(Duration::from_secs(20), both)
                .await
                .unwrap_or_else(|_| panic!("{a:?} + {b:?} did not settle — deadlock"));

            // At most one of the pair may report a busy lock, because at most one can lose a two-way race.
            let busy = [&ra, &rb]
                .iter()
                .filter(|r| matches!(r, Err(m) if m.contains("busy")))
                .count();
            assert!(
                busy <= 1,
                "{a:?} + {b:?}: both reported busy ({ra:?}, {rb:?})"
            );

            let live = fx.host.status(ID).await.map(|s| s.status);
            assert!(
                matches!(
                    live,
                    None | Some(PluginRuntimeStatus::Running)
                        | Some(PluginRuntimeStatus::Crashed { .. })
                ),
                "{a:?} + {b:?} left a torn state: {live:?}"
            );

            let row = fx.repo.plugin_get_by_id(ID).await.unwrap();
            let in_registry = fx.host.registry().get(ID).is_some();
            let running = matches!(live, Some(PluginRuntimeStatus::Running));

            if row.is_none() {
                assert!(
                    !in_registry,
                    "{a:?} + {b:?}: the row is gone but the registry still has \
                     the manifest — `GET /api/plugins` and the DB disagree"
                );
                assert!(
                    !running,
                    "{a:?} + {b:?}: the row is gone but the plugin is still \
                     Running — a live child with no row behind it"
                );
            }

            if let Some(p) = row.as_ref() {
                assert!(
                    p.enabled || !running,
                    "{a:?} + {b:?} left a TORN terminal: the row says \
                     `enabled = false` and the runtime says Running. Nothing \
                     reconciles that — the next boot's autospawn skips the \
                     plugin *because* it is disabled. ({ra:?}, {rb:?})"
                );
                assert!(
                    in_registry,
                    "{a:?} + {b:?}: the row survived but the registry entry did \
                     not, so the plugin can never be spawned again"
                );
            }

            let _ = fx.host.stop(ID).await;
        }
    }
}

/// Barrier: a held DB write transaction parks the app spawn at its first repo write (the token mint), inside its guard.
#[tokio::test]
async fn a5_uninstall_is_refused_while_an_app_spawn_is_in_flight() {
    let fx = boot().await;
    let barrier = DbBarrier::hold(&fx.repo).await;

    let h = Arc::clone(&fx.host);
    let spawning = tokio::spawn(async move { h.spawn(ID).await });
    wait_until_locked(&fx.host, ID).await;

    let before = snapshot(&fx, ID).await;
    let err = refused("uninstall", fx.host.uninstall(ID))
        .await
        .expect_err("uninstall must be refused mid-spawn");
    assert_busy_calm(&err, "uninstall vs in-flight spawn");
    assert_eq!(
        snapshot(&fx, ID).await,
        before,
        "fail-closed: row / registry / token must all survive the refusal"
    );

    barrier.release().await;
    tokio::time::timeout(Duration::from_secs(20), spawning)
        .await
        .expect("spawn never returned")
        .expect("spawn task panicked")
        .expect("the spawn itself must still succeed");

    fx.host.uninstall(ID).await.expect("uninstall on retry");
    let after = snapshot(&fx, ID).await;
    assert_eq!(after.row, None);
    assert!(!after.in_registry);
    assert!(!after.has_token);
    assert_eq!(after.live, None, "no admission reservation may survive");
}

/// Barrier: the held DB transaction pins the winner inside its critical section; without it
/// the loser would simply get `plugin_conflict` on the first call.
#[tokio::test]
async fn a6_concurrent_installs_of_one_id_give_busy_then_conflict() {
    let fx = boot_with(BootOpts {
        seed: false,
        ..Default::default()
    })
    .await;
    let src = fx.plugins_dir.join(ID);
    let manifest =
        Manifest::parse(&std::fs::read_to_string(src.join("manifest.json")).unwrap()).unwrap();

    let barrier = DbBarrier::hold(&fx.repo).await;
    let (h, m, s) = (Arc::clone(&fx.host), manifest.clone(), src.clone());
    let winner = tokio::spawn(async move { h.install(m, &s).await });
    wait_until_locked(&fx.host, ID).await;

    let err = refused("install", fx.host.install(manifest.clone(), &src))
        .await
        .expect_err("the loser must be refused");
    assert_busy_calm(&err, "concurrent install");
    assert_ne!(
        err.code(),
        "plugin_conflict",
        "the loser never reached the duplicate-id probe; conflating the two \
         codes tells a retryable client to give up"
    );

    barrier.release().await;
    let plug = tokio::time::timeout(Duration::from_secs(20), winner)
        .await
        .expect("winner never returned")
        .expect("winner panicked")
        .expect("the winner's install must succeed");
    assert_eq!(plug.id, ID);
    let install_path = plug.install_path.clone();

    let err = fx
        .host
        .install(manifest, &src)
        .await
        .expect_err("retrying the loser must hit the duplicate-id refusal");
    assert_eq!(err.code(), "plugin_conflict", "{err:?}");

    let row = fx.repo.plugin_get_by_id(ID).await.unwrap().unwrap();
    assert_eq!(
        row.install_path, install_path,
        "the loser must not have overwritten the winner's row"
    );
    assert_eq!(row.version, plug.version);
}

#[tokio::test]
async fn a7_disable_overlapping_an_enable_is_refused_then_works() {
    let fx = boot_with(BootOpts {
        enabled: false,
        ..Default::default()
    })
    .await;
    let barrier = DbBarrier::hold(&fx.repo).await;
    let h = Arc::clone(&fx.host);
    let enabling = tokio::spawn(async move { h.enable(ID).await });
    wait_until_locked(&fx.host, ID).await;

    let err = refused("disable", fx.host.disable(ID))
        .await
        .expect_err("disable must be refused inside enable's critical section");
    assert_busy_calm(&err, "disable vs enable");
    assert!(
        !fx.repo.plugin_get_by_id(ID).await.unwrap().unwrap().enabled,
        "the refused disable must not have touched the enabled bit \
         (and enable's own write is still uncommitted)"
    );

    barrier.release().await;
    let plug = tokio::time::timeout(Duration::from_secs(20), enabling)
        .await
        .expect("enable never returned")
        .expect("enable panicked")
        .expect("enable must succeed");
    assert!(plug.enabled);
    assert!(matches!(
        fx.host.status(ID).await.map(|s| s.status),
        Some(PluginRuntimeStatus::Running)
    ));

    let plug = fx.host.disable(ID).await.expect("disable on retry");
    assert!(!plug.enabled);
    assert_eq!(fx.host.status(ID).await.map(|s| s.status), None);
}

#[tokio::test]
async fn a7_enable_overlapping_a_disable_is_refused_then_works() {
    let fx = boot().await;
    fx.host.spawn(ID).await.unwrap();
    let barrier = DbBarrier::hold(&fx.repo).await;
    let h = Arc::clone(&fx.host);
    let disabling = tokio::spawn(async move { h.disable(ID).await });
    wait_until_locked(&fx.host, ID).await;

    let err = refused("enable", fx.host.enable(ID))
        .await
        .expect_err("enable must be refused inside disable's critical section");
    assert_busy_calm(&err, "enable vs disable");

    barrier.release().await;
    let plug = tokio::time::timeout(Duration::from_secs(20), disabling)
        .await
        .expect("disable never returned")
        .expect("disable panicked")
        .expect("disable must succeed");
    assert!(!plug.enabled);
    assert_eq!(fx.host.status(ID).await.map(|s| s.status), None);

    let plug = fx.host.enable(ID).await.expect("enable on retry");
    assert!(plug.enabled);
    assert!(matches!(
        fx.host.status(ID).await.map(|s| s.status),
        Some(PluginRuntimeStatus::Running)
    ));
    fx.host.stop(ID).await.unwrap();
}

/// Persisted `plugin.state` wire names for `ID`, oldest first.
async fn state_events(fx: &Fx) -> Vec<String> {
    fx.repo
        .events_since(0, 1000)
        .await
        .expect("events")
        .into_iter()
        .filter_map(|(_, _, _, event)| match event {
            calm_server::event::Event::PluginState { id, state, .. } if id == ID => Some(state),
            _ => None,
        })
        .collect()
}

async fn wait_for_events(fx: &Fx, pred: impl Fn(&[String]) -> bool, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let ev = state_events(fx).await;
        if pred(&ev) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting on the event stream; saw {ev:?}"
        );
        sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a9_backoff_does_not_hold_the_lock_and_does_not_resurrect() {
    const BACKOFF: u64 = 2_000;
    let fx = boot_with(BootOpts {
        stub: CRASH_BIN,
        backoff: Some((vec![BACKOFF], Duration::from_secs(300), 50)),
        ..Default::default()
    })
    .await;

    fx.host.spawn(ID).await.expect("spawn");
    wait_for_status(
        &fx.host,
        ID,
        |s| matches!(s, Some(PluginRuntimeStatus::Crashed { .. })),
        Duration::from_secs(10),
    )
    .await;

    let started = Instant::now();
    fx.host.disable(ID).await.expect("disable during backoff");
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(BACKOFF / 2),
        "disable waited on the backoff sleep ({elapsed:?}); the sleep must be \
         outside the lifecycle guard"
    );

    // Wait out the FULL backoff plus slack, then check nothing came back.
    sleep(Duration::from_millis(BACKOFF + 1_500)).await;
    assert!(
        !fx.repo.plugin_get_by_id(ID).await.unwrap().unwrap().enabled,
        "disable must have persisted"
    );
    assert_eq!(
        fx.host.status(ID).await.map(|s| s.status),
        None,
        "the supervisor must not have respawned a disabled plugin"
    );
    let ev = state_events(&fx).await;
    assert_eq!(
        ev.last().map(String::as_str),
        Some("disabled"),
        "the event log's last word must be `disabled`: {ev:?}"
    );
}

#[tokio::test]
async fn a10_uninstall_during_backoff_prevents_the_respawn() {
    const BACKOFF: u64 = 2_000;
    let fx = boot_with(BootOpts {
        stub: CRASH_BIN,
        backoff: Some((vec![BACKOFF], Duration::from_secs(300), 50)),
        ..Default::default()
    })
    .await;

    fx.host.spawn(ID).await.expect("spawn");
    wait_for_status(
        &fx.host,
        ID,
        |s| matches!(s, Some(PluginRuntimeStatus::Crashed { .. })),
        Duration::from_secs(10),
    )
    .await;

    fx.host
        .uninstall(ID)
        .await
        .expect("uninstall during backoff");

    sleep(Duration::from_millis(BACKOFF + 1_500)).await;
    assert_eq!(fx.host.status(ID).await.map(|s| s.status), None);
    assert!(fx.repo.plugin_get_by_id(ID).await.unwrap().is_none());
    assert!(fx.host.registry().get(ID).is_none());
}

/// The crash stub exits the moment the handshake completes, before `spawn_under` has finished,
/// so the supervisor's `child.wait()` returns while the guard is held, by construction.
#[tokio::test]
async fn a14a_a_crash_inside_the_spawns_own_guard_is_still_accounted() {
    let fx = boot_with(BootOpts {
        stub: CRASH_BIN,
        // The stub sleeps before answering `initialize`, which makes the window below a real window rather than a race.
        stub_args: vec!["--delay-ms=600"],
        // Long enough that the respawn cannot mask the assertion.
        backoff: Some((vec![60_000], Duration::from_secs(300), 50)),
        ..Default::default()
    })
    .await;

    let h = Arc::clone(&fx.host);
    let spawning = tokio::spawn(async move { h.spawn(ID).await });

    // Once `spawning` is on the wire the token mint is done; the only repo write left inside the guard is the `running` emission.
    wait_for_events(
        &fx,
        |ev| ev.contains(&"spawning".to_string()),
        Duration::from_secs(10),
    )
    .await;
    let barrier = DbBarrier::hold(&fx.repo).await;

    wait_for_status(
        &fx.host,
        ID,
        |s| matches!(s, Some(PluginRuntimeStatus::Running)),
        Duration::from_secs(10),
    )
    .await;
    // Give the supervisor time to observe the exit and reach the lock.
    sleep(Duration::from_millis(300)).await;
    assert!(
        !spawning.is_finished(),
        "the spawn must still be inside its guard — otherwise this test never \
         exercised the collision it is named for"
    );

    barrier.release().await;
    tokio::time::timeout(Duration::from_secs(20), spawning)
        .await
        .expect("spawn never returned")
        .expect("spawn task panicked")
        .expect("spawn itself succeeds; the child died after the handshake");

    wait_for_status(
        &fx.host,
        ID,
        |s| matches!(s, Some(PluginRuntimeStatus::Crashed { .. })),
        Duration::from_secs(15),
    )
    .await;
    wait_for_events(
        &fx,
        |ev| ev.last().map(String::as_str) == Some("crashed"),
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(
        state_events(&fx).await,
        vec!["spawning", "running", "crashed"],
        "the crash must have been accounted and announced exactly once"
    );
}

#[tokio::test]
async fn a14b_a_busy_lock_at_the_end_of_backoff_does_not_strand_the_plugin() {
    let fx = boot_with(BootOpts {
        stub: CRASH_BIN,
        backoff: Some((vec![800], Duration::from_secs(300), 50)),
        ..Default::default()
    })
    .await;

    fx.host.spawn(ID).await.expect("spawn");
    wait_for_status(
        &fx.host,
        ID,
        |s| matches!(s, Some(PluginRuntimeStatus::Crashed { .. })),
        Duration::from_secs(10),
    )
    .await;

    // Hold the lock straight across the moment the backoff elapses.
    let held = fx
        .host
        .try_lock_lifecycle(ID)
        .expect("lock is free mid-backoff");
    sleep(Duration::from_millis(1_600)).await;
    let ev = state_events(&fx).await;
    assert_eq!(
        ev.iter().filter(|s| *s == "running").count(),
        1,
        "nothing may respawn while the lock is held: {ev:?}"
    );
    drop(held);

    wait_for_events(
        &fx,
        |ev| ev.iter().filter(|s| *s == "running").count() >= 2,
        Duration::from_secs(15),
    )
    .await;
}

/// Fake [`LifecycleDb`] with a one-shot read failure and a pause gate that holds the failure
/// window open until the test closes it.
struct FaultyDb {
    repo: Arc<dyn Repo>,
    fail_next: AtomicBool,
    failures: AtomicUsize,
    paused: AtomicBool,
    resume: tokio::sync::Notify,
}

impl FaultyDb {
    fn new(repo: Arc<dyn Repo>) -> Arc<Self> {
        Arc::new(Self {
            repo,
            fail_next: AtomicBool::new(false),
            failures: AtomicUsize::new(0),
            paused: AtomicBool::new(false),
            resume: tokio::sync::Notify::new(),
        })
    }

    /// Arm one read failure and hold every read after it until [`Self::resume`].
    fn arm(&self) {
        self.paused.store(true, Ordering::SeqCst);
        self.fail_next.store(true, Ordering::SeqCst);
    }

    async fn wait_failure_consumed(&self) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while self.failures.load(Ordering::SeqCst) == 0 {
            assert!(
                Instant::now() < deadline,
                "the injected read failure was never consumed — the supervisor \
                 never reached its third segment, so this test proved nothing"
            );
            sleep(Duration::from_millis(10)).await;
        }
    }

    fn release(&self) {
        self.paused.store(false, Ordering::SeqCst);
        self.resume.notify_waiters();
    }
}

#[async_trait]
impl LifecycleDb for FaultyDb {
    async fn enabled_row(&self, id: &str) -> Result<Option<bool>, CalmError> {
        if self.fail_next.swap(false, Ordering::SeqCst) {
            self.failures.fetch_add(1, Ordering::SeqCst);
            return Err(CalmError::Internal(
                "injected plugin-row read failure".into(),
            ));
        }
        while self.paused.load(Ordering::SeqCst) {
            self.resume.notified().await;
        }
        Ok(self.repo.plugin_get_by_id(id).await?.map(|p| p.enabled))
    }

    async fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), CalmError> {
        self.repo.plugin_update_enabled(id, enabled).await?;
        Ok(())
    }
}

#[tokio::test]
async fn a15a_a_plugin_row_read_failure_defers_the_respawn_and_recovery_resumes_it() {
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let faulty = FaultyDb::new(repo.clone());
    let fx = boot_with(BootOpts {
        stub: CRASH_BIN,
        backoff: Some((vec![400], Duration::from_secs(300), 50)),
        repo: Some(repo),
        lifecycle_db: Some(faulty.clone()),
        ..Default::default()
    })
    .await;

    faulty.arm();
    fx.host.spawn(ID).await.expect("spawn");
    faulty.wait_failure_consumed().await;

    // The failure window: the plugin stays exactly where the supervisor left it.
    sleep(Duration::from_millis(600)).await;
    assert!(
        matches!(
            fx.host.status(ID).await.map(|s| s.status),
            Some(PluginRuntimeStatus::Crashed { .. })
        ),
        "a read failure must not be treated as `probably still enabled`"
    );
    assert_eq!(
        state_events(&fx)
            .await
            .iter()
            .filter(|s| *s == "running")
            .count(),
        1,
        "nothing may have respawned while the row could not be read"
    );

    faulty.release();
    wait_for_events(
        &fx,
        |ev| ev.iter().filter(|s| *s == "running").count() >= 2,
        Duration::from_secs(15),
    )
    .await;
}

/// The plugin is disabled through a direct `repo` write that bypasses the host, so `live` and
/// `run_epoch` are untouched and only the DB read can stop the respawn.
#[tokio::test]
async fn a15b_a_read_failure_never_respawns_a_plugin_the_db_says_is_disabled() {
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let faulty = FaultyDb::new(repo.clone());
    let fx = boot_with(BootOpts {
        stub: CRASH_BIN,
        backoff: Some((vec![400], Duration::from_secs(300), 50)),
        repo: Some(repo),
        lifecycle_db: Some(faulty.clone()),
        ..Default::default()
    })
    .await;

    faulty.arm();
    fx.host.spawn(ID).await.expect("spawn");
    faulty.wait_failure_consumed().await;

    fx.repo
        .plugin_update_enabled(ID, false)
        .await
        .expect("bypass write");

    faulty.release();
    sleep(Duration::from_secs(2)).await;
    let ev = state_events(&fx).await;
    assert_eq!(
        ev.iter().filter(|s| *s == "running").count(),
        1,
        "the plugin must NEVER come back: the row says disabled, and the epoch \
         proves only that the runtime instance was not replaced — not that it \
         is still enabled. Saw {ev:?}"
    );
    assert!(
        matches!(
            fx.host.status(ID).await.map(|s| s.status),
            Some(PluginRuntimeStatus::Crashed { .. })
        ),
        "and it stays observably Crashed rather than vanishing"
    );
}

/// A [`LifecycleDb`] that samples `PluginHost::status` at the instant `set_enabled` is called.
/// A DB barrier cannot witness this ordering on `sqlite::memory:`: readers have no snapshot isolation.
struct OrderProbe {
    repo: Arc<dyn Repo>,
    host: std::sync::OnceLock<std::sync::Weak<PluginHost>>,
    seen: std::sync::Mutex<Vec<(bool, Option<String>)>>,
}

impl OrderProbe {
    fn new(repo: Arc<dyn Repo>) -> Arc<Self> {
        Arc::new(Self {
            repo,
            host: std::sync::OnceLock::new(),
            seen: std::sync::Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl LifecycleDb for OrderProbe {
    async fn enabled_row(&self, id: &str) -> Result<Option<bool>, CalmError> {
        Ok(self.repo.plugin_get_by_id(id).await?.map(|p| p.enabled))
    }

    async fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), CalmError> {
        let host = self
            .host
            .get()
            .and_then(|w| w.upgrade())
            .expect("host wired before use");
        let observed = host
            .status(id)
            .await
            .map(|s| s.status.wire_name().to_string());
        self.seen.lock().unwrap().push((enabled, observed));
        self.repo.plugin_update_enabled(id, enabled).await?;
        Ok(())
    }
}

#[tokio::test]
async fn a16_disable_stops_the_plugin_before_it_writes_the_row() {
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let probe = OrderProbe::new(repo.clone());
    let fx = boot_with(BootOpts {
        repo: Some(repo),
        lifecycle_db: Some(probe.clone()),
        ..Default::default()
    })
    .await;
    probe.host.set(Arc::downgrade(&fx.host)).ok();

    fx.host.spawn(ID).await.expect("spawn");
    assert!(matches!(
        fx.host.status(ID).await.map(|s| s.status),
        Some(PluginRuntimeStatus::Running)
    ));

    fx.host.disable(ID).await.expect("disable");

    let seen = probe.seen.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec![(false, None)],
        "at the instant the row is written, the plugin must already be stopped: \
         writing first leaves `enabled = false` beside a running plugin whenever \
         the stop then fails, and the next boot skips it *because* it is disabled"
    );
}

/// The revive is an explicit `spawn`, not `reload`/`restart`: those abort the sleeping supervisor,
/// whereas a spawn over a `Crashed` entry replaces the live entry and leaves the old supervisor sleeping.
#[tokio::test]
async fn a9b_a_late_supervisor_leaves_a_newer_run_instance_alone() {
    // Both supervisors start their BACKOFF sleep right after emitting their own `crashed`, so an observed `crashed` is a barrier for that sleep.
    const BACKOFF: u64 = 3_000;
    const STAGGER: u64 = 1_500;
    /// The stale supervisor's give-up line, verbatim from `respawn_after_backoff`.
    const STALE_GAVE_UP: &str = "backoff elapsed but the run instance is gone or has moved on";
    let capture = LogCapture::install();
    let fx = boot_with(BootOpts {
        stub: CRASH_BIN,
        backoff: Some((vec![BACKOFF], Duration::from_secs(300), 50)),
        ..Default::default()
    })
    .await;

    fx.host.spawn(ID).await.expect("spawn");
    // Wait for the observed first crash before staggering, so `STAGGER` is measured from the stale supervisor's sleep.
    wait_for_events(
        &fx,
        |ev| ev.iter().filter(|s| *s == "crashed").count() >= 1,
        Duration::from_secs(10),
    )
    .await;
    let crashed1_at = Instant::now();
    wait_for_status(
        &fx.host,
        ID,
        |s| matches!(s, Some(PluginRuntimeStatus::Crashed { .. })),
        Duration::from_secs(10),
    )
    .await;

    sleep(Duration::from_millis(STAGGER)).await;
    let revive_at = Instant::now();
    fx.host.spawn(ID).await.expect("explicit revive");
    wait_for_events(
        &fx,
        |ev| ev.iter().filter(|s| *s == "crashed").count() >= 2,
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(
        state_events(&fx)
            .await
            .iter()
            .filter(|s| *s == "running")
            .count(),
        2,
        "setup: exactly the two spawns this test made"
    );

    capture
        .wait_for(
            STALE_GAVE_UP,
            Duration::from_millis(BACKOFF) + Duration::from_secs(10),
            "the stale supervisor never woke from its backoff and reached the \
             epoch check, so the assertion below would say nothing",
        )
        .await;
    // Upper end: the newer supervisor's `crashed` cannot precede its own spawn, so its deadline is at or after `revive_at + BACKOFF`.
    assert!(
        revive_at.elapsed() < Duration::from_millis(BACKOFF),
        "the sample drifted past the NEWER supervisor's earliest possible \
         deadline ({:?} since the revive spawn); the window is gone",
        revive_at.elapsed()
    );

    let ev = state_events(&fx).await;
    assert_eq!(
        ev.iter().filter(|s| *s == "running").count(),
        2,
        "the stale supervisor has woken and given up (observed, {:?} after its \
         own `crashed`) and it must have left the newer run instance alone; \
         only `run_epoch` tells the two apart here — status, crash_attempt and \
         stopping are identical. Saw {ev:?}",
        crashed1_at.elapsed()
    );

    wait_for_events(
        &fx,
        |ev| ev.iter().filter(|s| *s == "running").count() >= 3,
        Duration::from_secs(15),
    )
    .await;
}

#[tokio::test]
async fn a19_a_wedged_lifecycle_lock_cannot_hang_boot() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    std::fs::create_dir_all(&plugins_data_dir).unwrap();
    let dir = write_plugin_with_args(&plugins_dir, ID, ECHO_BIN, &[]);

    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    repo.plugin_install(calm_server::model::NewPlugin {
        id: ID.into(),
        version: "0.1.0".into(),
        install_path: dir.display().to_string(),
        manifest: json!({}),
        enabled: true,
        user_config: json!({}),
    })
    .await
    .unwrap();

    let (registry, _) = PluginRegistry::load_from_dir(&plugins_dir).unwrap();
    let host = Arc::new(
        PluginHost::new_full(
            Arc::new(registry),
            repo.clone(),
            plugins_dir,
            plugins_data_dir,
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::track_area_cache::TrackAreaCache::new(),
            ),
        )
        .with_app_autospawn_wall(Duration::from_millis(300)),
    );

    // Wedge it. Nothing in this test ever drops the guard.
    let _wedged = host.try_lock_lifecycle(ID).expect("lock is free");

    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(10), host.autospawn_enabled())
        .await
        .expect(
            "boot never returned: an `app` plugin whose lifecycle lock is held \
             hangs autospawn forever without a fence",
        );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "boot took {:?}; the fence is supposed to be ~300 ms here",
        started.elapsed()
    );
}

struct WedgedPluginListDb;

#[async_trait]
impl PluginListDb for WedgedPluginListDb {
    async fn plugins_list_all(&self) -> Result<Vec<calm_server::model::Plugin>, CalmError> {
        std::future::pending().await
    }
}

#[tokio::test]
async fn a22_a_wedged_plugin_list_cannot_hang_boot() {
    const WALL: Duration = Duration::from_millis(300);
    let fx = boot_with(BootOpts {
        plugin_list_db: Some(Arc::new(WedgedPluginListDb)),
        plugin_list_wall: Some(WALL),
        ..Default::default()
    })
    .await;

    let started = Instant::now();
    tokio::time::timeout(
        Duration::from_secs(5),
        fx.host.autospawn_enabled_within(Duration::from_millis(100)),
    )
    .await
    .expect("boot never returned: `plugins_list_all` is outside every autospawn fence");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= WALL,
        "boot returned in {elapsed:?}, before the wedged list read's {WALL:?} fence fired"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "boot took {elapsed:?} against a {WALL:?} plugin-list wall"
    );
}

/// A [`LifecycleDb`] that runs a full `disable` inside `reload`'s pre-guard existence probe,
/// then answers the probe with the value that was true before it did.
struct StaleProbeWindow {
    repo: Arc<dyn Repo>,
    host: std::sync::OnceLock<std::sync::Weak<PluginHost>>,
    /// One-shot: only the first probe opens the window.
    armed: AtomicBool,
    fired: AtomicBool,
}

impl StaleProbeWindow {
    fn new(repo: Arc<dyn Repo>) -> Arc<Self> {
        Arc::new(Self {
            repo,
            host: std::sync::OnceLock::new(),
            armed: AtomicBool::new(false),
            fired: AtomicBool::new(false),
        })
    }
}

#[async_trait]
impl LifecycleDb for StaleProbeWindow {
    async fn enabled_row(&self, id: &str) -> Result<Option<bool>, CalmError> {
        let before = self.repo.plugin_get_by_id(id).await?.map(|p| p.enabled);
        if self.armed.swap(false, Ordering::SeqCst) {
            let host = self
                .host
                .get()
                .and_then(|w| w.upgrade())
                .expect("host wired before use");
            host.disable(id).await.expect("the racing disable must win");
            self.fired.store(true, Ordering::SeqCst);
        }
        Ok(before)
    }

    async fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), CalmError> {
        self.repo.plugin_update_enabled(id, enabled).await?;
        Ok(())
    }
}

#[tokio::test]
async fn a17_reload_decides_on_the_row_it_reads_inside_its_guard() {
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let window = StaleProbeWindow::new(repo.clone());
    let fx = boot_with(BootOpts {
        repo: Some(repo),
        lifecycle_db: Some(window.clone()),
        ..Default::default()
    })
    .await;
    window.host.set(Arc::downgrade(&fx.host)).ok();

    fx.host.spawn(ID).await.expect("spawn");
    assert!(matches!(
        fx.host.status(ID).await.map(|s| s.status),
        Some(PluginRuntimeStatus::Running)
    ));

    window.armed.store(true, Ordering::SeqCst);
    fx.host.reload(ID).await.expect("reload");
    assert!(
        window.fired.load(Ordering::SeqCst),
        "the racing disable never ran — this test proved nothing"
    );

    assert!(
        !fx.repo.plugin_get_by_id(ID).await.unwrap().unwrap().enabled,
        "the disable committed inside the window and nothing may undo it"
    );
    assert_eq!(
        fx.host.status(ID).await.map(|s| s.status),
        None,
        "a plugin the DB says is disabled must not be left Running by a reload \
         that decided on a pre-guard read: nothing reconciles that state, and \
         the next boot's autospawn skips it *because* it is disabled"
    );
    let ev = state_events(&fx).await;
    assert_eq!(
        ev.last().map(String::as_str),
        Some("disabled"),
        "the event log's last word must be `disabled`, not `running`: {ev:?}"
    );
}

/// A [`LifecycleDb`] whose `enabled_row` never succeeds.
struct UnreadableDb {
    repo: Arc<dyn Repo>,
    failures: AtomicUsize,
}

#[async_trait]
impl LifecycleDb for UnreadableDb {
    async fn enabled_row(&self, _id: &str) -> Result<Option<bool>, CalmError> {
        self.failures.fetch_add(1, Ordering::SeqCst);
        Err(CalmError::Internal(
            "injected permanent read failure".into(),
        ))
    }

    async fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), CalmError> {
        self.repo.plugin_update_enabled(id, enabled).await?;
        Ok(())
    }
}

#[tokio::test]
async fn a18_exhausted_respawn_retries_publish_a_terminal_state() {
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let db = Arc::new(UnreadableDb {
        repo: repo.clone(),
        failures: AtomicUsize::new(0),
    });
    let fx = boot_with(BootOpts {
        stub: CRASH_BIN,
        backoff: Some((vec![200], Duration::from_secs(300), 50)),
        repo: Some(repo),
        lifecycle_db: Some(db.clone()),
        ..Default::default()
    })
    .await;

    fx.host.spawn(ID).await.expect("spawn");

    // 5 retries × 200 ms of retry delay + the backoff itself.
    wait_for_events(
        &fx,
        |ev| ev.last().map(String::as_str) == Some("unavailable"),
        Duration::from_secs(20),
    )
    .await;

    assert!(
        db.failures.load(Ordering::SeqCst) >= 5,
        "the retry budget must actually have been spent, saw {} reads",
        db.failures.load(Ordering::SeqCst)
    );

    let st = fx
        .host
        .status(ID)
        .await
        .expect("a terminal entry must exist");
    let reason = match &st.status {
        PluginRuntimeStatus::Unavailable { reason } => reason.clone(),
        other => panic!(
            "after giving up, the live entry must be an explicit terminal, not \
             {other:?} — an operator reading `GET /api/plugins/{{id}}` has no \
             other way to learn the kernel stopped trying"
        ),
    };
    assert!(
        reason.contains("gave up") && reason.contains("enable"),
        "the terminal must say what happened and what the operator has to do: {reason}"
    );

    // And it stays there: nothing retries behind the operator's back.
    sleep(Duration::from_secs(1)).await;
    let ev = state_events(&fx).await;
    assert_eq!(ev.last().map(String::as_str), Some("unavailable"), "{ev:?}");
}

#[tokio::test]
async fn a1_unknown_id_is_still_404_when_that_id_is_busy() {
    let fx = boot_with(BootOpts {
        seed: false,
        ..Default::default()
    })
    .await;
    const GHOST: &str = "test.never.installed";
    let _held = fx
        .host
        .try_lock_lifecycle(GHOST)
        .expect("an uninstalled id still has a lock cell");

    for (what, res) in [
        ("enable", fx.host.enable(GHOST).await.map(|_| ())),
        ("disable", fx.host.disable(GHOST).await.map(|_| ())),
        ("reload", fx.host.reload(GHOST).await.map(|_| ())),
        ("uninstall", fx.host.uninstall(GHOST).await),
    ] {
        let err = res.expect_err(what);
        assert_eq!(
            err.code(),
            "not_found",
            "{what} on an unknown id must stay a 404 even while that id's lock \
             is held; the lock must not be able to change an endpoint's error \
             code. Got {err:?}"
        );
    }
}

#[tokio::test]
async fn a20_config_disabled_ids_keep_their_error_codes_on_every_entry() {
    const APP: &str = "test.disabled.app";
    const CONNECTOR: &str = "test.disabled.connector";
    const GHOST: &str = "test.disabled.ghost";

    let tmp = tempfile::tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    std::fs::create_dir_all(&plugins_data_dir).unwrap();

    let app_dir = write_plugin_with_args(&plugins_dir, APP, ECHO_BIN, &[]);
    // A registered connector, never brought up: rotation refuses on `kind` before touching the network.
    let conn_dir = plugins_dir.join(CONNECTOR);
    std::fs::create_dir_all(&conn_dir).unwrap();
    std::fs::write(
        conn_dir.join("manifest.json"),
        json!({
            "manifest_version": 1,
            "kind": "mcp-http",
            "id": CONNECTOR,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Disabled Connector",
            "mcp_http": {
                "url": "http://127.0.0.1:1/never-contacted",
                "api_key_secret": "NEVER",
                "api_key_in": "bearer",
                "tools_allow": ["noop"],
                "request_timeout_ms": 1_000,
            }
        })
        .to_string(),
    )
    .unwrap();

    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    for (id, dir) in [(APP, &app_dir), (CONNECTOR, &conn_dir)] {
        repo.plugin_install(calm_server::model::NewPlugin {
            id: id.into(),
            version: "0.1.0".into(),
            install_path: dir.display().to_string(),
            manifest: json!({}),
            enabled: true,
            user_config: json!({}),
        })
        .await
        .unwrap();
    }
    repo.plugin_token_set(APP, "hashed", i64::MAX)
        .await
        .unwrap();

    let (registry, report) = PluginRegistry::load_from_dir(&plugins_dir).unwrap();
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert!(
        registry.get(CONNECTOR).is_some() && registry.get(GHOST).is_none(),
        "fixture: the connector must be registered and the ghost must not"
    );

    let host = Arc::new(PluginHost::new_full(
        Arc::new(registry),
        repo.clone(),
        plugins_dir,
        plugins_data_dir,
        vec![APP.into(), CONNECTOR.into(), GHOST.into()],
        EventBus::new(),
        calm_server::state::WriteContext::new(
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::track_area_cache::TrackAreaCache::new(),
        ),
    ));

    for (what, res) in [
        ("spawn(registered app)", host.spawn(APP).await),
        ("spawn(unregistered)", host.spawn(GHOST).await),
        ("restart(registered app)", host.restart(APP).await),
        ("restart(unregistered)", host.restart(GHOST).await),
    ] {
        let err = res.expect_err(what);
        assert!(
            matches!(err, HostError::Disabled(_)),
            "{what}: a config-disabled id must answer `Disabled` — the kill \
             switch is the first thing `spawn_under` checks. Got {err:?}"
        );
    }
    assert!(
        host.status(APP).await.is_none(),
        "nothing may have been started"
    );

    let err = host
        .rotate_plugin_token(GHOST)
        .await
        .expect_err("rotate ghost");
    assert!(
        matches!(err, HostError::NotFound(_)),
        "rotate on an id the registry does not know is a 404 (`plugin {GHOST} \
         is not loaded`), and being in `plugins_disabled` does not change that. \
         `Disabled` here is a 500 through the route's catch-all. Got {err:?}"
    );
    let err = host
        .rotate_plugin_token(CONNECTOR)
        .await
        .expect_err("rotate connector");
    assert!(
        matches!(err, HostError::UnsupportedForKind { .. }),
        "rotate on a connector is a 400 — connectors are never issued a token — \
         and being in `plugins_disabled` does not change that. Got {err:?}"
    );
    assert!(
        repo.plugin_token_get(APP).await.unwrap().is_some(),
        "neither refusal may have touched an unrelated token row"
    );

    let err = host
        .rotate_plugin_token(APP)
        .await
        .expect_err("rotate disabled app");
    assert!(
        matches!(err, HostError::Disabled(_)),
        "a registered app in `plugins_disabled` reaches `spawn_under` and fails \
         there. Got {err:?}"
    );
    assert!(
        repo.plugin_token_get(APP).await.unwrap().is_none(),
        "…and it got there THROUGH the token delete: that is precisely why this \
         cell is not a 4xx like the two above it"
    );
}

#[tokio::test]
async fn a21_the_app_boot_fence_terminal_never_waits_on_the_event_store() {
    let fx = boot_with(BootOpts {
        app_wall: Some(Duration::from_millis(300)),
        ..Default::default()
    })
    .await;

    let barrier = DbBarrier::hold(&fx.repo).await;

    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(10), fx.host.autospawn_enabled())
        .await
        .expect(
            "boot never returned: the `app` fence's terminal arm awaited the \
             same wedged event store that made the fence fire, and that await \
             is outside the fence",
        );
    let elapsed = started.elapsed();
    // A boot that never met the fence would also be fast, and would prove nothing.
    assert!(
        elapsed >= Duration::from_millis(300),
        "boot returned in {elapsed:?}, faster than the 300 ms wall it was given \
         — the DB barrier is no longer parking the spawn"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "boot took {elapsed:?} against a 300 ms app wall"
    );

    let st = fx
        .host
        .status(ID)
        .await
        .expect("a cut-off app plugin must still leave an observable entry");
    assert!(
        matches!(st.status, PluginRuntimeStatus::Unavailable { .. }),
        "expected a terminal Unavailable entry, got {:?}",
        st.status
    );

    fx.host
        .try_lock_lifecycle(ID)
        .expect("the terminal arm must not have kept the lifecycle guard");

    barrier.release().await;
}

#[test]
fn the_app_autospawn_wall_is_the_documented_one() {
    use calm_server::plugin_host::{
        APP_AUTOSPAWN_WALL, MAX_CONNECTOR_AUTOSPAWN_WALL, PLUGIN_LIST_WALL, boot_autospawn_ceiling,
    };
    use calm_truth::db::sqlite::{SQLITE_ACQUIRE_TIMEOUT_MS, SQLITE_BUSY_TIMEOUT_MS};

    assert_eq!(PLUGIN_LIST_WALL, Duration::from_secs(40));
    assert!(
        PLUGIN_LIST_WALL
            > Duration::from_millis(SQLITE_ACQUIRE_TIMEOUT_MS + SQLITE_BUSY_TIMEOUT_MS)
    );
    assert_eq!(APP_AUTOSPAWN_WALL, Duration::from_secs(30));
    assert_eq!(boot_autospawn_ceiling(0), Duration::from_millis(71_500));
    assert_eq!(boot_autospawn_ceiling(1), Duration::from_millis(101_500));
    assert_eq!(
        boot_autospawn_ceiling(4),
        Duration::from_millis(191_500),
        "if a constituent wall moves, change this pinned total deliberately"
    );
    assert!(boot_autospawn_ceiling(1) > MAX_CONNECTOR_AUTOSPAWN_WALL);
    assert!(boot_autospawn_ceiling(1) > APP_AUTOSPAWN_WALL);
    assert!(boot_autospawn_ceiling(0) > PLUGIN_LIST_WALL);
}

#[tokio::test]
async fn two_wedged_app_plugins_cost_two_walls_not_one() {
    const A: &str = "test.lock.two.a";
    const B: &str = "test.lock.two.b";
    /// Small enough for a fast test, large enough that 1 × and 2 × cannot be told apart by scheduling noise.
    const WALL: Duration = Duration::from_millis(500);

    let tmp = tempfile::tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    std::fs::create_dir_all(&plugins_data_dir).unwrap();

    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    for id in [A, B] {
        let dir = write_plugin_with_args(&plugins_dir, id, ECHO_BIN, &[]);
        repo.plugin_install(calm_server::model::NewPlugin {
            id: id.into(),
            version: "0.1.0".into(),
            install_path: dir.display().to_string(),
            manifest: json!({}),
            enabled: true,
            user_config: json!({}),
        })
        .await
        .unwrap();
    }

    let (registry, report) = PluginRegistry::load_from_dir(&plugins_dir).unwrap();
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert!(
        registry.get(A).is_some() && registry.get(B).is_some(),
        "fixture: boot must actually have two enabled app plugins to iterate"
    );
    let host = Arc::new(
        PluginHost::new_full(
            Arc::new(registry),
            repo.clone(),
            plugins_dir,
            plugins_data_dir,
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::track_area_cache::TrackAreaCache::new(),
            ),
        )
        .with_app_autospawn_wall(WALL),
    );

    // Wedge BOTH; nothing in this test ever drops either guard.
    let _wedged_a = host.try_lock_lifecycle(A).expect("A's lock is free");
    let _wedged_b = host.try_lock_lifecycle(B).expect("B's lock is free");

    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(20), host.autospawn_enabled())
        .await
        .expect("boot never returned even with both fences in place");
    let elapsed = started.elapsed();

    assert!(
        elapsed >= 2 * WALL - Duration::from_millis(300),
        "boot returned in {elapsed:?} for TWO wedged app plugins against a \
         {WALL:?} wall. The two fences are sharing a budget, so \
         `boot_autospawn_ceiling`'s `N ×` term describes something boot does \
         not do — and with a shared budget the LAST plugin in \
         `plugins_list_all()` order gets no bring-up time at all",
    );
    assert!(
        elapsed < 2 * WALL + Duration::from_secs(3),
        "boot took {elapsed:?}, well past the 2 × {WALL:?} the ceiling allows; \
         something outside the fences is unbounded"
    );

    for id in [A, B] {
        assert!(
            host.status(id).await.is_none(),
            "{id}: the wedged guard means nothing may have started and the \
             terminal arm cannot have run"
        );
        assert!(
            host.try_lock_lifecycle(id).is_err(),
            "{id}: the test still holds this guard; boot must not have taken it"
        );
    }
}

#[tokio::test]
async fn rotate_token_does_not_resurrect_an_operator_disabled_plugin() {
    let fx = boot().await;

    let row = fx.host.disable(ID).await.expect("disable");
    assert!(!row.enabled, "fixture: disable must have cleared the bit");
    assert!(
        fx.host.status(ID).await.is_none(),
        "fixture: the plugin must not be running before the rotation"
    );

    fx.host.rotate_plugin_token(ID).await.expect(
        "rotating a disabled plugin's token is still Ok — the token is the \
         request, the restart is only a side effect",
    );

    let live = fx.host.status(ID).await.map(|s| s.status);
    assert!(
        !matches!(
            live,
            Some(PluginRuntimeStatus::Running) | Some(PluginRuntimeStatus::Spawning)
        ),
        "rotate-token started a plugin the operator had disabled: runtime says \
         {live:?}. Nothing reconciles that — the next boot's autospawn skips \
         this plugin *because* its row says `enabled = false`"
    );

    let row = fx
        .repo
        .plugin_get_by_id(ID)
        .await
        .unwrap()
        .expect("the row must survive a rotation");
    assert!(
        !row.enabled,
        "rotation must not touch the `enabled` bit in either direction"
    );

    assert!(
        fx.repo.plugin_token_get(ID).await.unwrap().is_none(),
        "the token row must be deleted even though the restart is skipped — \
         otherwise the caller's actual request went unserved"
    );
}

#[tokio::test]
async fn spawn_refuses_an_id_whose_row_says_disabled() {
    let fx = boot().await;
    fx.host.disable(ID).await.expect("disable");

    let err = fx
        .host
        .spawn(ID)
        .await
        .expect_err("spawn must refuse a plugin whose row says `enabled = false`");
    assert!(
        matches!(err, HostError::OperatorDisabled(_)),
        "the refusal must be its own typed variant, not a kernel-fault claim. \
         Got {err:?}"
    );
    assert!(
        fx.host.status(ID).await.is_none(),
        "a refused spawn leaves nothing behind"
    );
}

/// A row-less `app` cannot complete a spawn on this repo (`plugin_tokens.plugin_id` REFERENCES
/// plugins), so reaching the token-mint failure is the proof that admission did not intercept.
#[tokio::test]
async fn spawn_treats_an_absent_row_as_unchanged_not_as_disabled() {
    let fx = boot_with(BootOpts {
        seed: false,
        ..Default::default()
    })
    .await;
    assert!(
        fx.repo.plugin_get_by_id(ID).await.unwrap().is_none(),
        "fixture: this case is about an id with NO row"
    );

    let err = fx.host.spawn(ID).await.expect_err(
        "fixture: a row-less app cannot finish a spawn on this repo — the \
         token mint's foreign key is what stops it",
    );
    assert!(
        !matches!(err, HostError::OperatorDisabled(_)),
        "an absent row must not be read as a disabled row"
    );
    assert!(
        matches!(&err, HostError::BadState(m) if m.contains("plugin_token_set")),
        "the spawn must reach the token mint — i.e. it got past admission, past \
         the min-kernel check, through the configuration gate and into the \
         token mint exactly as it did before #1226. Got {err:?}"
    );
}

/// Deleting the token row does not stop an orphan: the token is checked once at `initialize`, and
/// no callback path reads `plugin_tokens` afterwards. The torn state has no production producer
/// any more, so the row is written directly.
#[tokio::test]
async fn rotate_token_reconciles_a_plugin_left_running_beside_a_disabled_row() {
    let fx = boot().await;
    fx.host.spawn(ID).await.expect("spawn");
    assert!(
        matches!(
            fx.host.status(ID).await.map(|s| s.status),
            Some(PluginRuntimeStatus::Running)
        ),
        "fixture: the plugin has to be running for this to be the torn state"
    );

    // The tear: the row says disabled while the process runs on.
    fx.repo
        .plugin_update_enabled(ID, false)
        .await
        .expect("bypass write");

    fx.host
        .rotate_plugin_token(ID)
        .await
        .expect("rotation still succeeds — the token is the request");

    let live = fx.host.status(ID).await.map(|s| s.status);
    assert!(
        !matches!(
            live,
            Some(PluginRuntimeStatus::Running) | Some(PluginRuntimeStatus::Spawning)
        ),
        "rotate left an orphan running beside a row that says `enabled = false`: \
         {live:?}. Deleting the token does not stop it — the token is verified \
         once at the `initialize` handshake against an in-memory value and no \
         callback path re-reads `plugin_tokens` — so the process would keep \
         working indefinitely with nothing left to reconcile it"
    );
    assert!(
        !fx.repo
            .plugin_get_by_id(ID)
            .await
            .unwrap()
            .expect("row")
            .enabled,
        "reconciling the runtime must not flip the operator's bit back"
    );
    assert!(
        fx.repo.plugin_token_get(ID).await.unwrap().is_none(),
        "…and the caller's actual request — clear the token — still happened"
    );
}

/// The break is a real `BEFORE DELETE` trigger that aborts, so the row survives and can be read back.
/// On the enabled branch `ensure_plugin_token` re-mints via `INSERT … ON CONFLICT DO UPDATE`, so the
/// hash is overwritten whether or not the DELETE landed.
#[tokio::test]
async fn a_failing_token_delete_is_fatal_only_where_nothing_re_mints() {
    let sqlx_repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let repo: Arc<dyn Repo> = sqlx_repo.clone();
    let fx = boot_with(BootOpts {
        repo: Some(repo),
        ..Default::default()
    })
    .await;

    fx.host.spawn(ID).await.expect("spawn");
    let before_hash = fx
        .repo
        .plugin_token_get(ID)
        .await
        .unwrap()
        .expect("the spawn minted a token")
        .0;
    let before_pid = fx
        .host
        .status(ID)
        .await
        .expect("running")
        .pid
        .expect("a live pid");

    sqlx::query(
        "CREATE TRIGGER block_token_delete BEFORE DELETE ON plugin_tokens \
         BEGIN SELECT RAISE(ABORT, 'blocked'); END",
    )
    .execute(sqlx_repo.pool())
    .await
    .expect("arm the failing delete");

    fx.host.rotate_plugin_token(ID).await.expect(
        "the rotation must still happen: the restart's UPSERT does not \
                 need the DELETE, so refusing here would be a false 500",
    );

    let after_hash = fx
        .repo
        .plugin_token_get(ID)
        .await
        .unwrap()
        .expect("the restart re-minted through the UPSERT")
        .0;
    assert_ne!(
        before_hash, after_hash,
        "the rotation must still happen even though the DELETE was refused — \
         `ensure_plugin_token` overwrites the row through \
         `INSERT … ON CONFLICT DO UPDATE` and never needs the delete"
    );
    let after_pid = fx
        .host
        .status(ID)
        .await
        .expect("still running after the rotation")
        .pid
        .expect("a live pid");
    assert_ne!(
        before_pid, after_pid,
        "…and the restart half really ran: a rotation that left the same \
         process behind would have handed the plugin a token it never \
         handshook with"
    );

    fx.repo
        .plugin_update_enabled(ID, false)
        .await
        .expect("bypass write");
    let armed_hash = fx.repo.plugin_token_get(ID).await.unwrap().expect("row").0;

    let err = fx
        .host
        .rotate_plugin_token(ID)
        .await
        .expect_err("the disabled branch must report the failed delete");
    assert!(
        err.to_string().contains("plugin_token_delete"),
        "the failure must name what could not be done: {err}"
    );
    assert_eq!(
        fx.repo.plugin_token_get(ID).await.unwrap().map(|t| t.0),
        Some(armed_hash),
        "the old hash is still there — which is exactly why answering `Ok` \
         would have been a lie"
    );
    assert!(
        fx.host.status(ID).await.is_none(),
        "the branch still stopped the plugin before it tried the delete"
    );
}

/// `PluginHost::status` is a snapshot of the runtime table, not a liveness check: it answers `Some`
/// for `Crashed`, `Unavailable` and `Spawning` as readily as for `Running`.
#[tokio::test]
async fn the_disabled_branch_clears_the_token_of_a_crashed_plugin() {
    let fx = boot_with(BootOpts {
        stub: CRASH_BIN,
        ..Default::default()
    })
    .await;
    let _ = fx.host.spawn(ID).await;
    wait_for_events(
        &fx,
        |ev| ev.iter().any(|s| s == "crashed"),
        Duration::from_secs(15),
    )
    .await;

    fx.repo
        .plugin_update_enabled(ID, false)
        .await
        .expect("bypass write");

    fx.host.rotate_plugin_token(ID).await.expect(
        "a crashed plugin is not a running one — the rotation must not \
                 be refused, and refusing would lock the token row out for good",
    );
    assert!(
        fx.repo.plugin_token_get(ID).await.unwrap().is_none(),
        "the token row must actually be cleared"
    );
    assert!(
        !matches!(
            fx.host.status(ID).await.map(|s| s.status),
            Some(PluginRuntimeStatus::Running)
        ),
        "and nothing may have been started"
    );
}
