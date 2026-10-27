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
