#![cfg(unix)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::config::Config;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::dispatcher::Dispatcher;
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::{ActorId, CoveId, WaveId};
use calm_server::model::{CardRole, NewCove, NewWave};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{CodexClient, DaemonClient};
use calm_server::terminal_renderer::TerminalRendererRegistry;
use clap::Parser;
use serde_json::{Value, json};
use tempfile::TempDir;

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn fake_codex_bin() -> String {
    env!("CARGO_BIN_EXE_osc-probe-child").to_string()
}

struct Boot {
    repo: Arc<dyn Repo>,
    events: EventBus,
    cache: CardRoleCache,
    wcc: calm_server::wave_cove_cache::WaveCoveCache,
    wave_id: WaveId,
    cove_id: CoveId,
    codex: Arc<CodexClient>,
    daemon: Arc<DaemonClient>,
    renderer: Arc<TerminalRendererRegistry>,
    shared: Arc<SharedCodexAppServer>,
    _tmp: TempDir,
}

async fn boot(start_shared: bool) -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "worker-shared".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "worker-shared".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let events = EventBus::new();
    let cache = CardRoleCache::new();
    repo.seed_card_role_cache(&cache).await.unwrap();
    let wcc = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wcc).await.unwrap();

    let mut codex = CodexClient::new_stub();
    codex.codex_bin = fake_codex_bin();
    let codex = Arc::new(codex);
    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().join("terminals"),
        proc_supervisor_sock: None,
    });
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let renderer = TerminalRendererRegistry::new_with_repo(route_repo);

    let fake_codex_bin = fake_codex_bin();
    let cfg = Config::parse_from([
        "calm-server",
        "--data-dir",
        tmp.path().to_str().unwrap(),
        "--codex-bin",
        fake_codex_bin.as_str(),
        "--shared-codex-appserver-restart-initial-delay-ms",
        "10",
        "--shared-codex-appserver-restart-max-delay-ms",
        "50",
    ]);
    let home = calm_server::shared_codex_home::SharedCodexHome::new(
        cfg.data_dir_resolved().join("codex-home"),
        cfg.data_dir_resolved().join("codex-homes"),
    );
    home.seed_from(None).unwrap();
    let shared = SharedCodexAppServer::new_with_pending(&cfg, Arc::new(home), repo.clone(), None);
    if start_shared {
        shared.start_or_takeover().await.unwrap();
    }

    Boot {
        repo,
        events,
        cache,
        wcc,
        wave_id: wave.id,
        cove_id: cove.id,
        codex,
        daemon,
        renderer,
        shared,
        _tmp: tmp,
    }
}

fn spawn_dispatcher(boot: &Boot) -> Dispatcher {
    spawn_dispatcher_with_permits(boot, 4)
}

fn spawn_dispatcher_with_permits(boot: &Boot, permits: usize) -> Dispatcher {
    Dispatcher::spawn_with_terminal_renderer(
        boot.repo.clone(),
        boot.events.clone(),
        boot.cache.clone(),
        boot.wcc.clone(),
        boot.codex.clone(),
        boot.daemon.clone(),
        boot.renderer.clone(),
        None,
        calm_server::spec_push::SpecPushRegistry::new(),
        boot.shared.clone(),
        permits,
    )
}

fn codex_req(idem: &str, goal: &str) -> Event {
    Event::CodexJobRequested {
        idempotency_key: idem.into(),
        goal: goal.into(),
        context: json!({"from": "worker-shared-test"}),
        acceptance_criteria: Some("finish".into()),
    }
}

fn wave_scope(wave: &WaveId, cove: &CoveId) -> EventScope {
    EventScope::Wave {
        wave: wave.clone(),
        cove: cove.clone(),
    }
}

async fn dispatch(boot: &Boot, idem: &str, goal: &str) {
    boot.repo
        .log_pure_event(
            ActorId::User,
            wave_scope(&boot.wave_id, &boot.cove_id),
            None,
            &boot.events,
            &boot.cache,
            &boot.wcc,
            codex_req(idem, goal),
        )
        .await
        .unwrap();
}

async fn wait_for<F, Fut, T>(timeout: Duration, mut f: F) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(v) = f().await {
            return Some(v);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_requests(path: &Path, min_count: usize) -> Vec<Value> {
    for _ in 0..50 {
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
    panic!("timed out waiting for fake codex requests");
}

async fn worker_card_count_by_idem(boot: &Boot, idem: &str) -> usize {
    boot.repo
        .cards_by_wave(boot.wave_id.as_str())
        .await
        .unwrap()
        .into_iter()
        .filter(|card| card.payload.get("idempotency_key").and_then(Value::as_str) == Some(idem))
        .count()
}

async fn worker_card_count_with_prefix(boot: &Boot, prefix: &str) -> usize {
    boot.repo
        .cards_by_wave(boot.wave_id.as_str())
        .await
        .unwrap()
        .into_iter()
        .filter(|card| {
            card.payload
                .get("idempotency_key")
                .and_then(Value::as_str)
                .is_some_and(|idem| idem.starts_with(prefix))
        })
        .count()
}

#[tokio::test]
async fn worker_via_shared_daemon_dedupes_same_idempotency_key() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot(true).await;
    let _dispatcher = spawn_dispatcher(&boot);

    let idem = "shared-dup-key";
    dispatch(&boot, idem, "dedup shared worker").await;
    dispatch(&boot, idem, "dedup shared worker").await;

    wait_for(Duration::from_secs(5), || async {
        (worker_card_count_by_idem(&boot, idem).await == 1).then_some(())
    })
    .await
    .expect("first shared worker card minted");
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        worker_card_count_by_idem(&boot, idem).await,
        1,
        "duplicate shared-worker idempotency_key must create exactly one card"
    );
}

#[tokio::test]
async fn worker_via_shared_daemon_dedupes_under_real_concurrent_race() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot(true).await;
    let _dispatcher = spawn_dispatcher(&boot);

    let idem = "shared-race-key";
    tokio::join!(
        dispatch(&boot, idem, "race shared worker"),
        dispatch(&boot, idem, "race shared worker"),
    );

    wait_for(Duration::from_secs(5), || async {
        (worker_card_count_by_idem(&boot, idem).await == 1).then_some(())
    })
    .await
    .expect("one shared worker card minted after concurrent duplicate requests");
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        worker_card_count_by_idem(&boot, idem).await,
        1,
        "concurrent duplicate shared-worker dispatches must not both mint cards"
    );
}

#[tokio::test]
async fn worker_via_shared_daemon_semaphore_caps_concurrent_spawns() {
    let _guard = ENV_LOCK.lock().await;
    let boot = boot(true).await;
    let dispatcher = spawn_dispatcher_with_permits(&boot, 1);
    assert_eq!(dispatcher.permits(), 1);
    let sem = dispatcher.semaphore();
    let held_permit = sem.clone().acquire_owned().await.unwrap();

    for i in 0..2 {
        dispatch(&boot, &format!("shared-cap-{i}"), "cap shared worker").await;
    }

    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        worker_card_count_with_prefix(&boot, "shared-cap-").await,
        0,
        "shared workers must wait while the only permit is occupied"
    );
    assert_eq!(sem.available_permits(), 0);

    drop(held_permit);

    wait_for(Duration::from_secs(10), || async {
        (worker_card_count_with_prefix(&boot, "shared-cap-").await >= 1).then_some(())
    })
    .await
    .expect("a queued shared worker should mint after the permit is released");
}

#[tokio::test]
async fn worker_via_shared_daemon_persists_thread_mapping() {
    let _guard = ENV_LOCK.lock().await;
    let capture = TempDir::new().unwrap();
    let capture_file = capture.path().join("requests.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture_file);
    }
    let boot = boot(true).await;
    let _dispatcher = spawn_dispatcher(&boot);
    dispatch(&boot, "shared-worker-1", "do shared worker thing").await;
    let card = wait_for(Duration::from_secs(5), || async {
        let cards = boot
            .repo
            .cards_by_wave(boot.wave_id.as_str())
            .await
            .unwrap();
        cards.into_iter().find(|c| {
            c.payload.get("idempotency_key").and_then(Value::as_str) == Some("shared-worker-1")
                && c.payload.get("codex_source").and_then(Value::as_str) == Some("shared")
        })
    })
    .await
    .expect("shared worker card with runtime markers");
    unsafe {
        std::env::remove_var("FAKE_CODEX_CAPTURE_REQUESTS");
    }

    assert_eq!(card.payload["codex_source"], "shared");
    assert_eq!(card.payload["codex_thread_id"], "fake-thread-0001");
    assert_eq!(card.payload["appserver_sock"], boot.shared.remote_uri());
    assert!(card.payload.get("appserver_pgid").is_none());
    let mapping = boot
        .repo
        .card_codex_thread_get_by_card(card.id.as_str())
        .await
        .unwrap()
        .expect("mapping");
    assert_eq!(mapping.role, CardRole::Worker);
    assert_eq!(mapping.thread_id, "fake-thread-0001");
    let terminal_id = card.payload["terminal_id"].as_str().unwrap();
    let entry = wait_for(Duration::from_secs(3), || async {
        boot.renderer.get(terminal_id)
    })
    .await
    .expect("renderer entry");
    let shell_line = &entry.config().args[1];
    assert!(
        shell_line.contains("codex resume 'fake-thread-0001' --remote 'unix://"),
        "shared worker TUI must resume the shared thread: {shell_line}"
    );
    assert!(
        !shell_line.contains("do shared worker thing"),
        "shared worker TUI argv must not carry the positional prompt: {shell_line}"
    );
    let envs = entry.config().envs.to_vec();
    assert!(
        envs.iter()
            .any(|(k, v)| k == "CODEX_HOME" && v == &boot.shared.status_snapshot().codex_home),
        "shared worker TUI env must use shared CODEX_HOME: {envs:?}"
    );
    let rows = wait_for_requests(&capture_file, 3).await;
    assert!(
        rows.iter()
            .any(|row| row.get("method").and_then(Value::as_str) == Some("turn/start")),
        "shared daemon should receive turn/start: {rows:?}"
    );
    // The shared worker must be started with the Worker-role developer
    // instructions — otherwise the agent on the shared daemon behaves like
    // a plain prompt session and skips the calm.task_completed /
    // calm.task_failed reporting contract the legacy per-card path enforces
    // via CODEX_HOME/config.toml. Assert thread/start carried them.
    let thread_start = rows
        .iter()
        .find(|row| row.get("method").and_then(Value::as_str) == Some("thread/start"))
        .expect("shared daemon should receive thread/start");
    let developer_instructions = thread_start
        .pointer("/params/developerInstructions")
        .and_then(Value::as_str)
        .or_else(|| {
            thread_start
                .pointer("/params/developer_instructions")
                .and_then(Value::as_str)
        })
        .expect("thread/start params must carry developer_instructions");
    assert!(
        developer_instructions.contains("worker agent under spec card"),
        "developer_instructions must be the Worker prompt: {developer_instructions}"
    );
    assert!(
        developer_instructions.contains("calm.task_completed"),
        "developer_instructions must include the task reporting contract: {developer_instructions}"
    );
}

#[tokio::test]
async fn worker_shared_daemon_stopped_rolls_back_card() {
    // ENV_LOCK protects against env-var pollution from concurrent tests
    // (FAKE_CODEX_CAPTURE_REQUESTS / FAKE_CODEX_PTY_FAIL / etc) that would
    // affect the fake daemon and the renderer-entry expectation here.
    let _guard = ENV_LOCK.lock().await;
    let boot = boot(false).await;
    let _dispatcher = spawn_dispatcher(&boot);
    let mut rx = boot.events.subscribe();
    dispatch(&boot, "shared-stopped-1", "shared daemon stopped").await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let env = rx.recv().await.unwrap();
            if let Event::TaskFailed {
                idempotency_key, ..
            } = env.event
                && idempotency_key == "shared-stopped-1"
            {
                break;
            }
        }
    })
    .await
    .expect("task.failed");

    let cards = boot
        .repo
        .cards_by_wave(boot.wave_id.as_str())
        .await
        .unwrap();
    assert!(
        cards.iter().all(|card| {
            card.payload.get("idempotency_key").and_then(Value::as_str) != Some("shared-stopped-1")
        }),
        "failed shared worker spawn must roll back orphan worker card"
    );
}

#[tokio::test]
async fn worker_turn_start_failure_rolls_back_mapping_and_payload() {
    let _guard = ENV_LOCK.lock().await;
    unsafe {
        std::env::set_var("FAKE_CODEX_FAIL_TURN_START", "1");
    }
    let boot = boot(true).await;
    let _dispatcher = spawn_dispatcher(&boot);
    let mut rx = boot.events.subscribe();
    dispatch(&boot, "turn-fail-1", "turn start should fail").await;
    let failed = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let env = rx.recv().await.unwrap();
            if let Event::TaskFailed {
                idempotency_key, ..
            } = env.event
                && idempotency_key == "turn-fail-1"
            {
                break;
            }
        }
    })
    .await;
    unsafe {
        std::env::remove_var("FAKE_CODEX_FAIL_TURN_START");
    }
    failed.expect("task.failed");

    // After PR7b-worker R3: shared-worker turn_start failure runs
    // rollback_orphan_worker which DELETES the card + terminal rows entirely.
    // The card with idempotency_key="turn-fail-1" should not exist anywhere
    // (cards_by_wave returns no row with that key), and no worker-role
    // card_codex_threads mapping should remain. This clears the
    // idempotency_key so a retry of the same job can succeed (vs. being
    // short-circuited by find_card_by_idempotency_key_tx as already-done).
    // We poll briefly because the dispatcher's rollback happens async after
    // task.failed is emitted.
    let leftover = wait_for(Duration::from_secs(2), || async {
        let cards = boot
            .repo
            .cards_by_wave(boot.wave_id.as_str())
            .await
            .unwrap();
        let any_left = cards.into_iter().any(|c| {
            c.payload.get("idempotency_key").and_then(Value::as_str) == Some("turn-fail-1")
        });
        if any_left { None } else { Some(()) }
    })
    .await;
    assert!(
        leftover.is_some(),
        "turn_start rollback must delete the worker card row so idempotency_key clears for retry"
    );
}

#[tokio::test]
async fn worker_spawn_fail_after_turn_start_interrupts_turn() {
    let _guard = ENV_LOCK.lock().await;
    let capture = TempDir::new().unwrap();
    let capture_file = capture.path().join("requests.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture_file);
        std::env::set_var("FAKE_CODEX_PTY_FAIL", "1");
    }
    let boot = boot(true).await;
    let _dispatcher = spawn_dispatcher(&boot);
    let mut rx = boot.events.subscribe();
    dispatch(&boot, "pty-fail-1", "turn starts but pty fails").await;
    let failed = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let env = rx.recv().await.unwrap();
            if let Event::TaskFailed {
                idempotency_key, ..
            } = env.event
                && idempotency_key == "pty-fail-1"
            {
                break;
            }
        }
    })
    .await;
    let rows = wait_for_requests(&capture_file, 4).await;
    unsafe {
        std::env::remove_var("FAKE_CODEX_CAPTURE_REQUESTS");
        std::env::remove_var("FAKE_CODEX_PTY_FAIL");
    }
    failed.expect("task.failed");

    assert!(
        rows.iter().any(|row| {
            row.get("method").and_then(Value::as_str) == Some("turn/interrupt")
                && row.pointer("/params/threadId").and_then(Value::as_str)
                    == Some("fake-thread-0001")
                && row.pointer("/params/turnId").and_then(Value::as_str) == Some("fake-turn-0001")
        }),
        "worker PTY spawn failure must interrupt the in-flight shared turn: {rows:?}"
    );
    let leftover = wait_for(Duration::from_secs(2), || async {
        let cards = boot
            .repo
            .cards_by_wave(boot.wave_id.as_str())
            .await
            .unwrap();
        let any_left = cards.into_iter().any(|c| {
            c.payload.get("idempotency_key").and_then(Value::as_str) == Some("pty-fail-1")
        });
        if any_left { None } else { Some(()) }
    })
    .await;
    assert!(
        leftover.is_some(),
        "PTY spawn rollback must delete the worker card row so idempotency_key clears for retry"
    );
}
