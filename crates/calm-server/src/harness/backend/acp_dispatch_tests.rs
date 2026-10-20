#![cfg(test)]
use super::*;
use crate::acp_planner::config::{AcpAgentConfig, AcpPlannerConfig, AcpPlannerHost};
use crate::acp_planner::session::{AcpPlannerSession, SessionParams};
use crate::thread_seals::ThreadSeals;

async fn fixture() -> (
    PlannerBackend,
    Arc<AcpPlannerSession>,
    super::dispatch_fixture::Owner,
    tempfile::TempDir,
) {
    let (backend, session, owner, dir, _held) = fixture_full().await;
    (backend, session, owner, dir)
}

async fn fixture_full() -> (
    PlannerBackend,
    Arc<AcpPlannerSession>,
    super::dispatch_fixture::Owner,
    tempfile::TempDir,
    crate::harness::held_requests::HeldRequestReceiver,
) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("scenario"), "reply").unwrap();
    let owner = super::dispatch_fixture::seed(dir.path(), AgentProvider::OpenCode).await;
    let host = Arc::new(
        AcpPlannerHost::new(
            Some(AcpPlannerConfig {
                agents: vec![AcpAgentConfig {
                    provider: AgentProvider::OpenCode,
                    command: "/usr/bin/python3".into(),
                    args: vec![
                        "-u".into(),
                        concat!(
                            env!("CARGO_MANIFEST_DIR"),
                            "/tests/fixtures/acp_planner/agent.py"
                        )
                        .into(),
                    ],
                    env: [(
                        "ACP_FIXTURE_ROOT".into(),
                        dir.path().to_string_lossy().into_owned(),
                    )]
                    .into(),
                    default_permission_mode: None,
                    min_agent_version: semver::Version::new(1, 0, 0),
                }],
            }),
            &dir.path().join("data"),
            dir.path().join("unused-shim"),
            dir.path().join("mcp.sock"),
        )
        .unwrap(),
    );
    let session = Arc::new(AcpPlannerSession::open(SessionParams {
        host,
        provider: AgentProvider::OpenCode,
        repo: owner.repo.clone(),
        seals: Arc::new(ThreadSeals::new()),
        worker_session_id: owner.worker.clone(),
        card_id: owner.card.clone(),
        track_id: owner.track.clone(),
        cwd: dir.path().into(),
        instructions: "fixture".into(),
    }));
    let (held, rx) = tokio::sync::mpsc::unbounded_channel();
    session.mark_installed(held);
    (
        PlannerBackend(Arm::Acp(session.clone())),
        session,
        owner,
        dir,
        rx,
    )
}

#[tokio::test]
async fn canonical_input_is_unsupported_before_admission_or_native_effects() {
    let (backend, session, owner, _dir) = fixture().await;
    let thread = owner.thread;
    let proof = super::dispatch_tests::proof();
    let report = backend
        .dispatch_input(
            None,
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
                || Box::pin(async { panic!("unsupported backend must not admit native effects") }),
            ),
        )
        .await;
    assert!(matches!(report.delivery(), InputDelivery::NotDispatched(_)));
    assert!(
        report
            .outcome()
            .as_ref()
            .unwrap_err()
            .to_string()
            .contains("unsupported")
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM acp_submissions")
        .fetch_one(owner.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    session.shutdown().await.unwrap();
}
