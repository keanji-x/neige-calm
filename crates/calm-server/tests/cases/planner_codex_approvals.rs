//! #2348 A2 — the Codex approval adapter end to end. A Planner harness runs on the shared daemon
//! supervisor over the fake `codex app-server` (`tests/fixtures/osc-probe-child`), whose turns
//! pause on a scripted approval request; the answer goes through the production answer route and
//! back over the daemon connection. The turn settings are also checked against the in-process fake
//! daemon, which records what each `turn/start` said.

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use calm_server::codex_appserver::TurnApprovals;
use calm_server::config::Config;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{AskDelivery, AskQuestion, Event};
use calm_server::harness::HarnessState;
use calm_server::planner_permission_mode::PlannerPermissionMode;
use calm_server::shared_codex_appserver::{ReplacePrecondition, SharedCodexAppServer};
use clap::Parser;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::planner_hold_ask::{Rig, rig_on, wait_for};

/// The thread every turn of the fake daemon runs on.
const THREAD: &str = "fake-thread-0001";

struct EnvGuard(&'static str);

impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var(self.0);
        }
    }
}

async fn repo() -> Arc<SqlxRepo> {
    Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap())
}

/// The rig over a live shared daemon running the fake `codex app-server`.
async fn live(tmp: &TempDir, permission_mode: &str) -> Rig {
    let repo = repo().await;
    let fake_codex = env!("CARGO_BIN_EXE_osc-probe-child");
    let cfg = Config::parse_from([
        "calm-server",
        "--data-dir",
        tmp.path().to_str().unwrap(),
        "--codex-bin",
        fake_codex,
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
    let daemon = SharedCodexAppServer::new(&cfg, Arc::new(home), repo.clone());
    daemon.start_or_takeover().await.unwrap();
    rig_on(repo, daemon, THREAD, permission_mode).await
}

const OPTIONS: [&str; 3] = ["Allow", "Allow for this session", "Deny"];

fn command_question() -> AskQuestion {
    AskQuestion {
        title: "Run `cargo test --workspace`?\nIn /work\nReason: needs the network".into(),
        options: OPTIONS.iter().map(|option| (*option).into()).collect(),
    }
}

/// Poll `read` until it has an answer.
async fn poll<T, F, Fut>(what: &str, mut read: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(found) = read().await {
            return found;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The newest `hold` ask and its questions.
async fn newest_hold_ask(rig: &Rig) -> Option<(i64, Vec<AskQuestion>)> {
    match rig.asks().await.pop() {
        Some((
            id,
            _,
            Event::AskRequested {
                questions,
                delivery: AskDelivery::Hold,
                ..
            },
        )) => Some((id, questions)),
        _ => None,
    }
}

/// The newest `hold` ask, once the harness holds its request.
async fn held_ask(rig: &Rig) -> (i64, Vec<AskQuestion>) {
    poll("the held hold ask", || async {
        newest_hold_ask(rig)
            .await
            .filter(|(id, _)| rig.harness.held_requests().contains(*id))
    })
    .await
}

async fn running_turn(rig: &Rig) -> Option<String> {
    match rig.harness.state_for_test().await {
        HarnessState::TurnRunning { turn_id, .. } => Some(turn_id),
        _ => None,
    }
}

fn sidecar(rig: &Rig, extension: &str) -> std::path::PathBuf {
    rig.daemon.sock_path().with_extension(extension)
}

fn lines(path: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// An `ask` Planner's turn tells codex to ask the person; the command it pauses on is a hold ask
/// with the three options, and "Allow for this session" reaches codex as `acceptForSession`.
#[tokio::test]
async fn an_ask_turn_pauses_on_a_command_and_the_chosen_option_reaches_codex() {
    let tmp = TempDir::new().unwrap();
    let capture = tmp.path().join("requests.ndjson");
    unsafe {
        std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture);
    }
    let _env = EnvGuard("FAKE_CODEX_CAPTURE_REQUESTS");
    let rig = live(&tmp, "ask").await;
    rig.start_turn_with("fake-approval: command").await;
    let (ask_id, questions) = held_ask(&rig).await;
    assert_eq!(questions, vec![command_question()]);

    let starts: Vec<Value> = lines(&capture)
        .into_iter()
        .filter(|frame| frame["method"] == "turn/start")
        .collect();
    let [start] = starts.as_slice() else {
        panic!("one turn/start, got {starts:?}");
    };
    assert_eq!(
        start["params"]["approvalPolicy"],
        json!({"granular": {
            "sandbox_approval": true, "rules": true, "request_permissions": false,
            "mcp_elicitations": false, "skill_approval": false,
        }})
    );
    assert_eq!(start["params"]["approvalsReviewer"], "user");
    assert_eq!(
        start["params"]["sandboxPolicy"],
        json!({"type": "workspaceWrite", "networkAccess": true})
    );

    let (status, body) = rig.answer(ask_id, json!([{ "option": 1 }])).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let answers = sidecar(&rig, "approval-answers");
    wait_for("codex to get the answer", || async {
        !lines(&answers).is_empty()
    })
    .await;
    assert_eq!(
        lines(&answers),
        vec![
            json!({"jsonrpc": "2.0", "id": "approval-0001", "result": {"decision": "acceptForSession"}})
        ]
    );
    wait_for("the turn to end", || async {
        running_turn(&rig).await.is_none()
    })
    .await;
    assert!(rig.answered(ask_id).await);
    assert!(!rig.withdrawn(ask_id).await);
    rig.harness.shutdown().await.unwrap();
}

/// Codex settling the request itself (`serverRequest/resolved`) withdraws its ask while the turn
/// still runs; the request let go unanswered is declined, which codex ignores.
#[tokio::test]
async fn codex_settling_a_request_withdraws_its_ask() {
    let tmp = TempDir::new().unwrap();
    let rig = live(&tmp, "ask").await;
    let turn = rig.start_turn_with("fake-approval-resolved: command").await;
    let (ask_id, questions) = poll("the hold ask", || newest_hold_ask(&rig)).await;
    assert_eq!(questions, vec![command_question()]);
    let rig = &rig;
    wait_for("the ask to be withdrawn", move || rig.withdrawn(ask_id)).await;
    let answers = sidecar(rig, "approval-answers");
    wait_for("the decline", || async { !lines(&answers).is_empty() }).await;
    assert_eq!(
        lines(&answers),
        vec![json!({"jsonrpc": "2.0", "id": "approval-0001", "result": {"decision": "decline"}})]
    );
    assert_eq!(running_turn(rig).await, Some(turn));
    assert!(!rig.answered(ask_id).await);
    rig.harness.shutdown().await.unwrap();
}

/// Replacing the daemon while a request is held ends the connection it came on: the ask is
/// withdrawn, and the turn watchdog counts again, so 31 minutes of turn time now interrupt it.
#[tokio::test]
async fn replacing_the_daemon_while_a_request_is_held_withdraws_its_ask() {
    let tmp = TempDir::new().unwrap();
    let rig = live(&tmp, "ask").await;
    let turn = rig.start_turn_with("fake-approval: command").await;
    let (ask_id, _) = held_ask(&rig).await;
    wait_for("the watchdog to see the held request", || {
        rig.harness.watchdog_saw_held_request_for_test()
    })
    .await;

    rig.daemon
        .transition_replace_for_test(
            "replace while a request is held",
            ReplacePrecondition::Always,
        )
        .await
        .unwrap();
    let rig = &rig;
    wait_for("the ask to be withdrawn", move || rig.withdrawn(ask_id)).await;
    assert!(!rig.harness.held_requests().contains(ask_id));
    wait_for("the watchdog to count again", || async {
        !rig.harness.watchdog_saw_held_request_for_test().await
    })
    .await;
    assert_eq!(running_turn(rig).await, Some(turn));
    rig.harness
        .rewind_turn_clock_for_test(Duration::from_secs(31 * 60))
        .await;
    let methods = sidecar(rig, "methods");
    wait_for("the watchdog to interrupt the turn", || async {
        std::fs::read_to_string(&methods)
            .unwrap_or_default()
            .lines()
            .any(|method| method == "turn/interrupt")
    })
    .await;
    rig.harness.shutdown().await.unwrap();
}

/// Every Planner turn says the card's mode as it stands; an unreadable mode issues no turn.
#[tokio::test]
async fn a_planner_turn_says_its_cards_mode_and_an_unreadable_mode_issues_nothing() {
    for (stored, sent) in [
        ("ask", PlannerPermissionMode::Ask),
        ("never", PlannerPermissionMode::Never),
    ] {
        let repo = repo().await;
        let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
        let rig = rig_on(repo, daemon, THREAD, stored).await;
        rig.start_turn_with("Read the track goal.").await;
        assert_eq!(
            rig.daemon.started_turn_approvals_for_test(),
            vec![TurnApprovals::Explicit(sent)]
        );
        rig.harness.shutdown().await.unwrap();
    }

    let repo = repo().await;
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let rig = rig_on(repo, daemon, THREAD, "full").await;
    rig.harness
        .observe(calm_server::harness::Observation::TrackGoal {
            text: "Read the track goal.".into(),
        })
        .unwrap();
    wait_for("the turn to be refused", || async {
        rig.harness.refused_issuances_for_test() > 0
    })
    .await;
    assert_eq!(rig.daemon.turn_start_count_for_test(), 0);
    rig.harness.shutdown().await.unwrap();
}
