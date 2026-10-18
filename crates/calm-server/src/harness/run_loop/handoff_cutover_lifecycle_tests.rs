//! Lifecycle consumers of the production identity operation; no provider process is started.
use super::completed_commit_tests::Fixture;
use super::handoff_cutover_tests::{commit, plan};
use super::*;
use crate::db::prelude::*;
use crate::model::CardRole;

#[tokio::test]
async fn handoff_cutover_history_remains_readonly_while_parent_track_can_be_deleted() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    a.repo
        .harness_item_insert(
            &a.harness.inner.worker_session_id,
            a.harness.inner.card_id.as_str(),
            a.harness.inner.track_id.as_str(),
            "thread-commit-wake",
            Some("retained-turn"),
            Some("history-item"),
            Some("agentMessage"),
            "item/completed",
            "{}",
            None,
        )
        .await
        .unwrap();
    let plan = plan(&a, &b).await;
    commit(&a, plan.clone()).await.unwrap();
    assert!(
        sqlx::query("UPDATE cards SET title='cannot edit history' WHERE id=?1")
            .bind(plan.scope.source_card_id.as_str())
            .execute(a.repo.pool())
            .await
            .is_err()
    );
    // Track deletion is its existing user/kernel management contract, never history authority.
    a.repo
        .track_delete(plan.scope.track_id.as_str())
        .await
        .unwrap();
    assert!(
        a.repo
            .track_get(plan.scope.track_id.as_str())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        a.repo
            .card_get(plan.scope.source_card_id.as_str())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn handoff_cutover_preserves_queue_floor_then_reads_only_suffix_from_existing_owner() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let event = |key: &str| Event::TrackWakeRequested {
        track_id: a.harness.inner.track_id.clone(),
        source: "handoff-fixture".into(),
        key: key.into(),
        text: key.into(),
    };
    let (floor, original) = a.event_entry(&event("queued-prefix")).await;
    a.harness
        .observe_envelope(original.observation(), floor)
        .await
        .unwrap();
    let plan = plan(&a, &b).await;
    commit(&a, plan.clone()).await.unwrap();
    let row = a
        .repo
        .session_get_by_id(&plan.scope.new_session_id.as_str().into())
        .await
        .unwrap()
        .unwrap();
    let snapshot = HarnessSnapshot::parse_known(row.handle_state_json.unwrap()).unwrap();
    assert_eq!(snapshot.push_watermark, floor);
    assert_eq!(snapshot.pending_entries(), vec![original]);
    let (suffix, entry) = b.event_entry(&event("suffix-after-cutover")).await;
    let observations = crate::harness::catch_up::observations_since(
        a.repo.as_ref(),
        &plan.scope.track_id,
        snapshot.push_watermark,
        Some(suffix),
    )
    .await
    .unwrap();
    assert_eq!(observations, vec![(suffix, entry.observation())]);
    assert_eq!(a.daemon.turn_start_count_for_test(), 0);
}

#[tokio::test]
async fn handoff_cutover_registered_providers_pause_and_commit_without_native_start() {
    for provider in crate::harness::backend::PlannerBackend::registered_providers() {
        let mut a = Fixture::new().await;
        let mut b = a.sibling(CardRole::Assistant).await;
        let mut ingress = vec![];
        // Configure each fixture before its pause. Open only the existing backend's inert
        // session wrapper: its documented contract creates no native resource before a turn.
        for f in [&mut a, &mut b] {
            let role = f
                .repo
                .card_role_get(f.harness.inner.card_id.as_str())
                .await
                .unwrap()
                .unwrap();
            let key = if role == CardRole::Planner {
                "$.planner_provider"
            } else {
                "$.assistant_provider"
            };
            sqlx::query("UPDATE cards SET payload=json_set(payload,?1,json(?2)) WHERE id=?3")
                .bind(key)
                .bind(serde_json::to_string(&provider).unwrap())
                .bind(f.harness.inner.card_id.as_str())
                .execute(f.repo.pool())
                .await
                .unwrap();
            sqlx::query("UPDATE worker_sessions SET provider=?1 WHERE id=?2")
                .bind(calm_types::worker::WorkerProviderKind::from(&provider).as_db_str())
                .bind(&f.harness.inner.worker_session_id)
                .execute(f.repo.pool())
                .await
                .unwrap();
            let card = f
                .repo
                .card_get(f.harness.inner.card_id.as_str())
                .await
                .unwrap()
                .unwrap();
            let binding = crate::harness::profile::PlannerBinding::from_card(&card, role).unwrap();
            let wiring = crate::claude_planner::wiring::ClaudePlannerWiring::unconfigured_for_test(
                f.repo.clone(),
            );
            let backend = crate::harness::backend::PlannerBackend::open(
                provider.clone(),
                f.daemon.clone(),
                f.daemon.thread_seals().clone(),
                &wiring,
                f.repo.clone(),
                crate::harness::wiring::PlannerRow {
                    binding,
                    worker_session_id: &f.harness.inner.worker_session_id,
                    card_id: f.harness.inner.card_id.as_str(),
                    track_id: f.harness.inner.track_id.as_str(),
                    prior_total_tokens: 0,
                },
            )
            .await
            .unwrap();
            let mut params = super::handoff_pause_tests::params(f, f.stored().await);
            params.backend = backend;
            let (harness, receiver) = PlannerHarness::run_unstarted_for_test(params, 8);
            f.harness = harness;
            ingress.push(receiver);
        }
        let plan = plan(&a, &b).await;
        commit(&a, plan.clone()).await.unwrap();
        let row = a
            .repo
            .session_get_by_id(&plan.scope.new_session_id.as_str().into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.provider,
            calm_types::worker::WorkerProviderKind::from(&provider)
        );
        assert_eq!(
            row.state,
            crate::session_projection_repo::WorkerSessionState::PendingActivation
        );
        assert!(
            row.thread_id.is_none()
                && row.agent_session_id.is_none()
                && row.mcp_token_hash.is_none()
        );
        assert_eq!(a.daemon.turn_start_count_for_test(), 0);
        assert_eq!(b.daemon.turn_start_count_for_test(), 0);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM acp_managed_sessions")
                .fetch_one(a.repo.pool())
                .await
                .unwrap(),
            0
        );
        drop(ingress);
    }
}
