//! Tables exercise the real command owners, not a copy of their policy.
#![cfg(test)]
use super::completed_commit_tests::Fixture;
use super::*;
use provider::control::{
    CompactRequest, CompactSubmission, EditPreparation, InterruptOutcome, NativeEditTarget,
    SubmissionEvidence, TurnIdentity, TurnRequest, TurnStartOutcome,
};

#[tokio::test]
async fn m1_mapping_steer_host_admission_and_native_requeue() {
    for case in [
        "taken",
        "refused",
        "unknown",
        "idle",
        "missing-thread",
        "missing-entry",
        "stale",
        "duplicate",
        "hold",
    ] {
        let f = Fixture::new().await;
        let inner = &f.harness.inner;
        let entry = QueueEntry::user_message("mapping input".into(), None, vec![]);
        let id = entry.id().unwrap().clone();
        inner
            .inputs
            .historical_queue()
            .unwrap()
            .lock()
            .await
            .push_back(entry.clone());
        *inner.state.lock().await = HarnessState::turn_running("running".into(), Instant::now());
        f.daemon
            .set_active_turn_for_test("thread-commit-wake", "running");
        let mut rev = 0;
        match case {
            "refused" => f.daemon.reject_turn_steer_for_test(Some("exact refusal")),
            "unknown" => f.daemon.fail_turn_steer_for_test(true),
            "idle" => *inner.state.lock().await = HarnessState::Idle,
            "missing-thread" => *inner.thread_id.write().await = None,
            "missing-entry" => inner.inputs.historical_queue().unwrap().lock().await.clear(),
            "stale" => rev = 1,
            "duplicate" => inner.inputs.historical_queue().unwrap().lock().await.push_back(entry.clone()),
            "hold" => f.daemon.emit_notification_for_test(crate::codex_appserver::Notification::Other {
                method: "error".into(), params: serde_json::json!({"error":{"message":"Your access token could not be refreshed because your refresh token was already used.","codexErrorInfo":"unauthorized"}}),
            }),
            _ => {}
        }
        let result = handle_steer(inner, &id, rev, &ActorId::User).await.unwrap();
        match (case, result) {
            ("taken", Ok(applied)) => {
                assert_eq!(applied.entry_id, id);
                assert_eq!(applied.rev, 0);
                assert_eq!(applied.turn_id, "running");
            }
            ("refused", Err(SteerRefused::NotTaken { message, phase })) => {
                assert_eq!(message, "exact refusal");
                assert_eq!(phase, HarnessPhaseTag::TurnRunning);
            }
            ("unknown", Err(SteerRefused::Unanswered { message, phase })) => {
                assert_eq!(
                    message,
                    CalmError::CodexAppServer("request turn/steer timed out".into()).to_string()
                );
                assert_eq!(phase, HarnessPhaseTag::TurnRunning);
            }
            ("idle" | "missing-thread", Err(SteerRefused::NoRunningTurn { .. })) => {}
            ("missing-entry", Err(SteerRefused::Queue(MutationRefused::NotFound))) => {}
            ("stale", Err(SteerRefused::Queue(MutationRefused::Stale { entry_id, rev, .. }))) => {
                assert_eq!(entry_id, id);
                assert_eq!(rev, 0);
            }
            ("duplicate", Err(SteerRefused::Queue(MutationRefused::AmbiguousId { count, .. }))) => {
                assert_eq!(count, 2)
            }
            ("hold", Err(SteerRefused::NotTaken { message, .. })) => {
                assert_eq!(Some(message), inner.backend.issuance_hold())
            }
            other => panic!("wrong mapping: {other:?}"),
        }
        let queue = inner.inputs.historical_queue().unwrap().lock().await;
        match case {
            "taken" | "missing-entry" => assert!(queue.is_empty()),
            "duplicate" => assert_eq!(queue.len(), 2),
            _ => assert_eq!(
                queue.front(),
                Some(&entry),
                "{case}: original entry and revision"
            ),
        }
        let called = matches!(case, "taken" | "refused" | "unknown");
        assert_eq!(
            f.daemon.steered_turns_for_test().len(),
            usize::from(called),
            "{case}"
        );
        let rows = inner
            .repo
            .transcript_rows_of_thread(inner.card_id.as_str(), "thread-commit-wake")
            .await
            .unwrap();
        assert_eq!(rows.len(), usize::from(case == "taken"), "{case}");
    }
}

#[tokio::test]
async fn m1_mapping_compact_host_admission_keeps_phase_and_never_calls_native() {
    for case in [
        "busy",
        "shutdown",
        "missing-thread",
        "sealed",
        "queued",
        "projection",
        "no-history",
        "archived",
        "edit",
        "active",
        "hold",
    ] {
        let f = Fixture::new().await;
        let inner = &f.harness.inner;
        *inner.last_turn_id.lock().await = Some("old".into());
        let expected = match case {
            "busy" => {
                *inner.state.lock().await =
                    HarnessState::turn_running("running".into(), Instant::now());
                "Wait for the current turn to finish before compacting."
            }
            "shutdown" => {
                inner.shutting_down.store(true, Ordering::SeqCst);
                "the conversation is shutting down"
            }
            "missing-thread" => {
                *inner.thread_id.write().await = None;
                "Send a message before compacting this conversation."
            }
            "sealed" => {
                f.daemon
                    .thread_seals()
                    .seal_for_deletion("thread-commit-wake");
                "the conversation is being deleted"
            }
            "queued" => {
                inner
                    .inputs
                    .historical_queue()
                    .unwrap()
                    .lock()
                    .await
                    .push_back(QueueEntry::user_message("pending".into(), None, vec![]));
                "Wait for pending messages and edits to finish before compacting."
            }
            "projection" => {
                *inner.projection_client_id.lock().await =
                    Some(QueueEntryId::from_wire("pending".into()));
                "Wait for pending messages and edits to finish before compacting."
            }
            "no-history" => {
                *inner.last_turn_id.lock().await = None;
                "Send a message before compacting this conversation."
            }
            "archived" => {
                sqlx::query("INSERT INTO assistant_conversation_archives(card_id, archived_at_ms) VALUES (?1, 1)")
                    .bind(inner.card_id.as_str()).execute(f.repo.pool()).await.unwrap();
                "This conversation is archived. Restore it before continuing."
            }
            "edit" => {
                *inner.pending_rewind.lock().await = Some(
                    serde_json::from_value(
                        serde_json::json!({"provider":"codex","before_turn_id":"old"}),
                    )
                    .unwrap(),
                );
                "Wait for pending messages and edits to finish before compacting."
            }
            "active" => {
                f.daemon
                    .set_active_turn_for_test("thread-commit-wake", "running");
                "Wait for pending messages and edits to finish before compacting."
            }
            _ => {
                f.daemon.emit_notification_for_test(crate::codex_appserver::Notification::Other {
                method: "error".into(), params: serde_json::json!({"error":{"message":"Your access token could not be refreshed because your refresh token was already used.","codexErrorInfo":"unauthorized"}}),
            });
                crate::codex_authentication::SIGN_IN_REQUIRED
            }
        };
        let expected = if case == "hold" {
            inner
                .backend
                .issuance_hold()
                .expect("the original host hold")
        } else {
            expected.to_owned()
        };
        let before = HarnessPhaseTag::from(&*inner.state.lock().await);
        assert!(
            matches!(f.harness.compact().await, Err(CalmError::Conflict(message)) if message == expected),
            "{case}"
        );
        assert_eq!(HarnessPhaseTag::from(&*inner.state.lock().await), before);
        assert!(inner.compact_outcome.lock().await.is_none());
        assert!(f.daemon.compacted_threads_for_test().is_empty());
    }
}

#[tokio::test]
async fn m1_mapping_codex_live_interrupt_and_sealed_compact_boundaries() {
    let f = Fixture::new().await;
    assert!(matches!(
        f.daemon
            .interrupt_active_outcome("thread-commit-wake")
            .await,
        provider::control::NativeInterruptOutcome::NoTarget
    ));
    for fail in [false, true] {
        f.daemon.fail_turn_interrupt_for_test(fail);
        f.daemon
            .set_active_turn_for_test("thread-commit-wake", "running");
        let outcome = IssueTurnHandle::from_reconciliation(&f.harness.inner)
            .await
            .turn_interrupt(InterruptRequest {
                turn: TurnIdentity::Native("running".into()),
            })
            .await;
        match &outcome {
            InterruptOutcome::NativeAck => assert!(!fail),
            InterruptOutcome::Unconfirmed { error, evidence } => {
                assert!(fail);
                assert!(error.downcast_ref::<CalmError>().is_some());
                assert_eq!(*evidence, SubmissionEvidence::Unobserved);
            }
            other => panic!("wrong mapping: {other:?}"),
        }
    }
    // The existing coarse classifier includes this local pre-RPC conflict. It must still wedge.
    f.daemon
        .thread_seals()
        .seal_for_deletion("thread-commit-wake");
    assert!(
        matches!(IssueTurnHandle::from_reconciliation(&f.harness.inner).await.compact_start(CompactRequest {
        selection: provider::TurnModelSelection::inherit(),
    }).await.unwrap(), CompactSubmission::Unconfirmed { error: CalmError::Conflict(message), evidence: SubmissionEvidence::Unobserved } if message == "the conversation is being deleted")
    );
}

#[tokio::test]
async fn m1_mapping_local_backend_host_refusals_and_interrupt_old_projection() {
    use crate::harness::profile::{HarnessProfile, PlannerBinding};
    use crate::harness::wiring::{PlannerRow, PlannerWiring};
    use crate::session_projection_repo::AgentProvider;
    for provider in [AgentProvider::Claude, AgentProvider::OpenCode] {
        let mut f = Fixture::new().await;
        let inner = &f.harness.inner;
        let wiring = PlannerWiring::unconfigured_for_test(f.repo.clone());
        let backend = crate::harness::backend::factory::PlannerFactory::new(
            std::sync::Arc::downgrade(&f.daemon),
            &wiring.claude,
            &wiring.acp,
        )
        .open(
            f.daemon.thread_seals().clone(),
            &wiring,
            f.repo.clone(),
            PlannerRow {
                binding: PlannerBinding {
                    profile: HarnessProfile::Planner,
                    provider: provider.clone(),
                },
                worker_session_id: inner.worker_session_id.as_str(),
                card_id: inner.card_id.as_str(),
                track_id: inner.track_id.as_str(),
                prior_total_tokens: 0,
            },
        )
        .await
        .unwrap();
        Arc::get_mut(&mut f.harness.inner)
            .expect("exclusive fixture owner")
            .backend = backend;
        let inner = &f.harness.inner;
        let backend = &inner.backend;
        if provider == AgentProvider::OpenCode {
            assert!(matches!(
                IssueTurnHandle::from_reconciliation(&f.harness.inner).await
            .prepare_rewind(NativeEditTarget::BeforeTurn {
                            before_turn_id: "turn".into()
                        }
                    )
                    .await
                    .unwrap(),
                EditPreparation::Failed { error: CalmError::Conflict(message) } if message == "ACP history replacement is unsupported"
            ));
        } else {
            let plan = super::super::rewind::RewindPlan {
                boundary: 0,
                last_row: 0,
                row_count: 0,
                previous_turn_id: None,
                previous_turn_rows: vec![],
                prompt_client_id: None,
            };
            assert!(
                matches!(backend.native_edit_target("turn", &plan), Err(CalmError::Conflict(message)) if message == "the first message of a Claude conversation cannot be edited: Claude has nothing before it to go back to")
            );
        }
        assert!(!PlannerBackend::metadata(&backend.provider()).steer);
        let identity = backend.turn_identity("local-turn".into());
        assert_eq!(
            identity,
            TurnIdentity::Local(CorrelationKey("local-turn".into()))
        );

        assert!(
            matches!(IssueTurnHandle::from_reconciliation(&f.harness.inner).await.turn_steer(SteerRequest { turn: identity.clone(), input: InputRequest { items: vec![], correlation: CorrelationKey("key".into()) } }).await,
            Err(CalmError::Internal(message)) if message == "a Claude Planner cannot steer; the run loop checks supports_steer first")
        );
        let request = backend.compact_request(inner.as_ref()).await;
        if provider == AgentProvider::Claude {
            let expected = crate::claude_planner::compact::refused(
                crate::claude_planner::config::unavailable_message(),
            )
            .to_string();
            assert!(matches!(request, Err(CalmError::BadRequest(_))));
            assert_eq!(request.unwrap_err().to_string(), expected);
        } else {
            assert!(
                matches!(IssueTurnHandle::from_reconciliation(&f.harness.inner).await.compact_start(request.unwrap()).await,
                Err(CalmError::BadRequest(message)) if message == "Manual context compaction is available for Codex and Claude conversations.")
            );
        }
        let outcome = IssueTurnHandle::from_reconciliation(&f.harness.inner)
            .await
            .turn_interrupt(InterruptRequest { turn: identity })
            .await;
        assert!(matches!(outcome, InterruptOutcome::NoTarget));
        assert!(f.daemon.compacted_threads_for_test().is_empty());
    }
}

#[tokio::test]
async fn m1_mapping_compact_failed_snapshot_restores_phase_and_previous_feedback() {
    use crate::harness::compact_outcome::{CompactOutcome, CompactStatus};
    for database_error in [false, true] {
        let f = Fixture::new().await;
        let inner = &f.harness.inner;
        *inner.last_turn_id.lock().await = Some("old".into());
        let previous = CompactOutcome {
            operation_id: "previous".into(),
            thread_id: "thread-commit-wake".into(),
            turn_id: Some("previous-maintenance".into()),
            status: CompactStatus::Completed,
        };
        *inner.compact_outcome.lock().await = Some(previous.clone());
        if database_error {
            sqlx::query(
                r#"CREATE TRIGGER fail_compact_snapshot
                   BEFORE UPDATE OF handle_state_json ON worker_sessions
                   BEGIN
                       SELECT RAISE(ABORT, 'snapshot diagnostic');
                   END"#,
            )
            .execute(f.repo.pool())
            .await
            .unwrap();
        } else {
            sqlx::query("UPDATE worker_sessions SET state='failed' WHERE id=?1")
                .bind(inner.worker_session_id.as_str())
                .execute(f.repo.pool())
                .await
                .unwrap();
        }
        let error = f.harness.compact().await.unwrap_err();
        if database_error {
            assert!(
                matches!(&error, CalmError::Internal(message) if message == "error returned from database: (code: 1811) snapshot diagnostic"),
                "{error:?}"
            );
        } else {
            assert!(
                matches!(error, CalmError::PlannerHarnessRuntimeSuperseded(message) if message == "The conversation changed; compaction was not submitted.")
            );
        }
        assert_eq!(
            HarnessPhaseTag::from(&*inner.state.lock().await),
            HarnessPhaseTag::Idle
        );
        assert_eq!(*inner.compact_outcome.lock().await, Some(previous));
        assert!(f.daemon.compacted_threads_for_test().is_empty());
    }
}

#[tokio::test]
async fn m1_mapping_interrupt_host_noops_never_issue_native_call() {
    for case in [
        "idle",
        "missing-thread",
        "no-known-turn",
        "wedged",
        "running-missing-thread",
    ] {
        let f = Fixture::new().await;
        let inner = &f.harness.inner;
        match case {
            "missing-thread" => {
                *inner.thread_id.write().await = None;
                *inner.state.lock().await = HarnessState::Issuing {
                    since: Instant::now(),
                    kind: IssuingKind::TurnStart,
                };
            }
            "no-known-turn" => {
                *inner.state.lock().await = HarnessState::Issuing {
                    since: Instant::now(),
                    kind: IssuingKind::TurnStart,
                }
            }
            "running-missing-thread" => {
                *inner.thread_id.write().await = None;
                *inner.state.lock().await =
                    HarnessState::turn_running("running".into(), Instant::now());
            }
            "wedged" => {
                *inner.state.lock().await = HarnessState::Wedged {
                    since: Instant::now(),
                    reason: "existing wedge".into(),
                }
            }
            "idle" => {}
            other => panic!("unknown case {other}"),
        }
        let before = HarnessPhaseTag::from(&*inner.state.lock().await);
        if case == "wedged" {
            issue_interrupt_for_turn(inner, "running".into(), "stop".into())
                .await
                .unwrap();
        } else {
            issue_interrupt(inner, "stop".into()).await.unwrap();
        }
        assert_eq!(HarnessPhaseTag::from(&*inner.state.lock().await), before);
        assert!(inner.interrupt_deadline.lock().await.is_none());
        assert!(f.daemon.interrupted_turns_for_test().is_empty());
    }
}

/// Two real harness owners share one daemon and identical native turn IDs.
#[tokio::test]
async fn m2c_two_owner_decoy_is_untouched_by_managed_operations() {
    let f = Fixture::new().await;
    let mut decoy = Fixture::new().await;
    let owner = &decoy.harness.inner;
    let backend = PlannerBackend::codex(
        f.daemon.clone(),
        owner.card_id.as_str(),
        owner.track_id.as_str(),
        &owner.worker_session_id,
    )
    .unwrap();
    Arc::get_mut(&mut decoy.harness.inner).unwrap().backend = backend;
    *decoy.harness.inner.thread_id.write().await = Some("decoy".into());
    let before = serde_json::to_value(decoy.harness.snapshot().await.unwrap()).unwrap();
    let selected = "thread-commit-wake";
    let turn = "fake-turn-0001";
    f.daemon.set_active_turn_for_test("decoy", turn);
    let EditPreparation::Prepared { plan } = IssueTurnHandle::from_reconciliation(&f.harness.inner)
        .await
        .prepare_rewind(NativeEditTarget::BeforeTurn {
            before_turn_id: "old-turn".into(),
        })
        .await
        .unwrap()
    else {
        panic!("edit not prepared")
    };
    let mut rewind = BackendRewind::from_plan(plan);
    let TurnStartOutcome::Started { turn: started } =
        IssueTurnHandle::from_reconciliation(&f.harness.inner)
            .await
            .turn_start(
                TurnRequest {
                    input: InputRequest {
                        items: vec![InputItem::text("bound start")],
                        correlation: CorrelationKey("projection".into()),
                    },
                    selection: TurnModelSelection::inherit(),
                    permission: PlannerPermissionMode::Never,
                    edit: rewind.turn_edit(),
                },
                &[],
                Some(&mut rewind),
            )
            .await
            .unwrap()
    else {
        panic!("bound start failed")
    };
    assert_eq!(started.into_string(), turn);
    assert_eq!(f.daemon.started_turns_for_test()[0].0, selected);
    let reverted = f.daemon.reverted_threads_for_test();
    assert_eq!(reverted.len(), 1);
    assert_eq!((&*reverted[0].0, &*reverted[0].1), (selected, "old-turn"));
    f.daemon.set_active_turn_for_test(selected, turn);
    assert!(matches!(
        IssueTurnHandle::from_reconciliation(&f.harness.inner)
            .await
            .turn_steer(SteerRequest {
                turn: TurnIdentity::Native(turn.into()),
                input: InputRequest {
                    items: vec![],
                    correlation: CorrelationKey("key".into())
                },
            })
            .await
            .unwrap(),
        SteerOutcome::Taken { .. }
    ));
    assert!(matches!(
        IssueTurnHandle::from_reconciliation(&f.harness.inner)
            .await
            .compact_start(CompactRequest {
                selection: TurnModelSelection::inherit(),
            })
            .await
            .unwrap(),
        CompactSubmission::Submitted { .. }
    ));
    assert!(matches!(
        IssueTurnHandle::from_reconciliation(&f.harness.inner)
            .await
            .turn_interrupt(InterruptRequest {
                turn: TurnIdentity::Native(turn.into()),
            })
            .await,
        InterruptOutcome::NativeAck
    ));
    // The fake interrupt clears its active cache; give shutdown its own live target.
    f.daemon.set_active_turn_for_test(selected, turn);
    f.harness.shutdown().await.unwrap();
    let steered = f.daemon.steered_turns_for_test();
    assert_eq!(steered.len(), 1);
    assert_eq!((&*steered[0].0, &*steered[0].1), (selected, turn));
    assert!(steered[0].2.is_empty());
    assert_eq!(steered[0].3.as_deref(), Some("key"));
    assert_eq!(f.daemon.compacted_threads_for_test(), vec![selected]);
    assert_eq!(
        f.daemon.interrupted_turns_for_test(),
        vec![
            (selected.into(), turn.into()),
            (selected.into(), turn.into())
        ]
    );
    assert_eq!(
        f.daemon.active_turn_id_for_thread("decoy").as_deref(),
        Some(turn)
    );
    assert_eq!(
        serde_json::to_value(decoy.harness.snapshot().await.unwrap()).unwrap(),
        before
    );
}

/// Ordinary replacement keeps its weak stop contract; strict deletion propagates the same error.
#[tokio::test]
async fn m2c_shutdown_error_projection_and_cached_before_persisted_fallback() {
    for cached in [false, true] {
        for strict in [false, true] {
            let f = Fixture::new().await;
            let thread = "thread-commit-wake";
            *f.harness.inner.last_turn_id.lock().await = Some("persisted-turn".into());
            if cached {
                f.daemon.set_active_turn_for_test(thread, "cached-turn");
            }
            f.daemon.fail_turn_interrupt_for_test(true);
            let result = if strict {
                f.harness
                    .shutdown_for_deletion(f.daemon.thread_seals().clone())
                    .await
                    .map(|_| ())
            } else {
                f.harness.shutdown().await
            };
            assert_eq!(result.is_err(), strict);
            if let Err(error) = result {
                assert!(matches!(error, CalmError::CodexAppServer(message)
                    if message == "fixture: turn/interrupt failed"));
            }
            assert_eq!(
                f.daemon.interrupted_turns_for_test(),
                vec![(
                    thread.into(),
                    if cached {
                        "cached-turn"
                    } else {
                        "persisted-turn"
                    }
                    .into()
                )]
            );
            assert!(
                !f.daemon.thread_seals().is_sealed(thread),
                "failed strict stop releases its seal"
            );
        }
    }
}
