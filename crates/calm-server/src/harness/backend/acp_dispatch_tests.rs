#![cfg(test)]
use super::*;
use crate::acp_planner::config::{AcpAgentConfig, AcpPlannerConfig, AcpPlannerHost};
use crate::acp_planner::session::{AcpPlannerSession, SessionParams};
use crate::thread_seals::ThreadSeals;
use std::collections::BTreeMap;

async fn fixture() -> (
    PlannerBackend,
    Arc<AcpPlannerSession>,
    super::dispatch_fixture::Owner,
    crate::test_support::execution::Directory,
) {
    let (backend, session, owner, dir, _held) = fixture_full().await;
    (backend, session, owner, dir)
}

async fn fixture_full() -> (
    PlannerBackend,
    Arc<AcpPlannerSession>,
    super::dispatch_fixture::Owner,
    crate::test_support::execution::Directory,
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
    crate::test_support::execution::Directory,
    crate::harness::held_requests::HeldRequestReceiver,
) {
    fixture_storage_for(AgentProvider::OpenCode, persistent).await
}

async fn fixture_storage_for(
    provider: AgentProvider,
    persistent: bool,
) -> (
    PlannerBackend,
    Arc<AcpPlannerSession>,
    super::dispatch_fixture::Owner,
    crate::test_support::execution::Directory,
    crate::harness::held_requests::HeldRequestReceiver,
) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("scenario"), "reply").unwrap();
    let url = if persistent {
        format!("sqlite:{}", dir.path().join("receipt.db").display())
    } else {
        "sqlite::memory:".into()
    };
    let owner = super::dispatch_fixture::seed_with_url(dir.path(), provider.clone(), &url).await;
    let execution =
        crate::test_support::execution::Fixture::new(owner.repo.sqlite_db().unwrap()).await;
    let dir = crate::test_support::execution::Directory {
        _execution: execution,
        directory: dir,
    };
    let (backend, session, rx) = session_for_provider(&owner, &dir, provider);
    (backend, session, owner, dir, rx)
}

fn session_for(
    owner: &super::dispatch_fixture::Owner,
    dir: &crate::test_support::execution::Directory,
) -> (
    PlannerBackend,
    Arc<AcpPlannerSession>,
    crate::harness::held_requests::HeldRequestReceiver,
) {
    session_for_provider(owner, dir, AgentProvider::OpenCode)
}

fn session_for_provider(
    owner: &super::dispatch_fixture::Owner,
    dir: &crate::test_support::execution::Directory,
    provider: AgentProvider,
) -> (
    PlannerBackend,
    Arc<AcpPlannerSession>,
    crate::harness::held_requests::HeldRequestReceiver,
) {
    session_for_command(owner, dir, provider, "/usr/bin/python3")
}

/// The spy peer's environment for `provider`: Grok's answers `initialize` as Grok Build 1.0.62
/// does (no `agentInfo`, `_meta.agentVersion`, `cached_token`, its `_x.ai/*` traffic).
fn peer_env(
    provider: &AgentProvider,
    dir: &crate::test_support::execution::Directory,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::from([(
        "ACP_FIXTURE_ROOT".to_owned(),
        dir.path().to_string_lossy().into_owned(),
    )]);
    match provider {
        AgentProvider::OpenCode => {}
        AgentProvider::Grok => env.extend(
            [
                ("ACP_FIXTURE_IDENTITY", "meta"),
                ("ACP_FIXTURE_VENDOR", "xai"),
                ("ACP_FIXTURE_AUTH_METHODS", "cached_token"),
                ("ACP_FIXTURE_VERSION", "1.0.62"),
            ]
            .map(|(key, value)| (key.to_owned(), value.to_owned())),
        ),
        AgentProvider::Codex | AgentProvider::Claude => panic!("{provider:?} does not speak ACP"),
    }
    env
}

fn session_for_command(
    owner: &super::dispatch_fixture::Owner,
    dir: &crate::test_support::execution::Directory,
    provider: AgentProvider,
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
                    provider: provider.clone(),
                    command: command.into(),
                    args: vec![
                        "-u".into(),
                        concat!(
                            env!("CARGO_MANIFEST_DIR"),
                            "/tests/fixtures/acp_planner/agent.py"
                        )
                        .into(),
                    ],
                    env: peer_env(&provider, dir),
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
    host.execution.bind(dir._execution.engine.clone());
    let session = Arc::new(AcpPlannerSession::open(SessionParams {
        engine: host.execution.engine().cloned(),
        host,
        provider,
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
