//! Cleanup failures must keep their historical host classification and precedence.
#![cfg(test)]
use super::*;
use crate::acp_planner::test_seams::{IssueStage, pause_issue};
use std::time::Duration;

#[derive(Clone, Copy)]
pub(super) enum Failure {
    SpawnRevoke,
    PreparedRevoke,
    PreparedStop,
    PreparedBoth,
}
impl Failure {
    pub(super) fn selection(self) -> TurnModelSelection {
        TurnModelSelection {
            model: Some("undeclared-model".into()),
            effort: None,
        }
    }
    pub(super) fn stage(self) -> IssueStage {
        match self {
            Self::SpawnRevoke => IssueStage::BeforeMint,
            Self::PreparedRevoke | Self::PreparedStop | Self::PreparedBoth => {
                IssueStage::AfterSpawn
            }
        }
    }
    pub(super) fn expected(self) -> &'static str {
        match self {
            Self::SpawnRevoke | Self::PreparedRevoke | Self::PreparedBoth => "refuse revoke",
            Self::PreparedStop => "failed (fixture)",
        }
    }
    pub(super) async fn arm(self, owner: &super::super::dispatch_fixture::Owner) {
        match self {
            Self::SpawnRevoke | Self::PreparedRevoke | Self::PreparedBoth => {
                sqlx::query(concat!(
                    "CREATE TRIGGER refuse_revoke BEFORE UPDATE OF mcp_token_hash ON worker_sessions ",
                    "WHEN NEW.mcp_token_hash IS NULL BEGIN SELECT RAISE(ABORT,'refuse revoke'); END"
                )).execute(owner.repo.pool()).await.unwrap();
            }
            Self::PreparedStop => {}
        }
        match self {
            Self::PreparedStop | Self::PreparedBoth => {
                crate::planner_process::fail_claude_planner_stop_for_test(&owner.worker);
            }
            Self::SpawnRevoke | Self::PreparedRevoke => {}
        }
    }
    pub(super) async fn clear(self, owner: &super::super::dispatch_fixture::Owner) {
        match self {
            Self::SpawnRevoke | Self::PreparedRevoke | Self::PreparedBoth => {
                sqlx::query("DROP TRIGGER refuse_revoke")
                    .execute(owner.repo.pool())
                    .await
                    .unwrap();
            }
            Self::PreparedStop => {}
        }
        crate::planner_process::clear_claude_planner_stop_failure_for_test(&owner.worker);
    }
}
async fn historical(failure: Failure) {
    let (_backend, mut session, owner, dir) = fixture().await;
    if matches!(failure, Failure::SpawnRevoke) {
        let (_backend, replacement, _held) =
            session_for_command(&owner, &dir, "/missing-acp-executable");
        session = replacement;
    }
    let pause = pause_issue(&owner.worker, failure.stage());
    let request = provider::control::TurnRequest {
        input: provider::control::InputRequest {
            items: vec![provider::InputItem::text("historical")],
            correlation: provider::control::CorrelationKey("historical-cleanup".into()),
        },
        selection: failure.selection(),
        permission: PlannerPermissionMode::Never,
        edit: provider::control::TurnEdit::NoEdit,
    };
    let issue = session.start_turn(&owner.thread, request, &[]);
    tokio::pin!(issue);
    tokio::select! {
        result = &mut issue => panic!("issue skipped cleanup fault window: {result:?}"),
        result = tokio::time::timeout(Duration::from_secs(20), pause.entered.notified()) => result.unwrap(),
    }
    failure.arm(&owner).await;
    pause.release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(20), issue)
        .await
        .unwrap();
    // Disarm faults before asserting, so a red test still cleans up its process.
    failure.clear(&owner).await;
    session.shutdown().await.unwrap();
    assert!(
        matches!(result, Err(TurnStartFailure::Transient(ref error))
        if error.to_string().contains(failure.expected())),
        "{result:?}"
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM acp_submissions")
        .fetch_one(owner.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}
#[tokio::test]
async fn acp_cleanup_historical_spawn_revoke() {
    historical(Failure::SpawnRevoke).await;
}
#[tokio::test]
async fn acp_cleanup_historical_prepared_revoke() {
    historical(Failure::PreparedRevoke).await;
}
#[tokio::test]
async fn acp_cleanup_historical_prepared_stop() {
    historical(Failure::PreparedStop).await;
}
#[tokio::test]
async fn acp_cleanup_historical_revoke_precedes_stop() {
    historical(Failure::PreparedBoth).await;
}
