pub(in crate::harness) mod canonical;
use super::*;
use crate::claude_planner::config::{ClaudePlannerConfig, ClaudePlannerHost};
use crate::claude_planner::session::ClaudePlannerSessionParams;
use crate::thread_seals::ThreadSeals;

async fn fixture(
    scenario: &str,
) -> (
    PlannerBackend,
    Arc<ClaudePlannerSession>,
    tempfile::TempDir,
    String,
) {
    let (backend, session, dir, owner, _held) = fixture_full(scenario).await;
    (backend, session, dir, owner.thread)
}

async fn fixture_full(
    scenario: &str,
) -> (
    PlannerBackend,
    Arc<ClaudePlannerSession>,
    tempfile::TempDir,
    super::dispatch_fixture::Owner,
    crate::harness::held_requests::HeldRequestReceiver,
) {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("claude");
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/claude_planner_fake/claude.sh"
        ),
        &bin,
    )
    .unwrap();
    std::fs::write(dir.path().join("scenario"), scenario).unwrap();
    let owner = super::dispatch_fixture::seed(dir.path(), AgentProvider::Claude).await;
    let host = Arc::new(
        ClaudePlannerHost::new(
            Some(ClaudePlannerConfig {
                claude_binary: bin,
                claude_version: "2.1.280".into(),
                config_dir: dir.path().join("config"),
            }),
            &dir.path().join("data"),
            dir.path().join("unused-shim"),
            dir.path().join("mcp.sock"),
        )
        .unwrap(),
    );
    let session = Arc::new(
        ClaudePlannerSession::open(ClaudePlannerSessionParams {
            host,
            worker_session_id: owner.worker.clone(),
            card_id: owner.card.clone(),
            track_id: owner.track.clone(),
            cwd: dir.path().into(),
            instructions: "fixture".into(),
            calm_tools: provider::claude::translate::ToolNames::new("neige", Vec::<String>::new()),
            proxy: vec![],
            prior_total_tokens: 0,
            repo: owner.repo.clone(),
            seals: Arc::new(ThreadSeals::new()),
        })
        .await
        .unwrap(),
    );
    let (held, rx) = tokio::sync::mpsc::unbounded_channel();
    session.mark_installed(held);
    (
        PlannerBackend::claude_for_test(session.clone()),
        session,
        dir,
        owner,
        rx,
    )
}

#[tokio::test]
async fn canonical_claude_local_write_is_unknown_and_running_without_native_id() {
    let (backend, session, _dir, thread) = fixture("exit").await;
    let proof = super::dispatch_tests::proof();
    let report = backend
        .dispatch_input(
            &super::dispatch_tests::logical_turn_id(),
            proof.attempt(),
            &thread,
            &proof,
            vec![InputItem::Text {
                text: "input".into(),
            }],
            &TurnModelSelection::inherit(),
            PlannerPermissionMode::Never,
            &crate::harness::native_admission::NativePromptAdmission::new(
                || Ok(()),
                || Box::pin(async { Ok(()) }),
            ),
        )
        .await;
    assert!(matches!(report.delivery(), InputDelivery::Unknown(_)));
    assert!(report.outcome().is_ok());
    let correlation = report.delivery().correlation();
    assert!(correlation.native_turn_id().is_none());
    assert!(correlation.native_session_id().is_none());
    assert!(uuid::Uuid::parse_str(correlation.local_turn_id().unwrap()).is_ok());
    assert_eq!(session.user_line_writes_for_test(), 1);
    session.shutdown().await.unwrap();
}
