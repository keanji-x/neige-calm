//! Recovery uses the real start adapter's transfer, then controls the successor before issuance.
use super::completed_commit_tests::Fixture;
use super::input_inspection::InputInspection;
use super::*;
use crate::operation::planner_harness_start_adapter::{
    HarnessProfile, PlannerHarnessStartAdapter, PlannerHarnessStartOperationPayload,
};
use crate::operation::{Operation, Phase, ProviderAdapter};
use crate::plugin_host::{PluginHost, PluginRegistry};
use calm_types::conversation_receipt::InitialInputUnavailableReason as Reason;
use serde_json::json;

/// Consume a real normalized provider completion through the production reducer.
pub(super) async fn confirm_projection(
    harness: &PlannerHarness,
    daemon: &crate::shared_codex_appserver::SharedCodexAppServer,
    client_id: &QueueEntryId,
) {
    let inner = &harness.inner;
    let before = inner
        .repo
        .harness_item_list_by_card(inner.card_id.as_str(), 0, 500, false)
        .await
        .unwrap()
        .into_iter()
        .find(|row| row.item_uuid.as_deref() == Some(client_id.as_str()))
        .unwrap();
    assert!(
        before.turn_id.is_none(),
        "the real projection is initially provisional"
    );
    let turn_id = harness.snapshot().await.unwrap().last_turn_id.unwrap();
    let thread_id = inner.thread_id.read().await.clone().unwrap();
    let mut events = inner.backend.subscribe_events();
    daemon.emit_notification_for_test(crate::codex_appserver::Notification::Item {
        method: "item/completed".into(),
        params: json!({"threadId":thread_id,"turn":{"id":turn_id},"item":{
            "id":format!("provider-item-{client_id}-{turn_id}"),"clientId":client_id.as_str(),"type":"userMessage",
            "content":[{"type":"text","text":"provider confirms the original projection"}]
        }}),
    });
    let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        &event.kind,
        PlannerEventKind::Item {
            phase: ItemPhase::Completed,
            ..
        }
    ));
    on_notification(inner, &inner.live_claim.writer(), event)
        .await
        .unwrap();
    let after = inner
        .repo
        .harness_item_list_by_card(inner.card_id.as_str(), 0, 500, false)
        .await
        .unwrap()
        .into_iter()
        .find(|row| row.id == before.id)
        .unwrap();
    assert_eq!(after.turn_id.as_deref(), Some(turn_id.as_str()));
    assert_eq!(
        after.input_segments, before.input_segments,
        "the provider binds the existing exact entry segments"
    );
}

fn unstarted(fx: &Fixture, id: String, snapshot: HarnessSnapshot) -> PlannerHarness {
    let backend = crate::harness::backend::PlannerBackend::codex(
        fx.daemon.clone(),
        fx.harness.inner.card_id.as_str(),
        fx.harness.inner.track_id.as_str(),
        &id,
    )
    .unwrap();
    let (harness, _ingress) = PlannerHarness::run_unstarted_for_test(
        PlannerHarnessParams {
            worker_session_id: id,
            track_id: fx.harness.inner.track_id.clone(),
            card_id: fx.harness.inner.card_id.clone(),
            thread_id: snapshot.last_thread_id.clone(),
            repo: fx.repo.clone(),
            events: fx.harness.inner.events.clone(),
            card_role_cache: fx.harness.inner.card_role_cache.clone(),
            track_area_cache: fx.harness.inner.track_area_cache.clone(),
            backend,
            live_replies: LiveReplies::for_test(),
            config: HarnessConfig::default(),
            snapshot: snapshot.try_into().unwrap(),
        },
        8,
    );
    harness
}

async fn transfer_before_issuance(fx: &Fixture) -> PlannerHarness {
    fx.harness.shutdown().await.unwrap();
    let inner = &fx.harness.inner;
    let plugin = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        fx.repo.clone(),
        std::path::PathBuf::new(),
        std::env::temp_dir().join("receipt-recovery-plugins"),
        Vec::new(),
        inner.events.clone(),
        crate::state::WriteContext::new(
            inner.card_role_cache.clone(),
            inner.track_area_cache.clone(),
        ),
    ));
    let adapter = PlannerHarnessStartAdapter::new(
        fx.repo.clone(),
        fx.daemon.clone(),
        fx.daemon.thread_seals().clone(),
        crate::harness::HarnessRegistry::new(),
        plugin,
        inner.card_role_cache.clone(),
        inner.track_area_cache.clone(),
        None,
        Arc::new(crate::claude_planner::config::ClaudePlannerHost::unconfigured_scratch().unwrap()),
        Arc::new(crate::acp_planner::config::AcpPlannerHost::unconfigured_scratch().unwrap()),
    );
    let payload = serde_json::to_value(PlannerHarnessStartOperationPayload {
        actor: ActorId::User,
        track_id: inner.track_id.to_string(),
        planner_card_id: inner.card_id.clone(),
        report_card_id: None,
        sort: None,
        cwd: "/tmp".into(),
        goal: None,
        reset_harness_items: false,
        force_new_thread: true,
        profile: HarnessProfile::Planner,
        create_card: None,
        first_message: None,
        create_request_sha256: None,
        opening_briefing: None,
    })
    .unwrap();
    let op = Operation {
        id: "restart-inspection".into(),
        operation_key: "restart-inspection".into(),
        kind: adapter.kind().into(),
        idempotency_key: None,
        payload_hash: String::new(),
        target_type: "card".into(),
        target_id: Some(inner.card_id.to_string()),
        target: json!({}),
        payload: payload.clone(),
        tx_output: None,
        phase: Phase::Pending,
        phase_detail: None,
        attempt: 1,
        last_error: None,
        compensation_state: None,
        lease_owner: None,
        lease_until_ms: None,
        spawn_artifacts: None,
        parked_at_ms: None,
        parked_deadline_ms: None,
    };
    let mut tx = fx.repo.pool().begin().await.unwrap();
    let output = adapter.prepare_tx(&mut tx, &payload, &op).await.unwrap();
    tx.commit().await.unwrap();
    let id = output.data["runtime_id"].as_str().unwrap().to_string();
    assert_ne!(id, inner.worker_session_id);
    let snapshot = HarnessSnapshot::from_value_strict(output.data["snapshot"].clone());
    assert!(snapshot.projection_client_id.is_none());
    unstarted(fx, id, snapshot)
}

#[tokio::test]
async fn initial_input_inspection_recovery_does_not_certify_a_predecessors_failed_projection() {
    let fx = Fixture::new().await;
    let entry = QueueEntry::user_message("failed before recovery".into(), None, vec![]);
    let id = entry.id().unwrap().clone();
    fx.enqueue(vec![entry]).await;
    sqlx::query(concat!(
        "CREATE TRIGGER refuse_recovery_cleanup BEFORE DELETE ON ",
        "harness_items BEGIN SELECT RAISE(ABORT, 'forced cleanup failure'); END"
    ))
    .execute(fx.repo.pool())
    .await
    .unwrap();
    fx.daemon.fail_turn_start_for_test();
    fx.issue().await;
    assert_eq!(
        fx.harness
            .snapshot()
            .await
            .unwrap()
            .projection_client_id
            .as_ref(),
        Some(&id)
    );
    let successor = transfer_before_issuance(&fx).await;
    assert!(matches!(
        successor.inspect_input(id.clone()).await.unwrap(),
        InputInspection::Queued { .. }
    ));
    successor
        .mutate_pending_entry(
            QueueMutation::Delete {
                entry_id: id.clone(),
                if_entry_rev: 0,
            },
            ActorId::User,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        successor.inspect_input(id).await.unwrap(),
        InputInspection::Unavailable(Reason::RuntimeUnavailable)
    );
}

async fn rowless_steer(fx: &Fixture) -> QueueEntryId {
    let mut events = fx.harness.inner.backend.subscribe_events();
    fx.enqueue(vec![QueueEntry::user_message(
        "start a running turn".into(),
        None,
        vec![],
    )])
    .await;
    fx.issue().await;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap();
        let started = matches!(&event.kind, PlannerEventKind::TurnStarted { .. });
        on_notification(
            &fx.harness.inner,
            &fx.harness.inner.live_claim.writer(),
            event,
        )
        .await
        .unwrap();
        if started {
            break;
        }
    }
    let entry = QueueEntry::user_message("steer without a transcript row".into(), None, vec![]);
    let id = entry.id().unwrap().clone();
    fx.enqueue(vec![entry]).await;
    sqlx::query(concat!(
        "CREATE TRIGGER refuse_recovery_steer BEFORE INSERT ON ",
        "harness_items BEGIN SELECT RAISE(ABORT, 'forced steer write failure'); END"
    ))
    .execute(fx.repo.pool())
    .await
    .unwrap();
    fx.harness
        .steer_pending_entry(id.clone(), 0, ActorId::User)
        .await
        .unwrap()
        .unwrap();
    assert!(
        fx.harness
            .inner
            .steered_into_running_turn
            .lock()
            .await
            .iter()
            .any(|steered| steered.entry.id() == Some(&id) && steered.row_id.is_none())
    );
    assert_eq!(
        fx.harness.inspect_input(id.clone()).await.unwrap(),
        InputInspection::Unavailable(Reason::RuntimeUnavailable)
    );
    id
}

#[tokio::test]
async fn initial_input_inspection_recovery_does_not_lose_a_predecessors_rowless_steer() {
    let fx = Fixture::new().await;
    let id = rowless_steer(&fx).await;
    let successor = transfer_before_issuance(&fx).await;
    assert_eq!(
        successor.inspect_input(id).await.unwrap(),
        InputInspection::Unavailable(Reason::RuntimeUnavailable)
    );
}

#[tokio::test]
async fn initial_input_inspection_recovery_does_not_lose_a_same_runtime_rowless_steer() {
    let fx = Fixture::new().await;
    let id = rowless_steer(&fx).await;
    fx.harness.shutdown().await.unwrap();
    let recovered = unstarted(
        &fx,
        fx.harness.inner.worker_session_id.clone(),
        fx.stored().await,
    );
    assert!(
        recovered
            .inner
            .steered_into_running_turn
            .lock()
            .await
            .is_empty()
    );
    assert_eq!(
        recovered.inspect_input(id).await.unwrap(),
        InputInspection::Unavailable(Reason::RuntimeUnavailable)
    );
}

#[tokio::test]
async fn initial_input_inspection_recovery_rejects_same_runtime_provisional_residue() {
    let fx = Fixture::new().await;
    let entry =
        QueueEntry::user_message("provider has not confirmed this row".into(), None, vec![]);
    let id = entry.id().unwrap().clone();
    fx.enqueue(vec![entry]).await;
    fx.issue().await;
    assert_eq!(
        fx.harness.inspect_input(id.clone()).await.unwrap(),
        InputInspection::Unavailable(Reason::RuntimeUnavailable)
    );
    fx.harness.shutdown().await.unwrap();
    let recovered = unstarted(
        &fx,
        fx.harness.inner.worker_session_id.clone(),
        fx.stored().await,
    );
    assert_eq!(
        recovered.inspect_input(id).await.unwrap(),
        InputInspection::Unavailable(Reason::RuntimeUnavailable)
    );
}

#[tokio::test]
async fn initial_input_inspection_recovery_keeps_a_predecessors_real_provider_confirmation() {
    let fx = Fixture::new().await;
    let entry = QueueEntry::user_message("confirmed before transfer".into(), None, vec![]);
    let id = entry.id().unwrap().clone();
    fx.enqueue(vec![entry]).await;
    fx.issue().await;
    confirm_projection(&fx.harness, &fx.daemon, &id).await;
    let confirmed = fx.harness.inspect_input(id.clone()).await.unwrap();
    assert!(matches!(confirmed, InputInspection::Projected { .. }));
    let successor = transfer_before_issuance(&fx).await;
    assert_eq!(successor.inspect_input(id).await.unwrap(), confirmed);
}

#[tokio::test]
async fn initial_input_inspection_recovery_keeps_a_successors_real_provider_confirmation() {
    let fx = Fixture::new().await;
    let entry = QueueEntry::user_message("confirmed after transfer".into(), None, vec![]);
    let id = entry.id().unwrap().clone();
    fx.enqueue(vec![entry]).await;
    let successor = transfer_before_issuance(&fx).await;
    let thread = fx
        .daemon
        .thread_start_mint_for_card(
            fx.harness.inner.card_id.as_str(),
            crate::shared_codex_appserver::SharedThreadStartParams {
                cwd: "/tmp".into(),
                approval_policy: "never".into(),
                sandbox_mode: "workspace-write".into(),
                developer_instructions: None,
                config: crate::shared_codex_appserver::ThreadConfig::NoMcp,
            },
        )
        .await
        .unwrap();
    *successor.inner.thread_id.write().await = Some(thread);
    *successor.inner.state.lock().await = HarnessState::Idle;
    maybe_issue_turn(&successor.inner).await.unwrap();
    assert!(
        successor
            .snapshot()
            .await
            .unwrap()
            .pending_entries()
            .is_empty()
    );
    assert_eq!(
        successor.inspect_input(id.clone()).await.unwrap(),
        InputInspection::Unavailable(Reason::RuntimeUnavailable)
    );
    confirm_projection(&successor, &fx.daemon, &id).await;
    assert!(matches!(
        successor.inspect_input(id).await.unwrap(),
        InputInspection::Projected { .. }
    ));
}
