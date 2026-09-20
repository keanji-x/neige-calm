//! Smoke tests for `PluginHost` against the stub plugin binaries
//! (`plugin-host-stub-echo`, `plugin-host-stub-crash`).

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

/// The stub is symlinked into `<plugins_dir>/<id>/bin/stub` so manifest validation
/// (which rejects absolute `entrypoint.command`) sees a relative path.
async fn boot_host(plugin_id: &str, stub_bin: &str) -> (Arc<PluginHost>, TempDir, EventBus) {
    boot_host_with_min_kernel(plugin_id, stub_bin, "0.0.1").await
}

async fn boot_host_with_min_kernel(
    plugin_id: &str,
    stub_bin: &str,
    min_kernel_version: &str,
) -> (Arc<PluginHost>, TempDir, EventBus) {
    let (host, _repo, tmp, events) = host_parts(plugin_id, stub_bin, min_kernel_version).await;
    (Arc::new(host), tmp, events)
}

/// Handed back unbuilt so a caller can apply a post-construction builder before wrapping in `Arc`.
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
    // Seed the plugins row directly to satisfy the `plugin_tokens.plugin_id` FK at token-set time.
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

/// The cold `WriteContext` every `PluginHost` fixture is built with. Call this rather than
/// inlining: the cache paths are vocabulary the terminology-ratchet gate counts by occurrence.
pub(super) fn test_write_context() -> calm_server::state::WriteContext {
    calm_server::state::WriteContext::new(
        calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::track_area_cache::TrackAreaCache::new(),
    )
}

/// [`boot_host`] with the crash-window / backoff knobs injected, and the repo handed back so the test can count events.
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

    let live = host.status("test.echo").await.expect("status");
    assert!(live.pid.is_some(), "expected pid populated, got {:?}", live);

    host.stop("test.echo").await.expect("stop");
}

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

#[tokio::test]
async fn crash_stub_respawns_after_first_crash() {
    let (host, _tmp, mut events_rx) = boot_host_with_subscribe("test.crash1", CRASH_BIN).await;
    host.spawn("test.crash1").await.expect("spawn");

    // The crash stub returns from initialize then immediately exits, so the Running phase is brief.
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

/// The gate is the count, not a dwell time: a stub that re-crashes instantly sits in
/// `Crashed` for nearly all of any window you sample.
#[tokio::test]
async fn crash_loop_stops_respawning_at_the_window_limit() {
    const LIMIT: u32 = 4;
    let (host, repo, _tmp) = boot_host_with_backoff(
        "test.crashloop",
        CRASH_BIN,
        // Short, flat backoff: the point under test is the counter.
        vec![80],
        Duration::from_secs(300),
        LIMIT,
    )
    .await;

    host.spawn("test.crashloop").await.expect("spawn");

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
            assert_ne!(k.actual.to_string(), "99.0.0");
        }
        other => panic!("expected KernelTooOld, got {other:?}"),
    }
    assert!(host.status("test.toonew").await.is_none());
}

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
