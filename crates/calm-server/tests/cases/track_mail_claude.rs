//! #2130 S1 acceptance on a Claude Planner: the §2 trace where the Weekly review Track's Planner
//! runs on the Claude backend (the fake `claude` of `tests/fixtures/claude_planner_fake`), mailing
//! a Codex Planner, woken by its reply and reading it. The Claude Planner's tool calls drive the
//! mail module below the transport: its MCP credential is the one its own first turn mints.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use calm_server::claude_planner::config::{ClaudePlannerConfig, ClaudePlannerHost};
use calm_server::claude_planner::session::{ClaudePlannerSession, ClaudePlannerSessionParams};
use calm_server::claude_planner::stop::sigkill_verified_for_test;
use calm_server::claude_planner::translate::ToolNames;
use calm_server::harness::backend::PlannerBackend;
use calm_server::harness::{
    HarnessConfig, HarnessState, Observation, PlannerHarness, PlannerHarnessParams,
};
use calm_server::mail::{Recipient, SendRequest};
use calm_server::session_projection_repo::AgentProvider;
use serde_json::json;

use super::claude_planner_session_fixture::marked;
use super::track_mail_fixture::{World, idle_snapshot};

const FAKE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/claude_planner_fake/claude.sh"
);

/// Every process the host marked for this session, killed on drop.
struct Marked(String);

impl Drop for Marked {
    fn drop(&mut self) {
        for (pid, start_time) in marked(&HashSet::from([self.0.clone()])) {
            sigkill_verified_for_test(pid, start_time);
        }
    }
}

/// The fake's record of every turn's input line, oldest first.
fn spawned_inputs(bin: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(bin.join("stdin"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

async fn wait_completed_turn(harness: &PlannerHarness, bin: &std::path::Path, n: usize) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let inputs = spawned_inputs(bin);
        let done = matches!(
            harness.state_for_test().await,
            HarnessState::TurnCompleted { .. }
        );
        if inputs.len() >= n && done {
            return inputs[n - 1].clone();
        }
        assert!(
            Instant::now() < deadline,
            "Claude turn {n} never completed: {inputs:?}; state={:?}; refused={}",
            harness.state_for_test().await,
            harness.refused_issuances_for_test()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mail_trace_reaches_a_claude_planner() {
    let w = World::new(&["NVDA research"]).await;
    let n = w.p(0);
    let r = w
        .add_planner_without_harness(&w.area_id, "Weekly review")
        .await;
    let bin = w.dir.path().join("claude-bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::copy(FAKE, bin.join("claude")).unwrap();
    std::fs::write(bin.join("scenario"), "exit").unwrap();
    std::fs::create_dir_all(w.dir.path().join("claude-data")).unwrap();
    let host = Arc::new(
        ClaudePlannerHost::new(
            Some(ClaudePlannerConfig {
                claude_binary: bin.join("claude"),
                claude_version: "2.1.280".into(),
                config_dir: w.dir.path().join("claude-config"),
            }),
            &w.dir.path().join("claude-data"),
            std::path::PathBuf::from("/nonexistent/neige-mcp-stdio-shim"),
            w.socket.clone(),
        )
        .unwrap(),
    );
    let _marked = Marked(host.instance.marker(&r.session_id));
    let cwd = w.dir.path().join("claude-ws");
    std::fs::create_dir_all(&cwd).unwrap();
    let session = ClaudePlannerSession::open(ClaudePlannerSessionParams {
        host,
        worker_session_id: r.session_id.clone(),
        card_id: r.card_id.to_string(),
        track_id: r.track_id.to_string(),
        cwd,
        instructions: "Planner instructions for the fake.".into(),
        calm_tools: ToolNames::new(
            calm_server::mcp_server::wiring::MCP_SERVER_KEY,
            ["neige.mail.send".to_string()],
        ),
        proxy: Vec::new(),
        prior_total_tokens: 0,
        repo: w.repo_dyn.clone(),
        seals: Arc::clone(w.daemon.thread_seals()),
    })
    .await
    .unwrap();
    session.mark_installed();
    let harness = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: r.session_id.clone(),
        track_id: r.track_id.clone(),
        card_id: r.card_id.clone(),
        thread_id: Some(r.thread_id.clone()),
        repo: w.repo_dyn.clone(),
        events: w.events.clone(),
        card_role_cache: w.role_cache.clone(),
        track_area_cache: w.area_cache.clone(),
        backend: PlannerBackend::claude_for_test(Arc::new(session)),
        live_replies: calm_server::harness::LiveReplies::for_test(),
        config: HarnessConfig::default(),
        snapshot: idle_snapshot(&r.thread_id),
    });
    w.registry.insert(r.session_id.clone(), harness.clone());
    let ctx = w.app_context();
    let mut identity = r.identity(&w.area_id);
    identity.provider = AgentProvider::Claude;

    // 1–4: the user's turn on the Claude Planner sends m1 at hop 1.
    harness
        .observe_for_test(
            Observation::UserMessage {
                text: "review this week".into(),
            },
            None,
        )
        .await;
    wait_completed_turn(&harness, &bin, 1).await;
    let request = SendRequest {
        to: Recipient::Track(n.track_id.to_string()),
        summary: "NVDA guidance below thesis".into(),
        text: "body".into(),
    };
    let m1 = calm_server::mail::send(&ctx, &identity, request)
        .await
        .unwrap();
    assert_eq!(m1.hop, 1);

    // 5–9: the Codex Planner is woken, reads m1 and replies from a task-completion turn.
    w.wait_turn(n, 1).await;
    assert_eq!(w.cat_json(n, &m1.mail_id).await["next_hop"], json!("2/6"));
    w.complete(n).await;
    w.task_turn(n).await;
    let (m2, hop) = w.reply_ok(n, &m1.mail_id).await;
    assert_eq!(hop, "2/6");

    // 10: the reply wakes the Claude Planner with the one line; reading it makes the next hop 3.
    let woken = wait_completed_turn(&harness, &bin, 2).await;
    assert!(woken.contains(&format!("neige mail cat {m2}")), "{woken}");
    let read = calm_server::mail::cat(&ctx, &identity, &m2).await.unwrap();
    assert_eq!(
        (&read["state"], &read["next_hop"]),
        (&json!("read"), &json!("3/6"))
    );
    let request = SendRequest {
        to: Recipient::Track(n.track_id.to_string()),
        summary: "updated the review".into(),
        text: "see area/reports/review.md".into(),
    };
    assert_eq!(
        calm_server::mail::send(&ctx, &identity, request)
            .await
            .unwrap()
            .hop,
        3
    );
    // 11: the Codex side lists both of its mails as read by their recipients.
    let listed = w.call(n, "neige.mail.ls", json!({})).await;
    let mails = super::track_mail_fixture::structured(&listed)["mails"].clone();
    assert_eq!(mails[1]["state"], json!("read"), "{mails}");
    assert_eq!(mails[2]["state"], json!("read"), "{mails}");
    harness.shutdown().await.unwrap();
}
