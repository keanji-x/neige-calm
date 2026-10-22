//! Retained C1 recovery must observe durable receipts, never replay a prompt.
use super::*;
use crate::db::ServerRepoOutOfDomainExt;
use serde_json::Value;

#[tokio::test]
async fn canonical_acp_settled_activity_requires_exact_completed_cleanup() {
    for scenario in ["reply", "lost"] {
        let fx = Fixture::new(scenario).await;
        fx.dispatch().await.unwrap();
        fx.wait_idle().await;
        if scenario == "reply" {
            fx.wait_end().await;
        }
        let admission = crate::harness::native_admission::NativePromptAdmission::new(
            || Ok(()),
            || Box::pin(async { Ok(()) }),
        );
        let settled = fx
            .backend
            .settled_activity(&fx.owner.card, &fx.owner.thread, &admission)
            .await
            .unwrap();
        assert_eq!(
            matches!(settled, SettledActivity::Settled),
            scenario == "reply"
        );
        assert!(matches!(
            fx.backend
                .settled_activity("wrong", &fx.owner.thread, &admission)
                .await
                .unwrap(),
            SettledActivity::Held
        ));
        fx.session.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn canonical_acp_retained_recovery_reopens_exact_completed_and_holds_unknown() {
    for scenario in ["reply", "lost"] {
        let fx = Fixture::with_storage(scenario, true).await;
        fx.dispatch().await.unwrap();
        fx.wait_idle().await;
        fx.session.shutdown().await.unwrap();
        let repo = Arc::new(
            crate::db::sqlite::SqlxRepo::open(&fx.owner.db_url)
                .await
                .unwrap(),
        );
        let owner = super::super::super::dispatch_fixture::Owner {
            repo,
            card: fx.owner.card.clone(),
            track: fx.owner.track.clone(),
            worker: fx.owner.worker.clone(),
            thread: fx.owner.thread.clone(),
            db_url: fx.owner.db_url.clone(),
        };
        let (backend, session, _held) = session_for(&owner, &fx.dir);
        let before = fx.prompts();
        crate::planner_process::fail_claude_planner_stop_for_test(&owner.worker);
        let refused = backend.recover_retained().await;
        crate::planner_process::clear_claude_planner_stop_failure_for_test(&owner.worker);
        assert!(
            refused.is_err(),
            "recovery must qualify predecessor cleanup"
        );
        assert!(
            crate::harness::input_dispatch::complete_once(
                owner.repo.as_ref(),
                &backend,
                &owner.card,
                &owner.worker
            )
            .await
            .unwrap()
            .is_none()
        );
        backend.recover_retained().await.unwrap();
        let complete = crate::harness::input_dispatch::complete_once(
            owner.repo.as_ref(),
            &backend,
            &owner.card,
            &owner.worker,
        )
        .await
        .unwrap();
        assert_eq!(complete.is_some(), scenario == "reply");
        assert!(
            crate::harness::input_dispatch::complete_once(
                owner.repo.as_ref(),
                &backend,
                &owner.card,
                &owner.worker
            )
            .await
            .unwrap()
            .is_none()
        );
        let rows: Vec<(i64, String, String)> = owner
            .repo
            .transcript_rows_of_thread(&owner.card, &owner.thread)
            .await
            .unwrap()
            .into_iter()
            .map(|row| (row.id, row.method, row.params))
            .collect();
        if scenario == "reply" {
            let receipt = crate::db::sqlite::acp_submission_get(
                owner.repo.pool(),
                &owner.worker,
                fx.submission.as_str(),
            )
            .await
            .unwrap()
            .unwrap();
            let outcome: Value =
                serde_json::from_str(receipt.outcome_json.as_deref().unwrap()).unwrap();
            let items = outcome["items"].as_array().unwrap();
            assert!(!items.is_empty());
            for item in items {
                assert!(rows.iter().any(|(_, method, params)| method
                    == item["method"].as_str().unwrap()
                    && serde_json::from_str::<Value>(params).unwrap() == item["params"]));
            }
        }
        session.shutdown().await.unwrap();
        let (backend, session, _held) = session_for(&owner, &fx.dir);
        backend.recover_retained().await.unwrap();
        assert!(
            crate::harness::input_dispatch::complete_once(
                owner.repo.as_ref(),
                &backend,
                &owner.card,
                &owner.worker
            )
            .await
            .unwrap()
            .is_none()
        );
        let after: Vec<(i64, String, String)> = owner
            .repo
            .transcript_rows_of_thread(&owner.card, &owner.thread)
            .await
            .unwrap()
            .into_iter()
            .map(|row| (row.id, row.method, row.params))
            .collect();
        assert_eq!(after, rows);
        assert_eq!(fx.prompts(), before);
        session.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn canonical_acp_retained_recovery_crash_windows_keep_sending_held_without_resend() {
    use crate::acp_planner::test_seams::{IssueStage, pause_issue};
    for stage in [
        IssueStage::BeforeReceipt,
        IssueStage::AfterReceipt,
        IssueStage::DriverInstalled,
    ] {
        let fx = Arc::new(Fixture::with_storage("reply", true).await);
        let pause = pause_issue(&fx.owner.worker, stage);
        let dispatch = tokio::spawn({
            let fx = fx.clone();
            async move { fx.dispatch().await }
        });
        tokio::time::timeout(Duration::from_secs(20), pause.entered.notified())
            .await
            .unwrap();
        // A SQLite snapshot captures the actual durable crash window while the
        // production issuer is paused; subsequent orderly teardown is excluded.
        let path = fx.dir.path().join("crash.db");
        sqlx::query("VACUUM INTO ?1")
            .bind(path.to_str().unwrap())
            .execute(fx.owner.repo.pool())
            .await
            .unwrap();
        dispatch.abort();
        match dispatch.await {
            Err(error) => assert!(error.is_cancelled()),
            Ok(_) => panic!("dispatch survived cancellation"),
        }
        fx.session.shutdown().await.unwrap();
        let repo = Arc::new(
            crate::db::sqlite::SqlxRepo::open(&format!("sqlite:{}", path.display()))
                .await
                .unwrap(),
        );
        let owner = super::super::super::dispatch_fixture::Owner {
            repo,
            card: fx.owner.card.clone(),
            track: fx.owner.track.clone(),
            worker: fx.owner.worker.clone(),
            thread: fx.owner.thread.clone(),
            db_url: fx.owner.db_url.clone(),
        };
        let before = crate::db::sqlite::acp_submission_get(
            owner.repo.pool(),
            &owner.worker,
            fx.submission.as_str(),
        )
        .await
        .unwrap();
        if let Some(receipt) = &before {
            assert!(matches!(
                receipt.state,
                crate::db::sqlite::AcpSubmissionState::Sending
            ));
        }
        let (backend, session, _held) = session_for(&owner, &fx.dir);
        let prompts = fx.prompts();
        backend.recover_retained().await.unwrap();
        backend.recover_retained().await.unwrap();
        assert!(
            crate::harness::input_dispatch::complete_once(
                owner.repo.as_ref(),
                &backend,
                &owner.card,
                &owner.worker
            )
            .await
            .unwrap()
            .is_none()
        );
        assert!(
            crate::harness::input_dispatch::initial(
                owner.repo.as_ref(),
                &fx.source,
                &owner.card,
                &owner.worker,
                &fx.submission,
                &backend,
                &TurnModelSelection::inherit(),
                PlannerPermissionMode::Never
            )
            .await
            .is_err()
        );
        assert_eq!(fx.prompts(), prompts);
        let after = crate::db::sqlite::acp_submission_get(
            owner.repo.pool(),
            &owner.worker,
            fx.submission.as_str(),
        )
        .await
        .unwrap();
        assert_eq!(
            before.map(|r| (format!("{:?}", r.state), r.input_json, r.outcome_json)),
            after.map(|r| (format!("{:?}", r.state), r.input_json, r.outcome_json))
        );
        session.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn canonical_acp_later_activity_invalidates_completed_settlement() {
    let fx = Fixture::new("reply").await;
    fx.dispatch().await.unwrap();
    fx.wait_idle().await;
    fx.wait_end().await;
    let admission = crate::harness::native_admission::NativePromptAdmission::new(
        || Ok(()),
        || Box::pin(async { Ok(()) }),
    );
    assert!(matches!(
        fx.backend
            .settled_activity(&fx.owner.card, &fx.owner.thread, &admission)
            .await
            .unwrap(),
        SettledActivity::Settled
    ));
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
        .await
        .unwrap();
    crate::conversation_inputs::accept_human_tx(
        &mut tx,
        &fx.source.current_principal().unwrap(),
        &mut carrier,
        HumanPayload {
            text: "later human".into(),
            bound_attachments: vec![],
        },
    )
    .await
    .unwrap();
    let plan = pending_plan_for_prefix_tx(
        &mut tx,
        &carrier,
        std::num::NonZeroU32::new(1).unwrap(),
        vec![],
    )
    .await
    .unwrap();
    let claim =
        crate::conversation_inputs::claim_initial_tx(&mut tx, &fx.source, &mut carrier, plan)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    std::fs::write(fx.dir.path().join("scenario"), "hold").unwrap();
    crate::harness::input_dispatch::initial(
        fx.owner.repo.as_ref(),
        &fx.source,
        &fx.owner.card,
        &fx.owner.worker,
        &claim.submission_id,
        &fx.backend,
        &TurnModelSelection::inherit(),
        PlannerPermissionMode::Never,
    )
    .await
    .unwrap();
    assert!(matches!(
        fx.backend
            .settled_activity(&fx.owner.card, &fx.owner.thread, &admission)
            .await
            .unwrap(),
        SettledActivity::Held
    ));
    fx.session.shutdown().await.unwrap();
    assert_eq!(fx.prompts(), 2);
}
