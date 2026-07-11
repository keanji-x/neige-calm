#![cfg(unix)]

use crate::support;

use std::path::PathBuf;
use std::process::{Command, Output};

use calm_server::event::Event;
use calm_server::ids::ActorId;
use calm_server::model::CardRole;
use serde_json::json;
use support::mcp::{CardBoot, boot_with_role, wait_for_kind};

fn neige_bin() -> PathBuf {
    // Never set in practice: cargo only injects CARGO_BIN_EXE_* for bins of
    // the package under test (calm-server), and `neige` lives in the separate
    // neige-cli package. Kept in case the bin ever moves in-package.
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_neige") {
        return PathBuf::from(path);
    }

    // cargo puts workspace bins next to the integration-test binary
    // (target/{debug,release}/neige), so a workspace test build has already
    // produced it — short-circuit before shelling out to cargo build.
    //
    // CI-only: exists() proves neither provenance nor freshness. In GHA
    // (CI=true is always set) the workspace-level test build runs first in
    // the same invocation, so the sibling bin is guaranteed current. Locally
    // a developer may edit neige-cli and then run only this test file — that
    // build doesn't touch neige-cli (not a calm-server dependency), and the
    // short-circuit would silently pick up a stale bin. So unless CI=true we
    // keep the original semantics: always rebuild via the cargo fallback.
    if std::env::var("CI").is_ok_and(|v| v == "true") {
        let mut candidate = std::env::current_exe().expect("current_exe");
        candidate.pop(); // .../deps/
        candidate.pop(); // .../debug/ or .../release/
        candidate.push(format!("neige{}", std::env::consts::EXE_SUFFIX));
        if candidate.exists() {
            return candidate;
        }
    }

    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .args(["build", "-p", "neige-cli", "--bin", "neige", "--locked"])
        .status()
        .expect("build neige binary");
    assert!(
        status.success(),
        "cargo build -p neige-cli --bin neige --locked failed"
    );

    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            root.pop();
            root.pop();
            root.join("target")
        });
    target_dir
        .join("debug")
        .join(format!("neige{}", std::env::consts::EXE_SUFFIX))
}

async fn run_neige(boot: &CardBoot, args: &[&str]) -> Output {
    let bin = neige_bin();
    let socket_path = boot.socket_path.clone();
    let token = boot.raw_token.clone();
    let args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
    tokio::task::spawn_blocking(move || {
        Command::new(bin)
            .env("NEIGE_MCP_SOCKET", socket_path)
            .env("NEIGE_MCP_TOKEN", token)
            .env_remove("NEIGE_MCP_DAEMON_TOKEN")
            .args(args)
            .output()
    })
    .await
    .expect("join neige process")
    .expect("run neige")
}

#[tokio::test]
async fn neige_task_completed_emits_task_completed_event() {
    let boot = boot_with_role(CardRole::Worker).await;
    let mut rx = boot.events.subscribe_filtered();

    let out = run_neige(
        &boot,
        &[
            "task-completed",
            "--idempotency-key",
            "cli-completed-1",
            "--result",
            r#"{"ok":true}"#,
        ],
    )
    .await;
    assert!(
        out.status.success(),
        "stderr = {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let env = wait_for_kind(&mut rx, "task.completed").await;
    match &env.actor {
        ActorId::AiCodexSession(session_id) => {
            assert_eq!(session_id.as_str(), boot.session_id.as_str())
        }
        other => panic!("expected AiCodexSession actor; got {other:?}"),
    }
    match &env.event {
        Event::TaskCompleted {
            idempotency_key,
            result,
            artifacts,
            ..
        } => {
            assert_eq!(idempotency_key, "cli-completed-1");
            assert_eq!(result, &json!({ "ok": true }));
            assert!(artifacts.is_empty());
        }
        other => panic!("expected TaskCompleted; got {other:?}"),
    }
    let _ = &boot.server;
}

#[tokio::test]
async fn neige_task_failed_emits_task_failed_event() {
    let boot = boot_with_role(CardRole::Worker).await;
    let mut rx = boot.events.subscribe_filtered();

    let out = run_neige(
        &boot,
        &[
            "task-failed",
            "--idempotency-key",
            "cli-failed-1",
            "--reason",
            "stub failure",
        ],
    )
    .await;
    assert!(
        out.status.success(),
        "stderr = {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let env = wait_for_kind(&mut rx, "task.failed").await;
    match &env.actor {
        ActorId::AiCodexSession(session_id) => {
            assert_eq!(session_id.as_str(), boot.session_id.as_str())
        }
        other => panic!("expected AiCodexSession actor; got {other:?}"),
    }
    match &env.event {
        Event::TaskFailed {
            idempotency_key,
            reason,
            ..
        } => {
            assert_eq!(idempotency_key, "cli-failed-1");
            assert_eq!(reason, "stub failure");
        }
        other => panic!("expected TaskFailed; got {other:?}"),
    }
    let _ = &boot.server;
}
