//! Capability context reaches the actual resumed harness turn, independent of startup snapshots.
use super::completed_commit_tests::{Fixture, initial_budget_segment};
use super::*;
use crate::model::CardRole;

#[tokio::test]
async fn resumed_planner_and_assistant_receive_live_workspace_report_instructions() {
    for role in [CardRole::Planner, CardRole::Assistant] {
        let fx = Fixture::for_role(role).await;
        let inner = &fx.harness.inner;
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
        let user = QueueEntry::user_message("Read other Areas' reports".into(), None, vec![]);
        let mut expected = input_segments_for_entries(&inner.card_id, std::slice::from_ref(&user));
        if role == CardRole::Planner {
            expected.insert(0, initial_budget_segment());
        }
        fx.enqueue(vec![user]).await;
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
            fx.projected_segments().await,
            expected,
            "workspace capability instructions stay outside input segments; only Planner adds budget context"
        );
        // Same native session: revoke the fixture's live grant after a real granted turn.
        let turn_id = inner.last_turn_id.lock().await.clone().unwrap();
        let live = inner.live_claim.writer();
        on_notification(
            inner,
            &live,
            PlannerEvent {
                thread_id: inner.thread_id.read().await.clone(),
                kind: PlannerEventKind::TurnStarted {
                    turn_id: turn_id.clone(),
                },
            },
        )
        .await
        .unwrap();
        assert_eq!(
            fx.harness.snapshot().await.phase,
            HarnessPhaseTag::TurnRunning
        );
        on_notification(
            inner,
            &live,
            PlannerEvent {
                thread_id: inner.thread_id.read().await.clone(),
                kind: PlannerEventKind::TurnCompleted {
                    turn: serde_json::json!({"id": turn_id, "status": "completed"}),
                },
            },
        )
        .await
        .unwrap();
        assert_eq!(
            fx.harness.snapshot().await.phase,
            HarnessPhaseTag::TurnCompleted
        );
        sqlx::query(
            "UPDATE managed_track_identities SET report_read_scope='area' WHERE track_id=?1",
        )
        .bind(inner.track_id.as_str())
        .execute(fx.repo.pool())
        .await
        .unwrap();
        fx.enqueue(vec![QueueEntry::user_message(
            "Continue after revocation".into(),
            None,
            vec![],
        )])
        .await;
        fx.issue().await;
        let sent = fx.daemon.started_turns_for_test();
        assert_eq!(sent.len(), 2);
        let InputItem::Text { text } = &sent[1].1[0] else {
            panic!("expected text")
        };
        assert!(text.contains("Continue after revocation"));
        assert!(!text.contains("neige_workspace_ls"));
    }
}

#[tokio::test]
async fn ordinary_revoked_and_worker_turns_do_not_receive_workspace_grants() {
    for (role, scope) in [
        (CardRole::Assistant, None),
        (CardRole::Assistant, Some("area")),
        (CardRole::Worker, Some("workspace")),
    ] {
        let fx = Fixture::for_role(role).await;
        let inner = &fx.harness.inner;
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
