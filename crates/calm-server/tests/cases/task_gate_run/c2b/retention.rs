//! Retention through the real driver and both recovery entries.
use super::*;

async fn spawn_started(fx: &Fx, id: &str) {
    sqlx::query("UPDATE operations SET phase='spawn_started',parked_at_ms=NULL,parked_deadline_ms=NULL WHERE id=?")
        .bind(id).execute(&fx.pool()).await.unwrap();
}
async fn errors(fx: &Fx) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM retention_errors")
        .fetch_one(&fx.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn c2b_malformed_record_is_retained_with_backoff() {
    for parked in [false, true] {
        for recovering in [false, true] {
            let (fx, a, _resume) = held("malformed").await;
            let before = op(&fx, &a.task.id).await;
            if !parked {
                spawn_started(&fx, &before.id).await;
            }
            let bad = json!({"version":1,"execution_id":false});
            rewrite(&fx, &before.id, &bad).await;
            sqlx::query("CREATE TABLE retention_errors (message TEXT)")
                .execute(&fx.pool())
                .await
                .unwrap();
            sqlx::query("CREATE TRIGGER count_retention AFTER UPDATE OF last_error ON operations WHEN NEW.last_error IS \
         NOT NULL BEGIN INSERT INTO retention_errors VALUES (NEW.last_error); END")
                .execute(&fx.pool()).await.unwrap();
            if recovering {
                boot(&fx).await;
            } else if parked {
                fx.runtime.sweep_parked().await.unwrap();
            } else {
                fx.runtime.drive().await.unwrap();
            }
            let retained = op(&fx, &a.task.id).await;
            assert_eq!(
                retained.phase.tag(),
                if parked {
                    PhaseTag::Parked
                } else {
                    PhaseTag::SpawnStarted
                }
            );
            assert_eq!(record(&retained), bad);
            assert!(
                retained
                    .last_error
                    .as_deref()
                    .unwrap()
                    .contains("owned execution retained")
            );
            assert!(retained.lease_owner.is_some());
            assert!(retained.lease_until_ms.unwrap() > calm_server::model::now_ms());
            let tries = if recovering && parked { 2 } else { 1 };
            assert_eq!(errors(&fx).await, tries);
            fx.runtime.drive().await.unwrap();
            fx.runtime.sweep_parked().await.unwrap();
            tokio::time::sleep(Duration::from_millis(150)).await;
            assert_eq!(errors(&fx).await, tries, "lease prevents hot retries");
            assert_eq!(op(&fx, &a.task.id).await.lease_owner, retained.lease_owner);
            expire(&fx, &before.id).await;
            if parked {
                fx.runtime.sweep_parked().await.unwrap();
            } else {
                fx.runtime.drive().await.unwrap();
            }
            assert_eq!(errors(&fx).await, tries + 1, "one retry after expiry");
            assert_eq!(execution_count(&fx, &before.id).await, 1);
            let settlement: Option<String> = sqlx::query_scalar(
                "SELECT settlement_json FROM workspace_executions WHERE execution_id=?",
            )
            .bind(record(&before)["execution_id"].as_str().unwrap())
            .fetch_one(&fx.pool())
            .await
            .unwrap();
            assert!(settlement.is_none(), "no termination of the held execution");
        }
    }
}

#[tokio::test]
async fn c2b_recorded_op_never_enters_compensation() {
    for plan_error in [false, true] {
        let (fx, a, _resume) = held("no-compensation").await;
        let before = op(&fx, &a.task.id).await;
        spawn_started(&fx, &before.id).await;
        if plan_error {
            // FrozenRun decoding fails in spawn_side_effect and then plan_compensation.
            sqlx::query("UPDATE operations SET tx_output_json=json_set(tx_output_json,'$.data.task_id',json('false')) WHERE id=?")
                .bind(&before.id).execute(&fx.pool()).await.unwrap();
        } else {
            // FrozenRun remains valid: spawn's owned-record decoder alone fails;
            // plan_compensation succeeds and set_compensating must retain the row.
            rewrite(&fx, &before.id, &json!({"version":1,"execution_id":false})).await;
        }
        let input = op(&fx, &a.task.id).await;
        let plan = TaskGateRunAdapter::new(fx.boot.ctx.gate_logs_dir.clone())
            .plan_compensation(
                PhaseTag::SpawnStarted,
                "spawn decode failed",
                input.tx_output.as_ref().unwrap(),
                &input,
            )
            .await;
        assert_eq!(
            plan.is_err(),
            plan_error,
            "exercise both driver error branches"
        );
        fx.runtime.drive().await.unwrap();
        let after = op(&fx, &a.task.id).await;
        assert_eq!(after.phase.tag(), PhaseTag::SpawnStarted);
        assert!(after.compensation_state.is_none());
        assert!(
            after
                .last_error
                .as_deref()
                .unwrap()
                .contains("owned execution retained")
        );
        assert!(after.lease_owner.is_some());
        assert_eq!(execution_count(&fx, &before.id).await, 1);
        assert_eq!(
            record(&after)["execution_id"],
            if plan_error {
                record(&before)["execution_id"].clone()
            } else {
                json!(false)
            }
        );
    }
}

#[tokio::test]
async fn c2b_park_write_failure_retains_owned_execution() {
    for recovering in [false, true] {
        let fx = world(Duration::from_millis(100)).await;
        let marker = outside(&fx, "park-failure-step");
        let a = running(
            &fx,
            "park-failure",
            json!([{"name":"unexpected","cmd":format!("touch '{}'",marker.display())}]),
        )
        .await;
        sqlx::query(
            "CREATE TRIGGER reject_gate_park BEFORE UPDATE OF phase ON operations WHEN \
         NEW.kind='task-gate-run' AND NEW.phase='parked' BEGIN SELECT RAISE(ABORT,'injected park \
         failure'); END",
        )
        .execute(&fx.pool())
        .await
        .unwrap();
        if recovering {
            calm_server::test_seams::admit_gate_run_for_test(
                &fx.db(),
                &a.task.id,
                &a.worker.card_id,
                fx.track(),
                calm_server::decision_sink::CommitMessage::parse("test: boot failed park").unwrap(),
            )
            .await
            .unwrap();
            boot(&fx).await;
        } else {
            gate_run(&fx, &a, Some("test: failed park retains execution"))
                .await
                .unwrap();
        }
        wait_until(async || op(&fx, &a.task.id).await.last_error.is_some()).await;
        let before = op(&fx, &a.task.id).await;
        assert_eq!(before.phase.tag(), PhaseTag::SpawnStarted);
        assert_eq!(record(&before)["release"], "Held");
        assert!(
            before
                .last_error
                .as_deref()
                .unwrap()
                .contains("owned execution retained")
        );
        assert_eq!(execution_count(&fx, &before.id).await, 1);
        assert!(!marker.exists());
        sqlx::query("DROP TRIGGER reject_gate_park")
            .execute(&fx.pool())
            .await
            .unwrap();
        expire(&fx, &before.id).await;
        fx.runtime.drive().await.unwrap();
        settled_infra(&fx, &a).await;
        assert_eq!(record(&op(&fx, &a.task.id).await)["release"], "Held");
        assert!(!marker.exists());
        // Authoritative settlement clears occupancy: the next admission succeeds.
        gate_run(&fx, &a, None).await.unwrap();
    }
}

#[tokio::test]
async fn c2b_owned_park_requires_decodable_record() {
    // task-verify remains native until C2b.2; this slice owns only task-gate-run.
    for value in [
        json!({"version":1,"execution_id":false}),
        json!({"version":2,"execution_id":"invalid","release":"Held","decision":"Observing"}),
        Value::Null,
    ] {
        let (fx, a, _resume) = held("decode-before-park").await;
        let before = op(&fx, &a.task.id).await;
        spawn_started(&fx, &before.id).await;
        rewrite(&fx, &before.id, &value).await;
        fx.runtime.drive().await.unwrap();
        let after = op(&fx, &a.task.id).await;
        assert_eq!(after.phase.tag(), PhaseTag::SpawnStarted);
        assert!(
            after
                .last_error
                .as_deref()
                .unwrap()
                .contains("owned execution retained")
        );
        assert_eq!(execution_count(&fx, &before.id).await, 1);
        assert!(after.parked_at_ms.is_none());
        assert_eq!(record(&after), value);
    }
}
