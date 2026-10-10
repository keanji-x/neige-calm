//! #2516: restarting a Claude card whose child is running, with real processes under a real proc
//! supervisor. The child is a stand-in for the Claude CLI that logs each start and each SIGTERM.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_proc_supervisor::test_support::InProcessProcSupervisor;
use serde_json::json;
use tower::ServiceExt;

use super::{ENV_LOCK, RealClaude, body, boot_with_real_claude, post, post_restart, response_json};

const BUDGET: Duration = Duration::from_secs(20);

fn write_fake_claude(dir: &Path, log: &Path) -> String {
    let bin = dir.join("fake-claude");
    let log = log.display();
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\n\
             echo \"start $$ $*\" >> '{log}'\n\
             trap \"echo term $$ >> '{log}'; exit 0\" TERM\n\
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

#[tokio::test]
async fn restart_of_a_running_card_stops_its_child_and_resumes_the_latest_session() {
    let _guard = ENV_LOCK.lock().await;
    let supervisor = InProcessProcSupervisor::start().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("claude.log");
    let boot = boot_with_real_claude(RealClaude {
        sock: supervisor.sock().to_path_buf(),
        claude_bin: write_fake_claude(dir.path(), &log),
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

    // The running child `/clear`s: its session id changes.
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
                        "session_id": "session-after-clear",
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

    // A double submit while the child runs: both succeed, one after the other.
    let (first, second) = tokio::join!(
        post_restart(boot.app.clone(), &card_id),
        post_restart(boot.app.clone(), &card_id),
    );
    assert_eq!(first.0, StatusCode::OK, "body={:?}", first.1);
    assert_eq!(second.0, StatusCode::OK, "body={:?}", second.1);

    eventually("both replacements start", || starts(&log).len() == 3).await;
    let started = starts(&log);
    for (pid, _) in &started[..2] {
        eventually("a replaced child gets SIGTERM and exits", || {
            got_sigterm(&log, *pid) && !alive(*pid)
        })
        .await;
    }
    let (last, _) = started[2];
    assert!(alive(last), "the last replacement runs");
    assert!(!got_sigterm(&log, last));
    let live: Vec<i32> = started
        .iter()
        .map(|(pid, _)| *pid)
        .filter(|pid| alive(*pid))
        .collect();
    assert_eq!(live, [last], "exactly one child runs: {started:?}");
    for (_, argv) in &started[1..] {
        assert!(
            argv.contains("--resume session-after-clear"),
            "a restart resumes the latest session id: {argv}"
        );
    }

    boot.state.terminal_renderer.drop_entry(&terminal_id).await;
}
