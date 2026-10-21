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
    fixture_storage(false).await
}

async fn fixture_storage(
    persistent: bool,
) -> (
    PlannerBackend,
    Arc<AcpPlannerSession>,
    super::dispatch_fixture::Owner,
    tempfile::TempDir,
    crate::harness::held_requests::HeldRequestReceiver,
) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("scenario"), "reply").unwrap();
    let url = if persistent {
        format!("sqlite:{}", dir.path().join("receipt.db").display())
    } else {
        "sqlite::memory:".into()
    };
    let owner =
        super::dispatch_fixture::seed_with_url(dir.path(), AgentProvider::OpenCode, &url).await;
    let (backend, session, rx) = session_for(&owner, &dir);
    (backend, session, owner, dir, rx)
}

fn session_for(
    owner: &super::dispatch_fixture::Owner,
    dir: &tempfile::TempDir,
) -> (
    PlannerBackend,
    Arc<AcpPlannerSession>,
    crate::harness::held_requests::HeldRequestReceiver,
) {
    session_for_command(owner, dir, "/usr/bin/python3")
}

fn session_for_command(
    owner: &super::dispatch_fixture::Owner,
    dir: &tempfile::TempDir,
    command: &str,
) -> (
    PlannerBackend,
    Arc<AcpPlannerSession>,
    crate::harness::held_requests::HeldRequestReceiver,
) {
    let host = Arc::new(
        AcpPlannerHost::new(
            Some(AcpPlannerConfig {
                agents: vec![AcpAgentConfig {
                    provider: AgentProvider::OpenCode,
                    command: command.into(),
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
        engine: host.execution.engine().cloned(),
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
    (PlannerBackend(Arm::Acp(session.clone())), session, rx)
}

mod canonical;

mod cleanup;
