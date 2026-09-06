//! Recovery requires durable evidence that preparation never committed.
use crate::mcp_track_report::{boot, call_tool, planner_identity};
use crate::task_recovery::{current, declaration, declare, finish, recovery_args};
use calm_server::task_recovery::task_recovery_view;

#[tokio::test]
async fn task_recovery_retained_operation_evidence_blocks_unbound_predecessor() {
    let cases = [
        (
            "committed",
            "terminal-worker",
            "tx_committed",
            None,
            None,
            Some("{}"),
            None,
            None,
        ),
        (
            "failed-after-spawn",
            "terminal-worker",
            "failed",
            Some(r#"{"from_phase":"spawn_started"}"#),
            None,
            Some("{}"),
            None,
            None,
        ),
        (
            "deleted-worker-card",
            "terminal-worker",
            "succeeded",
            None,
            Some("removed-card"),
            Some("{}"),
            None,
            None,
        ),
        (
            "unknown-failure-phase",
            "terminal-worker",
            "failed",
            None,
            None,
            None,
            None,
            None,
        ),
        (
            "contradictory-artifacts",
            "terminal-worker",
            "failed",
            Some(r#"{"from_phase":"pending"}"#),
            None,
            None,
            Some("{}"),
            None,
        ),
        (
            "compensation-started",
            "terminal-worker",
            "failed",
            Some(r#"{"from_phase":"pending"}"#),
            None,
            None,
            None,
            Some("{}"),
        ),
        (
            "foreign-adapter",
            "terminal-create",
            "pending",
            None,
            None,
            None,
            None,
            None,
        ),
    ];
    for (case, kind, phase, detail, target, output, artifacts, compensation) in cases {
        let boot = boot().await;
        declare(&boot, declaration("b", &[])).await;
        let b = current(&boot, "b").await;
        finish(&boot, &b, false).await;
        let pool = boot.repo.sqlite_pool().unwrap();
        // No live card/session row to consult: the permanent operation record
        // must continue to deny recovery after normal UI/history cleanup.
        sqlx::query("INSERT INTO operations(id,operation_key,kind,idempotency_key,payload_hash,target_type,target_id,target_json,payload_json,phase,phase_detail_json,tx_output_json,spawn_artifacts_json,compensation_state,created_at_ms,updated_at_ms) VALUES(?1,?1,?2,?3,'h',?10,?4,'{}','{}',?5,?6,?7,?8,?9,1,1)")
            .bind(case).bind(kind).bind(&b.id).bind(target.unwrap_or(boot.track_id.as_str())).bind(phase).bind(detail).bind(output).bind(artifacts).bind(compensation).bind(if target.is_some() { "card" } else { "track" })
            .execute(&pool).await.unwrap();
        let error = call_tool(
            &boot,
            "calm.plan.recover",
            planner_identity(&boot),
            recovery_args(&b, case),
        )
        .await
        .expect_err(case);
        assert_eq!(error.code, -32409, "{case}: {error:?}");
        assert!(
            error.message.contains("uncertain external effects"),
            "{case}: {error:?}"
        );
        let view = task_recovery_view(
            boot.repo.as_ref(),
            boot.track_id.as_str(),
            "b",
            calm_server::ids::ActorId::User,
            calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
        )
        .await
        .unwrap();
        assert!(!view.recovery.allowed, "{case}");
        assert_eq!(view.current.as_ref().unwrap().attempt_id, b.id);
        assert_eq!(view.attempts.len(), 1);
        assert_eq!(view.recovery.code, "predecessor_not_quiescent");
        assert!(view.recovery.reason.contains("before worker preparation"));
    }
}
