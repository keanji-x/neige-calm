//! #2516: Update of a Claude card whose child is running, with real processes under a real proc
//! supervisor. The child is a stand-in for the Claude CLI that logs each start and each SIGTERM.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_proc_supervisor::test_support::InProcessProcSupervisor;
use calm_server::db::prelude::*;
use calm_server::test_seams::{PausePoint, VIEWER_REATTACH_PROBED, install_pause_for_test};
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

/// Whether `pid` runs: it exists and is not a zombie its parent has yet to reap.
fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 delivers nothing; it only checks that the pid exists.
    let exists = unsafe { libc::kill(pid, 0) == 0 };
    let zombie = std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit_once(") ")
            .is_some_and(|(_, rest)| rest.starts_with('Z'))
    });
    exists && !zombie
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
    eventually("the first child starts", || starts(&log).len() == 1).await;
    LiveCard {
        boot,
        _supervisor: supervisor,
        _dir: dir,
        log,
        card_id,
        terminal_id,
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

async fn post_session_start(boot: &Boot, card_id: &str, session_id: &str) {
    let hook = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/internal/claude/hook?card_id={card_id}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "hook_event_name": "SessionStart",
                        "session_id": session_id,
                        "source": "clear",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = response_json(hook).await;
    assert_eq!(status, StatusCode::OK, "body={body:?}");
}

/// Update of a running card: the old child gets SIGTERM and its exit is recorded, then exactly one
/// new child resumes the session the card last started.
#[tokio::test]
async fn update_of_a_running_card_stops_its_child_then_resumes_the_latest_session() {
    let _guard = ENV_LOCK.lock().await;
    let live = live_card().await;
    let (boot, log) = (&live.boot, &live.log);
    let (old_pid, _) = starts(log)[0];
    post_session_start(boot, &live.card_id, SESSION_AFTER_CLEAR).await;

    let (status, response) = post_restart(boot.app.clone(), &live.card_id).await;
    assert_eq!(status, StatusCode::OK, "body={response:?}");
    assert_eq!(response["runtime"]["status"], "running", "{response}");
    eventually("the replacement starts", || starts(log).len() == 2).await;
    assert!(got_sigterm(log, old_pid) && !alive(old_pid));
    let (new_pid, argv) = starts(log)[1].clone();
    assert!(alive(new_pid));
    assert!(
        argv.contains(&format!("--resume={SESSION_AFTER_CLEAR}")),
        "{argv}"
    );
    assert_eq!(
        runtime_states(boot, &live.card_id).await,
        ["exited", "running"]
    );
    assert_eq!(terminal_exit(boot, &live.terminal_id).await, (None, false));

    boot.state
        .terminal_renderer
        .drop_entry(&live.terminal_id)
        .await;
}

/// Two Updates at once, after a server restart left the child with no renderer: they run one
/// after the other, so exactly one child runs at the end and every runtime but the last is over.
#[tokio::test]
async fn two_concurrent_updates_leave_exactly_one_live_child() {
    let _guard = ENV_LOCK.lock().await;
    let live = live_card().await;
    let (boot, log) = (&live.boot, &live.log);
    boot.state
        .terminal_renderer
        .forget_entry_for_test(&live.terminal_id);

    let (first, second) = tokio::join!(
        post_restart(boot.app.clone(), &live.card_id),
        post_restart(boot.app.clone(), &live.card_id),
    );
    assert_eq!(first.0, StatusCode::OK, "body={:?}", first.1);
    assert_eq!(second.0, StatusCode::OK, "body={:?}", second.1);

    eventually("both replacements start", || starts(log).len() == 3).await;
    let started = starts(log);
    let live_pids: Vec<i32> = started
        .iter()
        .map(|(pid, _)| *pid)
        .filter(|pid| alive(*pid))
        .collect();
    assert_eq!(
        live_pids,
        [started[2].0],
        "exactly one child runs: {started:?}"
    );
    for (pid, _) in &started[..2] {
        assert!(got_sigterm(log, *pid), "a replaced child gets SIGTERM");
    }
    let states = runtime_states(boot, &live.card_id).await;
    assert_eq!(states.len(), 3, "{states:?}");
    assert_eq!(states[2], "running", "{states:?}");
    assert!(
        states[..2]
            .iter()
            .all(|state| state == "exited" || state == "failed"),
        "{states:?}"
    );
    assert_eq!(terminal_exit(boot, &live.terminal_id).await, (None, false));

    boot.state
        .terminal_renderer
        .drop_entry(&live.terminal_id)
        .await;
}

/// A viewer's reattach that saw a live child, and reaches the supervisor only once that child is
/// gone (as an Update's stop leaves it), never starts `term.program`: the card's original
/// `--session-id=` command line runs once, at the card's creation.
///
/// The child is stopped out of band here: a reattach that found no renderer holds the track's
/// fence, which an Update's stop of a renderer-less child takes too, so the two meet only here.
#[tokio::test]
async fn a_viewer_reattach_never_starts_the_terminal_program() {
    let _guard = ENV_LOCK.lock().await;
    let live = live_card_exiting_after(Duration::from_secs(2)).await;
    let (boot, log) = (&live.boot, &live.log);
    let (old_pid, _) = starts(log)[0];
    boot.state
        .terminal_renderer
        .forget_entry_for_test(&live.terminal_id);
    let paused = PausePoint {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    install_pause_for_test(VIEWER_REATTACH_PROBED, &live.terminal_id, paused.clone());

    let state = boot.state.clone();
    let terminal_id = live.terminal_id.clone();
    let reattach = tokio::spawn(async move {
        calm_server::ws::terminal::resolve_live_renderer_for_test(&state, &terminal_id).await
    });
    tokio::time::timeout(BUDGET, paused.entered.notified())
        .await
        .expect("the reattach sees the live child");
    // SAFETY: a plain signal to the stand-in child this test started.
    assert_eq!(unsafe { libc::kill(old_pid, libc::SIGKILL) }, 0);
    eventually("the old child is gone", || !alive(old_pid)).await;
    paused.release.notify_one();

    let reattached = reattach.await.unwrap().unwrap();
    assert!(
        matches!(
            reattached,
            calm_server::ws::terminal::TestLiveRenderer::ChildExited { .. }
        ),
        "a reattach to a child that is gone gets no renderer"
    );
    let with_session_id = starts(log)
        .iter()
        .filter(|(_, argv)| argv.contains("--session-id="))
        .count();
    assert_eq!(with_session_id, 1, "{:?}", starts(log));
    assert_eq!(starts(log).len(), 1, "{:?}", starts(log));
}

/// An Update of a running Claude card that is not owner-created (a Planner task worker's: no
/// `owner_created` marker, and a `tasks` row names it) keeps the dead-child restart's refusal:
/// 409, and the child runs on untouched. Its Planner owns that worker.
#[tokio::test]
async fn update_of_a_running_task_worker_card_is_refused_and_leaves_its_child() {
    let _guard = ENV_LOCK.lock().await;
    let live = live_card().await;
    let (boot, log) = (&live.boot, &live.log);
    let (pid, _) = starts(log)[0];
    // A task worker's card never carries the creation-time marker; drop the one the route
    // stamped, below the API (which keeps it sticky), and make a task own the card.
    sqlx::query("UPDATE cards SET payload = json_remove(payload, '$.owner_created') WHERE id = ?1")
        .bind(&live.card_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tasks (id, track_id, key, kind, goal, context_json, depends_on_json, status, \
           worker_card_id, created_at_ms, updated_at_ms) \
         VALUES (?1, ?2, 'worker', 'claude', 'work', 'null', '[]', 'running', ?3, 1, 1)",
    )
    .bind(format!("{}:worker", boot.track_id))
    .bind(&boot.track_id)
    .bind(&live.card_id)
    .execute(boot.repo.pool())
    .await
    .unwrap();

    let (status, response) = post_restart(boot.app.clone(), &live.card_id).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={response:?}");
    assert!(
        alive(pid) && !got_sigterm(log, pid),
        "the worker's child runs on"
    );
    assert_eq!(starts(log).len(), 1);
    assert_eq!(runtime_states(boot, &live.card_id).await, ["running"]);
    assert_eq!(terminal_exit(boot, &live.terminal_id).await, (None, false));

    boot.state
        .terminal_renderer
        .drop_entry(&live.terminal_id)
        .await;
}

async fn patch_payload(
    boot: &Boot,
    card_id: &str,
    payload: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let resp = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/cards/{card_id}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "payload": payload }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    response_json(resp).await
}

/// The payload facts a restart depends on are the kernel's: a client can neither set a Claude
/// card's `settings_path` nor drop it.
#[tokio::test]
async fn a_client_can_neither_set_nor_drop_a_claude_cards_settings_path() {
    let _guard = ENV_LOCK.lock().await;
    let live = live_card().await;
    let boot = &live.boot;
    let minted = boot
        .repo
        .card_get(&live.card_id)
        .await
        .unwrap()
        .unwrap()
        .payload["settings_path"]
        .clone();

    let (status, refused) = patch_payload(
        boot,
        &live.card_id,
        json!({ "schemaVersion": 1, "settings_path": "/" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={refused:?}");
    let (status, patched) = patch_payload(
        boot,
        &live.card_id,
        json!({ "schemaVersion": 1, "icon_bg": "#fff" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={patched:?}");
    let stored = boot
        .repo
        .card_get(&live.card_id)
        .await
        .unwrap()
        .unwrap()
        .payload;
    assert_eq!(stored["settings_path"], minted, "{stored}");
    assert_eq!(stored["owner_created"], true, "{stored}");

    boot.state
        .terminal_renderer
        .drop_entry(&live.terminal_id)
        .await;
}

/// An Update that cannot restart the card stops nothing: a `settings_path` with no parent
/// directory (seeded below the API, which refuses it) is refused by the restart's prepare, which
/// the Update dry-runs before the running child is touched.
#[tokio::test]
async fn update_that_cannot_restart_the_card_leaves_its_child_running() {
    let _guard = ENV_LOCK.lock().await;
    let live = live_card().await;
    let (boot, log) = (&live.boot, &live.log);
    let (pid, _) = starts(log)[0];
    sqlx::query(
        "UPDATE cards SET payload = json_set(payload, '$.settings_path', '/') WHERE id = ?1",
    )
    .bind(&live.card_id)
    .execute(boot.repo.pool())
    .await
    .unwrap();

    let (status, response) = post_restart(boot.app.clone(), &live.card_id).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body={response:?}");
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("settings_path has no parent directory"),
        "body={response:?}"
    );
    assert!(alive(pid) && !got_sigterm(log, pid), "the child runs on");
    assert_eq!(starts(log).len(), 1);
    assert_eq!(runtime_states(boot, &live.card_id).await, ["running"]);
    assert_eq!(terminal_exit(boot, &live.terminal_id).await, (None, false));

    boot.state
        .terminal_renderer
        .drop_entry(&live.terminal_id)
        .await;
}

#[tokio::test]
async fn update_of_an_owner_card_whose_terminal_a_task_owns_leaves_its_child_running() {
    let _guard = ENV_LOCK.lock().await;
    let live = live_card().await;
    let (boot, log) = (&live.boot, &live.log);
    let (pid, _) = starts(log)[0];
    sqlx::query(
        "INSERT INTO tasks (id, track_id, key, kind, goal, context_json, depends_on_json, status, \
           worker_card_id, created_at_ms, updated_at_ms) \
         VALUES (?1, ?2, 'owned', 'claude', 'work', 'null', '[]', 'running', ?3, 1, 1)",
    )
    .bind(format!("{}:owned", boot.track_id))
    .bind(&boot.track_id)
    .bind(&live.card_id)
    .execute(boot.repo.pool())
    .await
    .unwrap();

    let (status, response) = post_restart(boot.app.clone(), &live.card_id).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={response:?}");
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("a task owns its terminal"),
        "body={response:?}"
    );
    assert!(alive(pid) && !got_sigterm(log, pid), "the child runs on");
    assert_eq!(starts(log).len(), 1);
    assert_eq!(runtime_states(boot, &live.card_id).await, ["running"]);

    boot.state
        .terminal_renderer
        .drop_entry(&live.terminal_id)
        .await;
}

/// An Update whose restart the actor may not make stops nothing: the restart's prepare refuses
/// `ai:codex` at the role gate, and its dry run says so before the child is touched.
#[tokio::test]
async fn update_by_an_actor_the_restart_refuses_leaves_its_child_running() {
    let _guard = ENV_LOCK.lock().await;
    let live = live_card().await;
    let (boot, log) = (&live.boot, &live.log);
    let (pid, _) = starts(log)[0];

    let resp = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/cards/{}/claude/restart", live.card_id))
                .header("X-Calm-Actor", "ai:codex")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, response) = response_json(resp).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body={response:?}");
    assert!(alive(pid) && !got_sigterm(log, pid), "the child runs on");
    assert_eq!(starts(log).len(), 1);
    assert_eq!(runtime_states(boot, &live.card_id).await, ["running"]);
    assert_eq!(terminal_exit(boot, &live.terminal_id).await, (None, false));

    boot.state
        .terminal_renderer
        .drop_entry(&live.terminal_id)
        .await;
}
