//! #1196 + #1169 S1 — acceptance suite for the per-plugin lifecycle lock.
//!
//! Design: `docs/architecture/1196-plugin-lifecycle-lock.md` §4.
//!
//! Two disciplines run through the whole file and are worth stating once:
//!
//! * **Every concurrent case names its barrier.** Merely launching two
//!   operations at once does not prove the loser ever met the lock: if the
//!   winner's critical section finishes first, the loser gets `plugin_conflict`
//!   (acceptance 6) or plain success (acceptance 7) and the prose assertions
//!   below would still all pass — a test that never once exercised the lock and
//!   cannot tell you so. Each such test therefore pins the winner *inside* its
//!   critical section and asserts the loser observed `plugin_busy` /
//!   `LifecycleBusy` explicitly.
//! * **Reject semantics.** `try_lock_lifecycle` is non-blocking, so a refused
//!   caller is finished — it does not queue and resume at the linearization
//!   point. Every case that wants the loser's work done retries it explicitly.
//!
//! **Acceptance 2 is a compile-time fact, not a test in this file.** The
//! `PluginHost::emit_state(id, status)` overload is deleted; the only emitter is
//! `emit_state_under(&LifecycleGuard, ..)`. Nothing here can witness that from
//! outside the crate (both are private), and the honest statement is that the
//! compiler witnesses it: reintroducing a call to `emit_state` does not build.
//! What is explicitly NOT claimed is that `Event::PluginState` cannot be
//! constructed and written elsewhere — it can, and closing that needs a type
//! fence on the event variant, tracked as #1210.

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

// ===========================================================================
// Fixture
// ===========================================================================

struct Fx {
    host: Arc<PluginHost>,
    repo: Arc<dyn Repo>,
    plugins_dir: PathBuf,
    _tmp: TempDir,
}

/// Write a plugin tree (`manifest.json` + `bin/stub` symlink) under
/// `plugins_dir/<id>` and return its path.
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
    /// Override for the initial plugin-list wall, so the wedged-read gate does
    /// not wait out the production allowance.
    plugin_list_wall: Option<Duration>,
    /// Pre-built repo, for the tests that must construct their
    /// [`LifecycleDb`] fake around the same handle the host will use.
    repo: Option<Arc<dyn Repo>>,
    /// `config.plugins_disabled`, i.e. the operator's kill switch. Default
    /// empty; `a20` is the only case that populates it.
    plugins_disabled: Vec<String>,
    /// Override for `APP_AUTOSPAWN_WALL`, so a gate can watch the `app` boot
    /// fence fire without waiting out the production 30 s.
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
            calm_server::wave_cove_cache::WaveCoveCache::new(),
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

// ===========================================================================
// Barriers
// ===========================================================================

/// Holds a `BEGIN IMMEDIATE` transaction open, so **every** write to the
/// database parks. Any host operation that reaches a repo write — a
/// `plugin.state` emission, a token mint, an `enabled` flip — therefore stalls
/// *inside* its lifecycle guard.
///
/// Scope warning: this blocks the whole database, not one plugin's rows. Do not
/// drive a second plugin while it is held, and do not use it as a *window*
/// barrier (one expecting a row to change while it is held): `tests/` run on
/// `sqlite::memory:`, `journal_mode = WAL` is a no-op there, and readers get no
/// snapshot isolation — the blocked write simply never commits.
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

/// Captures `tracing` events into a buffer for the duration of one test.
///
/// The guard is **thread-local** (`tracing::subscriber::set_default`, not
/// `set_global_default`), and every test in this file runs on a
/// `#[tokio::test]` current-thread runtime — so every task the host spawns is
/// polled on the same thread that installed the subscriber, and no other test
/// in the binary is affected.
///
/// This exists so a test can observe a branch that, by design, changes nothing:
/// a supervisor that wakes from its backoff and declines to act writes no event,
/// no status and no row. Its `tracing::info!` is the only thing it leaves
/// behind, and it is a production statement, not a test seam.
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
    ///
    /// Matching on the message text is deliberate and its failure mode is the
    /// safe one: if the production string is reworded, this hangs to its
    /// deadline and fails with the whole captured log attached — it cannot go
    /// silently vacuous the way an arithmetic identity about `Instant`s can.
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
///
/// This is the positive observation that makes "the winner is inside its
/// critical section" a fact rather than a hope: it succeeds only when a real
/// `try_lock_lifecycle` fails. Combined with [`DbBarrier`] (which guarantees the
/// holder cannot leave), it pins the window deterministically.
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

/// Run a lifecycle call that is expected to be **refused at the entry**, under
/// a hard time bound.
///
/// The bound is part of the assertion, not defensive padding: a refusal is
/// non-blocking and answers immediately, whereas a call that got *into* the
/// critical section parks on whatever barrier the winner is parked on. Without
/// the bound, a regression that admits the loser shows up as a hung test with
/// no message instead of a named failure.
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

// ===========================================================================
// Acceptance 1 — every entry point takes the lock, refuses inertly, and works
// on retry. ONE FIXTURE PER ENTRY.
//
// Separate fixtures are not stylistic: a single fixture that calls every entry
// under one held guard would, after the `uninstall` retry succeeded, be able to
// answer only `NotFound` for `spawn` / `enable` / `reload`, so the "retry
// succeeds" half would be untestable for everything after the first
// destructive entry.
//
// Mutation witness for the whole block: delete the `try_lock_lifecycle` line
// from any one entry point and that entry's fixture fails at its
// `expect_err`.
// ===========================================================================

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

/// Acceptance 1 also has to pin the *ordering* the guard sits at inside
/// `install`: after the read-only min-kernel check, before the duplicate probe.
///
/// Mutation witness: move the `try_lock_lifecycle` to `install`'s first line and
/// this returns 409 `plugin_busy` instead of 422 `plugin_kernel_too_old` — an
/// error code silently changed by the lock.
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

/// Acceptance 1 + 11 — the uninstall refusal is fail-closed: everything the
/// operation would have destroyed is still there afterwards.
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

// ===========================================================================
// Acceptance 12 — liveness and re-entrancy
// ===========================================================================

/// (b) **With no contention, no entry point may answer `Busy`.**
///
/// This is the re-entrancy gate. `tokio::Mutex` is not re-entrant, so a
/// `*_under` body that mistakenly called a lock-taking wrapper would deadlock
/// under the waiting semantics — but under the non-blocking one it does not
/// hang and does not time out. It returns a silent 409 to a caller with no
/// competitor, which no timeout-based test can see.
///
/// Mutation witness: make `stop_under` call `self.stop(id)` instead of doing
/// the work (i.e. re-enter through the wrapper). `stop`, `restart` and
/// `rotate_plugin_token` then answer `LifecycleBusy` with no competitor and this
/// test fails on them.
///
/// **What that mutation does NOT catch, stated because the earlier note here
/// claimed it did:** `disable` / `uninstall` / `reload` also re-enter, but each
/// funnels every non-`NotFound` stop error into
/// `CalmError::Internal("stop failed: ...")`, so their code is `internal`, not
/// `plugin_busy`, and the `not_busy!` arms above stay green. Those three are
/// covered by the `HostError` half of the same mutation one call deeper, not by
/// their own arms. Widening the arms to "must not fail at all" is not an option:
/// several of these legitimately fail on a plugin that is already stopped.
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

    // The `HostError` entries, same question.
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

/// (a) Every pair of entry points, **including each with itself**, run
/// concurrently must all settle. A hang is the failure mode this catches; the
/// bound is the assertion.
///
/// Each pair also states the outcomes it will accept, so "everything returned
/// `Busy` instantly" cannot pass as liveness.
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

            // Not "everything may fail": at most one of the pair may report a
            // busy lock, because at most one can lose a two-way race.
            let busy = [&ra, &rb]
                .iter()
                .filter(|r| matches!(r, Err(m) if m.contains("busy")))
                .count();
            assert!(
                busy <= 1,
                "{a:?} + {b:?}: both reported busy ({ra:?}, {rb:?})"
            );

            // And the final runtime state must be one of the two operations'
            // legitimate terminals, never a torn one.
            let live = fx.host.status(ID).await.map(|s| s.status);
            assert!(
                matches!(
                    live,
                    None | Some(PluginRuntimeStatus::Running)
                        | Some(PluginRuntimeStatus::Crashed { .. })
                ),
                "{a:?} + {b:?} left a torn state: {live:?}"
            );

            // #1196 S1 review P1-5 — the shape above is not enough. It accepts
            // `busy = 0` + `Running` while the row says `enabled = false`, which
            // is precisely the tear P0-1 produced (`reload` respawning on an
            // `enabled` bit it read outside its guard). So cross-check the three
            // stores against each other.
            let row = fx.repo.plugin_get_by_id(ID).await.unwrap();
            let in_registry = fx.host.registry().get(ID).is_some();
            let running = matches!(live, Some(PluginRuntimeStatus::Running));

            // Holds for EVERY pair: a plugin that has been uninstalled leaves
            // nothing behind in either of the other two stores.
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

            // Holds for the `enabled`-aware pairs only. `spawn` / `restart` /
            // `rotate_plugin_token` deliberately ignore the `enabled` bit (an
            // operator may start a disabled plugin by hand), so "Running while
            // disabled" is a legitimate terminal for any pair containing one of
            // them and asserting against it would be asserting a falsehood.
            let enabled_aware =
                |op| matches!(op, Op::Enable | Op::Disable | Op::Reload | Op::Uninstall);
            if enabled_aware(a)
                && enabled_aware(b)
                && let Some(p) = row.as_ref()
            {
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

// ===========================================================================
// Acceptance 5 (app half) — uninstall vs an in-flight spawn
//
// The connector half lives in `connector_host.rs`
// (`uninstall_is_refused_while_a_connector_spawn_is_in_flight`). Both halves
// are required because the connector path has a mitigation the app path never
// had: `set_exposes_tools` no-ops for an absent id and abandons the spawn,
// whereas the app spawn looks the registry up once at the top and never again.
// ===========================================================================

/// Barrier: a held DB write transaction parks the app spawn at its first repo
/// write (the token mint), inside its guard; `wait_until_locked` is the
/// positive observation that the guard is actually held.
///
/// Mutation witness: delete the `try_lock_lifecycle` from `PluginHost::spawn`
/// and the first `uninstall` succeeds — deleting the row and the token of a
/// plugin that then completes its spawn and runs on as a live entry with no
/// row behind it.
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

// ===========================================================================
// Acceptance 6 — two concurrent installs of one id
// ===========================================================================

/// The loser must get `plugin_busy`, **not** `plugin_conflict`: under reject
/// semantics it never reached the duplicate-id probe at all. Only after an
/// explicit retry — once the winner has committed — is `plugin_conflict` the
/// right answer. And the winner's row must not have been overwritten: the
/// underlying insert is an `ON CONFLICT DO UPDATE`, which is what made the
/// probe/insert pair a TOCTOU before the lock.
///
/// Barrier: the held DB transaction pins the winner inside its critical
/// section. Without it the winner would simply finish first and the loser would
/// get `plugin_conflict` on the first call — every prose assertion below would
/// still hold and the lock would never have been touched.
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

    // The explicit retry: NOW it is a permanent conflict.
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

// ===========================================================================
// Acceptance 7 — overlapping enable / disable, both directions
// ===========================================================================

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

    // Explicit retry of the loser: DB bit and runtime agree afterwards.
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

// ===========================================================================
// Acceptance 9 / 10 — the backoff sleep is OUTSIDE the lock, and waking up
// re-decides
// ===========================================================================

/// A `disable` issued while a crashed plugin is in its respawn backoff must
/// complete **within** the backoff, and after the backoff has fully elapsed the
/// plugin must be down and stay down.
///
/// Mutation witness: move the `tokio::time::sleep` inside the guard (i.e. hold
/// the guard across segments 1–3) → the `disable` blocks for the whole backoff
/// and the `elapsed` assertion fails. That is the half this test owns.
///
/// **What the second half does NOT witness, corrected from the earlier note
/// here:** deleting segment 3's live/epoch/attempt predicate leaves this test
/// green. `disable` calls `stop_under`, which **aborts** the sleeping supervisor
/// task outright (`rp.supervisor.take()` → `abort()`), so the mutated predicate
/// is never evaluated — there is no task left to evaluate it. The same is true
/// of `a10`. The predicate's real gate is `a9b`, which reaches it through the
/// one shape that does *not* abort the supervisor (an explicit `spawn` over a
/// `Crashed` entry), and the `enabled`-bit gate is `a15b`.
///
/// The terminal assertions below are still worth keeping: they pin that the
/// abort + the `enabled` write together leave the plugin down and the event
/// log's last word `disabled` — they just are not a witness for the epoch
/// predicate.
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

/// Same shape for `uninstall`: after an uninstall during the backoff, the
/// plugin must be gone and stay gone.
///
/// Same correction as `a9`: this is **not** a witness for segment 3's
/// registry/epoch predicates. `uninstall` also goes through `stop_under`, which
/// aborts the sleeping supervisor, so the mutated predicate is never reached.
/// What this pins is the composite operation's own terminal — row, registry and
/// live entry all gone, and nothing brings them back.
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

// ===========================================================================
// Acceptance 14 — the supervisor contends for the lock (§2.5's two holes)
// ===========================================================================

/// (a) The supervisor's FIRST segment collides with the spawn's own guard.
///
/// The crash stub exits the moment the handshake completes, which is before
/// `spawn_under` has finished — it still owes a live insert and a `running`
/// emission, all inside its guard. So the supervisor's `child.wait()` returns
/// while the guard is held, **by construction**. A `try`-and-give-up there
/// would leave a live `Running` entry over a dead process with no task left to
/// correct it.
///
/// The window is pinned rather than hoped for: a DB write barrier is taken as
/// soon as the `spawning` event is seen (after which the only remaining repo
/// write in `spawn_under` is the `running` emission), and the test asserts the
/// spawn is *still running* — i.e. still holding the guard — while the child is
/// already gone.
///
/// Mutation witness: replace `await_lifecycle` with `try_lock_lifecycle` +
/// early return in segment 1. The crash is never accounted, the entry stays
/// `Running`, and the final `Crashed` assertion times out.
#[tokio::test]
async fn a14a_a_crash_inside_the_spawns_own_guard_is_still_accounted() {
    let fx = boot_with(BootOpts {
        stub: CRASH_BIN,
        // The stub sleeps before answering `initialize`, which is what makes
        // the window below a real window rather than a race: the `spawning`
        // emission has committed, the child is alive, and the handshake has
        // not happened yet.
        stub_args: vec!["--delay-ms=600"],
        // Long enough that the respawn cannot mask the assertion.
        backoff: Some((vec![60_000], Duration::from_secs(300), 50)),
        ..Default::default()
    })
    .await;

    let h = Arc::clone(&fx.host);
    let spawning = tokio::spawn(async move { h.spawn(ID).await });

    // Once `spawning` is on the wire the token mint is done; the only repo
    // write left inside the guard is the trailing `running` emission.
    wait_for_events(
        &fx,
        |ev| ev.contains(&"spawning".to_string()),
        Duration::from_secs(10),
    )
    .await;
    let barrier = DbBarrier::hold(&fx.repo).await;

    // The live insert happens before that emission, so this proves the spawn
    // has handshaken (hence the crash stub has exited) and is now parked.
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

    // The terminal must NOT be a false `Running` over a dead child.
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

/// (b) The supervisor's THIRD segment collides with an unrelated lock holder
/// that ends up doing nothing. The plugin must still be respawned, not left
/// permanently `Crashed`.
///
/// The holder here is the test itself via the `pub` `try_lock_lifecycle` — the
/// reason that function is public (design §5 R7).
///
/// Mutation witness: replace `await_lifecycle` with `try_lock_lifecycle` +
/// early return in `respawn_after_backoff`. The supervisor gives up and the
/// second `running` never arrives.
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

// ===========================================================================
// Acceptance 15 — the supervisor's third segment fails CLOSED on a DB read
// failure (fault injection through the narrow port)
// ===========================================================================

/// Fake [`LifecycleDb`] with a one-shot read failure and a pause gate.
///
/// The pause gate is not decoration. `respawn_after_backoff` retries a failed
/// read a bounded number of times a few hundred ms apart; without a gate the
/// second attempt would succeed while the test was still setting up its
/// assertion, and the whole thing would drift into "sometimes green for the
/// wrong reason". With it, the failure window stays open until the test closes
/// it, and `failures` is the acknowledgement that the injected failure was
/// actually consumed by production code rather than sitting unused.
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

/// 15a (**liveness**) — a read failure must not respawn, and recovery must.
///
/// Mutation witness: make the `Err` arm of the `enabled_row` match fall through
/// to the respawn (fail-open) and the "still `Crashed` during the failure
/// window" assertion fails.
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

    // Reads recover — and the plugin IS still enabled, so it comes back.
    faulty.release();
    wait_for_events(
        &fx,
        |ev| ev.iter().filter(|s| *s == "running").count() >= 2,
        Duration::from_secs(15),
    )
    .await;
}

/// 15b (**correctness + the mutation witness**) — the `enabled` bit stays
/// authoritative across a read failure.
///
/// The plugin is disabled through the residual design §2.3 registers
/// explicitly: a direct `repo` write that bypasses the host entirely. That
/// leaves `live` and `run_epoch` untouched, so the supervisor's epoch check
/// still says "this is my instance" — the only thing that can stop the respawn
/// is the DB read, which is exactly what fail-closed is for.
///
/// Mutation witness: change the `Err` arm to skip the DB check and respawn (the
/// r3 shape). The variant respawns at the instant of the read failure, bringing
/// back a plugin the database says is disabled, and the `never running again`
/// assertion fails.
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

    // The residual: a plugin-row write that never goes through the host.
    // `live` and `run_epoch` are untouched by it, by construction.
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

// ===========================================================================
// Acceptance 16 — `disable` stops BEFORE it writes the row
// ===========================================================================

/// A [`LifecycleDb`] that samples `PluginHost::status` at the instant
/// `set_enabled` is called.
///
/// This is the whole reason the port exists (design §4 acceptance 16). A DB
/// barrier cannot witness this ordering in this repo: `tests/` run on
/// `sqlite::memory:` where `journal_mode = WAL` is a no-op, readers have no
/// snapshot isolation, and `plugin_update_enabled` is a bare autocommit
/// `UPDATE` — under the old order it would simply park on the barrier and never
/// commit, so both orders would look identical and green.
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

/// Mutation witness: swap the two statements in `PluginHost::disable` back to
/// S0's order (`set_enabled` then `stop_under`) and the observation becomes
/// `Some("running")` instead of `None`.
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

/// Acceptance 9/10, third predicate — a supervisor that wakes from its backoff
/// onto a **different run instance** must leave it alone.
///
/// **#1196 S1 review P1-4 — `run_epoch` is now the only discriminator.** The
/// first version of this test revived the plugin with the *echo* stub, so the
/// replacement entry was `Running` with `crash_attempt = 0`. Deleting
/// `rp.run_epoch == run_epoch` from segment 3's predicate left that test green,
/// because `matches!(rp.status, Crashed { .. })` and `rp.crash_attempt ==
/// attempt` each rejected the stale supervisor on their own — the epoch
/// predicate had no gate at all, which is the thing acceptance 9/10's third
/// predicate is *for*.
///
/// So the replacement instance is deliberately built to satisfy every other
/// conjunct:
///
/// * same stub, so it crashes too and the entry is `Crashed { .. }` — ✓ status;
/// * its own supervisor increments `crash_attempt` from the fresh entry's `0` to
///   `1`, which is exactly the `attempt` the *stale* supervisor captured from
///   the first crash — ✓ attempt;
/// * nothing is stopping it — ✓ `!stopping`;
/// * `spawn_under` allocates a fresh `run_epoch` — ✗ epoch, and only epoch.
///
/// The revive is an explicit `spawn` and not `reload`/`restart` on purpose:
/// those call `stop_under`, which **aborts** the sleeping supervisor outright,
/// and an aborted task cannot witness anything. An explicit `spawn` on a
/// `Crashed` entry is admitted, replaces the live entry (and with it the
/// supervisor handle — dropping a `JoinHandle` does not abort its task), and
/// leaves the old supervisor alive and sleeping. That is the one reachable shape
/// in which a stale supervisor meets a newer run instance.
///
/// The observation is the respawn COUNT inside the window between the two
/// supervisors' deadlines: the stale one is due first and must do nothing, the
/// newer one is due later and must respawn. A variant that lets the stale
/// supervisor through respawns twice, and the extra `running` lands inside the
/// window.
///
/// **The window's barrier** (file header rule 1; #1196 S1 review r4/r5). The two
/// ends are established by different means, and they are not equally strong:
///
/// * **Lower end — observed.** The test blocks until the stale supervisor logs
///   its own give-up line. That is a direct observation that it woke, took the
///   lifecycle guard, compared `run_epoch` and declined; there is no clock
///   arithmetic and no unasserted margin left in it. r5 replaced the previous
///   shape here, which slept to a computed instant and then asserted an
///   inequality that the sleep had just made true — a tautology that could only
///   have caught `sleep_until` returning early, resting on an unstated
///   assumption (that a 400 ms margin covers the gap between the `crashed`
///   commit the test sees and the `sleep` a few statements later).
/// * **Upper end — asserted on the clock.** `revive_at.elapsed() < BACKOFF` is a
///   real assertion (a slow machine fails it loudly), but it is not an
///   observation: the newer supervisor is supposed to still be asleep at this
///   point, and a sleeping task emits nothing to observe. It is a genuine lower
///   bound on its deadline, since its `crashed` cannot precede its own spawn.
///
/// So one end is witnessed and one end is bounded, and the file no longer says
/// "both ends are asserted" as if they were the same kind of claim.
///
/// Mutation witnesses (each applied alone to `respawn_after_backoff`'s `ok`
/// block), **with the assertion that actually goes red** — r5 re-ran both after
/// changing the lower-end barrier, and neither lands where r4's note said:
/// * delete `rp.run_epoch == run_epoch` → the stale supervisor respawns instead
///   of declining, so its give-up line never appears and the **`wait_for`
///   barrier** fails (13 s deadline, whole captured log attached). The
///   `exactly two` assertion below is never reached;
/// * force `ok = false` → *both* supervisors decline, so the barrier and
///   `exactly two` both pass and the **liveness wait** at the end (`>= 3
///   running`) is what fails, on `["spawning","running","crashed","spawning",
///   "running","crashed"]`. That is the correct place for it: this mutation
///   does not break the epoch discrimination, it freezes everything, and the
///   liveness half exists precisely to say so.
#[tokio::test]
async fn a9b_a_late_supervisor_leaves_a_newer_run_instance_alone() {
    // Both supervisors sleep BACKOFF, and both start that sleep immediately
    // after emitting their own `crashed` — see `supervise_inner`: the emission
    // is the last statement of segment 1 and `sleep` is segment 2. So an
    // observed `crashed` event is a barrier for that supervisor's sleep, and the
    // window is named in terms of it rather than in terms of wall clock since
    // the test started.
    const BACKOFF: u64 = 3_000;
    const STAGGER: u64 = 1_500;
    /// The stale supervisor's give-up line, verbatim from `respawn_after_backoff`
    /// check (a). Seeing it is a POSITIVE observation that the supervisor woke
    /// AND read the epoch AND declined — strictly stronger than "its backoff has
    /// elapsed", which is all the window this test used to compute could claim.
    const STALE_GAVE_UP: &str = "backoff elapsed but the run instance is gone or has moved on";
    let capture = LogCapture::install();
    let fx = boot_with(BootOpts {
        stub: CRASH_BIN,
        backoff: Some((vec![BACKOFF], Duration::from_secs(300), 50)),
        ..Default::default()
    })
    .await;

    fx.host.spawn(ID).await.expect("spawn");
    // #1196 S1 review r4 — wait for the OBSERVED first crash before staggering,
    // so `STAGGER` is measured from the stale supervisor's sleep and not from
    // `t0`. (r4 also used this instant to compute a sample point; r5 dropped
    // that computation entirely — see the give-up wait below. `crashed1_at`
    // survives only as a diagnostic.)
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

    // Stagger, then revive with an explicit `spawn` of the SAME crash stub. The
    // replacement crashes in its turn and parks its own supervisor, so the two
    // supervisors differ in `run_epoch` and in nothing else the predicate reads.
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

    // ---- the sample point, and the two halves of the window ---------------
    //
    // Lower end — a REAL observation, not an arithmetic identity. The earlier
    // shape slept to `crashed1_at + BACKOFF + MARGIN` and then asserted
    // `crashed1_at.elapsed() >= BACKOFF + MARGIN`, which the preceding
    // `sleep_until` had just made true: it could only ever catch `sleep_until`
    // returning early, and the barrier it *claimed* — that `MARGIN` covers the
    // gap between the `crashed` commit the test saw and the `sleep` a few
    // statements later — was an unasserted assumption. Waiting for the stale
    // supervisor's own give-up line replaces both: when it appears, that
    // supervisor has provably woken, taken the lifecycle guard, read
    // `run_epoch`, found it stale and returned. No margin, no assumption.
    capture
        .wait_for(
            STALE_GAVE_UP,
            Duration::from_millis(BACKOFF) + Duration::from_secs(10),
            "the stale supervisor never woke from its backoff and reached the \
             epoch check, so the assertion below would say nothing",
        )
        .await;
    // Upper end: the newer supervisor's `crashed` cannot precede its own spawn,
    // so its deadline is at or after `revive_at + BACKOFF`. If the sample drifted
    // past that, "only two running events" would be a statement about a
    // supervisor that has not woken yet either — vacuous in the other direction,
    // and this end has no positive observation available (the newer supervisor
    // is *supposed* to still be asleep, and a sleeping task emits nothing), so
    // it stays an assertion on the clock.
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

    // Liveness half: the NEWER supervisor is still owed its respawn, so this is
    // not "the epoch check froze everything".
    wait_for_events(
        &fx,
        |ev| ev.iter().filter(|s| *s == "running").count() >= 3,
        Duration::from_secs(15),
    )
    .await;
}

// ===========================================================================
// #1196 S1 review P1-6 — boot's `app` branch is bounded
// ===========================================================================

/// A lifecycle lock nobody releases must not hang boot.
///
/// The `app` branch of `autospawn_enabled_within` had no fence at all: it goes
/// through `autospawn_one`, whose `Busy` fallback is `await_lifecycle`, which is
/// unbounded by design. The design's defence was "boot's only contender is a
/// crash supervisor, whose work is bounded" — but §5 R6 says a timing argument
/// is not a proof, `try_lock_lifecycle` is `pub`, and a supervisor's own
/// `spawn_under` can park on a slow event store indefinitely. Connectors already
/// had `timeout_at`; this is the same fence for the other half.
///
/// The lock here is held for the whole test, i.e. genuinely never released — so
/// "it finished" cannot be luck.
///
/// Mutation witness: delete the `tokio::time::timeout` wrapper around the `app`
/// branch's `autospawn_one` call (keeping the body). r5 ran it: the test fails
/// in ~10 s on its own outer `timeout(…).expect("boot never returned: …")`, with
/// that message. It does **not** hang — an earlier version of this note said it
/// runs until nextest's slow-timeout kills it, which the outer bound below has
/// always prevented. The distinction matters because "hangs" is what the
/// *production* failure looks like; the test's job is to turn that into a
/// readable red, and it does.
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
                calm_server::wave_cove_cache::WaveCoveCache::new(),
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

// ===========================================================================
// #1238 — boot's initial plugin enumeration is bounded
// ===========================================================================

/// A repo substitute whose boot-time plugin enumeration genuinely never
/// returns. It is intentionally narrower than `RouteRepo`: implementing that
/// ~100-method trait would bury this one behavior in forwarding boilerplate.
struct WedgedPluginListDb;

#[async_trait]
impl PluginListDb for WedgedPluginListDb {
    async fn plugins_list_all(&self) -> Result<Vec<calm_server::model::Plugin>, CalmError> {
        std::future::pending().await
    }
}

/// A wedged `plugins_list_all` read must not hang boot before any per-plugin or
/// connector fence can run.
///
/// Red witness before the fence was implemented: the test's one-second outer
/// watchdog fired, and nextest reported `Elapsed(())` after 1.177 s. The
/// pending future guarantees that a green result cannot come from the fake
/// repo eventually recovering.
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

// ===========================================================================
// #1196 S1 review P0-1 — `reload` may not decide on a row read outside its
// guard
// ===========================================================================

/// A [`LifecycleDb`] that runs a full `disable` **inside** `reload`'s pre-guard
/// existence probe, and then answers the probe with the value that was true
/// before it did.
///
/// That is not a contrived value: it is exactly what the real interleaving
/// produces. `reload`'s probe reads the row; a concurrent `disable` takes the
/// guard (which `reload` does not hold yet), stops the plugin, commits
/// `enabled = false` and releases; `reload` then takes the guard holding a row
/// that is already history. The fake makes that window deterministic instead of
/// hoping the scheduler produces it.
struct StaleProbeWindow {
    repo: Arc<dyn Repo>,
    host: std::sync::OnceLock<std::sync::Weak<PluginHost>>,
    /// One-shot: only the first probe opens the window, so the `disable` we run
    /// inside it (and any later reload) sees a plain delegating port.
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
        // The pre-window value. Any caller that treats a probe as a decision
        // gets exactly this.
        Ok(before)
    }

    async fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), CalmError> {
        self.repo.plugin_update_enabled(id, enabled).await?;
        Ok(())
    }
}

/// A `disable` that lands between `reload`'s existence probe and `reload`'s
/// guard must not be overwritten by the reload: the terminal may not be
/// "DB says disabled, runtime says Running".
///
/// This is #1169 race 3 one endpoint over, and S1 re-introduced it: the probe
/// was moved outside the guard (correctly — otherwise `unknown id + busy`
/// answers 409 instead of 404) but the same read kept feeding `plug.enabled` and
/// `plug.install_path` to the decision below it.
///
/// Mutation witness: in `PluginHost::reload`, bind the probe
/// (`let probed = self.lifecycle_db.enabled_row(id).await?`) and branch on it
/// instead of on the in-guard re-read's `plug.enabled`. The reload then respawns
/// the plugin the operator just disabled and both terminal assertions below
/// fail.
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

// ===========================================================================
// #1196 S1 review P0-3 — giving up on the respawn is a terminal, not silence
// ===========================================================================

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

/// When the bounded fail-closed retry is exhausted, the supervisor must publish
/// an explicit terminal state — not stop at a `tracing::error!` nobody reads.
///
/// The pre-fix ending left `live` at `Crashed`, the event stream's last word at
/// the `crashed` emitted before the backoff, and no background path that would
/// ever reconcile it: only an explicit `spawn` or a kernel restart. That is the
/// same argument §2.5 makes for a `Busy` autospawn and the same reason the
/// `Unavailable` entry exists — it was simply never applied to this path.
///
/// Mutation witness: delete the `publish_unavailable_under` call at the tail of
/// `respawn_after_backoff` (leaving the `tracing::error!`) and both assertions
/// below fail — the last event stays `crashed` and the live entry stays
/// `Crashed`.
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

// ===========================================================================
// Acceptance 1, ordering half — the 404 probes sit BEFORE the guard
//
// Same shape as `a1_install_reports_kernel_too_old_even_when_the_id_is_busy`,
// one endpoint over. `enable` / `disable` / `uninstall` raise part of their
// unknown-id 404 from the *write* (`plugin_update_enabled` / `plugin_delete`
// report `NotFound` on `rows_affected() == 0`) and `reload` raises it only from
// its explicit probe. Put the guard first and "unknown id AND busy" answers 409
// instead of 404 on all four — an error code silently changed by the lock, and
// invisible to S0's unknown-id gates because those hold no lock.
// ===========================================================================

/// Mutation witness: move any of the four `try_lock_lifecycle` calls above its
/// `plugin_row_or_404` probe and that endpoint's arm below reports
/// `plugin_busy` instead of `not_found`.
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

// ===========================================================================
// #1196 S1 review r4 — `plugins_disabled` × {spawn, restart, rotate-token}
//
// This cell of the matrix had ZERO coverage, which is why a pre-lock probe
// shared between three entry points could silently rewrite rotate's error
// codes: the probe answered `HostError::Disabled` where rotation's own opening
// pair answers `NotFound` (unregistered) or `UnsupportedForKind` (connector),
// and the rotate route maps `Disabled` through its catch-all to **500** —
// "the kernel is broken" for a request that deleted nothing and restarted
// nothing. Same class as install 422→409 and enable 404→409, which this slice
// spent two rounds preventing.
//
// The HTTP half of the contract lives with the mapping function
// (`routes::plugins::rotate_error_mapping_tests`); this is the host half, and
// only the two together state an endpoint contract.
// ===========================================================================

/// Every `plugins_disabled` cell of the three `HostError` lifecycle entries.
///
/// Mutation witnesses (each applied alone):
/// * add the `plugins_disabled` check back to `rotate_admission_check` (i.e.
///   re-share `spawn_admission_check` with rotate) → the `ghost`/`connector`
///   rotate arms below go red, reporting `Disabled` where 404 / 400 are owed;
/// * delete the `plugins_disabled` check from `spawn_admission_check` → the
///   `spawn`/`restart` arms go red (`NotFound`, or a real spawn, instead of
///   `Disabled`).
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
    // A registered connector. It is never brought up here — rotation refuses on
    // `kind` before touching the network — so a placeholder url is honest.
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
                "api_key_in": "query:api_key",
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
    // A token to watch: the one cell that legitimately reaches the delete must
    // be shown reaching it, and the two that must not must be shown not to.
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
        // Every id under test is behind the operator's kill switch — including
        // the ghost, which is the combination the shared probe got wrong.
        vec![APP.into(), CONNECTOR.into(), GHOST.into()],
        EventBus::new(),
        calm_server::state::WriteContext::new(
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::wave_cove_cache::WaveCoveCache::new(),
        ),
    ));

    // ---- spawn / restart: `Disabled` wins over `NotFound`, both before and
    // ---- inside the guard. This is the pair `spawn_under` really opens with.
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

    // ---- rotate: the kill switch is NOT rotation's opening question, so it
    // ---- must not be allowed to answer for these two cells.
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

    // ---- the documented residual, pinned so it cannot drift either way.
    // A REGISTERED APP in `plugins_disabled` does reach the delete and the
    // restart, and only then fails with `Disabled` (→ 500). "Before #1196
    // touched it" here means S1's first commit `695813b1` (parent: the merge
    // `3dd32702`), at which #1164's registry+kind guard and the route's
    // 404/400 arms were already in place and this cell already answered 500 —
    // NOT `main`'s merge-base with this branch, which predates #1164 and mapped
    // every rotate error to 500. It is a separate decision to change, and this
    // arm is what makes changing it deliberate.
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

// ===========================================================================
// #1196 S1 review r4 — the `app` boot fence's terminal arm is await-free
// ===========================================================================

/// The fence fires, the lock IS available, and the event store is still wedged.
///
/// `a19` cannot reach this: it holds the lifecycle lock for the whole test, so
/// `try_lock_lifecycle` always fails and only the log-and-move-on branch ever
/// runs. The publishing branch — the one the timeout arm exists for — had never
/// been executed by any gate, and it contained an **unbounded await outside the
/// fence** (`publish_unavailable_under` → `emit_state_under` → `log_pure_event`).
///
/// The reachable path is the one the fence's own reason string names: an `app`
/// spawn parks on a slow event store → the fence fires → dropping that future
/// releases its guard → `try_lock` now succeeds → boot waits on the same wedged
/// store, forever. `APP_AUTOSPAWN_WALL` then bounds nothing.
///
/// Mutation witness: change the timeout arm back to
/// `self.publish_unavailable_under(&g, None, reason).await` and this test fails
/// on its 10 s `expect` — boot never returns while the DB writer is held.
#[tokio::test]
async fn a21_the_app_boot_fence_terminal_never_waits_on_the_event_store() {
    let fx = boot_with(BootOpts {
        app_wall: Some(Duration::from_millis(300)),
        ..Default::default()
    })
    .await;

    // Every repo write parks from here on, including the token mint that is the
    // first thing an `app` spawn does inside its guard.
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
    // The fixture has to have been in force: a boot that never met the fence
    // would also be fast, and would prove nothing.
    assert!(
        elapsed >= Duration::from_millis(300),
        "boot returned in {elapsed:?}, faster than the 300 ms wall it was given \
         — the DB barrier is no longer parking the spawn"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "boot took {elapsed:?} against a 300 ms app wall"
    );

    // What is kept is the table half — the one `GET /api/plugins/{id}` reads.
    // Losing it is the "reported as if it had never been enabled" failure the
    // whole arm exists to prevent; losing the event half is the acknowledged
    // price, exactly as on the connector side.
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

    // And the guard was released, not leaked, on the way out.
    fx.host
        .try_lock_lifecycle(ID)
        .expect("the terminal arm must not have kept the lifecycle guard");

    barrier.release().await;
}

// ===========================================================================
// #1196 S1 review r4 — boot's upper bound is a pinned number, not a shape
// ===========================================================================

/// `PLUGIN_LIST_WALL`, `APP_AUTOSPAWN_WALL`, and the composed boot ceiling,
/// pinned as literals.
///
/// The behavioral gates override their walls through `PluginHost` builders, so
/// they cannot see the production constants at all. This is the app/list-side
/// counterpart of `the_connector_phase_ceiling_is_the_documented_one`: it pins
/// each constant and literal outputs of the one composition that turns them
/// into boot's closed bound, without duplicating that composition in the test.
#[test]
fn the_app_autospawn_wall_is_the_documented_one() {
    use calm_server::plugin_host::{
        APP_AUTOSPAWN_WALL, MAX_CONNECTOR_AUTOSPAWN_WALL, PLUGIN_LIST_WALL, boot_autospawn_ceiling,
    };

    // One local indexed DB list read, with boot continuing on expiry.
    assert_eq!(PLUGIN_LIST_WALL, Duration::from_secs(5));
    // Sized against a local fork/exec + `initialize` handshake + a few
    // persisted events — not against a network round trip.
    assert_eq!(APP_AUTOSPAWN_WALL, Duration::from_secs(30));
    // Pin literal results, not a second copy of the production arithmetic.
    assert_eq!(boot_autospawn_ceiling(0), Duration::from_millis(36_500));
    assert_eq!(boot_autospawn_ceiling(1), Duration::from_millis(66_500));
    assert_eq!(
        boot_autospawn_ceiling(4),
        Duration::from_millis(156_500),
        "if a constituent wall moves, change this pinned total deliberately"
    );
    // The ceiling is a real ceiling in both directions: it is never below
    // either component.
    assert!(boot_autospawn_ceiling(1) > MAX_CONNECTOR_AUTOSPAWN_WALL);
    assert!(boot_autospawn_ceiling(1) > APP_AUTOSPAWN_WALL);
    assert!(boot_autospawn_ceiling(0) > PLUGIN_LIST_WALL);
}

// ===========================================================================
// #1196 S1 review r5 — the `n > 1` term of `boot_autospawn_ceiling` is executed,
// not just computed
// ===========================================================================

/// Two wedged `app` plugins cost **two** walls, not one shared one.
///
/// `the_app_autospawn_wall_is_the_documented_one` pins the four-app output of
/// `boot_autospawn_ceiling`, but that is only an output of a `const fn`: it would
/// hold verbatim if the loop fenced all apps with a single shared deadline, in
/// which case the documented `N ×` shape would be a claim about a function
/// nothing in boot uses that way. `a19` runs the fence for exactly one plugin,
/// so no gate had ever executed the multiplier.
/// This one does: it is the smallest `n` at which "additive" and "shared" give
/// different answers.
///
/// Both plugins are wedged the way `a19` wedges its one — the lifecycle guard is
/// taken by the test and never released — so each iteration must run its fence
/// to expiry, and the only thing that can make boot return early is the fences
/// sharing a budget.
///
/// Mutation witness: hoist the `app` branch's bound out of the loop — compute
/// `let deadline = Instant::now() + self.app_autospawn_wall;` before the `for`
/// and swap the per-iteration `tokio::time::timeout(self.app_autospawn_wall, …)`
/// for `tokio::time::timeout_at(deadline, …)`. The second plugin then gets
/// whatever is left of one wall (nothing), boot returns in ~1 × WALL, and the
/// lower-bound assertion below goes red.
#[tokio::test]
async fn two_wedged_app_plugins_cost_two_walls_not_one() {
    const A: &str = "test.lock.two.a";
    const B: &str = "test.lock.two.b";
    /// Small enough for a fast test, large enough that 1 × and 2 × cannot be
    /// told apart by scheduling noise (the assertions leave a 300 ms band on
    /// the low side and 3 s of headroom on the high side).
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
                calm_server::wave_cove_cache::WaveCoveCache::new(),
            ),
        )
        .with_app_autospawn_wall(WALL),
    );

    // Wedge BOTH. Nothing in this test ever drops either guard, so neither
    // iteration can finish early for a reason other than its own fence.
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

    // The fixture has to have been in force for BOTH, not just the first: a
    // plugin that was never reached is also a plugin that never started, and
    // that would be indistinguishable from a fence firing if we only looked at
    // wall clock. Neither may have been started, and — because this test holds
    // both guards for its whole life — the timeout arm's `try_lock_lifecycle`
    // fails for both, so both take the log-and-move-on branch and leave no
    // runtime entry. (`a21` is the gate for the other branch, where the guard
    // IS available and a terminal `Unavailable` must appear.)
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
