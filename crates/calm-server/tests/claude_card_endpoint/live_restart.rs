//! #2516: restarting a Claude card whose child is running, with real processes under a real proc
//! supervisor. The child is a stand-in for the Claude CLI that logs each start and each SIGTERM.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_proc_supervisor::test_support::InProcessProcSupervisor;
use calm_server::db::prelude::*;
use calm_server::test_seams::{CLAUDE_RESTART_BEFORE_STOP, PausePoint, install_pause_for_test};
use serde_json::json;
use tokio::sync::Notify;
use tower::ServiceExt;

use super::{
    Boot, ENV_LOCK, RealClaude, body, boot_with_real_claude, post, post_restart, response_json,
};

/// An anti-hang guard, not a latency contract: under a loaded host a start or an exit can take
/// many seconds, and a correct run never waits this long.
const BUDGET: Duration = Duration::from_secs(120);

/// The session id the running child's `/clear` starts; Claude's session ids are UUIDs.
const SESSION_AFTER_CLEAR: &str = "0b8e5d3a-2f41-4c6e-8a97-3c5d1e2f4a6b";

/// `term_delay`: how long the stand-in takes to exit after SIGTERM, as Claude flushing its session.
fn write_fake_claude(dir: &Path, log: &Path, term_delay: Duration) -> String {
    let bin = dir.join("fake-claude");
    let log = log.display();
    let delay = term_delay.as_secs();
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\n\
             echo \"start $$ $*\" >> '{log}'\n\
             trap \"echo term $$ >> '{log}'; sleep {delay}; exit 0\" TERM\n\
             sleep 600 &\n\
             wait\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin.display().to_string()
}

/// `(pid, argv)` of each start, in order.
fn starts(log: &Path) -> Vec<(i32, String)> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.strip_prefix("start "))
        .map(|rest| {
            let (pid, argv) = rest.split_once(' ').unwrap_or((rest, ""));
            (pid.parse().unwrap(), argv.to_string())
        })
        .collect()
}

fn got_sigterm(log: &Path, pid: i32) -> bool {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .any(|line| line == format!("term {pid}"))
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 delivers nothing; it only checks that the pid exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

async fn eventually(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + BUDGET;
    while !condition() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

struct LiveCard {
    boot: Boot,
    _supervisor: InProcessProcSupervisor,
    _dir: tempfile::TempDir,
    log: std::path::PathBuf,
    card_id: String,
    terminal_id: String,
    settings_path: String,
}

/// A Claude card created through the route, its child running under a real supervisor.
async fn live_card() -> LiveCard {
    live_card_exiting_after(Duration::ZERO).await
}

async fn live_card_exiting_after(term_delay: Duration) -> LiveCard {
    let supervisor = InProcessProcSupervisor::start().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("claude.log");
    let boot = boot_with_real_claude(RealClaude {
        sock: supervisor.sock().to_path_buf(),
        claude_bin: write_fake_claude(dir.path(), &log, term_delay),
    })
    .await;
    let mut create = body(None);
    create["cwd"] = json!(dir.path());
    let (status, created) = post(boot.app.clone(), &boot.track_id, create, None, None).await;
    assert_eq!(status, StatusCode::CREATED, "body={created:?}");
    let card_id = created["id"].as_str().unwrap().to_string();
    let terminal_id = created["payload"]["terminal_id"]
        .as_str()
        .unwrap()
        .to_string();
    let settings_path = created["payload"]["settings_path"]
        .as_str()
        .unwrap()
        .to_string();
    eventually("the first child starts", || starts(&log).len() == 1).await;
    LiveCard {
        boot,
        _supervisor: supervisor,
        _dir: dir,
        log,
        card_id,
        terminal_id,
        settings_path,
    }
}

async fn runtime_states(boot: &Boot, card_id: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT state FROM worker_sessions WHERE card_id = ?1 ORDER BY created_at_ms ASC, id ASC",
    )
    .bind(card_id)
    .fetch_all(boot.repo.pool())
    .await
    .unwrap()
}

async fn terminal_exit(boot: &Boot, terminal_id: &str) -> (Option<i32>, bool) {
    let term = boot.repo.terminal_get(terminal_id).await.unwrap().unwrap();
    (term.exit_code, term.signal_killed)
}

#[tokio::test]
async fn restart_of_a_running_card_stops_its_child_and_resumes_the_latest_session() {
    let _guard = ENV_LOCK.lock().await;
    let live = live_card().await;
    let (boot, log) = (&live.boot, &live.log);

    // The running child `/clear`s: its session id changes.
    let hook = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/internal/claude/hook?card_id={}", live.card_id))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "hook_event_name": "SessionStart",
                        "session_id": SESSION_AFTER_CLEAR,
                        "source": "clear",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let (hook_status, hook_body) = response_json(hook).await;
    assert_eq!(hook_status, StatusCode::OK, "body={hook_body:?}");
    // The server restarted while the child kept running: no renderer holds it.
    boot.state
        .terminal_renderer
        .forget_entry_for_test(&live.terminal_id);

    // A double submit while the child runs: both succeed. The drive interleaves them by phase, so
    // either each replaces the child in turn, or the first stands down for the second, whose
    // prepare superseded it.
    let (first, second) = tokio::join!(
        post_restart(boot.app.clone(), &live.card_id),
        post_restart(boot.app.clone(), &live.card_id),
    );
    assert_eq!(first.0, StatusCode::OK, "body={:?}", first.1);
    assert_eq!(second.0, StatusCode::OK, "body={:?}", second.1);

    eventually(
        "only the last child runs; every earlier one got SIGTERM",
        || {
            let started = starts(log);
            let Some(((last, _), earlier)) = started.split_last() else {
                return false;
            };
            started.len() >= 2
                && alive(*last)
                && !got_sigterm(log, *last)
                && earlier
                    .iter()
                    .all(|(pid, _)| got_sigterm(log, *pid) && !alive(*pid))
        },
    )
    .await;
    let started = starts(log);
    assert!(matches!(started.len(), 2 | 3), "{started:?}");
    for (_, argv) in &started[1..] {
        assert!(
            argv.contains(&format!("--resume={SESSION_AFTER_CLEAR}")),
            "a restart resumes the latest session id: {argv}"
        );
    }
    assert_eq!(
        runtime_states(boot, &live.card_id).await,
        ["superseded", "superseded", "running"]
    );
    assert_eq!(terminal_exit(boot, &live.terminal_id).await, (None, false));

    boot.state
        .terminal_renderer
        .drop_entry(&live.terminal_id)
        .await;
}

/// The old child exits after the restart committed its replacement runtime on the same terminal
/// and before the restart stopped it: that exit ends the old runtime, never the replacement.
#[tokio::test]
async fn an_old_child_exiting_inside_the_restart_does_not_end_the_replacement() {
    let _guard = ENV_LOCK.lock().await;
    let live = live_card().await;
    let (boot, log) = (&live.boot, &live.log);
    let (old_pid, _) = starts(log)[0];
    let old_entry = boot
        .state
        .terminal_renderer
        .get(&live.terminal_id)
        .expect("the first child's renderer");
    let paused = PausePoint {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    install_pause_for_test(CLAUDE_RESTART_BEFORE_STOP, &live.card_id, paused.clone());

    let app = boot.app.clone();
    let card_id = live.card_id.clone();
    let restart = tokio::spawn(async move { post_restart(app, &card_id).await });
    tokio::time::timeout(BUDGET, paused.entered.notified())
        .await
        .expect("the restart reaches its stop");
    // SAFETY: a plain signal to the stand-in child this test started.
    assert_eq!(unsafe { libc::kill(old_pid, libc::SIGTERM) }, 0);
    assert!(
        old_entry.wait_exit_persisted_for_test(BUDGET).await,
        "the old child's reader handles its exit"
    );
    paused.release.notify_one();

    let (status, response) = restart.await.unwrap();
    assert_eq!(status, StatusCode::OK, "body={response:?}");
    eventually("the replacement starts", || starts(log).len() == 2).await;
    assert_eq!(
        runtime_states(boot, &live.card_id).await,
        ["superseded", "running"]
    );
    assert_eq!(terminal_exit(boot, &live.terminal_id).await, (None, false));

    boot.state
        .terminal_renderer
        .drop_entry(&live.terminal_id)
        .await;
}

/// A restart that fails before it stops the child leaves that child running, and its terminal
/// row says so: compensation records no signal exit for a child it never stopped.
#[tokio::test]
async fn a_restart_failing_before_its_stop_leaves_the_child_and_its_row_live() {
    let _guard = ENV_LOCK.lock().await;
    let live = live_card().await;
    let (boot, log) = (&live.boot, &live.log);
    let (pid, _) = starts(log)[0];
    // The restart rewrites the settings file before it stops anything; a directory there fails it.
    std::fs::remove_file(&live.settings_path).unwrap();
    std::fs::create_dir(&live.settings_path).unwrap();

    let (status, response) = post_restart(boot.app.clone(), &live.card_id).await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "body={response:?}"
    );
    assert!(
        alive(pid) && !got_sigterm(log, pid),
        "the child was never stopped"
    );
    assert_eq!(starts(log).len(), 1);
    // The card shows the running child: its runtime is back in its pre-restart state, and the
    // replacement that never started is failed.
    assert_eq!(
        runtime_states(boot, &live.card_id).await,
        ["running", "failed"]
    );
    assert_eq!(terminal_exit(boot, &live.terminal_id).await, (None, false));

    boot.state
        .terminal_renderer
        .drop_entry(&live.terminal_id)
        .await;
}

/// A WS reconnect while the restart stops the old child (it takes ~2 s to exit after SIGTERM)
/// sets up no renderer on that child: its exit cannot end the replacement, and the restart gets a
/// renderer and a child of its own.
#[tokio::test]
async fn a_reattach_while_the_restart_stops_the_old_child_does_not_take_its_place() {
    let _guard = ENV_LOCK.lock().await;
    let live = live_card_exiting_after(Duration::from_secs(2)).await;
    let (boot, log) = (&live.boot, &live.log);
    let (old_pid, _) = starts(log)[0];

    let app = boot.app.clone();
    let card_id = live.card_id.clone();
    let restart = tokio::spawn(async move { post_restart(app, &card_id).await });
    eventually("the old child gets SIGTERM", || got_sigterm(log, old_pid)).await;
    assert!(alive(old_pid), "the old child is still exiting");
    calm_server::ws::terminal::resolve_live_renderer_for_test(&boot.state, &live.terminal_id)
        .await
        .unwrap();

    let (status, response) = restart.await.unwrap();
    assert_eq!(status, StatusCode::OK, "body={response:?}");
    eventually("the replacement starts", || starts(log).len() == 2).await;
    assert_eq!(
        runtime_states(boot, &live.card_id).await,
        ["superseded", "running"]
    );
    assert_eq!(response["runtime"]["status"], "running", "{response}");
    assert_eq!(terminal_exit(boot, &live.terminal_id).await, (None, false));

    boot.state
        .terminal_renderer
        .drop_entry(&live.terminal_id)
        .await;
}
