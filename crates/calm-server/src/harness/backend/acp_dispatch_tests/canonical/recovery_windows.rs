//! Crash images come only from production dispatch/recovery, before teardown.
use super::super::super::dispatch_fixture::fail_terminal_outcome;
use super::*;
use calm_truth::db::RepoOutOfDomain;

type Owner = super::super::super::dispatch_fixture::Owner;
type Rows = Vec<(i64, String, String)>;
type Receipt = (String, String, Option<String>);

async fn rows(owner: &Owner) -> Rows {
    owner
        .repo
        .transcript_rows_of_thread(&owner.card, &owner.thread)
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.id, row.method, row.params))
        .collect()
}

async fn receipt(owner: &Owner) -> Receipt {
    sqlx::query_as(
        "SELECT state,input_json,outcome_json FROM acp_submissions WHERE worker_session_id=?1",
    )
    .bind(&owner.worker)
    .fetch_one(owner.repo.pool())
    .await
    .unwrap()
}

async fn reopen(owner: &Owner, url: String) -> Owner {
    Owner {
        repo: Arc::new(crate::db::sqlite::SqlxRepo::open(&url).await.unwrap()),
        card: owner.card.clone(),
        track: owner.track.clone(),
        worker: owner.worker.clone(),
        thread: owner.thread.clone(),
        db_url: url,
    }
}

async fn snapshot(
    owner: &Owner,
    dir: &crate::test_support::execution::Directory,
    name: &str,
) -> Owner {
    let path = dir.path().join(name);
    sqlx::query("VACUUM INTO ?1")
        .bind(path.to_str().unwrap())
        .execute(owner.repo.pool())
        .await
        .unwrap();
    reopen(owner, format!("sqlite:{}", path.display())).await
}

async fn complete(owner: &Owner, backend: &PlannerBackend) -> bool {
    crate::harness::input_dispatch::complete_once(
        owner.repo.as_ref(),
        backend,
        &owner.card,
        &owner.worker,
    )
    .await
    .unwrap()
    .is_some()
}

fn preserves_complete(before: &Rows, after: &Rows) {
    for row in before {
        // Started items may legitimately advance to their final receipt frame.
        if row.1 == "item/completed" || row.1 == "turn/completed" {
            assert!(
                after.contains(row),
                "recovery rewrote complete projection {row:?}"
            );
        }
    }
}

fn one_terminal(rows: &Rows) {
    assert_eq!(
        rows.iter().filter(|row| row.1 == "turn/completed").count(),
        1
    );
}

#[tokio::test]
async fn canonical_acp_completed_crash_recovery_preserves_receipt_and_projection_windows() {
    for window in ["receipt", "lost-report", "restored-items", "projected"] {
        let fx = Fixture::with_storage("reply", true).await;
        if window != "projected" {
            fail_terminal_outcome(fx.owner.repo.as_ref()).await;
        }
        if window == "lost-report" {
            sqlx::query("CREATE TRIGGER refuse_report BEFORE UPDATE OF handle_state_json ON worker_sessions
                WHEN json_extract(NEW.handle_state_json,'$.canonical_inputs.attempts[0].delivery.kind')='unknown'
                BEGIN SELECT RAISE(ABORT,'lost canonical report'); END")
                .execute(fx.owner.repo.pool()).await.unwrap();
            let error = fx
                .dispatch()
                .await
                .err()
                .expect("canonical report must fail");
            assert!(
                error.to_string().contains("lost canonical report"),
                "{error}"
            );
        } else {
            fx.dispatch().await.unwrap();
        }
        fx.wait_driver_finished().await;
        if window != "projected" {
            assert!(
                calm_truth::db::sqlite::provider_execution::load(
                    fx.owner.repo.pool(),
                    &fx.owner.worker
                )
                .await
                .unwrap()
                .is_some(),
                "failed owner confirmation retains the claim"
            );
        }
        // Idle is cleared after the receipt and the attempted terminal write. A
        // VACUUM image excludes any subsequent shutdown/cleanup effects.
        let durable = receipt(&fx.owner).await;
        assert_eq!(durable.0, "completed");
        let initial_rows = rows(&fx.owner).await;
        assert_eq!(
            initial_rows
                .iter()
                .filter(|row| row.1 == "turn/completed")
                .count(),
            usize::from(window == "projected")
        );
        let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
        let carrier = current_input_fact_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
            .await
            .unwrap();
        assert_eq!(
            matches!(
                carrier
                    .checkpoint()
                    .attempt(&fx.submission)
                    .unwrap()
                    .delivery,
                InputDeliveryFact::CrossingUnknown
            ),
            window == "lost-report"
        );
        tx.commit().await.unwrap();
        let mut owner = snapshot(&fx.owner, &fx.dir, "completed-crash.db").await;
        fx.session.shutdown().await.unwrap();
        if window == "lost-report" {
            sqlx::query("DROP TRIGGER refuse_report")
                .execute(owner.repo.pool())
                .await
                .unwrap();
        }
        if window == "restored-items" {
            let (backend, session, _held) = session_for(&owner, &fx.dir);
            let before = rows(&owner).await;
            let error = backend.recover_retained().await.unwrap_err();
            assert!(error.to_string().contains("outcome failed"), "{error}");
            assert!(
                !complete(&owner, &backend).await,
                "partial recovery installed a certificate"
            );
            let restored = rows(&owner).await;
            preserves_complete(&before, &restored);
            assert!(restored.iter().any(|row| row.1 == "item/completed"));
            assert!(!restored.iter().any(|row| row.1 == "turn/completed"));
            assert_eq!(receipt(&owner).await, durable);
            // Crash again after restore committed, before terminal/certificate.
            owner = snapshot(&owner, &fx.dir, "recovery-crash.db").await;
            session.shutdown().await.unwrap();
        }
        if window != "projected" {
            sqlx::query("DROP TRIGGER fail_outcome")
                .execute(owner.repo.pool())
                .await
                .unwrap();
        }
        let before = rows(&owner).await;
        assert_eq!(receipt(&owner).await, durable);
        let prompts = fx.prompts();
        let (backend, session, _held) = session_for(&owner, &fx.dir);
        backend.recover_retained().await.unwrap();
        assert!(complete(&owner, &backend).await, "missing END at {window}");
        assert!(!complete(&owner, &backend).await);
        let recovered = rows(&owner).await;
        preserves_complete(&initial_rows, &recovered);
        preserves_complete(&before, &recovered);
        one_terminal(&recovered);
        assert_eq!(receipt(&owner).await, durable);
        let outcome: serde_json::Value =
            serde_json::from_str(durable.2.as_deref().unwrap()).unwrap();
        let items = outcome["items"].as_array().unwrap();
        assert!(!items.is_empty());
        for item in items {
            assert!(recovered.iter().any(|(_, method, params)| method
                == item["method"].as_str().unwrap()
                && serde_json::from_str::<serde_json::Value>(params).unwrap() == item["params"]));
        }
        session.shutdown().await.unwrap();
        let owner = reopen(&owner, owner.db_url.clone()).await;
        let (backend, session, _held) = session_for(&owner, &fx.dir);
        backend.recover_retained().await.unwrap();
        assert!(
            !complete(&owner, &backend).await,
            "duplicate END at {window}"
        );
        assert_eq!(rows(&owner).await, recovered);
        assert_eq!(receipt(&owner).await, durable);
        assert_eq!(fx.prompts(), prompts);
        session.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn canonical_acp_sending_unknown_crash_recovery_after_authenticated_keep_stays_held() {
    for scenario in ["hold", "lost"] {
        let fx = Fixture::with_storage(scenario, true).await;
        fx.dispatch().await.unwrap();
        if scenario == "lost" {
            fx.wait_idle().await;
        }
        let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
        let carrier = current_input_fact_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
            .await
            .unwrap();
        let attempt = carrier
            .checkpoint()
            .attempt(&fx.submission)
            .unwrap()
            .clone();
        tx.commit().await.unwrap();
        let request = KeepConversationInputRequest::from(&RetryConversationInputRequest {
            carrier_id: fx.owner.worker.clone(),
            submission_id: fx.submission.as_str().into(),
            attempt_id: attempt.attempt.attempt_id.as_str().into(),
            revision: attempt.revision.to_string(),
        });
        let kept = crate::conversation_inputs::resolution::keep(
            fx.owner.repo.as_ref(),
            fx.source.clone(),
            fx.owner.card.clone(),
            "keep-before-crash".into(),
            request,
        )
        .await
        .unwrap()
        .0;
        assert!(kept.waiting_for_settlement);
        let durable = receipt(&fx.owner).await;
        assert_eq!(
            durable.0,
            if scenario == "hold" {
                "sending"
            } else {
                "unknown"
            }
        );
        let before = rows(&fx.owner).await;
        let owner = snapshot(&fx.owner, &fx.dir, "keep-crash.db").await;
        fx.session.shutdown().await.unwrap();
        let prompts = fx.prompts();
        for _ in 0..2 {
            let owner = reopen(&owner, owner.db_url.clone()).await;
            let (backend, session, _held) = session_for(&owner, &fx.dir);
            backend.recover_retained().await.unwrap();
            assert!(!complete(&owner, &backend).await);
            let admission = crate::harness::native_admission::NativePromptAdmission::new(
                || Ok(()),
                || Box::pin(async { Ok(()) }),
            );
            assert!(matches!(
                backend
                    .settled_activity(&owner.card, &owner.thread, &admission)
                    .await
                    .unwrap(),
                SettledActivity::Held
            ));
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
            assert_eq!(receipt(&owner).await, durable);
            assert_eq!(rows(&owner).await, before);
            assert_eq!(fx.prompts(), prompts);
            session.shutdown().await.unwrap();
        }
    }
}

#[tokio::test]
async fn c4_acp_owner_retry_preserves_the_normal_drivers_known_receipt() {
    let fx = Fixture::new("reply").await;
    fail_terminal_outcome(fx.owner.repo.as_ref()).await;
    fx.dispatch().await.unwrap();
    fx.wait_driver_finished().await;
    let pool = fx.owner.repo.db();
    let claim =
        calm_truth::db::sqlite::provider_execution::load(pool.read_pool(), &fx.owner.worker)
            .await
            .unwrap()
            .expect("owner write failure keeps its claim");
    assert_eq!(receipt(&fx.owner).await.0, "completed");
    sqlx::query("DROP TRIGGER fail_outcome")
        .execute(pool.writer_pool())
        .await
        .unwrap();
    fx.session
        .host()
        .provider_execution
        .finish(
            pool,
            fx.session.host().execution.engine(),
            &fx.owner.worker,
            &claim,
            None,
        )
        .await
        .unwrap();
    assert!(
        calm_truth::db::sqlite::provider_execution::load(pool.read_pool(), &fx.owner.worker)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        fx.complete().await.is_some(),
        "retry must preserve facts observed by the normally joined driver"
    );
    fx.session.shutdown().await.unwrap();
}
