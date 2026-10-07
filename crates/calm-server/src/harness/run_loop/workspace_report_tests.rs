//! Capability context reaches the actual resumed harness turn, independent of startup snapshots.
use super::completed_commit_tests::Fixture;
use super::*;
use crate::model::CardRole;

#[tokio::test]
async fn resumed_planner_and_assistant_receive_live_workspace_report_instructions() {
    for role in [CardRole::Planner, CardRole::Assistant] {
        let fx = Fixture::new().await;
        let inner = &fx.harness.inner;
        sqlx::query("UPDATE cards SET role=?1 WHERE id=?2")
            .bind(role.as_db_str())
            .bind(inner.card_id.as_str())
            .execute(fx.repo.pool())
            .await
            .unwrap();
        // No template snapshot and an existing thread: updating the capability must suffice.
        sqlx::query(concat!(
            "INSERT INTO managed_track_identities(owner,identity,track_id,report_read_scope,",
            "report_time_zone,tool_policy,kernel_controls_lifecycle) ",
            "VALUES('test','2026-10-04',?1,'workspace','Asia/Shanghai','reports',1)",
        ))
        .bind(inner.track_id.as_str())
        .execute(fx.repo.pool())
        .await
        .unwrap();
        fx.enqueue(vec![QueueEntry::user_message(
            "Read other Areas' reports".into(),
            None,
            vec![],
        )])
        .await;
        fx.issue().await;
        let sent = fx.daemon.started_turns_for_test();
        assert_eq!(sent.len(), 1);
        let InputItem::Text { text } = &sent[0].1[0] else {
            panic!("expected text")
        };
        for name in [
            "neige_workspace_ls",
            "neige_workspace_cat",
            "neige_workspace_diff",
            "neige_workspace_log",
            "read-only",
            "all user-visible Areas",
        ] {
            assert!(text.contains(name), "{role:?}: missing {name}");
        }
        assert!(
            text.find("neige_workspace_ls").unwrap()
                < text.find("Read other Areas' reports").unwrap()
        );
        assert_eq!(
            fx.projected_segments().await.len(),
            1,
            "capability context is not a user message"
        );
    }
}

#[tokio::test]
async fn ordinary_revoked_and_worker_turns_do_not_receive_workspace_grants() {
    for (role, scope) in [
        (CardRole::Assistant, None),
        (CardRole::Assistant, Some("area")),
        (CardRole::Worker, Some("workspace")),
    ] {
        let fx = Fixture::new().await;
        let inner = &fx.harness.inner;
        sqlx::query("UPDATE cards SET role=?1 WHERE id=?2")
            .bind(role.as_db_str())
            .bind(inner.card_id.as_str())
            .execute(fx.repo.pool())
            .await
            .unwrap();
        if let Some(scope) = scope {
            sqlx::query(concat!(
                "INSERT INTO managed_track_identities(owner,identity,track_id,report_read_scope,",
                "report_time_zone,tool_policy,kernel_controls_lifecycle) ",
                "VALUES('test','2026-10-04',?1,?2,'Asia/Shanghai','reports',1)",
            ))
            .bind(inner.track_id.as_str())
            .bind(scope)
            .execute(fx.repo.pool())
            .await
            .unwrap();
        }
        fx.enqueue(vec![QueueEntry::user_message("Hello".into(), None, vec![])])
            .await;
        fx.issue().await;
        let sent = fx.daemon.started_turns_for_test();
        assert_eq!(sent.len(), 1);
        let InputItem::Text { text } = &sent[0].1[0] else {
            panic!("expected text")
        };
        assert!(!text.contains("neige_workspace_ls"), "{role:?} / {scope:?}");
    }
}
