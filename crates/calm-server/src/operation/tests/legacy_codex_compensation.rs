use super::*;
use crate::db::sqlite::{session_mark_superseded_runtime_tx, session_start_runtime_tx};
use crate::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionProjectionRepo,
    WorkerSessionState,
};

pub(super) fn adapter(
    harness: &LegacyCompensationHarness,
) -> crate::operation::codex_adapter::CodexAdapter {
    let repo: Arc<dyn crate::db::Repo> = harness.repo.clone();
    crate::operation::codex_adapter::CodexAdapter::new(
        harness.route_repo.clone(),
        Arc::new(crate::state::CodexClient::new_stub()),
        crate::shared_codex_appserver::SharedCodexAppServer::new_stub(repo.clone()),
        Arc::new(
            crate::pending_codex_threads::PendingThreadStartRegistry::new(
                repo,
                harness.events.clone(),
            ),
        ),
        Arc::new(Mutex::new(())),
        crate::card_role_cache::CardRoleCache::new(),
        crate::track_area_cache::TrackAreaCache::new(),
    )
}

async fn harness() -> LegacyCompensationHarness {
    legacy_compensation_harness(
        "codex",
        WorkerSessionKind::CodexCard,
        Some(AgentProvider::Codex),
    )
    .await
}

fn failed_step(card_id: &str) -> CompensationStep {
    CompensationStep::new(
        "runtime_set_status_failed_for_card",
        json!({"card_id":card_id}),
    )
}

#[tokio::test]
async fn legacy_compensation_never_selects_a_successor_from_a_reused_terminal() {
    for reuse_terminal in [false, true] {
        let harness = harness().await;
        let adapter = adapter(&harness);
        let successor = new_id();
        let original = harness.worker_session_id.clone();
        let card = harness.card_id.clone();
        let next = successor.clone();
        let terminal = if reuse_terminal {
            Some(harness.output.output_string("terminal_id", "test").unwrap())
        } else {
            None
        };
        crate::db::write_in_tx_typed(harness.repo.as_ref(), move |tx| {
            Box::pin(async move {
                session_mark_superseded_runtime_tx(tx, &original).await?;
                session_start_runtime_tx(
                    tx,
                    WorkerSessionInit {
                        id: next,
                        card_id: card,
                        kind: WorkerSessionKind::CodexCard,
                        agent_provider: Some(AgentProvider::Codex),
                        status: WorkerSessionState::Running,
                        terminal_run_id: terminal,
                        thread_id: Some("successor-thread".into()),
                        session_id: None,
                        active_turn_id: None,
                        handle_state_json: None,
                        spawn_op_id: None,
                        now_ms: now_ms(),
                    },
                )
                .await?;
                Ok(())
            })
        })
        .await
        .unwrap();
        let before = harness
            .repo
            .session_projection_by_id(&successor)
            .await
            .unwrap();
        let result = adapter
            .compensate_step(
                &failed_step(&harness.card_id),
                &harness.output,
                &harness.op,
                &harness.spawn_ctx,
            )
            .await;
        if reuse_terminal {
            assert!(matches!(result, Err(CalmError::Conflict(_))));
        } else {
            result.unwrap();
        }
        let interrupt = CompensationStep::new(
            "interrupt_shared_codex_turn",
            json!({"card_id":harness.card_id}),
        );
        let result = adapter
            .compensate_step(&interrupt, &harness.output, &harness.op, &harness.spawn_ctx)
            .await;
        if reuse_terminal {
            assert!(matches!(result, Err(CalmError::Conflict(_))));
        } else {
            result.unwrap();
        }
        assert_eq!(
            harness
                .repo
                .session_projection_by_id(&successor)
                .await
                .unwrap(),
            before
        );
        assert_eq!(
            harness
                .repo
                .session_projection_state_by_id(&harness.worker_session_id)
                .await
                .unwrap(),
            Some(WorkerSessionState::Superseded)
        );
    }
}

#[tokio::test]
async fn legacy_compensation_refuses_missing_or_malformed_identity_fields() {
    let harness = harness().await;
    let adapter = adapter(&harness);
    for (field, value) in [
        ("terminal_id", Value::Null),
        ("terminal_id", json!("")),
        ("terminal_id", json!("missing-terminal")),
        ("card_id", json!("other-card")),
        ("runtime_id", Value::Null),
        ("runtime_id", json!(42)),
        ("workspace_environment_thread", json!(true)),
    ] {
        let mut output = harness.output.clone();
        output.data[field] = value;
        for step in [
            failed_step(&harness.card_id),
            CompensationStep::new(
                "interrupt_shared_codex_turn",
                json!({"card_id":harness.card_id}),
            ),
        ] {
            assert!(
                adapter
                    .compensate_step(&step, &output, &harness.op, &harness.spawn_ctx)
                    .await
                    .is_err()
            );
        }
    }
    let mut output = harness.output.clone();
    output.target_id = Some("other-card".into());
    assert!(
        adapter
            .compensate_step(
                &failed_step(&harness.card_id),
                &output,
                &harness.op,
                &harness.spawn_ctx
            )
            .await
            .is_err()
    );
    assert_eq!(
        harness
            .repo
            .session_projection_state_by_id(&harness.worker_session_id)
            .await
            .unwrap(),
        Some(WorkerSessionState::Running)
    );
}

#[tokio::test]
async fn legacy_compensation_plan_uses_the_original_terminal_binding() {
    let harness = harness().await;
    let adapter = adapter(&harness);
    let state = adapter
        .plan_compensation(
            PhaseTag::SpawnStarted,
            "failed",
            &harness.output,
            &harness.op,
        )
        .await
        .unwrap();
    for step in state
        .steps
        .iter()
        .filter(|step| step.op != "reap_terminal_pty")
    {
        adapter
            .compensate_step(step, &harness.output, &harness.op, &harness.spawn_ctx)
            .await
            .unwrap();
    }
    assert_eq!(
        harness
            .repo
            .session_projection_state_by_id(&harness.worker_session_id)
            .await
            .unwrap(),
        Some(WorkerSessionState::Failed)
    );
}
