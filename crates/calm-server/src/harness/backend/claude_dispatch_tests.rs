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
    crate::test_support::execution::Directory,
    String,
) {
    let (backend, session, dir, owner, _held) = fixture_full(scenario).await;
    (backend, session, dir, owner.thread)
}

enum FixtureStorage {
    Memory,
    Persistent,
}

async fn fixture_full(
    scenario: &str,
) -> (
    PlannerBackend,
    Arc<ClaudePlannerSession>,
    crate::test_support::execution::Directory,
    super::dispatch_fixture::Owner,
    crate::harness::held_requests::HeldRequestReceiver,
) {
    fixture_full_with_storage(scenario, FixtureStorage::Memory, true).await
}

async fn fixture_full_with_storage(
    scenario: &str,
    storage: FixtureStorage,
    boot: bool,
) -> (
    PlannerBackend,
    Arc<ClaudePlannerSession>,
    crate::test_support::execution::Directory,
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
    let db_url: String = match storage {
        FixtureStorage::Memory => "sqlite::memory:".into(),
        FixtureStorage::Persistent => format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("canonical.sqlite").display()
        ),
    };
    let owner =
        super::dispatch_fixture::seed_with_url(dir.path(), AgentProvider::Claude, &db_url).await;
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
    if boot {
        crate::claude_planner::lifecycle::boot(owner.repo.as_ref(), &host)
            .await
            .unwrap();
    }
    let execution =
        crate::test_support::execution::Fixture::new(owner.repo.sqlite_db().unwrap()).await;
    host.execution.bind(execution.engine.clone());
    let session = Arc::new(
        ClaudePlannerSession::open(ClaudePlannerSessionParams {
            engine: host.execution.engine().cloned(),
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
        crate::test_support::execution::Directory {
            _execution: execution,
            directory: dir,
        },
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
                || Box::pin(async { Ok(()) }),
            ),
            &backend.handoff_for_test().await,
        )
        .await;
    assert!(
        matches!(report.delivery(), InputDelivery::Unknown(_)),
        "delivery: {:?}; outcome: {:?}",
        report.delivery(),
        report.outcome()
    );
    assert!(report.outcome().is_ok(), "outcome: {:?}", report.outcome());
    let correlation = report.delivery().correlation();
    assert!(correlation.native_turn_id().is_none());
    assert!(correlation.native_session_id().is_none());
    assert!(uuid::Uuid::parse_str(correlation.local_turn_id().unwrap()).is_ok());
    assert_eq!(session.user_line_writes_for_test(), 1);
    session.shutdown().await.unwrap();
}
