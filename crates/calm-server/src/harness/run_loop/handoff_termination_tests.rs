//! Initial production-entry reproductions for handoff termination, not the complete matrix.
//! These reproduce #2892; remove the ignores when implementing its termination protocol.
use super::completed_commit_tests::Fixture;
use super::handoff_coordinator_tests::{active, entered, request, runtime, start_at};
use crate::model::CardRole;
use crate::operation::ProviderAdapter;
use crate::operation::handoff_coordinator as handoff;
use crate::operation::planner_harness_start_adapter::{
    HarnessProfile, PlannerHarnessStartOperationPayload,
};

async fn terminal_error(a: &Fixture, id: &str, label: &str) -> Option<String> {
    let active = active(a).await;
    let output: Option<String> =
        sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE id=?1")
            .bind(id)
            .fetch_one(a.repo.pool())
            .await
            .unwrap();
    let state = output.map(|output| {
        serde_json::from_str::<serde_json::Value>(&output).unwrap()["result"]["state"].clone()
    });
    if active == 0 && state.as_ref().and_then(serde_json::Value::as_str) == Some("abandoned") {
        None
    } else {
        Some(format!("{label}: active={active}, state={state:?}"))
    }
}

#[tokio::test]
#[ignore = "https://github.com/keanji-x/neige-calm/issues/2892: handoff termination protocol not implemented yet"]
async fn handoff_termination_reset_replacement_settles_original_owner() {
    let mut failures = vec![];
    for stage in ["SourcePaused", "Committed"] {
        let a = Fixture::new().await;
        let b = a.sibling(CardRole::Assistant).await;
        let (runtime, registry) = runtime(&a, &b);
        let approval = request(&a, &b).await;
        let (id, hook, drive) = start_at(&runtime, &approval, stage).await;
        entered(&hook).await;
        let card = if stage == "Committed" { &b } else { &a };
        let start =
            super::handoff_activation_test_support::start_adapter(&a, a.repo.clone(), registry);
        let payload = serde_json::to_value(PlannerHarnessStartOperationPayload {
            actor: crate::ids::ActorId::User,
            track_id: approval.track_id.to_string(),
            planner_card_id: card.harness.inner.card_id.clone(),
            report_card_id: None,
            sort: None,
            cwd: "/tmp".into(),
            goal: None,
            reset_harness_items: true,
            force_new_thread: true,
            profile: HarnessProfile::Planner,
            create_card: None,
            first_message: None,
            create_request_sha256: None,
            opening_briefing: None,
        })
        .unwrap();
        let op = runtime
            .find_by_kind_and_idempotency("handoff-workflow", &approval.key)
            .await
            .unwrap()
            .unwrap();
        let mut tx = crate::db::sqlite::begin_immediate_tx(a.repo.db())
            .await
            .unwrap();
        let replacement = start.prepare_tx(&mut tx, &payload, &op).await;
        if replacement.is_ok() {
            tx.commit().await.unwrap();
        } else {
            tx.rollback().await.unwrap();
        }
        drive.abort();
        assert!(drive.await.unwrap_err().is_cancelled());
        hook.release.notify_one();
        if let Err(error) = replacement {
            failures.push(format!("{stage}: replacement refused: {error}"));
        }
        failures.extend(terminal_error(&a, &id, stage).await);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
#[ignore = "https://github.com/keanji-x/neige-calm/issues/2892: handoff termination protocol not implemented yet"]
async fn handoff_termination_cancel_native_intent_and_unknown_release_exclusion() {
    let mut failures = vec![];
    for unknown in [false, true] {
        let a = Fixture::new().await;
        let b = a.sibling(CardRole::Assistant).await;
        let (runtime, _) = runtime(&a, &b);
        let approval = request(&a, &b).await;
        let expected = if unknown {
            a.daemon.fail_next_thread_start_for_test();
            "unknown"
        } else {
            // The native call succeeds but binding fails. Never seed a successful receipt.
            sqlx::query(
                r#"
                CREATE TRIGGER termination_bind_fault BEFORE UPDATE OF state ON worker_sessions
                WHEN OLD.state='pending_activation' AND NEW.state='starting'
                BEGIN SELECT RAISE(ABORT,'lost native binding'); END
            "#,
            )
            .execute(a.repo.db().writer_pool())
            .await
            .unwrap();
            "native_intent"
        };
        let id = handoff::begin(&runtime, approval.clone()).await.unwrap();
        let op = runtime
            .find_by_kind_and_idempotency("handoff-workflow", &approval.key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(op.tx_output.unwrap().result["resources"]["state"], expected);
        handoff::cancel(&runtime, &approval).await.unwrap();
        failures.extend(terminal_error(&a, &id, expected).await);
        assert_eq!(a.daemon.started_thread_params_for_test().len(), 1);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
#[ignore = "https://github.com/keanji-x/neige-calm/issues/2892: handoff termination protocol not implemented yet"]
async fn handoff_termination_cancel_each_live_checkpoint_releases_exclusion() {
    let mut failures = vec![];
    for stage in [
        "Reserved",
        "SourceIntent",
        "SourcePaused",
        "TargetIntent",
        "BothPaused",
        "BeforeCommit",
        "Committed",
    ] {
        let a = Fixture::new().await;
        let b = a.sibling(CardRole::Assistant).await;
        let (runtime, _) = runtime(&a, &b);
        let approval = request(&a, &b).await;
        // start_at's Reserved seam is the admission boundary, before tx_output exists.
        let (id, hook, drive) = start_at(&runtime, &approval, stage).await;
        if stage != "Reserved" {
            entered(&hook).await;
        }
        handoff::cancel(&runtime, &approval).await.unwrap();
        // Settlement cannot depend on the blocked/lost original caller returning.
        drive.abort();
        assert!(drive.await.unwrap_err().is_cancelled());
        hook.release.notify_one();
        failures.extend(terminal_error(&a, &id, stage).await);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
