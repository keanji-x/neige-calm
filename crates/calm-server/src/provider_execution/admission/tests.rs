use super::*;

pub(super) struct Owner;
#[async_trait]
impl ProviderExecutionOwner for Owner {
    fn consumer(&self) -> &'static str {
        "claude-planner"
    }
    fn alias_prefix(&self) -> AliasPrefix {
        AliasPrefix::planner()
    }
    fn alias(&self, session: &str, id: &str) -> String {
        self.alias_prefix().alias(session, id)
    }
    async fn complete(
        &self,
        _: &str,
        _: &Record,
        _: CompletionEvidence,
        _: Option<KnownEnd>,
    ) -> Result<()> {
        Ok(())
    }
}
async fn refused(empty: bool) {
    let dir = tempfile::tempdir().unwrap();
    let binding = crate::harness::backend::dispatch_fixture::seed(
        dir.path(),
        crate::session_projection_repo::AgentProvider::Claude,
    )
    .await;
    let repo = &binding.repo;
    let fixture = crate::test_support::execution::Fixture::new(repo.db().clone()).await;
    let owner = ProviderExecution {
        owner: Arc::new(Owner),
        drivers: Default::default(),
    };
    let mut intent = workspace_runtime::execution::test_support::intent(dir.path().into());
    intent.metadata.consumer = Owner.consumer().into();
    intent.metadata.track.0 = binding.track.clone();
    intent.metadata.alias.0 = Owner.alias(&binding.worker, intent.metadata.id.as_str());
    let fence = crate::harness::run_loop::issuance::track_fence_for_test(if empty {
        &intent.metadata.track.0
    } else {
        "other-track"
    })
    .await;
    let handoff = TrackFenceHandoff::new(fence);
    if empty {
        handoff.release();
    }
    let result = owner
        .admit(
            repo.db(),
            &fixture.engine,
            &binding.worker,
            PreparedIntent(intent),
            Purpose::RewindCheck,
            OccupancyFence::ReleaseOnCommit(&handoff),
        )
        .await;
    assert!(matches!(result, Err(CalmError::Conflict(_))));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM workspace_executions")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    // A valid control must reach admission, so another Conflict cannot make
    // these negative assertions pass without the fence validation.
    let mut control = workspace_runtime::execution::test_support::intent(dir.path().into());
    control.metadata.consumer = Owner.consumer().into();
    control.metadata.track.0 = binding.track.clone();
    control.metadata.alias.0 = Owner.alias(&binding.worker, control.metadata.id.as_str());
    let control_fence = TrackFenceHandoff::new(
        crate::harness::run_loop::issuance::track_fence_for_test(&binding.track).await,
    );
    let (claim, driver) = owner
        .admit(
            repo.db(),
            &fixture.engine,
            &binding.worker,
            PreparedIntent(control),
            Purpose::RewindCheck,
            OccupancyFence::ReleaseOnCommit(&control_fence),
        )
        .await
        .expect("valid fence admits the same current owner");
    let id = execution_id(claim.record()).unwrap();
    owner
        .stop(
            repo.db(),
            Some(&fixture.engine),
            &binding.worker,
            &claim,
            StopReason::Finished,
        )
        .await
        .unwrap();
    wait_settled(&fixture.engine, &id, Duration::from_secs(10))
        .await
        .unwrap();
    drop(driver);
    owner
        .finish(
            repo.db(),
            Some(&fixture.engine),
            &binding.worker,
            &claim,
            None,
        )
        .await
        .unwrap();
}
#[tokio::test]
async fn c5d_mismatched_track_fence_conflicts() {
    refused(false).await;
}
#[tokio::test]
async fn c5d_empty_track_handoff_conflicts() {
    refused(true).await;
}
/// Availability is checked once, at entry. An Engine lost while admission waits for
/// the writer leaves the original order: the empty handoff still refuses as Conflict.
#[tokio::test]
async fn c5d_engine_loss_during_writer_wait_keeps_fence_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let binding = crate::harness::backend::dispatch_fixture::seed(
        dir.path(),
        crate::session_projection_repo::AgentProvider::Claude,
    )
    .await;
    let repo = &binding.repo;
    let fixture = crate::test_support::execution::Fixture::new(repo.db().clone()).await;
    let engine = fixture.engine.clone();
    let owner = ProviderExecution {
        owner: Arc::new(Owner),
        drivers: Default::default(),
    };
    let mut intent = workspace_runtime::execution::test_support::intent(dir.path().into());
    intent.metadata.consumer = Owner.consumer().into();
    intent.metadata.track.0 = binding.track.clone();
    intent.metadata.alias.0 = Owner.alias(&binding.worker, intent.metadata.id.as_str());
    let handoff = TrackFenceHandoff::new(
        crate::harness::run_loop::issuance::track_fence_for_test(&binding.track).await,
    );
    handoff.release();
    let writer = calm_truth::db::sqlite::begin_immediate_tx(repo.db())
        .await
        .unwrap();
    let admit = owner.admit(
        repo.db(),
        &engine,
        &binding.worker,
        PreparedIntent(intent),
        Purpose::RewindCheck,
        OccupancyFence::ReleaseOnCommit(&handoff),
    );
    tokio::pin!(admit);
    // One poll runs the entry check, then parks on the writer this test holds.
    tokio::select! {
        biased;
        _ = &mut admit => panic!("admission passed the held writer"),
        _ = std::future::ready(()) => {}
    }
    fixture.disconnect().await;
    assert!(matches!(
        super::super::engine(Some(&engine)),
        Err(CalmError::ExecutionUnavailable)
    ));
    drop(writer);
    assert!(matches!(admit.await, Err(CalmError::Conflict(_))));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM workspace_executions")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}
