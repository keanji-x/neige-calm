use super::*;
use crate::db::RepoRead;
use crate::harness::{HarnessRegistry, wiring::PlannerWiring};
use crate::operation::agent_worker_adapter::AgentWorkerAdapter;
use crate::session_projection_repo::WorkerSessionProjectionRepo;
use crate::shared_codex_appserver::native_owner_tests::{connect_with_reader, request, send};

#[tokio::test]
async fn agent_worker_native_turn_streams_without_terminal_and_settles_unreported_attempt() {
    native_worker(EndMode::Unreported).await;
}

#[tokio::test]
async fn agent_worker_cancel_interrupts_native_turn_and_releases_lease() {
    native_worker(EndMode::Cancel).await;
}

#[tokio::test]
async fn agent_worker_compensation_preserves_reported_gate_and_card() {
    native_worker(EndMode::ReportedGate).await;
}

#[tokio::test]
async fn agent_worker_startup_compensation_cleans_card_session_and_lease() {
    native_worker(EndMode::SpawnFailure).await;
}

#[tokio::test]
async fn agent_worker_native_error_interrupts_and_releases_lease() {
    native_worker(EndMode::NativeError).await;
}

#[derive(Clone, Copy)]
enum EndMode {
    Unreported,
    Cancel,
    ReportedGate,
    SpawnFailure,
    NativeError,
}

async fn native_worker(mode: EndMode) {
    let cancel = matches!(mode, EndMode::Cancel);
    let h = worker_lease_harness().await;
    h.repo
        .track_area_cache()
        .seed_from_db(h.repo.pool())
        .await
        .unwrap();
    let daemon = SharedCodexAppServer::new_stub(h.repo.clone());
    let (_client, mut peer, _forward) = connect_with_reader(&daemon).await;
    let wiring = PlannerWiring::unconfigured_for_test(h.repo.clone());
    let registry = HarnessRegistry::new();
    let adapter = Arc::new(AgentWorkerAdapter::new(
        h.repo.clone(),
        wiring.clone(),
        crate::planner_model::child_selection::ChildSelectionContext {
            availability: Arc::default(),
            claude: wiring.claude.clone(),
            acp: wiring.acp.clone(),
            codex: daemon.clone(),
        },
        crate::agent_backend_pool::codex::CodexBackends::single(daemon),
        registry.clone(),
        h.repo.card_role_cache().clone(),
        h.repo.track_area_cache().clone(),
        (!matches!(mode, EndMode::SpawnFailure)).then(|| h.repo_root.path().join("mcp.sock")),
        h.repo_root.path().into(),
        GateRunWait::DEFAULT,
    ));
    let task = format!("{}:headless", h.track_id);
    let gate = matches!(mode, EndMode::ReportedGate).then(|| {
        serde_json::to_string(&calm_types::report_blocks::tasks::GateInput {
            cwd: None,
            timeout_secs: None,
            steps: vec![calm_types::report_blocks::tasks::GateStepInput {
                name: "check".into(),
                cmd: "true".into(),
            }],
        })
        .unwrap()
    });
    h.repo.db().writer().execute(sqlx::query(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,depends_on_json,status,created_at_ms,updated_at_ms,gate_json)
         VALUES(?1,?2,'headless','codex','do headless','null','[]','dispatched',1,1,?3)")
        .bind(&task).bind(&h.track_id).bind(gate)).await.unwrap();
    let row = h.repo.task_get(&task).await.unwrap().unwrap();
    let (kind, payload) = crate::scheduler::build_worker_payload(&row).unwrap();
    let key = OperationKey {
        operation_key: new_id(),
        idempotency_key: Some(task.clone()),
        payload_hash: crate::routes::idempotency_key::stable_payload_hash(&payload).unwrap(),
    };
    let operations = Arc::new(SqlxOperationRepo::new(h.repo.db().clone()));
    let completion = OperationCompletionBus::new();
    let ctx = SpawnCtx::new(
        h.repo.clone(),
        operations.clone(),
        Arc::new(DaemonClient::new_stub()),
        TerminalRendererRegistry::new_with_repo(h.repo.clone()),
        h.events.clone(),
        completion.clone(),
    );
    let runtime = crate::operation::OperationRuntime::new(
        operations.clone(),
        vec![adapter.clone()],
        h.events.clone(),
        completion,
        ctx,
    )
    .await
    .unwrap();
    let spawn = runtime.submit_task_bounded(kind, key, payload);
    if matches!(mode, EndMode::SpawnFailure) {
        let (id, wait) = tokio::time::timeout(Duration::from_secs(5), spawn)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            wait,
            crate::operation::OperationWait::Settled(crate::operation::OperationResult {
                outcome: crate::operation::OperationOutcome::Failed { .. },
                ..
            })
        ));
        let output = operations
            .get_operation(&id)
            .await
            .unwrap()
            .unwrap()
            .tx_output
            .unwrap();
        let card = output.output_string("card_id", "test").unwrap();
        let worker = output.output_string("worker_session_id", "test").unwrap();
        assert!(h.repo.card_get(&card).await.unwrap().is_none());
        assert!(
            h.repo
                .session_projection_by_id(&worker)
                .await
                .unwrap()
                .is_none()
        );
        let state: String =
            sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id=?1")
                .bind(output.output_string("lease_id", "test").unwrap())
                .fetch_one(h.repo.pool())
                .await
                .unwrap();
        assert_eq!(state, "released");
        return;
    }
    let successful_spawn = async {
        let receipt = spawn.await.unwrap();
        assert!(
            matches!(
                &receipt.1,
                crate::operation::OperationWait::Settled(crate::operation::OperationResult {
                    outcome: crate::operation::OperationOutcome::Succeeded { .. },
                    ..
                })
            ),
            "unexpected worker startup: {:?}",
            receipt.1
        );
        receipt
    };
    let serve = async {
        let call = request(&mut peer).await;
        assert_eq!(call["method"], "thread/start");
        assert_eq!(call["params"]["cwd"], h.worktree.to_str().unwrap());
        send(
            &mut peer,
            json!({"id":call["id"],"result":{"thread":{"id":"headless-thread"}}}),
        )
        .await;
        let call = request(&mut peer).await;
        assert_eq!(call["method"], "turn/start");
        send(
            &mut peer,
            json!({"id":call["id"],"result":{"turn":{"id":"headless-turn"}}}),
        )
        .await;
    };
    let ((op_id, _), ()) = tokio::join!(successful_spawn, serve);
    let op = operations.get_operation(&op_id).await.unwrap().unwrap();
    let output = op.tx_output.clone().unwrap();
    let card = output.output_string("card_id", "test").unwrap();
    let lease = output.output_string("lease_id", "test").unwrap();
    let refusal = crate::terminal_interaction::TerminalInteraction::resolve_in_track(
        h.repo.as_ref(),
        &h.track_id,
        &crate::terminal_interaction::Target::Attempt(task.clone()),
    )
    .await
    .err()
    .expect("headless terminal refusal");
    let human_card = card.clone();
    let human_write = crate::db::write_in_tx_typed(h.repo.as_ref(), move |tx| {
        Box::pin(async move {
            crate::harness::profile::require_human_harness_card_tx(tx, &human_card).await
        })
    })
    .await;
    assert!(matches!(human_write, Err(CalmError::Forbidden(_))));
    assert_eq!(
        refusal
            .downcast_ref::<crate::terminal_interaction::InputRefused>()
            .unwrap()
            .refusal,
        "headless_worker"
    );

    for (method, params) in [
        (
            "turn/started",
            json!({"threadId":"headless-thread","turn":{"id":"headless-turn"}}),
        ),
        (
            "item/completed",
            json!({"threadId":"headless-thread","turnId":"headless-turn","item":{
                "id":"native-input","clientId":task,"type":"userMessage","content":[{"type":"text","text":output.data["prompt"]}]
            }}),
        ),
        (
            "item/started",
            json!({"threadId":"headless-thread","turnId":"headless-turn","item":{"id":"reply","type":"agentMessage","text":""}}),
        ),
        (
            "item/agentMessage/delta",
            json!({"threadId":"headless-thread","turnId":"headless-turn","itemId":"reply","delta":"streamed"}),
        ),
    ] {
        send(&mut peer, json!({"method":method,"params":params})).await;
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if registry
                .live_replies()
                .read(&card.clone().into())
                .items
                .iter()
                .any(|i| i.text == "streamed")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    send(&mut peer,json!({"method":"item/completed","params":{"threadId":"headless-thread","turnId":"headless-turn","item":{"id":"reply","type":"agentMessage","text":"streamed"}}})).await;
    if matches!(mode, EndMode::ReportedGate) {
        let task_id = task.clone();
        let card_id = card.clone();
        let track = h.track_id.clone();
        let flipped = crate::db::write_in_tx_typed(h.repo.as_ref(), move |tx| {
            Box::pin(async move {
                Ok(crate::db::sqlite::task_report_success_from_worker_tx(
                    tx,
                    &task_id,
                    &track,
                    crate::db::sqlite::TaskReporter::Card {
                        card_id: &card_id,
                        owns_key: false,
                    },
                    now_ms(),
                )
                .await?)
            })
        })
        .await
        .unwrap();
        assert!(matches!(
            flipped,
            crate::db::sqlite::SuccessReportFlip::Verifying
        ));
        let plan = adapter
            .plan_compensation(PhaseTag::SpawnStarted, "checkpoint failed", &output, &op)
            .await
            .unwrap();
        let compensation =
            adapter.compensate_step(&plan.steps[0], &output, &op, runtime.reclaim_context());
        tokio::pin!(compensation);
        tokio::time::timeout(Duration::from_secs(5),async {
            loop {
                tokio::select! {
                    result=&mut compensation=>{result.unwrap();break;}
                    call=request(&mut peer)=>{
                        assert_eq!(call["method"],"turn/interrupt");
                        send(&mut peer,json!({"id":call["id"],"result":{}})).await;
                        send(&mut peer,json!({"method":"turn/completed","params":{"threadId":"headless-thread","turn":{"id":"headless-turn","status":"interrupted"}}})).await;
                    }
                }
            }
        }).await.unwrap();
        assert_eq!(
            h.repo.task_get(&task).await.unwrap().unwrap().status,
            crate::model::TaskStatus::Verifying
        );
        assert!(h.repo.card_get(&card).await.unwrap().is_some());
        let state: String =
            sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id=?1")
                .bind(&lease)
                .fetch_one(h.repo.pool())
                .await
                .unwrap();
        assert_eq!(state, "held");
        return;
    }
    if matches!(mode, EndMode::NativeError) {
        send(&mut peer,json!({"method":"thread/status/changed","params":{"threadId":"headless-thread","status":{"type":"systemError"}}})).await;
        let call = request(&mut peer).await;
        assert_eq!(call["method"], "turn/interrupt");
        send(&mut peer, json!({"id":call["id"],"result":{}})).await;
    } else if cancel {
        let task_id = task.clone();
        let card_id = card.clone();
        h.repo
            .db()
            .writer()
            .with_writer(|session| {
                Box::pin(async move {
                    let mut tx = session.begin_immediate().await?;
                    crate::db::sqlite::task_cancel_running_tx(
                        &mut tx,
                        &task_id,
                        &card_id,
                        crate::scheduler::PLANNER_CANCELED,
                        now_ms(),
                    )
                    .await?;
                    tx.commit().await?;
                    Ok::<_, CalmError>(())
                })
            })
            .await
            .unwrap()
            .unwrap();
        registry.interrupt_task_worker(&output.output_string("worker_session_id", "test").unwrap());
        let call = request(&mut peer).await;
        assert_eq!(call["method"], "turn/interrupt");
        send(&mut peer, json!({"id":call["id"],"result":{}})).await;
    } else {
        send(&mut peer,json!({"method":"turn/completed","params":{"threadId":"headless-thread","turn":{"id":"headless-turn","status":"completed"}}})).await;
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let row = h.repo.task_get(&task).await.unwrap().unwrap();
            let lease_state: String =
                sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id=?1")
                    .bind(&lease)
                    .fetch_one(h.repo.pool())
                    .await
                    .unwrap();
            if lease_state == "released" {
                assert_eq!(
                    row.status,
                    if cancel {
                        crate::model::TaskStatus::Canceled
                    } else {
                        crate::model::TaskStatus::Failed
                    }
                );
                if !cancel {
                    assert_eq!(
                        row.status_detail.as_deref(),
                        Some(crate::scheduler::WORKER_TURN_ENDED)
                    );
                }
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let state: String = sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id=?1")
        .bind(lease)
        .fetch_one(h.repo.pool())
        .await
        .unwrap();
    assert_eq!(state, "released");
    let terminals: i64 = sqlx::query_scalar("SELECT count(*) FROM terminals WHERE card_id=?1")
        .bind(&card)
        .fetch_one(h.repo.pool())
        .await
        .unwrap();
    assert_eq!(terminals, 0);
    let replies: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM harness_items WHERE card_id=?1
         AND method='item/completed' AND json_extract(params,'$.item.text')='streamed'",
    )
    .bind(&card)
    .fetch_one(h.repo.pool())
    .await
    .unwrap();
    assert_eq!(replies, 1);
    let inputs:i64=sqlx::query_scalar("SELECT count(*) FROM harness_items WHERE card_id=?1 AND item_type='userMessage' AND method='item/completed'").bind(&card).fetch_one(h.repo.pool()).await.unwrap();
    assert_eq!(inputs, 1);
    let write = WriteContext::new(
        h.repo.card_role_cache().clone(),
        h.repo.track_area_cache().clone(),
    );
    let track = h.repo.track_get(&h.track_id).await.unwrap().unwrap();
    let view = calm_truth::track_fs_view::TrackFsView::new(h.repo.as_ref(), &write);
    let conversation = view
        .cat(&track, &format!("cards/{card}/conversation.md"))
        .await
        .unwrap();
    assert!(conversation.content.contains("streamed"));
}
