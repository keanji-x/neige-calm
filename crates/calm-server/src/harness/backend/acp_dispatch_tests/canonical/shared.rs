//! Canonical cleanup requires evidence from the original shared attachment.
use super::*;

#[tokio::test]
async fn canonical_antigravity_normal_end_has_body_free_receipt_and_exact_end() {
    let fx = Fixture::with_provider(AgentProvider::Antigravity, "reply", false).await;
    fx.dispatch().await.unwrap();
    fx.wait_idle().await;
    assert!(matches!(
        crate::db::sqlite::AcpSubmissionInput::decode(&fx.receipt_bytes(&fx.submission).await.1)
            .unwrap(),
        crate::db::sqlite::AcpSubmissionInput::Canonical(_)
    ));
    assert!(fx.complete().await.is_some());
    assert!(fx.complete().await.is_none());
    assert_eq!(fx.prompts(), 1);
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_antigravity_confirmed_close_allows_keep_and_same_process_continuation() {
    let fx = Fixture::with_provider(AgentProvider::Antigravity, "shared-unknown", false).await;
    fx.dispatch().await.unwrap();
    fx.wait_idle().await;
    let before = fx.receipt_bytes(&fx.submission).await;
    assert!(fx.complete().await.is_none());
    fx.keep_unknown(&fx.submission, "keep-original").await;
    assert!(fx.backend.settle_kept_execution().await.unwrap().is_some());
    assert!(fx.backend.settle_kept_execution().await.unwrap().is_none());
    std::fs::write(fx.dir.path().join("scenario"), "reply").unwrap();
    let successor = fx.claim_successor().await;
    fx.dispatch_submission(&successor).await;
    fx.wait_idle().await;
    assert!(fx.complete().await.is_some());
    assert_eq!(fx.receipt_bytes(&fx.submission).await, before);
    let requests = fx.requests();
    let prompts: Vec<_> = requests
        .iter()
        .filter(|r| r["method"] == "session/prompt")
        .collect();
    assert_eq!(prompts.len(), 2);
    assert_eq!(prompts[0]["_pid"], prompts[1]["_pid"]);
    assert_eq!(
        prompts[0]["params"]["sessionId"],
        prompts[1]["params"]["sessionId"]
    );
    assert!(requests.iter().any(|r| r["method"] == "session/close"
        && r["params"]["sessionId"] == prompts[0]["params"]["sessionId"]));
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_antigravity_unconfirmed_revocation_cannot_manufacture_keep_cleanup() {
    let fx = Fixture::with_provider(AgentProvider::Antigravity, "shared-unknown", false).await;
    fx.owner.repo.db().writer().execute(sqlx::query(
        "CREATE TRIGGER block_shared_revocation BEFORE UPDATE OF mcp_token_hash ON worker_sessions
         WHEN OLD.mcp_token_hash IS NOT NULL AND NEW.mcp_token_hash IS NULL
         BEGIN SELECT RAISE(ABORT, 'revocation unavailable'); END"
    )).await.unwrap();
    fx.dispatch().await.unwrap();
    fx.wait_idle().await;
    fx.keep_unknown(&fx.submission, "keep-without-close").await;
    assert!(fx.backend.settle_kept_execution().await.unwrap().is_none());
    let joins: i64 =
        sqlx::query_scalar("SELECT count(*) FROM acp_normal_joins WHERE worker_session_id=?")
            .bind(&fx.owner.worker)
            .fetch_one(fx.owner.repo.pool())
            .await
            .unwrap();
    assert_eq!(joins, 0);
    assert!(fx.complete().await.is_none());
    assert_eq!(fx.prompts(), 1);
    fx.owner
        .repo
        .db()
        .writer()
        .execute(sqlx::query("DROP TRIGGER block_shared_revocation"))
        .await
        .unwrap();
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_antigravity_unconfirmed_process_stop_cannot_release_kept_input() {
    let fx = Fixture::with_provider(AgentProvider::Antigravity, "lost", false).await;
    let fired = crate::test_seams::install_operation_fault_for_test(
        "acp-control-reap",
        &fx.session.stop_fault_scope_for_test(),
    );
    fx.dispatch().await.unwrap();
    tokio::time::timeout(Duration::from_secs(20), fired.notified())
        .await
        .unwrap();
    fx.wait_idle().await;
    fx.keep_unknown(&fx.submission, "keep-without-reap").await;
    assert!(fx.backend.settle_kept_execution().await.unwrap().is_none());
    let joins: i64 =
        sqlx::query_scalar("SELECT count(*) FROM acp_normal_joins WHERE worker_session_id=?")
            .bind(&fx.owner.worker)
            .fetch_one(fx.owner.repo.pool())
            .await
            .unwrap();
    assert_eq!(joins, 0);
    assert!(fx.complete().await.is_none());
    assert_eq!(fx.prompts(), 1);
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_antigravity_restart_resumes_native_and_dispatches_only_pending() {
    let fx = Fixture::with_provider(AgentProvider::Antigravity, "lost", true).await;
    fx.dispatch().await.unwrap();
    fx.wait_idle().await;
    fx.session.shutdown().await.unwrap();
    assert_eq!(fx.prompts(), 1);
    let original = fx.receipt_bytes(&fx.submission).await;
    let owner = &fx.owner;
    let write_db = owner.repo.db().clone();
    write_db
        .writer()
        .with_writer(|write_session| {
            let source = &fx.source;
            Box::pin(async move {
                let mut tx = write_session.begin_immediate().await.unwrap();
                let mut carrier = current_input_carrier_tx(&mut tx, &owner.card, &owner.worker)
                    .await
                    .unwrap();
                for text in [
                    "first queued before restart",
                    "second queued before restart",
                ] {
                    crate::conversation_inputs::accept_human_tx(
                        &mut tx,
                        &source.current_principal().unwrap(),
                        &mut carrier,
                        HumanPayload {
                            text: text.into(),
                            bound_attachments: vec![],
                        },
                    )
                    .await
                    .unwrap();
                }
                tx.commit().await.unwrap();
            })
        })
        .await
        .unwrap();

    let reopened = super::super::super::dispatch_fixture::Owner {
        repo: Arc::new(
            crate::db::sqlite::SqlxRepo::open(&owner.db_url)
                .await
                .unwrap(),
        ),
        card: owner.card.clone(),
        track: owner.track.clone(),
        worker: owner.worker.clone(),
        thread: owner.thread.clone(),
        db_url: owner.db_url.clone(),
    };
    let (backend, session, _held) =
        session_for_provider(&reopened, &fx.dir, AgentProvider::Antigravity);
    crate::acp_planner::config::boot(reopened.repo.as_ref(), session.host())
        .await
        .unwrap();
    backend.recover_retained().await.unwrap();
    let frame = runtime_frame_observation(reopened.repo.pool(), &reopened.card, &reopened.worker)
        .await
        .unwrap();
    assert_eq!(
        frame.snapshot.header.phase,
        crate::harness::HarnessPhaseTag::Idle
    );
    assert!(frame.snapshot.canonical_inputs.active.is_none());
    assert_eq!(frame.snapshot.canonical_inputs.pending.len(), 2);
    assert_eq!(fx.receipt_bytes(&fx.submission).await, original);
    let db = reopened.repo.db().clone();
    let next = db
        .writer()
        .with_writer(|write_session| {
            let reopened = &reopened;
            let source = &fx.source;
            Box::pin(async move {
                let mut tx = write_session.begin_immediate().await.unwrap();
                let mut carrier =
                    current_input_carrier_tx(&mut tx, &reopened.card, &reopened.worker)
                        .await
                        .unwrap();
                let plan = crate::db::sqlite::conversation::pending_plan_for_prefix_tx(
                    &mut tx,
                    &carrier,
                    std::num::NonZeroU32::new(2).unwrap(),
                    vec![],
                )
                .await
                .unwrap();
                let claimed = crate::conversation_inputs::claim_initial_tx(
                    &mut tx,
                    source,
                    &mut carrier,
                    plan,
                )
                .await
                .unwrap();
                tx.commit().await.unwrap();
                claimed.submission_id
            })
        })
        .await
        .unwrap();

    std::fs::write(fx.dir.path().join("scenario"), "reply").unwrap();
    crate::harness::input_dispatch::initial(
        reopened.repo.as_ref(),
        &fx.source,
        &reopened.card,
        &reopened.worker,
        &next,
        &backend,
        &TurnModelSelection::inherit(),
        PlannerPermissionMode::Never,
        &backend.handoff_for_test().await,
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        while !session.driver_finished_for_test() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        crate::harness::input_dispatch::complete_once(
            reopened.repo.as_ref(),
            &backend,
            &reopened.card,
            &reopened.worker,
        )
        .await
        .unwrap()
        .is_some()
    );
    session.shutdown().await.unwrap();

    let requests = fx.requests();
    let prompts: Vec<_> = requests
        .iter()
        .filter(|r| r["method"] == "session/prompt")
        .collect();
    assert_eq!(prompts.len(), 2);
    let native = &prompts[0]["params"]["sessionId"];
    assert_eq!(&prompts[1]["params"]["sessionId"], native);
    assert_ne!(prompts[0]["_pid"], prompts[1]["_pid"]);
    let prompt = prompts[1]["params"]["prompt"].to_string();
    let first = prompt.find("first queued before restart").unwrap();
    let second = prompt.find("second queued before restart").unwrap();
    assert!(first < second);
    assert!(!prompt.contains("canonical human"));
    assert!(
        requests
            .iter()
            .any(|r| r["method"] == "session/resume" && &r["params"]["sessionId"] == native)
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "session/new")
            .count(),
        1
    );
}

#[tokio::test]
async fn canonical_antigravity_restart_drops_initial_claim_before_native_attachment() {
    let fx = Fixture::with_provider(AgentProvider::Antigravity, "reply", true).await;
    let (_, session, _held) = session_for_provider(&fx.owner, &fx.dir, AgentProvider::Antigravity);
    crate::acp_planner::config::boot(fx.owner.repo.as_ref(), session.host())
        .await
        .unwrap();
    let frame = runtime_frame_observation(fx.owner.repo.pool(), &fx.owner.card, &fx.owner.worker)
        .await
        .unwrap();
    assert_eq!(frame.snapshot.header.phase, HarnessPhaseTag::Idle);
    assert!(frame.snapshot.canonical_inputs.active.is_none());
    assert!(frame.snapshot.canonical_inputs.pending.is_empty());
    assert!(fx.dispatch().await.is_err());
    assert_eq!(fx.prompts(), 0);
}

#[tokio::test]
async fn canonical_antigravity_restart_finishes_exact_completed_stop() {
    use calm_types::harness::{
        HarnessControlIntent, HarnessInterruptionIntent, HarnessStopRequest,
    };
    let fx = Fixture::with_provider(AgentProvider::Antigravity, "reply", true).await;
    fx.dispatch().await.unwrap();
    fx.wait_idle().await;
    fx.session.shutdown().await.unwrap();
    let old = fx.receipt_bytes(&fx.submission).await;
    assert_eq!(old.2, "completed");
    let owner = &fx.owner;
    let turn = old.0.clone();
    owner
        .repo
        .db()
        .writer()
        .with_writer(|writer| {
            Box::pin(async move {
                let mut tx = writer.begin_immediate().await.unwrap();
                let carrier = current_input_carrier_tx(&mut tx, &owner.card, &owner.worker)
                    .await
                    .unwrap();
                let fence = RuntimeExecutionFence::from_carrier(&carrier);
                let mut header = fence.header().clone();
                header["phase"] = serde_json::to_value(HarnessPhaseTag::IssuingInterrupt).unwrap();
                header["last_turn_id"] = serde_json::json!(&turn);
                header["interruption_intent"] =
                    serde_json::to_value(HarnessInterruptionIntent::Control(
                        HarnessControlIntent::UserStopV1(HarnessStopRequest {
                            session_id: owner.worker.clone(),
                            card_id: owner.card.clone(),
                            track_id: owner.track.clone(),
                            thread_id: owner.thread.clone(),
                            turn_id: turn,
                            requested_at_ms: crate::model::now_ms(),
                        }),
                    ))
                    .unwrap();
                persist_runtime_frame_tx(&mut tx, &owner.card, &owner.worker, &fence, header)
                    .await
                    .unwrap();
                tx.commit().await.unwrap();
            })
        })
        .await
        .unwrap();

    let (_, session, _held) = session_for_provider(owner, &fx.dir, AgentProvider::Antigravity);
    crate::acp_planner::config::boot(owner.repo.as_ref(), session.host())
        .await
        .unwrap();
    let frame = runtime_frame_observation(owner.repo.pool(), &owner.card, &owner.worker)
        .await
        .unwrap();
    assert_eq!(frame.snapshot.header.phase, HarnessPhaseTag::TurnCompleted);
    assert!(frame.snapshot.header.interruption_intent.is_none());
    assert!(frame.snapshot.canonical_inputs.active.is_none());
    assert_eq!(fx.receipt_bytes(&fx.submission).await, old);
    assert_eq!(fx.prompts(), 1);
}

#[tokio::test]
async fn canonical_antigravity_restart_keeps_fence_while_exact_process_is_live() {
    let fx = Fixture::with_provider(AgentProvider::Antigravity, "shared-unknown", true).await;
    fx.dispatch().await.unwrap();
    fx.wait_idle().await;
    let sql = "SELECT shared_restart_owner_json FROM acp_submissions WHERE worker_session_id=?1 AND client_id=?2";
    let sqlx::types::Json(owner): sqlx::types::Json<crate::db::sqlite::AcpSharedProcessIdentity> =
        sqlx::query_scalar(sql)
            .bind(&fx.owner.worker)
            .bind(fx.submission.as_str())
            .fetch_one(fx.owner.repo.pool())
            .await
            .unwrap();
    assert!(crate::proc_identity::verify_owned_pid(
        owner.pid,
        owner.start_time,
        &owner.boot_id
    ));
    let (_, session, _held) = session_for_provider(&fx.owner, &fx.dir, AgentProvider::Antigravity);
    crate::acp_planner::config::boot_with_empty_marker_scan_for_test(
        fx.owner.repo.as_ref(),
        session.host(),
    )
    .await
    .unwrap();
    assert!(
        crate::db::sqlite::acp_submission_unresolved(fx.owner.repo.pool(), &fx.owner.worker)
            .await
            .unwrap()
    );
    let frame = runtime_frame_observation(fx.owner.repo.pool(), &fx.owner.card, &fx.owner.worker)
        .await
        .unwrap();
    assert!(frame.snapshot.canonical_inputs.active.is_some());
    assert_eq!(fx.prompts(), 1);
    fx.session.shutdown().await.unwrap();
}
