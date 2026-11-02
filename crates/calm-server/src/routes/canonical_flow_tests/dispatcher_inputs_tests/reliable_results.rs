//! Lost live notification and duplicate catch-up must reach the actual provider once.
use super::*;
use crate::event::Event;
use calm_truth::db::RepoEventWrite as _;

#[tokio::test]
async fn forge_failure_result_reconciles_without_broadcast_and_is_accepted_once() {
    let mut flow = Flow::running().await;
    flow.state.dispatcher.abort_event_listener_for_test();
    let operation = broken_forge(&flow, "lost-live-failure").await;
    assert!(
        flow.state
            .operation_runtime
            .operation_result(&operation)
            .await
            .unwrap()
            .is_some()
    );
    let row = flow
        .fixture
        .repo
        .events_for_track(&flow.track, &["track.wake_requested"], None)
        .await
        .unwrap()
        .into_iter()
        .find(|row| {
            matches!(&row.event,
            Event::TrackWakeRequested { key, .. } if key == &operation)
        })
        .expect("the real Forge failure transaction must persist its wake");
    let id = row.id;
    let Event::TrackWakeRequested { text, .. } = row.event else {
        unreachable!()
    };
    for _ in 0..3 {
        flow.state.dispatcher.reconcile_tick_for_test().await;
    }
    let snapshot = flow.fixture.carrier(&flow.track).await.2;
    assert_eq!(snapshot.canonical_inputs.pending.len(), 1);
    assert_eq!(snapshot.canonical_inputs.pending[0].envelope_id(), Some(id));
    assert!(matches!(
        snapshot.canonical_inputs.pending[0],
        PendingInput::System { .. }
    ));
    flow.end(TURN).await;
    let call = flow
        .next()
        .await
        .expect("persisted failure must wake the original Planner without a user message");
    let input = call["params"]["input"].to_string();
    assert_eq!(input.matches(text.as_str()).count(), 1, "{input}");
    flow.accept(&call, "failure-result-turn").await;
    flow.end("failure-result-turn").await;
    flow.fixture
        .wait_phase(&flow.track, HarnessPhaseTag::TurnCompleted)
        .await;
    flow.state.dispatcher.reconcile_tick_for_test().await;
    assert!(
        flow.fixture
            .carrier(&flow.track)
            .await
            .2
            .canonical_inputs
            .pending
            .is_empty()
    );
    flow.close().await;
}

async fn broken_forge(flow: &Flow, key: &str) -> String {
    use crate::operation::{OperationKey, forge_action_adapter::FORGE_ACTION_KIND};
    let blocker = flow.fixture.root.path().join(key);
    std::fs::write(&blocker, "not a result directory").unwrap();
    let payload = json!({
        "track_id":flow.track,"card_id":flow.card,"subject":{"pr_number":42},
        "argv":["/bin/false"],"idem_key":key,
        "event_spec":{"event_kind":"forge.pr.merged","fields":{
            "head_sha":{"json_field":{"path":"/headRefOid"}},
            "merge_sha":{"json_field":{"path":"/mergeCommit/oid"}}
        }},
        "context":{},"probe":null,"cwd_lease":flow.fixture.root.path(),
        "result_path":blocker.join("result.json"),"deadline_ms":crate::model::now_ms()+60_000
    });
    flow.state
        .operation_runtime
        .submit(
            FORGE_ACTION_KIND,
            OperationKey {
                operation_key: crate::model::new_id(),
                idempotency_key: Some(key.into()),
                payload_hash: key.into(),
            },
            payload,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn preownership_notification_write_failure_recovers_on_periodic_reconcile() {
    let mut flow = Flow::running().await;
    flow.state.dispatcher.abort_event_listener_for_test();
    let guard = crate::per_card_lock::lock_key(
        flow.state.operation_runtime.track_locks_for_test(),
        &flow.track,
    )
    .await;
    (flow.fixture.repo.db())
        .writer()
        .execute(sqlx::query(
            "CREATE TRIGGER reject_failure_wake BEFORE INSERT ON events \
         WHEN NEW.kind='track.wake_requested' BEGIN \
         SELECT RAISE(ABORT, 'injected failure wake rejection'); END",
        ))
        .await
        .unwrap();
    let id = broken_forge(&flow, "retry-notification").await;
    assert!(
        flow.state
            .operation_runtime
            .operation_result(&id)
            .await
            .unwrap()
            .is_none()
    );
    let pause = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test("operation-busy-retry-failed", &id, pause.clone());
    drop(guard);
    tokio::time::timeout(Duration::from_secs(10), pause.entered.notified())
        .await
        .unwrap();
    assert!(
        flow.state
            .operation_runtime
            .operation_result(&id)
            .await
            .unwrap()
            .is_none(),
        "event rejection must roll back the terminal result"
    );
    (flow.fixture.repo.db())
        .writer()
        .execute(sqlx::query("DROP TRIGGER reject_failure_wake"))
        .await
        .unwrap();
    // Simulate lease time elapsing. The production tick must discover and drive
    // the retained operation; no wait() or explicit operation drive is called.
    (flow.fixture.repo.db())
        .writer()
        .execute(sqlx::query("UPDATE operations SET lease_until_ms=0 WHERE id=?1").bind(&id))
        .await
        .unwrap();
    pause.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            flow.state.dispatcher.reconcile_tick_for_test().await;
            if flow
                .state
                .operation_runtime
                .operation_result(&id)
                .await
                .unwrap()
                .is_some()
                && flow
                    .fixture
                    .carrier(&flow.track)
                    .await
                    .2
                    .canonical_inputs
                    .pending
                    .len()
                    == 1
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("periodic recovery must finish and admit its result without a new request");
    let settled = flow
        .state
        .operation_runtime
        .operation_result(&id)
        .await
        .unwrap()
        .is_some();
    let pending = flow
        .fixture
        .carrier(&flow.track)
        .await
        .2
        .canonical_inputs
        .pending
        .len();
    flow.end(TURN).await;
    let call = flow.next().await;
    if let Some(call) = &call {
        assert!(call["params"]["input"].to_string().contains(&id));
        flow.accept(call, "retried-failure-turn").await;
        flow.end("retried-failure-turn").await;
    }
    flow.fixture
        .wait_phase(&flow.track, HarnessPhaseTag::TurnCompleted)
        .await;
    flow.close().await;
    assert!(
        settled,
        "the normal periodic reconcile must retry the rolled-back failure without another submit"
    );
    assert_eq!(pending, 1);
    assert!(
        call.is_some(),
        "the retried result must reach the actual Planner"
    );
}

#[tokio::test]
async fn child_bootstrap_obligation_retries_lost_wake_through_canonical_acceptance_once() {
    assert_child_obligation_delivery(false).await;
}

#[tokio::test]
async fn child_failure_notice_retries_lost_wake_through_canonical_acceptance_once() {
    assert_child_obligation_delivery(true).await;
}

async fn assert_child_obligation_delivery(failed: bool) {
    let mut flow = Flow::running().await;
    flow.state.dispatcher.abort_event_listener_for_test();
    let task_id = format!("{}:observed-child", flow.track);
    let failure_child = if failed {
        let parent = flow
            .fixture
            .repo
            .track_get(&flow.track)
            .await
            .unwrap()
            .unwrap();
        let child = flow
            .fixture
            .repo
            .track_create(crate::model::NewTrack {
                area_id: parent.area_id,
                title: "failed child".into(),
                sort: None,
                cwd: "/tmp".into(),
                template_id: None,
                plugin_scope: None,
                template_input: None,
                attach_folder: false,
                theme: crate::model::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let card = flow
            .fixture
            .repo
            .card_create(crate::model::NewCard {
                track_id: child.id.clone(),
                title: None,
                kind: "codex".into(),
                sort: None,
                payload: serde_json::json!({"planner_provider":"codex"}),
            })
            .await
            .unwrap();
        Some((child, card))
    } else {
        None
    };
    let id = task_id.clone();
    let track = flow.track.clone();
    crate::db::write_in_tx_typed(flow.fixture.repo.as_ref(),move |tx|Box::pin(async move {
        sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,spawn,created_at_ms,updated_at_ms) \
                     VALUES(?1,?2,'observed-child','codex','handoff','{}','pending',?3,1,1)")
            .bind(&id).bind(&track).bind(calm_types::task_recovery::TASK_CHILD_TRACK_ROUTE)
            .execute(&mut **tx).await?;
        calm_truth::db::sqlite::task_claim_pending_tx(tx,&id,1,&[],false).await?;
        if let Some((child,card)) = failure_child {
            sqlx::query("UPDATE tracks SET parent_track_id=?1 WHERE id=?2").bind(&track).bind(child.id.as_str()).execute(&mut **tx).await?;
            sqlx::query("UPDATE cards SET role='planner' WHERE id=?1").bind(card.id.as_str()).execute(&mut **tx).await?;
            sqlx::query("UPDATE tasks SET child_track_id=?1 WHERE id=?2").bind(child.id.as_str()).bind(&id).execute(&mut **tx).await?;
            calm_truth::db::sqlite::task_mark_sub_track_running_tx(tx,&id,crate::model::now_ms()).await?;
            let session=format!("{id}:planner");
            calm_truth::db::sqlite::session_start_runtime_tx(tx,crate::session_projection_repo::WorkerSessionInit {
                id:session.clone(),card_id:card.id.to_string(),
                kind:crate::session_projection_repo::WorkerSessionKind::SharedPlanner,
                agent_provider:Some(crate::session_projection_repo::AgentProvider::Codex),
                status:calm_types::worker::WorkerSessionState::Idle,
                terminal_run_id:None,thread_id:None,session_id:None,active_turn_id:None,
                handle_state_json:None,spawn_op_id:None,now_ms:crate::model::now_ms(),
            }).await?;
            calm_truth::db::sqlite::session_fail_if_active_runtime_tx(tx,&session).await?;
        }
        Ok(())
    })).await.unwrap();
    // Persist the actual observation without a bus listener, before its retry tick.
    flow.state
        .scheduler_for_test()
        .sweep_progress_obligations()
        .await
        .unwrap();
    let key = format!("{task_id}:child-track-bootstrap");
    let row = flow
        .fixture
        .repo
        .events_for_track(&flow.track, &["track.wake_requested"], None)
        .await
        .unwrap()
        .into_iter()
        .find(|row| matches!(&row.event,Event::TrackWakeRequested{key:found,text,..} if if failed { text.contains(&task_id) } else { found==&key }))
        .unwrap();
    let event_id = row.id;
    let Event::TrackWakeRequested { text, .. } = row.event else {
        unreachable!()
    };
    for _ in 0..3 {
        flow.state
            .dispatcher
            .progress_tick_for_test()
            .await
            .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if flow
                .fixture
                .carrier(&flow.track)
                .await
                .2
                .canonical_inputs
                .pending
                .iter()
                .any(|pending| pending.envelope_id() == Some(event_id))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        flow.fixture
            .carrier(&flow.track)
            .await
            .2
            .canonical_inputs
            .pending
            .iter()
            .filter(|pending| pending.envelope_id() == Some(event_id))
            .count(),
        1
    );
    flow.end(TURN).await;
    let call = flow
        .next()
        .await
        .expect("the original Planner receives the durable child observation");
    assert_eq!(
        call["params"]["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|input| input["text"].as_str())
            .map(|input| input.matches(text.trim_end()).count())
            .sum::<usize>(),
        1
    );
    flow.accept(&call, "bootstrap-notice-turn").await;
    flow.end("bootstrap-notice-turn").await;
    flow.fixture
        .wait_phase(&flow.track, HarnessPhaseTag::TurnCompleted)
        .await;
    flow.state
        .dispatcher
        .progress_tick_for_test()
        .await
        .unwrap();
    assert!(
        flow.fixture
            .carrier(&flow.track)
            .await
            .2
            .canonical_inputs
            .pending
            .is_empty()
    );
    flow.close().await;
}
