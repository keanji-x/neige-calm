use super::completed_commit_tests::Fixture;
use super::*;
use crate::db::prelude::*;
use crate::harness::handoff_pause::{PauseReceipt, PauseRequest, PauseState};
use crate::model::CardRole;

pub(super) fn request(f: &Fixture) -> PauseRequest {
    PauseRequest::new(
        f.harness.inner.card_id.clone(),
        f.harness.inner.worker_session_id.clone(),
        f.harness.inner.track_id.clone(),
    )
}

pub(super) fn params(f: &Fixture, snapshot: HarnessSnapshot) -> PlannerHarnessParams {
    let inner = &f.harness.inner;
    PlannerHarnessParams {
        worker_session_id: inner.worker_session_id.clone(),
        track_id: inner.track_id.clone(),
        card_id: inner.card_id.clone(),
        thread_id: snapshot.last_thread_id.clone(),
        repo: f.repo.clone(),
        events: inner.events.clone(),
        card_role_cache: inner.card_role_cache.clone(),
        track_area_cache: inner.track_area_cache.clone(),
        backend: f.daemon.clone().into(),
        live_replies: crate::harness::LiveReplies::for_test(),
        config: inner.config,
        snapshot,
    }
}

pub(super) async fn running() -> Fixture {
    let mut f = Fixture::new().await;
    f.harness = PlannerHarness::run(params(&f, f.stored().await));
    f
}

pub(super) async fn system(f: &Fixture, id: i64) {
    f.harness
        .observe_envelope(
            Observation::TrackGoal {
                text: format!("system {id}"),
            },
            id,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn handoff_pause_persists_blocks_issuance_and_exact_resume() {
    let f = Fixture::new().await;
    system(&f, 9).await;
    let claim = request(&f);
    let receipt = f.harness.pause_for_handoff(claim.clone()).await.unwrap();
    system(&f, 10).await;
    f.issue().await;
    assert_eq!(f.daemon.turn_start_count_for_test(), 0);
    assert_eq!(f.stored().await.pending_entries().len(), 2);
    assert_eq!(f.stored().await.push_watermark, 10);
    assert_eq!(
        f.stored().await.handoff_pause,
        PauseState::Paused {
            receipt: receipt.clone()
        }
    );
    assert!(
        f.harness
            .resume_from_handoff(PauseReceipt::from_request(request(&f)))
            .await
            .is_err()
    );
    assert_eq!(
        f.harness.pause_for_handoff(claim.clone()).await.unwrap(),
        receipt
    );
    assert!(
        f.harness
            .observe_user_message_durable("refuse".into(), vec![], SendKey::unique_for_test())
            .await
            .is_err()
    );
    assert!(
        compact_command::handle_compact(&f.harness.inner)
            .await
            .is_err()
    );
    let id = QueueEntryId::mint();
    assert!(
        f.harness
            .mutate_pending_entry(
                QueueMutation::Delete {
                    entry_id: id.clone(),
                    if_entry_rev: 0
                },
                ActorId::Kernel
            )
            .await
            .is_err()
    );
    assert!(
        f.harness
            .steer_pending_entry(id, 0, ActorId::Kernel)
            .await
            .is_err()
    );
    assert!(
        replace_command::handle_replace(
            &f.harness.inner,
            "old",
            QueueEntry::user_message("replace".into(), None, vec![]),
            &SendKey::unique_for_test()
        )
        .await
        .is_err()
    );
    assert!(!f.harness.hibernate_if_idle().await.unwrap());
    f.harness.persist_snapshot().await.unwrap();
    assert_eq!(
        f.stored().await.handoff_pause,
        PauseState::Paused {
            receipt: receipt.clone()
        }
    );
    assert_eq!(
        f.harness.resume_from_handoff(receipt).await.unwrap(),
        crate::harness::handoff_pause::ReleaseOutcome::HealthyReleased
    );
    assert!(!f.stored().await.handoff_pause.is_paused());
    f.issue().await;
    assert_eq!(f.daemon.turn_start_count_for_test(), 1);
}

#[tokio::test]
async fn handoff_pause_two_session_prepare_refusal_compensates_original_receipt() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    b.enqueue(vec![QueueEntry::user_message(
        "pending".into(),
        None,
        vec![],
    )])
    .await;
    let receipt = a.harness.pause_for_handoff(request(&a)).await.unwrap();
    assert!(b.harness.pause_for_handoff(request(&b)).await.is_err());
    assert!(
        b.harness
            .resume_from_handoff(receipt.clone())
            .await
            .is_err()
    );
    assert_eq!(
        a.stored().await.handoff_pause,
        PauseState::Paused {
            receipt: receipt.clone()
        }
    );
    assert_eq!(
        a.harness.resume_from_handoff(receipt).await.unwrap(),
        crate::harness::handoff_pause::ReleaseOutcome::HealthyReleased
    );
    assert!(!a.stored().await.handoff_pause.is_paused());
    assert_eq!(b.stored().await.pending_entries().len(), 1);
}

#[tokio::test]
async fn handoff_pause_concurrent_claims_have_one_durable_winner() {
    let f = running().await;
    let (a, b) = tokio::join!(
        f.harness.pause_for_handoff(request(&f)),
        f.harness.pause_for_handoff(request(&f))
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let receipt = a.or(b).unwrap();
    assert_eq!(
        f.harness
            .query_handoff_pause(receipt.request().clone())
            .await
            .unwrap(),
        PauseState::Paused {
            receipt: receipt.clone()
        }
    );
    assert!(f.harness.query_handoff_pause(request(&f)).await.is_err());
    assert_eq!(
        f.harness.resume_from_handoff(receipt).await.unwrap(),
        crate::harness::handoff_pause::ReleaseOutcome::HealthyReleased
    );
    f.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn handoff_pause_dropped_reply_replay_and_system_ack_keep_claim() {
    let f = running().await;
    let claim = request(&f);
    let ObservationIngress::Running(sender) = &f.harness.inner.observations else {
        panic!()
    };
    let (answer, reply) = oneshot::channel();
    sender
        .send(HarnessObservationCommand::HandoffPause {
            request: claim.clone(),
            action: handoff_pause::Action::Pause,
            answer,
        })
        .await
        .unwrap();
    drop(reply);
    assert_eq!(
        f.harness.pause_for_handoff(claim.clone()).await.unwrap(),
        PauseReceipt::from_request(claim.clone())
    );
    system(&f, 11).await;
    super::handoff_pause_interrupt_tests::tick_completed(&f).await;
    assert_eq!(f.daemon.turn_start_count_for_test(), 0);
    assert_eq!(f.harness.snapshot().await.phase, HarnessPhaseTag::Idle);
    assert_eq!(f.stored().await.pending_entries().len(), 1);
    assert_eq!(
        f.stored().await.handoff_pause,
        PauseState::Paused {
            receipt: PauseReceipt::from_request(claim.clone())
        }
    );
    let (answer, reply) = oneshot::channel();
    sender
        .send(HarnessObservationCommand::HandoffPause {
            request: claim.clone(),
            action: handoff_pause::Action::Resume,
            answer,
        })
        .await
        .unwrap();
    drop(reply);
    assert_eq!(
        f.harness
            .resume_from_handoff(PauseReceipt::from_request(claim.clone()))
            .await
            .unwrap(),
        crate::harness::handoff_pause::ReleaseOutcome::HealthyReleased
    );
    assert_eq!(
        f.harness.query_handoff_pause(claim.clone()).await.unwrap(),
        PauseState::Released {
            receipt: PauseReceipt::from_request(claim),
            outcome: crate::harness::handoff_pause::ReleaseOutcome::HealthyReleased
        }
    );
    f.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn handoff_pause_database_failure_never_acknowledges_or_drops_claim() {
    let f = Fixture::new().await;
    let claim = request(&f);
    sqlx::query(concat!(
        "CREATE TRIGGER refuse_pause BEFORE UPDATE OF handle_state_json ON worker_sessions BEGIN ",
        "SELECT RAISE(ABORT, 'pause db failure'); END",
    ))
    .execute(f.repo.db().writer_pool())
    .await
    .unwrap();
    assert!(f.harness.pause_for_handoff(claim.clone()).await.is_err());
    assert_eq!(f.stored().await.handoff_pause, PauseState::Unpaused);
    sqlx::query("DROP TRIGGER refuse_pause")
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
    let receipt = f.harness.pause_for_handoff(claim).await.unwrap();
    sqlx::query(concat!(
        "CREATE TRIGGER refuse_pause BEFORE UPDATE OF handle_state_json ON worker_sessions BEGIN ",
        "SELECT RAISE(ABORT, 'pause db failure'); END",
    ))
    .execute(f.repo.db().writer_pool())
    .await
    .unwrap();
    assert!(
        f.harness
            .resume_from_handoff(receipt.clone())
            .await
            .is_err()
    );
    assert_eq!(
        f.stored().await.handoff_pause,
        PauseState::Paused {
            receipt: receipt.clone()
        }
    );
    assert_eq!(
        f.harness
            .query_handoff_pause(receipt.request().clone())
            .await
            .unwrap(),
        PauseState::Paused {
            receipt: receipt.clone()
        }
    );
    sqlx::query("DROP TRIGGER refuse_pause")
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
    assert_eq!(
        f.harness.resume_from_handoff(receipt).await.unwrap(),
        crate::harness::handoff_pause::ReleaseOutcome::HealthyReleased
    );
}

#[tokio::test]
async fn handoff_pause_phase_and_owner_conflicts_refuse_without_marker() {
    let f = Fixture::new().await;
    let states = [
        HarnessState::PendingThreadStart,
        HarnessState::Issuing {
            since: Instant::now(),
            kind: IssuingKind::TurnStart,
        },
        HarnessState::Compacting {
            since: Instant::now(),
        },
        HarnessState::CompactionRunning {
            turn_id: "maintenance".into(),
            started_at: Instant::now(),
        },
        HarnessState::turn_running("turn".into(), Instant::now()),
        HarnessState::Wedged {
            since: Instant::now(),
            reason: "unknown".into(),
        },
    ];
    for state in states {
        *f.harness.inner.state.lock().await = state;
        assert!(f.harness.pause_for_handoff(request(&f)).await.is_err());
        assert_eq!(f.stored().await.handoff_pause, PauseState::Unpaused);
    }
    *f.harness.inner.state.lock().await = HarnessState::Idle;
    *f.harness.inner.projection_client_id.lock().await = Some(QueueEntryId::mint());
    assert!(f.harness.pause_for_handoff(request(&f)).await.is_err());
    *f.harness.inner.projection_client_id.lock().await = None;
    f.harness.inner.provider_held.store(true, Ordering::SeqCst);
    assert!(f.harness.pause_for_handoff(request(&f)).await.is_err());
    f.harness.inner.provider_held.store(false, Ordering::SeqCst);
    *f.harness.inner.compact_outcome.lock().await =
        Some(crate::harness::compact_outcome::CompactOutcome {
            operation_id: "op".into(),
            thread_id: "thread".into(),
            turn_id: None,
            status: crate::harness::compact_outcome::CompactStatus::Unconfirmed,
        });
    assert!(f.harness.pause_for_handoff(request(&f)).await.is_err());
}

#[tokio::test]
async fn handoff_pause_assistant_allowed_plain_chat_refused_and_identity_unchanged() {
    use calm_types::worker::WorkerSessionId;
    let f = Fixture::for_role(CardRole::Assistant).await;
    let before = f
        .repo
        .session_get(&WorkerSessionId(f.harness.inner.worker_session_id.clone()))
        .await
        .unwrap()
        .unwrap();
    let receipt = f.harness.pause_for_handoff(request(&f)).await.unwrap();
    let after = f.repo.session_get(&before.id).await.unwrap().unwrap();
    assert_eq!(before.mcp_token_hash, after.mcp_token_hash);
    assert_eq!(before.contract, after.contract);
    assert_eq!(before.card_id, after.card_id);
    assert_eq!(
        f.harness.resume_from_handoff(receipt).await.unwrap(),
        crate::harness::handoff_pause::ReleaseOutcome::HealthyReleased
    );
    let plain = Fixture::for_role(CardRole::Worker).await;
    assert!(
        plain
            .harness
            .pause_for_handoff(request(&plain))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn handoff_pause_unknown_submission_and_rewind_refuse() {
    let f = Fixture::new().await;
    sqlx::query(concat!(
        "INSERT INTO acp_submissions(",
        "worker_session_id,client_id,turn_id,thread_id,native_session_id,",
        "input_json,state,created_at_ms) VALUES(?1,'c','t','th','native','{}','unknown',0)",
    ))
    .bind(&f.harness.inner.worker_session_id)
    .execute(f.repo.db().writer_pool())
    .await
    .unwrap();
    assert!(f.harness.pause_for_handoff(request(&f)).await.is_err());
    sqlx::query("DELETE FROM acp_submissions")
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
    *f.harness.inner.pending_rewind.lock().await = Some(
        serde_json::from_value(
            serde_json::json!({"provider":"codex","before_turn_id":"old","reverted":false}),
        )
        .unwrap(),
    );
    assert!(f.harness.pause_for_handoff(request(&f)).await.is_err());
}

#[tokio::test]
async fn handoff_pause_restart_recovery_installs_backend_but_stays_quiet() {
    let mut f = Fixture::new().await;
    let claim = f.harness.pause_for_handoff(request(&f)).await.unwrap();
    f.harness
        .observe_envelope(
            Observation::TrackWake {
                source: "handoff-test".into(),
                key: "boot-pending".into(),
                text: "source system backlog".into(),
            },
            12,
        )
        .await
        .unwrap();
    let before = f.stored().await;
    // The real boot/lazy/replacement owner reloads SQL, catches up and CASes that exact source.
    let runtime = f
        .repo
        .session_projection_by_id(&f.harness.inner.worker_session_id)
        .await
        .unwrap()
        .unwrap();
    let registry = crate::harness::HarnessRegistry::new();
    let recovered = crate::harness::spawn_recovered_harness(
        f.repo.clone(),
        f.harness.inner.events.clone(),
        f.harness.inner.card_role_cache.clone(),
        f.harness.inner.track_area_cache.clone(),
        f.daemon.clone(),
        f.daemon.thread_seals().clone(),
        &crate::harness::wiring::PlannerWiring::unconfigured_for_test(f.repo.clone()),
        &registry,
        &crate::per_card_lock::new_keyed_locks(),
        runtime,
        crate::harness::ClaimMode::Replace,
    )
    .await
    .unwrap()
    .installed()
    .expect("paused harness is recoverable");
    f.harness = recovered;
    super::handoff_pause_interrupt_tests::tick_completed(&f).await;
    assert_eq!(f.daemon.turn_start_count_for_test(), 0);
    assert_eq!(f.harness.snapshot().await.phase, HarnessPhaseTag::Idle);
    assert_eq!(
        f.stored().await.handoff_pause,
        PauseState::RecoveryHealthUnconfirmed {
            receipt: claim.clone()
        }
    );
    assert_eq!(f.stored().await.pending_entries(), before.pending_entries());
    assert_eq!(
        f.harness.resume_from_handoff(claim).await.unwrap(),
        crate::harness::handoff_pause::ReleaseOutcome::HealthUnconfirmedAfterRecovery
    );
    super::handoff_pause_interrupt_tests::tick_completed(&f).await;
    assert_eq!(f.daemon.turn_start_count_for_test(), 0);
    f.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn handoff_pause_stale_session_and_external_snapshot_writer_cannot_erase_marker() {
    let f = Fixture::new().await;
    let claim = f.harness.pause_for_handoff(request(&f)).await.unwrap();
    // A stale local capture must not overwrite the durable pause even through a public save.
    *f.harness.inner.handoff_pause.lock().await = PauseState::Unpaused;
    assert!(f.harness.persist_snapshot().await.is_err());
    assert_eq!(
        f.stored().await.handoff_pause,
        PauseState::Paused {
            receipt: claim.clone()
        }
    );
    *f.harness.inner.handoff_pause.lock().await = PauseState::Paused {
        receipt: claim.clone(),
    };
    // A receipt is bound to the original session. Moving the card pointer invalidates resume.
    sqlx::query("UPDATE cards SET session_id=NULL WHERE id=?1")
        .bind(f.harness.inner.card_id.as_str())
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
    assert!(f.harness.resume_from_handoff(claim.clone()).await.is_err());
    let value: String =
        sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id=?1")
            .bind(&f.harness.inner.worker_session_id)
            .fetch_one(f.repo.pool())
            .await
            .unwrap();
    let stored: HarnessSnapshot = serde_json::from_str(&value).unwrap();
    assert_eq!(stored.handoff_pause, PauseState::Paused { receipt: claim });
}

#[tokio::test]
async fn handoff_pause_auth_active_provider_and_held_requests_refuse() {
    let f = Fixture::new().await;
    f.daemon.emit_notification_for_test(crate::codex_appserver::Notification::Other {
        method: "error".into(), params: serde_json::json!({"error":{"message":"fixture","codexErrorInfo":"unauthorized"}}),
    });
    assert!(f.daemon.authentication_hold().is_some());
    assert!(f.harness.pause_for_handoff(request(&f)).await.is_err());
    let f = Fixture::new().await;
    f.daemon
        .set_active_turn_for_test("thread-commit-wake", "active");
    assert!(f.harness.pause_for_handoff(request(&f)).await.is_err());
    let f = Fixture::new().await;
    struct Responder;
    impl held_requests::HeldResponder for Responder {
        fn respond(self: Box<Self>, _: usize) {}
    }
    f.harness.inner.held_requests.insert(
        1,
        held_requests::RequestKey("req".into()),
        held_requests::ConnectionId("connection".into()),
        Box::new(Responder),
    );
    assert!(f.harness.pause_for_handoff(request(&f)).await.is_err());
}

#[tokio::test]
async fn handoff_pause_unqueryable_storage_fails_closed_without_clearing_memory() {
    let f = Fixture::new().await;
    let receipt = f.harness.pause_for_handoff(request(&f)).await.unwrap();
    f.repo.pool().close().await;
    assert!(
        f.harness
            .resume_from_handoff(receipt.clone())
            .await
            .is_err()
    );
    assert!(f.harness.inner.shutting_down.load(Ordering::SeqCst));
    assert_eq!(
        *f.harness.inner.handoff_pause.lock().await,
        PauseState::Paused { receipt }
    );
    f.issue().await;
    assert_eq!(f.daemon.turn_start_count_for_test(), 0);
}

#[tokio::test]
async fn handoff_pause_cancelled_caller_still_commits_before_replay_ack() {
    let f = running().await;
    let hook = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        crate::test_seams::HANDOFF_PAUSE_BEFORE_WRITE,
        f.harness.inner.card_id.as_str(),
        hook.clone(),
    );
    let claim = request(&f);
    let harness = f.harness.clone();
    let submitted = claim.clone();
    let call = tokio::spawn(async move { harness.pause_for_handoff(submitted).await });
    hook.entered.notified().await;
    assert!(!call.is_finished());
    assert_eq!(f.stored().await.handoff_pause, PauseState::Unpaused);
    call.abort();
    let _ = call.await;
    hook.release.notify_one();
    f.harness.pause_for_handoff(claim.clone()).await.unwrap();
    assert_eq!(
        f.harness.query_handoff_pause(claim.clone()).await.unwrap(),
        PauseState::Paused {
            receipt: PauseReceipt::from_request(claim.clone())
        }
    );
    assert_eq!(
        f.harness
            .resume_from_handoff(PauseReceipt::from_request(claim))
            .await
            .unwrap(),
        crate::harness::handoff_pause::ReleaseOutcome::HealthyReleased
    );
    f.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn handoff_pause_recovery_cas_rejects_pre_pause_capture() {
    let f = Fixture::new().await;
    let row = f
        .repo
        .session_projection_by_id(&f.harness.inner.worker_session_id)
        .await
        .unwrap()
        .unwrap();
    let source = row.handle_state_json.clone().unwrap();
    let stale = f.stored().await;
    let receipt = f.harness.pause_for_handoff(request(&f)).await.unwrap();
    assert!(
        !crate::harness::persist_recovered_snapshot(
            f.repo.clone(),
            &row,
            &f.harness.inner.track_id,
            source,
            &stale
        )
        .await
        .unwrap()
    );
    assert_eq!(
        f.stored().await.handoff_pause,
        PauseState::Paused { receipt }
    );
}

#[tokio::test]
async fn handoff_pause_old_receipt_never_pauses_or_resumes_replacement_session() {
    use crate::db::sqlite::{session_mark_superseded_runtime_tx, session_start_runtime_tx};
    use crate::session_projection_repo::{
        AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
    };
    let mut f = Fixture::new().await;
    let old = request(&f);
    let receipt = f.harness.pause_for_handoff(old.clone()).await.unwrap();
    let mut replacement = params(&f, HarnessSnapshot::initial(0, vec![]));
    replacement.worker_session_id = crate::model::new_id();
    replacement.snapshot.phase = HarnessPhaseTag::Idle;
    replacement.snapshot.last_thread_id = Some("replacement-thread".into());
    let mut tx = f.repo.pool().begin().await.unwrap();
    session_mark_superseded_runtime_tx(&mut tx, &f.harness.inner.worker_session_id)
        .await
        .unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: replacement.worker_session_id.clone(),
            card_id: replacement.card_id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: replacement.snapshot.last_thread_id.clone(),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&replacement.snapshot).unwrap()),
            spawn_op_id: None,
            now_ms: crate::model::now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(
        f.harness
            .resume_from_handoff(receipt.clone())
            .await
            .is_err()
    );
    let (handle, _ingress) = PlannerHarness::run_unstarted_for_test(replacement, 8);
    f.harness = handle;
    assert_eq!(f.stored().await.handoff_pause, PauseState::Unpaused);
    assert!(f.harness.pause_for_handoff(old).await.is_err());
    assert!(f.harness.resume_from_handoff(receipt).await.is_err());
    system(&f, 13).await;
    f.issue().await;
    assert_eq!(f.daemon.turn_start_count_for_test(), 1);
}

#[tokio::test]
async fn handoff_pause_worker_sibling_keeps_running_and_system_failure_keeps_pause() {
    let f = Fixture::new().await;
    let worker = f.sibling(CardRole::Worker).await;
    let receipt = f.harness.pause_for_handoff(request(&f)).await.unwrap();
    worker
        .enqueue(vec![QueueEntry::user_message(
            "continue worker".into(),
            None,
            vec![],
        )])
        .await;
    worker.issue().await;
    assert_eq!(worker.daemon.turn_start_count_for_test(), 1);
    let before = f.stored().await;
    sqlx::query(concat!(
        "CREATE TRIGGER refuse_system BEFORE UPDATE OF handle_state_json ON worker_sessions BEGIN ",
        "SELECT RAISE(ABORT, 'system db failure'); END",
    ))
    .execute(f.repo.db().writer_pool())
    .await
    .unwrap();
    assert!(
        f.harness
            .observe_envelope(
                Observation::TrackGoal {
                    text: "system".into()
                },
                14
            )
            .await
            .is_err()
    );
    assert_eq!(f.stored().await, before);
    assert_eq!(
        f.harness.snapshot().await.handoff_pause,
        PauseState::Paused { receipt }
    );
    sqlx::query("DROP TRIGGER refuse_system")
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
    system(&f, 14).await;
    assert_eq!(f.stored().await.push_watermark, 14);
}

#[tokio::test]
async fn handoff_pause_invalid_or_foreign_persisted_marker_refuses_real_recovery() {
    for invalid in [
        serde_json::Value::Null,
        serde_json::json!({"state":"faulted_paused", "receipt": {
            "claim_id": crate::model::new_id(), "card_id":"other", "session_id":"foreign", "track_id":"track"
        }, "fault":{"kind":"thread_system_error"}}),
        serde_json::json!({"state":"paused", "receipt": {
            "claim_id": crate::model::new_id(), "card_id":"other", "session_id":"foreign", "track_id":"track"
        }}),
    ] {
        let f = Fixture::new().await;
        let mut raw = serde_json::to_value(f.stored().await).unwrap();
        raw["handoff_pause"] = invalid;
        sqlx::query("UPDATE worker_sessions SET handle_state_json=?1 WHERE id=?2")
            .bind(serde_json::to_string(&raw).unwrap())
            .bind(&f.harness.inner.worker_session_id)
            .execute(f.repo.db().writer_pool())
            .await
            .unwrap();
        let runtime = f
            .repo
            .session_projection_by_id(&f.harness.inner.worker_session_id)
            .await
            .unwrap()
            .unwrap();
        let registry = crate::harness::HarnessRegistry::new();
        assert!(
            crate::harness::spawn_recovered_harness(
                f.repo.clone(),
                f.harness.inner.events.clone(),
                f.harness.inner.card_role_cache.clone(),
                f.harness.inner.track_area_cache.clone(),
                f.daemon.clone(),
                f.daemon.thread_seals().clone(),
                &crate::harness::wiring::PlannerWiring::unconfigured_for_test(f.repo.clone()),
                &registry,
                &crate::per_card_lock::new_keyed_locks(),
                runtime,
                crate::harness::ClaimMode::Replace,
            )
            .await
            .is_err()
        );
        assert_eq!(f.daemon.turn_start_count_for_test(), 0);
    }
}
