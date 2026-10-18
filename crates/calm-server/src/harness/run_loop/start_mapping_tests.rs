//! Tables drive the real facade and issue handle with the existing deterministic daemon.
#![cfg(test)]
use super::completed_commit_tests::Fixture;
use super::*;
use crate::harness::backend::{StartDiagnostic, ordinary_start_result};
use provider::control::*;

fn request(rewind: Option<&BackendRewind>) -> TurnRequest {
    TurnRequest {
        input: InputRequest {
            items: vec![
                InputItem::text("exact input"),
                InputItem::local_image("/image.png"),
            ],
            correlation: CorrelationKey("exact-client".into()),
        },
        selection: TurnModelSelection {
            model: Some("exact-model".into()),
            effort: Some("high".into()),
        },
        permission: PlannerPermissionMode::Never,
        edit: rewind.map_or(TurnEdit::NoEdit, BackendRewind::turn_edit),
    }
}

#[tokio::test]
async fn m1_mapping_start_facade_and_stored_edit_table() {
    for case in [
        "started",
        "refused",
        "unanswered",
        "revert-refused",
        "revert-start-failed",
        "applied",
        "mismatched",
        "hold",
        "sealed",
    ] {
        let f = Fixture::new().await;
        let mut rewind = match case {
            "revert-refused" | "revert-start-failed" => Some(serde_json::from_str::<BackendRewind>(r#"{"provider":"codex","before_turn_id":"old-turn"}"#).unwrap()),
            "applied" => Some(serde_json::from_str(r#"{"provider":"codex","before_turn_id":"old-turn","reverted":true}"#).unwrap()),
            "mismatched" => Some(serde_json::from_str(r#"{"provider":"claude","resume_at":"00000000-0000-0000-0000-000000000001","drops_turn":"00000000-0000-0000-0000-000000000002"}"#).unwrap()),
            _ => None,
        };
        match case {
            "refused" => f.daemon.reject_turn_start_for_test(),
            "unanswered" | "revert-start-failed" => f.daemon.fail_turn_start_for_test(),
            "revert-refused" => f.daemon.answer_revert_turn_not_found_for_test(true),
            "hold" => f.daemon.emit_notification_for_test(crate::codex_appserver::Notification::Other {
                method: "error".into(), params: serde_json::json!({"error":{"message":"Your access token could not be refreshed because your refresh token was already used.","codexErrorInfo":"unauthorized"}}),
            }),
            "sealed" => { f.daemon.thread_seals().seal_for_deletion("thread-commit-wake"); },
            _ => {},
        }
        let result = IssueTurnHandle::from_reconciliation(&f.harness.inner)
            .await
            .turn_start(request(rewind.as_ref()), &[], rewind.as_mut())
            .await;
        match (case, &result) {
            (
                "started" | "applied",
                Ok(TurnStartOutcome::Started {
                    turn: TurnIdentity::Native(turn),
                }),
            ) => assert_eq!(turn, "fake-turn-0001"),
            (
                "refused",
                Ok(TurnStartOutcome::Rejected {
                    error:
                        StartDiagnostic {
                            error: CalmError::CodexRefused(message),
                            reader,
                        },
                }),
            ) => {
                assert_eq!(message, "turn/start failed: unknown model (code -32602)");
                assert_eq!(
                    reader,
                    "codex refused to start a turn for this conversation, so \
                     your message has not been sent. If you changed the model \
                     recently, it may not be one this account can run — try \
                     another."
                );
            }
            (
                "revert-refused",
                Ok(TurnStartOutcome::Rejected {
                    error: StartDiagnostic { reader, .. },
                }),
            ) => assert_eq!(
                reader,
                "codex refused to remove the edited turn from this conversation, so your \
                 message has not been sent. Reset the conversation to continue."
            ),
            ("unanswered" | "revert-start-failed", Ok(TurnStartOutcome::Failed { .. })) => {}
            (
                "mismatched",
                Err(TurnStartFailure::Refused {
                    error: CalmError::Internal(message),
                    reader,
                }),
            ) => {
                assert_eq!(message, "a pending rewind names another provider");
                assert_eq!(
                    reader,
                    "this conversation holds an edit for another provider, so your message has not \
                     been sent. Reset the conversation to continue."
                );
            }
            ("sealed", Err(TurnStartFailure::Transient(CalmError::Conflict(message)))) => {
                assert_eq!(
                    message,
                    "thread thread-commit-wake is sealed because its track is being deleted"
                )
            }
            (
                "hold",
                Err(TurnStartFailure::AwaitingRecovery {
                    error: CalmError::ServiceUnavailable(error),
                    reader,
                }),
            ) => {
                assert_eq!(error, reader);
                assert!(reader.to_lowercase().contains("sign in"));
                assert!(reader.contains("Your message is still queued"));
                assert!(
                    !reader.contains("try another")
                        && !reader.contains("refresh token was already used")
                );
            }
            other => panic!("wrong start row: {other:?}"),
        }
        let old = ordinary_start_result(result);
        assert_eq!(old.is_ok(), matches!(case, "started" | "applied"));
        if case == "revert-start-failed" {
            let stored = serde_json::to_string(rewind.as_ref().unwrap()).unwrap();
            assert_eq!(
                stored,
                r#"{"provider":"codex","before_turn_id":"old-turn","reverted":true}"#
            );
            let mut restored: BackendRewind = serde_json::from_str(&stored).unwrap();
            f.daemon.clear_turn_start_failure_for_test();
            ordinary_start_result(
                IssueTurnHandle::from_reconciliation(&f.harness.inner)
                    .await
                    .turn_start(request(Some(&restored)), &[], Some(&mut restored))
                    .await,
            )
            .unwrap();
            assert_eq!(f.daemon.reverted_threads_for_test().len(), 1);
        }
        if matches!(case, "started" | "applied") {
            assert!(f.daemon.reverted_threads_for_test().is_empty());
            assert_eq!(
                f.daemon.started_turn_client_ids_for_test(),
                vec![Some("exact-client".into())]
            );
            assert_eq!(
                f.daemon.started_turn_selections_for_test()[0].1,
                request(None).selection
            );
        }
    }
}

#[tokio::test]
async fn m1_mapping_codex_edit_preparation_and_issue_handle() {
    let f = Fixture::new().await;
    let backend = &f.harness.inner.backend;
    let target = backend.native_edit_target("old-turn", None, None).unwrap();
    let EditPreparation::Prepared { plan } = IssueTurnHandle::from_reconciliation(&f.harness.inner)
        .await
        .prepare_rewind(target)
        .await
        .unwrap()
    else {
        panic!("not prepared")
    };
    let mut rewind = BackendRewind::from_plan(plan);
    assert_eq!(
        serde_json::to_string(&rewind).unwrap(),
        r#"{"provider":"codex","before_turn_id":"old-turn"}"#
    );
    assert!(f.daemon.reverted_threads_for_test().is_empty());
    let selection = TurnModelSelection::inherit();
    IssueTurnHandle::from_reconciliation(&f.harness.inner)
        .await
        .issue(
            vec![InputItem::text("through issue")],
            &selection,
            PlannerPermissionMode::Never,
            "projection",
            &[],
            Some(&mut rewind),
        )
        .await
        .unwrap();
    assert_eq!(f.daemon.reverted_threads_for_test().len(), 1);
}

#[tokio::test]
async fn m1_mapping_start_post_ack_fence_preserves_interrupt_error_precedence() {
    for interrupt_fails in [false, true] {
        let f = Fixture::new().await;
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        f.daemon.install_turn_start_return_hook_for_test(
            crate::shared_codex_appserver::TurnStartReturnHook {
                entered: entered.clone(),
                release: release.clone(),
            },
        );
        f.daemon.fail_turn_interrupt_for_test(interrupt_fails);
        let start = IssueTurnHandle::from_reconciliation(&f.harness.inner)
            .await
            .turn_start(request(None), &[], None);
        let (result, ()) = tokio::join!(start, async {
            entered.notified().await;
            f.daemon
                .thread_seals()
                .seal_for_deletion("thread-commit-wake");
            release.notify_one();
        });
        match (interrupt_fails, result) {
            (false, Err(TurnStartFailure::Transient(CalmError::Conflict(message)))) => assert_eq!(
                message,
                "thread thread-commit-wake was sealed while turn/start was in flight"
            ),
            (
                true,
                Ok(TurnStartOutcome::Failed {
                    error: CalmError::CodexAppServer(message),
                    ..
                }),
            ) => assert_eq!(message, "fixture: turn/interrupt failed"),
            other => panic!("wrong post-start fence {other:?}"),
        }
        assert_eq!(f.daemon.turn_start_count_for_test(), 1);
        assert_eq!(
            f.daemon.interrupted_turns_for_test(),
            vec![("thread-commit-wake".into(), "fake-turn-0001".into())]
        );
    }
}

#[tokio::test]
async fn m1_mapping_codex_signin_fact_is_captured_at_authentication_owner() {
    let f = Fixture::new().await;
    f.daemon.emit_notification_for_test(crate::codex_appserver::Notification::Other {
        method: "error".into(), params: serde_json::json!({"error":{"message":"Your access token could not be refreshed because your refresh token was already used.","codexErrorInfo":"unauthorized"}}),
    });
    let result = f
        .daemon
        .turn_start_control(
            "thread-commit-wake",
            vec![],
            &TurnModelSelection::inherit(),
            crate::codex_appserver::TurnApprovals::Unchanged,
            None,
        )
        .await;
    assert!(
        matches!(result, Err(crate::shared_codex_appserver::control::StartError::RecoveryRequired(CalmError::CodexRefused(message))) if message == crate::codex_authentication::SIGN_IN_REFUSAL)
    );
    assert_eq!(f.daemon.turn_start_count_for_test(), 0);
}

#[tokio::test]
async fn m1_mapping_local_start_admission_and_mismatched_rewind_table() {
    use crate::claude_planner::wiring::{ClaudePlannerRow, ClaudePlannerWiring};
    use crate::harness::profile::{HarnessProfile, PlannerBinding};
    use crate::session_projection_repo::AgentProvider;
    for provider in [AgentProvider::Claude, AgentProvider::OpenCode] {
        let mut f = Fixture::new().await;
        let inner = &f.harness.inner;
        let wiring = ClaudePlannerWiring::unconfigured_for_test(f.repo.clone());
        let backend = crate::harness::backend::factory::PlannerFactory::new(
            std::sync::Arc::downgrade(&f.daemon),
            &wiring.claude,
            &wiring.acp,
        )
        .open(
            f.daemon.thread_seals().clone(),
            &wiring,
            f.repo.clone(),
            ClaudePlannerRow {
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
        for json in [
            None,
            Some(r#"{"provider":"codex","before_turn_id":"old","reverted":false}"#),
            Some(r#"{"provider":"codex","before_turn_id":"old","reverted":true}"#),
        ] {
            let mut rewind = json.map(|json| serde_json::from_str::<BackendRewind>(json).unwrap());
            let result = IssueTurnHandle::from_reconciliation(&f.harness.inner)
                .await
                .turn_start(request(rewind.as_ref()), &[], rewind.as_mut())
                .await;
            match (json, &provider, result) {
                (
                    Some(_),
                    _,
                    Err(TurnStartFailure::Refused {
                        error: CalmError::Internal(message),
                        ..
                    }),
                ) => assert_eq!(message, "a pending rewind names another provider"),
                (
                    None,
                    AgentProvider::Claude,
                    Err(TurnStartFailure::Transient(CalmError::Conflict(message))),
                ) => assert_eq!(message, "claude planner harness is not installed yet"),
                (
                    None,
                    AgentProvider::OpenCode,
                    Err(TurnStartFailure::Refused {
                        reader,
                        error: CalmError::Conflict(message),
                    }),
                ) => {
                    assert_eq!(reader, "ACP harness has not been installed");
                    assert_eq!(message, reader);
                }
                other => panic!("wrong host admission {other:?}"),
            }
        }
        assert_eq!(f.daemon.turn_start_count_for_test(), 0);
    }
}

#[tokio::test]
async fn ready_factory_construction_and_open_leave_first_turn_work_lazy() {
    use crate::claude_planner::wiring::{ClaudePlannerRow, ClaudePlannerWiring};
    use crate::harness::backend::factory::PlannerFactory;
    use crate::harness::profile::{HarnessProfile, PlannerBinding};
    use crate::session_projection_repo::AgentProvider;

    let f = Fixture::new().await;
    let inner = &f.harness.inner;
    let wiring = ClaudePlannerWiring::unconfigured_for_test(f.repo.clone());
    let before: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT id, mcp_token_hash FROM worker_sessions ORDER BY id")
            .fetch_all(f.repo.pool())
            .await
            .unwrap();
    let credentials: i64 = sqlx::query_scalar("SELECT count(*) FROM card_mcp_tokens")
        .fetch_one(f.repo.pool())
        .await
        .unwrap();
    let factory = PlannerFactory::new(Arc::downgrade(&f.daemon), &wiring.claude, &wiring.acp);
    assert!(f.daemon.started_thread_params_for_test().is_empty());
    assert_eq!(f.daemon.turn_start_count_for_test(), 0);
    for provider in [
        AgentProvider::Codex,
        AgentProvider::Claude,
        AgentProvider::OpenCode,
    ] {
        // The local hosts are deliberately unconfigured: open must not attempt readiness or
        // native first-turn setup, which would refuse. Installation/issuance own that work.
        let backend = factory
            .open(
                f.daemon.thread_seals().clone(),
                &wiring,
                f.repo.clone(),
                ClaudePlannerRow {
                    binding: PlannerBinding {
                        profile: HarnessProfile::Planner,
                        provider,
                    },
                    worker_session_id: inner.worker_session_id.as_str(),
                    card_id: inner.card_id.as_str(),
                    track_id: inner.track_id.as_str(),
                    prior_total_tokens: 0,
                },
            )
            .await
            .unwrap();
        drop(backend);
    }
    let after: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT id, mcp_token_hash FROM worker_sessions ORDER BY id")
            .fetch_all(f.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        after, before,
        "construction/open must not mint session credentials"
    );
    let after_credentials: i64 = sqlx::query_scalar("SELECT count(*) FROM card_mcp_tokens")
        .fetch_one(f.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        after_credentials, credentials,
        "no early card credential mint"
    );
    assert!(
        f.daemon.started_thread_params_for_test().is_empty(),
        "no early native thread/start"
    );
    assert_eq!(
        f.daemon.turn_start_count_for_test(),
        0,
        "no early turn/start"
    );
}

#[tokio::test]
async fn m2c_mixed_legacy_and_versioned_input_uses_the_bound_start() {
    let f = Fixture::new().await;
    let legacy = QueueEntry::LegacyUser {
        text: "legacy accepted sentence".into(),
        envelope_id: None,
        message_ids: vec![],
    };
    let versioned = QueueEntry::user_message("versioned accepted sentence".into(), None, vec![]);
    let id = versioned.id().unwrap().clone();
    f.enqueue(vec![legacy.clone(), versioned.clone()]).await;
    let stored = f.stored().await;
    assert_eq!(stored.pending_entries(), vec![legacy, versioned]);
    let restored = HarnessSnapshot::parse_known(serde_json::to_value(stored).unwrap()).unwrap();
    let entries = restored.pending_entries();
    assert!(
        matches!(&entries[0], QueueEntry::LegacyUser { message_ids, .. } if message_ids.is_empty())
    );
    assert_eq!(entries[0].id(), None);
    assert_eq!(entries[1].id(), Some(&id));
    f.issue().await;
    let started = f.daemon.started_turns_for_test();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].0, "thread-commit-wake");
    let InputItem::Text { text } = &started[0].1[0] else {
        panic!("text input")
    };
    assert!(
        text.find("legacy accepted sentence").unwrap()
            < text.find("versioned accepted sentence").unwrap()
    );
    assert!(f.stored().await.pending_entries().is_empty());
}
