//! Smoke tests for `PluginHost` (Slice B).
//!
//! These tests build two stub plugins as side-artifact binaries in the same
//! crate (`plugin-host-stub-echo` and `plugin-host-stub-crash`) and locate
//! them at runtime via `env!("CARGO_BIN_EXE_<name>")`. The stubs implement a
//! minimal subset of the MCP `initialize` handshake — enough for the
//! supervisor to flip a plugin to `Running`.
//!
//! Slice B's binding spec lists four assertions; this file covers all of them:
//!
//!   1. Spawn echo stub → `PluginRuntimeStatus::Running` within 2 s.
//!   2. `stop()` → child exits within grace, status drops out of the table.
//!   3. Crash stub → state transitions to `Crashed`, then auto-respawns.
//!   4. Crash-loop disable: 5 fast crashes → status stuck at `Crashed`, no
//!      more respawns until an explicit `spawn(id)` is invoked.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::plugin_host::{
    HostError, Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus,
};
use serde_json::json;
use tempfile::TempDir;
use tokio::time::{Instant, sleep};

const ECHO_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-echo");
const CRASH_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-crash");

/// Build a `PluginHost` rooted at a fresh temp dir, with one manifest seeded
/// in the registry pointing at the requested stub binary.
///
/// We synthesize a faux install layout: `<plugins_dir>/<id>/bin/stub`. The
/// stub binary is symlinked from the real artifact so manifest validation
/// (which rejects absolute `entrypoint.command`) sees a sane relative path.
async fn boot_host(plugin_id: &str, stub_bin: &str) -> (Arc<PluginHost>, TempDir, EventBus) {
    boot_host_with_min_kernel(plugin_id, stub_bin, "0.0.1").await
}

/// Same as `boot_host`, but lets the test override the manifest's
/// `min_kernel_version`. Used by the issue-#45 spawn-refusal test.
async fn boot_host_with_min_kernel(
    plugin_id: &str,
    stub_bin: &str,
    min_kernel_version: &str,
) -> (Arc<PluginHost>, TempDir, EventBus) {
    let (host, _repo, tmp, events) = host_parts(plugin_id, stub_bin, min_kernel_version).await;
    (Arc::new(host), tmp, events)
}

/// The pieces every fixture in this file is built from, handed back **unbuilt**
/// so a caller can apply a post-construction builder before wrapping in `Arc`.
async fn host_parts(
    plugin_id: &str,
    stub_bin: &str,
    min_kernel_version: &str,
) -> (
    PluginHost,
    Arc<dyn calm_server::db::Repo>,
    TempDir,
    EventBus,
) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    let install_dir = plugins_dir.join(plugin_id);
    let bin_dir = install_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&plugins_data_dir).unwrap();
    // Symlink the stub binary into bin/stub (unix-only test).
    std::os::unix::fs::symlink(Path::new(stub_bin), bin_dir.join("stub")).unwrap();

    let manifest_json = json!({
        "manifest_version": 1,
        "id": plugin_id,
        "version": "0.1.0",
        "min_kernel_version": min_kernel_version,
        "display_name": "Smoke Stub",
        "entrypoint": { "command": "bin/stub" }
    });
    let manifest: Manifest = Manifest::parse(&manifest_json.to_string()).expect("manifest parses");

    let registry = PluginRegistry::from_manifests([(manifest, Some(install_dir.clone()))]);
    let events = EventBus::new();
    let repo: Arc<dyn calm_server::db::Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite repo"),
    );
    // Production flow inserts the plugins row via the REST install handler
    // before `spawn`; we bypass that here, so seed the row directly to satisfy
    // the `plugin_tokens.plugin_id` FK at token-set time.
    repo.plugin_install(calm_server::model::NewPlugin {
        id: plugin_id.into(),
        version: "0.1.0".into(),
        install_path: install_dir.display().to_string(),
        manifest: json!({}),
        enabled: true,
        user_config: json!({}),
    })
    .await
    .expect("seed plugin row");
    let host = PluginHost::new_full(
        Arc::new(registry),
        repo.clone(),
        plugins_dir,
        plugins_data_dir,
        Vec::new(),
        events.clone(),
        test_write_context(),
    );
    (host, repo, tmp, events)
}

/// The cold `WriteContext` a `PluginHost` fixture is built with: both caches
/// start empty and no case in `tests/plugin_suite.rs` reads them back, so one
/// constructor serves the whole suite.
///
/// Shared, rather than re-spelled in each case module, for a reason that is not
/// only tidiness: the two cache paths spell vocabulary that
/// `scripts/gate-1316-terminology-ratchet.sh` ratchets by occurrence under
/// `crates/`, so a new case that inlines this construction again raises those
/// counts and fails the gate while adding no coverage at all. Call this
/// instead; `plugin_config_delivery` does.
pub(super) fn test_write_context() -> calm_server::state::WriteContext {
    calm_server::state::WriteContext::new(
        calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::wave_area_cache::WaveAreaCache::new(),
    )
}

/// #1196 acceptance 13 — [`boot_host`] with the crash-window / backoff knobs
/// injected through the post-construction builder, and the repo handed back so
/// the test can count the *events* rather than sample a status.
///
/// A builder rather than `new_full` parameters: `new_full` has ~106 call sites
/// and none of them care. `#[cfg(test)]` was not an option — this file is an
/// integration test, i.e. a separate crate linking the non-test lib.
async fn boot_host_with_backoff(
    plugin_id: &str,
    stub_bin: &str,
    schedule_ms: Vec<u64>,
    crash_window: Duration,
    crash_window_limit: u32,
) -> (Arc<PluginHost>, Arc<dyn calm_server::db::Repo>, TempDir) {
    let (host, repo, tmp, _events) = host_parts(plugin_id, stub_bin, "0.0.1").await;
    let host = Arc::new(host.with_backoff_schedule(schedule_ms, crash_window, crash_window_limit));
    (host, repo, tmp)
}

async fn wait_for_status(
    host: &Arc<PluginHost>,
    id: &str,
    pred: impl Fn(&PluginRuntimeStatus) -> bool,
    timeout: Duration,
) -> PluginRuntimeStatus {
    let start = Instant::now();
    loop {
        if let Some(s) = host.status(id).await
            && pred(&s.status)
        {
            return s.status;
        }
        if start.elapsed() > timeout {
            let last = host.status(id).await.map(|s| s.status);
            panic!(
                "timeout waiting for status (got {:?}, elapsed {:?})",
                last,
                start.elapsed()
            );
        }
        sleep(Duration::from_millis(25)).await;
    }
}

// ---------------------------------------------------------------------------
// 1. Happy path: spawn → Running.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn echo_stub_reaches_running() {
    let (host, _tmp, _events) = boot_host("test.echo", ECHO_BIN).await;
    host.spawn("test.echo").await.expect("spawn");
    let status = wait_for_status(
        &host,
        "test.echo",
        |s| matches!(s, PluginRuntimeStatus::Running),
        Duration::from_secs(2),
    )
    .await;
    assert!(matches!(status, PluginRuntimeStatus::Running));

    // PID should be populated while running.
    let live = host.status("test.echo").await.expect("status");
    assert!(live.pid.is_some(), "expected pid populated, got {:?}", live);

    // Cleanup (also tests test #2 below — but stop is exercised separately).
    host.stop("test.echo").await.expect("stop");
}

// ---------------------------------------------------------------------------
// 2. Stop within grace.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn echo_stub_stops_within_grace() {
    let (host, _tmp, _events) = boot_host("test.echo2", ECHO_BIN).await;
    host.spawn("test.echo2").await.expect("spawn");
    wait_for_status(
        &host,
        "test.echo2",
        |s| matches!(s, PluginRuntimeStatus::Running),
        Duration::from_secs(2),
    )
    .await;
    let started = Instant::now();
    host.stop("test.echo2").await.expect("stop");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "stop took too long: {:?}",
        started.elapsed()
    );
    assert!(
        host.status("test.echo2").await.is_none(),
        "plugin should be gone from the table after stop"
    );
}

// ---------------------------------------------------------------------------
// 3. Crash stub → Running → Crashed → respawned to Running.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn crash_stub_respawns_after_first_crash() {
    let (host, _tmp, mut events_rx) = boot_host_with_subscribe("test.crash1", CRASH_BIN).await;
    host.spawn("test.crash1").await.expect("spawn");

    // Drain events looking for: Running → Crashed → Running. The crash stub
    // returns from initialize then immediately exits, so the Running phase
    // is brief.
    let mut saw_running_first = false;
    let mut saw_crashed = false;
    let deadline = Instant::now() + Duration::from_secs(8);

    while Instant::now() < deadline {
        let recv = tokio::time::timeout(Duration::from_secs(2), events_rx.recv()).await;
        let env = match recv {
            Ok(Ok(env)) => env,
            // Lagged or timeout — keep looping until the deadline.
            _ => continue,
        };
        if let calm_server::event::Event::PluginState { id, state, .. } = env.event
            && id == "test.crash1"
        {
            match (state.as_str(), saw_running_first, saw_crashed) {
                ("running", false, _) => saw_running_first = true,
                ("crashed", _, _) => saw_crashed = true,
                ("running", true, true) => {
                    // Respawn observed.
                    return;
                }
                _ => {}
            }
        }
    }
    panic!(
        "did not see full Running→Crashed→Running cycle (saw_running_first={saw_running_first}, saw_crashed={saw_crashed})"
    );
}

// ---------------------------------------------------------------------------
// 4. Crash-loop disable: CRASH_WINDOW_LIMIT crashes → no further respawn.
// ---------------------------------------------------------------------------

/// #1196 §1.3 / acceptance 13 — **rewritten**, because the previous version was
/// a fake gate.
///
/// It used to spawn the crash stub and pass if the status read `Crashed`, then
/// still read `Crashed` three seconds later. With the production backoff pinned
/// at 1 s (which is what the defect below caused) and a stub that re-crashes
/// instantly, the plugin sits in `Crashed` for nearly all of any window you
/// sample — so "still Crashed after 3 s" was satisfied by the crash **loop
/// itself**, the very thing the test claimed to prove had stopped. It passed
/// throughout the entire lifetime of the defect.
///
/// The defect: `supervise_inner` removed the live entry before respawning,
/// while `spawn` inherited `crashes_in_window` by reading that same entry — so
/// every respawn started from zero, `crashes_in_window` was permanently 1,
/// `exceeded` was permanently false, and the backoff never advanced past
/// `BACKOFF_SCHEDULE_MS[0]`. The supervisor now carries the counters across its
/// own `remove` explicitly.
///
/// The gate is the **count**, not a dwell time: exactly `LIMIT` crashes, then
/// no further `running`. The backoff schedule is injected through
/// `with_backoff_schedule` — a construction-time seam, because this is an
/// integration test and the lib's `#[cfg(test)]` is invisible here; falling back
/// to "wait 15 s and look at the status" would have rebuilt the fake gate.
///
/// Mutation witness: drop the `carried` argument in `respawn_after_backoff`
/// (pass `None`) and the crash count never reaches the limit — the plugin keeps
/// respawning and the "no more than LIMIT crashes" assertion fails.
#[tokio::test]
async fn crash_loop_stops_respawning_at_the_window_limit() {
    const LIMIT: u32 = 4;
    let (host, repo, _tmp) = boot_host_with_backoff(
        "test.crashloop",
        CRASH_BIN,
        // Short, flat backoff: the point under test is the counter, and a
        // production-shaped 1/2/4/8 s schedule would make this a ~15 s test.
        vec![80],
        Duration::from_secs(300),
        LIMIT,
    )
    .await;

    host.spawn("test.crashloop").await.expect("spawn");

    // Wait for the limit to be reached: LIMIT crash events, and no more.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let crashes = crash_count(&repo, "test.crashloop").await;
        if crashes >= LIMIT as usize {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the crash-window limit was never reached (saw {crashes} crashes);              `crashes_in_window` is not accumulating across respawns"
        );
        sleep(Duration::from_millis(25)).await;
    }

    // ... and it must STOP there. Two full backoff periods of slack.
    sleep(Duration::from_millis(800)).await;
    assert_eq!(
        crash_count(&repo, "test.crashloop").await,
        LIMIT as usize,
        "the supervisor kept respawning past the crash-window limit"
    );
    assert!(
        matches!(
            host.status("test.crashloop").await.map(|s| s.status),
            Some(PluginRuntimeStatus::Crashed { .. })
        ),
        "the plugin must stay observably Crashed so an operator can see it"
    );
    // The `Crashed` entry is kept on purpose: an explicit spawn revives it.
    host.spawn("test.crashloop").await.expect("explicit revive");
}

/// Persisted `crashed` events for `id`.
async fn crash_count(repo: &Arc<dyn calm_server::db::Repo>, id: &str) -> usize {
    repo.events_since(0, 1000)
        .await
        .expect("events")
        .into_iter()
        .filter(|(_, _, _, e)| {
            matches!(e, calm_server::event::Event::PluginState { id: who, state, .. }
                if who == id && state == "crashed")
        })
        .count()
}

// ---------------------------------------------------------------------------
// 5. Issue #45 — min_kernel_version gate refuses incompatible plugins.
// ---------------------------------------------------------------------------

/// A manifest demanding kernel 99.0.0 must fail `spawn` with `KernelTooOld`
/// *before* any process work. We assert: (a) the error variant is right,
/// (b) it carries both versions, and (c) no plugin row ever flipped to
/// `Running` / `Spawning` (the spawn aborts upstream of the processes map).
#[tokio::test]
async fn spawn_refuses_plugin_requiring_newer_kernel() {
    let (host, _tmp, _events) = boot_host_with_min_kernel("test.toonew", ECHO_BIN, "99.0.0").await;
    let err = host
        .spawn("test.toonew")
        .await
        .expect_err("spawn should refuse a 99.0.0-requiring plugin");
    match err {
        HostError::KernelTooOld(k) => {
            assert_eq!(k.required.to_string(), "99.0.0");
            // We don't pin actual; just confirm the field is populated and is
            // a non-99 kernel (otherwise the test environment is doing
            // something unexpected).
            assert_ne!(k.actual.to_string(), "99.0.0");
        }
        other => panic!("expected KernelTooOld, got {other:?}"),
    }
    // The spawn aborted before touching the processes map.
    assert!(host.status("test.toonew").await.is_none());
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn boot_host_with_subscribe(
    plugin_id: &str,
    stub_bin: &str,
) -> (
    Arc<PluginHost>,
    TempDir,
    tokio::sync::broadcast::Receiver<calm_server::event::BroadcastEnvelope>,
) {
    let (host, tmp, events) = boot_host(plugin_id, stub_bin).await;
    let rx = events.subscribe();
    (host, tmp, rx)
}

#[allow(dead_code)]
fn echo_bin_path() -> PathBuf {
    PathBuf::from(ECHO_BIN)
}
