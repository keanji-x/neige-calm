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
    HostError, Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus,
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
fn write_plugin_with_args(
    plugins_dir: &Path,
    id: &str,
    stub_bin: &str,
    args: &[&str],
) -> PathBuf {
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
    /// Pre-built repo, for the tests that must construct their
    /// [`LifecycleDb`] fake around the same handle the host will use.
    repo: Option<Arc<dyn Repo>>,
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
            repo: None,
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
        Vec::new(),
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

    let err = fx.host.restart(ID).await.expect_err("restart must be refused");
    assert_busy_host(&err, "restart");
    assert_eq!(snapshot(&fx, ID).await, before);
    assert_eq!(
        fx.host.status(ID).await.unwrap().pid,
        pid_before,
        "a refused restart must not have replaced the process"
    );

    drop(held);
    fx.host.restart(ID).await.expect("restart succeeds on retry");
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

    let err = fx.host.enable(ID).await.expect_err("enable must be refused");
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

    let err = fx.host.disable(ID).await.expect_err("disable must be refused");
    assert_busy_calm(&err, "disable");
    assert_eq!(snapshot(&fx, ID).await, before, "a refused disable did work");

    drop(held);
    let plug = fx.host.disable(ID).await.expect("disable succeeds on retry");
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

    let err = fx.host.reload(ID).await.expect_err("reload must be refused");
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
/// the work (i.e. re-enter through the wrapper) and every entry below that
/// stops — `disable`, `uninstall`, `reload`, `restart`, `rotate_plugin_token` —
/// starts answering `plugin_busy`.
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
            Op::Rotate => host.rotate_plugin_token(ID).await.map_err(|e| e.to_string()),
            Op::Enable => host.enable(ID).await.map(|_| ()).map_err(|e| e.to_string()),
            Op::Disable => host.disable(ID).await.map(|_| ()).map_err(|e| e.to_string()),
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
            assert!(busy <= 1, "{a:?} + {b:?}: both reported busy ({ra:?}, {rb:?})");

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
/// Mutation witnesses, one per half:
/// * move the `tokio::time::sleep` inside the guard (i.e. hold the guard across
///   segments 1–3) → the `disable` blocks for the whole backoff and the
///   `elapsed` assertion fails;
/// * delete segment 3's live/epoch/attempt predicate → the supervisor wakes and
///   respawns the plugin the operator disabled, and the terminal assertions
///   fail.
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

/// Same shape for `uninstall`: waking from the backoff onto a plugin that no
/// longer exists must not respawn it.
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

    fx.host.uninstall(ID).await.expect("uninstall during backoff");

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
    let held = fx.host.try_lock_lifecycle(ID).expect("lock is free mid-backoff");
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
            return Err(CalmError::Internal("injected plugin-row read failure".into()));
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
        state_events(&fx).await.iter().filter(|s| *s == "running").count(),
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
/// The `enabled` bit cannot catch this case (the plugin is still enabled) and
/// neither can the registry check (it is still installed). Only the per-instance
/// identity can: `run_epoch` (and the `Crashed`-state check) says "the entry in
/// front of me is not the one I was supervising". Without it the old supervisor
/// removes a healthy live entry and respawns over it, orphaning the running
/// child.
///
/// The reload swaps the entrypoint from the crash stub to the echo stub, so the
/// replacement instance is one that *stays* Running and a resurrection is
/// visible as a changed pid.
///
/// Mutation witness: delete the `ok` predicate block at the top of
/// `respawn_after_backoff` and the pid changes under you.
#[tokio::test]
async fn a9b_a_late_supervisor_leaves_a_newer_run_instance_alone() {
    const BACKOFF: u64 = 2_500;
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

    // Republish the manifest against a stub that STAYS up, then revive the
    // plugin with an explicit `spawn`.
    //
    // Deliberately not `reload`/`restart` here: those call `stop_under`, which
    // aborts the sleeping supervisor outright, and an aborted task cannot
    // witness anything. An explicit `spawn` on a `Crashed` entry is admitted,
    // replaces the live entry (and with it the supervisor handle — dropping a
    // `JoinHandle` does not abort its task), and leaves the old supervisor
    // alive and sleeping. That is the one reachable shape in which a stale
    // supervisor meets a newer run instance.
    let dir = fx.plugins_dir.join(ID);
    std::os::unix::fs::symlink(Path::new(ECHO_BIN), dir.join("bin").join("stub2")).unwrap();
    let echo_manifest = Manifest::parse(
        &json!({
            "manifest_version": 1,
            "id": ID,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Lock Stub",
            "entrypoint": { "command": "bin/stub2" },
        })
        .to_string(),
    )
    .unwrap();
    {
        // A runtime registry write needs the id's guard — `try_lock_lifecycle`
        // is `pub` for exactly this (design §5 R7).
        let g = fx.host.try_lock_lifecycle(ID).expect("lock is free mid-backoff");
        fx.host.registry_insert(&g, echo_manifest, Some(dir));
    }
    fx.host.spawn(ID).await.expect("explicit revive");
    let pid = fx
        .host
        .status(ID)
        .await
        .expect("live after the revive")
        .pid
        .expect("app plugin has a pid");

    // Now let the ORIGINAL supervisor's backoff elapse.
    sleep(Duration::from_millis(BACKOFF + 1_200)).await;

    let after = fx.host.status(ID).await.expect("still live");
    assert!(
        matches!(after.status, PluginRuntimeStatus::Running),
        "the newer instance must still be Running, got {:?}",
        after.status
    );
    assert_eq!(
        after.pid,
        Some(pid),
        "the stale supervisor respawned over a run instance that was not its own"
    );
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
