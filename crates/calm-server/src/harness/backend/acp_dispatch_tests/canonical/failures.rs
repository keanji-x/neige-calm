//! Native terminal frames cannot substitute for required cleanup or receipt persistence.
#![cfg(test)]
use super::*;

#[tokio::test]
async fn canonical_acp_revoke_stop_and_receipt_failures_never_qualify_end() {
    #[derive(Clone, Copy)]
    enum Failure {
        Revoke,
        Stop,
        Receipt,
    }
    for failure in [Failure::Revoke, Failure::Stop, Failure::Receipt] {
        let fx = super::grok_compact::fixture().await;
        let pause = crate::acp_planner::test_seams::pause_after_fence(&fx.owner.worker);
        fx.dispatch().await.unwrap();
        tokio::time::timeout(Duration::from_secs(20), pause.entered.notified())
            .await
            .unwrap();
        match failure {
            Failure::Revoke => {
                (fx.owner.repo.db()).writer().execute(sqlx::query(concat!(
                    "CREATE TRIGGER refuse_revoke BEFORE UPDATE OF mcp_token_hash ON worker_sessions ",
                    "WHEN NEW.mcp_token_hash IS NULL BEGIN SELECT RAISE(ABORT,'refuse revoke'); END"
                ))).await.unwrap();
            }
            Failure::Stop => crate::test_support::execution::block_stops(fx.owner.repo.db()).await,
            Failure::Receipt => {
                (fx.owner.repo.db()).writer().execute(sqlx::query("CREATE TRIGGER refuse_receipt BEFORE UPDATE ON acp_submissions BEGIN SELECT RAISE(ABORT,'refuse receipt'); END")).await.unwrap();
            }
        }
        pause.release.notify_one();
        tokio::time::timeout(Duration::from_secs(20), async {
            while !fx.session.driver_finished_for_test() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(fx.complete().await.is_none());
        let state: String = sqlx::query_scalar("SELECT state FROM acp_submissions")
            .fetch_one(fx.owner.repo.pool())
            .await
            .unwrap();
        assert_ne!(state, "completed");
        fx.keep_unknown(&fx.submission, "keep-failed-cleanup").await;
        assert!(fx.backend.settle_kept_execution().await.unwrap().is_none());
        assert_eq!(fx.prompts(), 1);
        match failure {
            Failure::Revoke => {
                (fx.owner.repo.db())
                    .writer()
                    .execute(sqlx::query("DROP TRIGGER refuse_revoke"))
                    .await
                    .unwrap();
            }
            Failure::Stop => {
                crate::test_support::execution::unblock_stops(fx.owner.repo.db()).await
            }
            Failure::Receipt => {
                (fx.owner.repo.db())
                    .writer()
                    .execute(sqlx::query("DROP TRIGGER refuse_receipt"))
                    .await
                    .unwrap();
            }
        }
        fx.session.shutdown().await.unwrap();
        assert!(fx.complete().await.is_none());
    }
}

#[tokio::test]
async fn canonical_acp_uncertain_receipt_commit_is_unknown_even_when_sqlite_rolled_back() {
    let fx = Fixture::new("reply").await;
    (fx.owner.repo.db()).writer().execute(sqlx::raw_sql("CREATE TABLE commit_fault(worker TEXT REFERENCES worker_sessions(id) DEFERRABLE INITIALLY DEFERRED);
        CREATE TRIGGER defer_receipt_failure AFTER INSERT ON acp_submissions
        BEGIN INSERT INTO commit_fault VALUES('missing-worker'); END;")).await.unwrap();
    let report = fx.dispatch().await.unwrap();
    assert!(matches!(
        report.report.delivery(),
        InputDelivery::Unknown(_)
    ));
    assert!(report.report.outcome().is_err());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM acp_submissions")
        .fetch_one(fx.owner.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "failed COMMIT rolled back, but is never factual nondispatch"
    );
    assert_eq!(fx.prompts(), 0);
    assert!(fx.complete().await.is_none());
    assert!(fx.dispatch().await.is_err());
    assert_eq!(fx.prompts(), 0);
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_acp_completed_receipt_cannot_bypass_failed_canonical_report_commit() {
    let fx = Fixture::new("reply").await;
    (fx.owner.repo.db()).writer().execute(sqlx::query("CREATE TRIGGER refuse_canonical_report BEFORE UPDATE OF handle_state_json ON worker_sessions
        WHEN json_extract(NEW.handle_state_json,'$.canonical_inputs.attempts[0].delivery.kind')='unknown'
        BEGIN SELECT RAISE(ABORT,'canonical report failed'); END")).await.unwrap();
    assert!(fx.dispatch().await.is_err());
    fx.wait_idle().await;
    let receipt = crate::db::sqlite::acp_submission_get(
        fx.owner.repo.pool(),
        &fx.owner.worker,
        fx.submission.as_str(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        receipt.state,
        crate::db::sqlite::AcpSubmissionState::Completed
    ));
    let write_db = (fx.owner.repo.db()).clone();
    write_db
        .writer()
        .with_writer(|write_session| {
            Box::pin(async move {
                let mut tx = write_session.begin_immediate().await.unwrap();
                let carrier =
                    current_input_fact_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
                        .await
                        .unwrap();
                let before = carrier.checkpoint().clone();
                assert!(matches!(
                    before.attempt(&fx.submission).unwrap().delivery,
                    InputDeliveryFact::CrossingUnknown
                ));
                tx.commit().await.unwrap();
                assert!(
                    fx.complete().await.is_none(),
                    "END must not bypass the canonical fact owner"
                );
                assert!(fx.dispatch().await.is_err());
                let mut tx = write_session.begin_immediate().await.unwrap();
                let carrier =
                    current_input_fact_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
                        .await
                        .unwrap();
                assert_eq!(carrier.checkpoint(), &before);
                tx.commit().await.unwrap();
                assert_eq!(fx.prompts(), 1);
                fx.session.shutdown().await.unwrap();
            })
        })
        .await
        .expect("independent write operation")
}

#[tokio::test]
async fn canonical_acp_invalid_receipted_turn_is_unknown_with_original_correlation() {
    let fx = Fixture::new("reply").await;
    let original = fx.dispatch().await.unwrap();
    fx.wait_idle().await;
    let receipt = crate::db::sqlite::acp_submission_get(
        fx.owner.repo.pool(),
        &fx.owner.worker,
        fx.submission.as_str(),
    )
    .await
    .unwrap()
    .unwrap();
    let input: serde_json::Value = serde_json::from_str(&receipt.input_json).unwrap();
    let logical: LogicalTurnId =
        serde_json::from_value(input["association"]["logical_turn_id"].clone()).unwrap();
    let proof: SubmissionDispatchProof =
        serde_json::from_value(input["association"]["proof"].clone()).unwrap();
    (fx.owner.repo.db()).writer().execute(sqlx::query("UPDATE acp_submissions SET turn_id='not-a-local-uuid' WHERE worker_session_id=?1 AND client_id=?2")
        .bind(&fx.owner.worker).bind(fx.submission.as_str())).await.unwrap();
    let report = fx
        .backend
        .dispatch_input(
            None,
            &logical,
            proof.attempt(),
            &fx.owner.thread,
            &proof,
            vec![InputItem::text("must never be resent")],
            &TurnModelSelection::inherit(),
            PlannerPermissionMode::Never,
            &crate::harness::native_admission::NativePromptAdmission::new(
                || Ok(()),
                || Box::pin(async { Ok(()) }),
            ),
            &fx.backend.handoff_for_test().await,
        )
        .await;
    assert!(matches!(report.delivery(), InputDelivery::Unknown(_)));
    let correlation = report.delivery().correlation();
    assert_eq!(correlation.attempt(), original.report.attempt());
    assert_eq!(correlation.provider(), &AgentProvider::OpenCode);
    assert_eq!(correlation.thread_id(), fx.owner.thread);
    assert_eq!(correlation.client_id(), fx.submission.as_str());
    assert_eq!(
        correlation.native_session_id(),
        Some(receipt.native_session_id.as_str())
    );
    assert_eq!(correlation.local_turn_id(), None);
    assert_eq!(correlation.native_turn_id(), None);
    assert_eq!(correlation.rpc_request_id(), None);
    let Err(TurnStartFailure::Transient(CalmError::Conflict(error))) = report.outcome() else {
        panic!("invalid receipt must return a conflict");
    };
    assert!(error.starts_with("ACP local receipt identity is invalid:"));
    assert_eq!(fx.prompts(), 1);
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_acp_oversized_prompt_is_refused_before_receipt_in_both_runtimes() {
    for provider in [AgentProvider::OpenCode, AgentProvider::Antigravity] {
        let fx = Fixture::with_instructions(
            provider.clone(),
            "reply",
            false,
            None,
            "x".repeat(8 * 1024 * 1024),
        )
        .await;
        let report = fx.dispatch().await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM acp_submissions")
            .fetch_one(fx.owner.repo.pool())
            .await
            .unwrap();
        fx.session.shutdown().await.unwrap();
        assert!(
            fx.requests()
                .iter()
                .any(|request| request["method"] == "session/new")
        );
        assert_eq!(fx.prompts(), 0, "{provider:?}");
        assert_eq!(count, 0, "{provider:?}");
        assert!(
            matches!(report.report.delivery(), InputDelivery::NotDispatched(_)),
            "{provider:?}"
        );
    }
}

#[tokio::test]
async fn typed_live_replies_both_driver_streams_publish_complete_partial_without_end_authority() {
    for provider in [AgentProvider::Grok, AgentProvider::Antigravity] {
        let fx = Fixture::with_provider(provider, "rich-unknown", false).await;
        let mut stream = fx.session.subscribe_events();
        fx.dispatch().await.unwrap();
        let mut items = Vec::new();
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let event = stream.recv().await.unwrap();
                match event.kind {
                    provider::events::PlannerEventKind::Item {
                        phase: provider::events::ItemPhase::Completed,
                        params,
                        ..
                    } => items.push(params),
                    provider::events::PlannerEventKind::TurnCompleted { turn } => {
                        assert_eq!(turn["status"], "failed");
                        break;
                    }
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        let output: Vec<_> = items
            .iter()
            .filter(|v| v["item"].get("outputSegment").is_some())
            .collect();
        assert_eq!(
            output.len(),
            2,
            "producer must seal both segments before unknown terminal: {items:?}"
        );
        assert_eq!(
            output[0]["item"]["outputContent"]["blocks"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            output[1]["item"]["outputContent"]["blocks"][0]["type"],
            "audio"
        );
        assert_eq!(output[1]["_partial"], true);
        fx.wait_driver_finished().await;
        assert!(
            fx.complete().await.is_none(),
            "content partial is not known END evidence"
        );
        let (state, outcome): (String, Option<String>) =
            sqlx::query_as("SELECT state,outcome_json FROM acp_submissions")
                .fetch_one(fx.owner.repo.pool())
                .await
                .unwrap();
        assert_eq!(state, "unknown");
        assert!(outcome.is_none());
        fx.session.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn typed_live_replies_receipt_failure_keeps_final_items_without_end_authority() {
    for provider in [AgentProvider::Grok, AgentProvider::Antigravity] {
        let fx = Fixture::with_provider(provider, "rich-normal", false).await;
        fx.owner
            .repo
            .db()
            .writer()
            .execute(sqlx::query(concat!(
                "CREATE TRIGGER refuse_rich_receipt BEFORE UPDATE ON acp_submissions ",
                "WHEN NEW.state='completed' ",
                "BEGIN SELECT RAISE(ABORT,'receipt unavailable'); END"
            )))
            .await
            .unwrap();
        let mut stream = fx.session.subscribe_events();
        fx.dispatch().await.unwrap();
        let mut output = Vec::new();
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let event = stream.recv().await.unwrap();
                match event.kind {
                    provider::events::PlannerEventKind::Item {
                        phase: provider::events::ItemPhase::Completed,
                        params,
                        ..
                    } if params["item"].get("outputSegment").is_some() => output.push(params),
                    provider::events::PlannerEventKind::TurnCompleted { turn } => {
                        assert_eq!(turn["status"], "failed");
                        break;
                    }
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(output.len(), 2);
        assert_eq!(
            output[0]["item"]["outputContent"]["blocks"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            output[1]["item"]["outputContent"]["blocks"][0]["type"],
            "audio"
        );
        assert!(output[1].get("_partial").is_none());
        fx.wait_driver_finished().await;
        assert!(fx.complete().await.is_none());
        let (state, outcome): (String, Option<String>) =
            sqlx::query_as("SELECT state,outcome_json FROM acp_submissions")
                .fetch_one(fx.owner.repo.pool())
                .await
                .unwrap();
        assert_eq!(state, "unknown");
        assert!(outcome.is_none());
        fx.session.shutdown().await.unwrap();
    }
}
