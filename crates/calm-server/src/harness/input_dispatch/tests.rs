mod failed_fact;
mod private_fence;
mod source_gate;

use super::*;
use crate::db::prelude::*;
use crate::db::sqlite::conversation::*;
use crate::db::sqlite::{SqlxRepo, begin_immediate_tx};
use crate::model::{CardRole, NewArea, NewCard, NewTrack, RequestTheme, new_id};
use crate::session_projection_repo::{WorkerSessionInit, WorkerSessionState};
use crate::shared_codex_appserver::{SharedCodexAppServer, TurnStartReturnHook};
use calm_types::conversation::*;
use calm_types::organization::LEGACY_OWNER_USER_ID;
use std::sync::Arc;
use std::time::Duration;

const BODY: &str = "unique human input, never a snapshot body";

struct Fixture {
    repo: Arc<SqlxRepo>,
    backend: PlannerBackend,
    daemon: Arc<SharedCodexAppServer>,
    card: String,
    runtime: String,
    submission: InputSubmissionId,
    message: MessageRevisionRef,
    auth: crate::auth::AuthState,
    session: String,
    source: crate::auth::HumanInputSource,
}

impl Fixture {
    async fn new() -> Self {
        Self::with_claim(true).await
    }

    async fn with_claim(claim: bool) -> Self {
        let auth = crate::auth::AuthState::new(crate::auth::AuthConfig {
            username: Some("Owner".into()),
            password: Some("fixture-password".into()),
            dev_autologin: false,
            display_name: "Owner".into(),
        });
        let session = auth.sessions.create(
            crate::auth::SessionIdentity::owner(&auth.config),
            crate::auth::SessionAuthority::PasswordLogin,
        );
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            format!("{}={session}", crate::auth::SESSION_COOKIE)
                .parse()
                .unwrap(),
        );
        let (status, source) = crate::auth::authenticated_source(auth.clone(), headers).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        let source = source.unwrap();
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let area = repo
            .area_create(
                LEGACY_OWNER_USER_ID,
                NewArea {
                    name: "canonical dispatch".into(),
                    color: "#123456".into(),
                    sort: None,
                },
            )
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                area_id: area.id,
                title: "canonical dispatch".into(),
                sort: None,
                cwd: "/tmp".into(),
                template_id: None,
                plugin_scope: None,
                template_input: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
        let card = crate::db::sqlite::card_create_with_id_tx(
            &mut tx,
            new_id(),
            NewCard {
                track_id: track.id,
                kind: "codex".into(),
                title: None,
                sort: None,
                payload: serde_json::json!({"planner_provider":"codex"}),
            },
            CardRole::Planner,
            true,
            repo.card_role_cache(),
        )
        .await
        .unwrap();
        let card = card.id.to_string();
        let runtime = new_id();
        crate::db::sqlite::session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit::shared_planner(
                runtime.clone(),
                card.clone(),
                AgentProvider::Codex,
                WorkerSessionState::Idle,
                Some("thread".into()),
                serde_json::to_value({
                    let mut frame =
                        calm_types::harness::snapshot::CanonicalHarnessSnapshot::dormant(0);
                    frame.header.last_thread_id = Some("thread".into());
                    frame
                })
                .unwrap(),
                0,
            ),
        )
        .await
        .unwrap();
        let mut carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
            .await
            .unwrap();
        let accepted = accept_pending_human_tx(
            &mut tx,
            &mut carrier,
            LEGACY_OWNER_USER_ID,
            HumanPayload {
                text: BODY.into(),
                bound_attachments: vec![],
            },
        )
        .await
        .unwrap();
        let AcceptedInputRef::Human { message } = accepted.input_ref else {
            panic!("human fixture")
        };
        let submission = if claim {
            crate::conversation_inputs::claim_initial_tx(
                &mut tx,
                &source,
                &mut carrier,
                SubmissionRenderPlan::V1 {
                    segments: vec![SubmissionRenderSegment::Input {
                        input_position: 0,
                        input: SubmissionRenderInput::Human {
                            message: message.clone(),
                            author: FrozenHumanAttribution {
                                registered_user_id: LEGACY_OWNER_USER_ID.into(),
                                display_name: "Owner".into(),
                            },
                        },
                    }],
                },
            )
            .await
            .unwrap()
            .submission_id
        } else {
            InputSubmissionId::parse("11111111111111111111111111111111").unwrap()
        };
        tx.commit().await.unwrap();
        let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
        let actual_card = repo.card_get(&card).await.unwrap().unwrap();
        let backend = PlannerBackend::codex(
            daemon.clone(),
            &card,
            actual_card.track_id.as_str(),
            &runtime,
        )
        .unwrap();
        Self {
            repo,
            backend,
            daemon,
            card,
            runtime,
            submission,
            message,
            auth,
            session,
            source,
        }
    }

    async fn dispatch(&self) -> Result<CommittedInputReport> {
        initial(
            self.repo.as_ref(),
            &self.source,
            &self.card,
            &self.runtime,
            &self.submission,
            &self.backend,
            &TurnModelSelection::inherit(),
            PlannerPermissionMode::Never,
        )
        .await
    }

    async fn checkpoint(&self) -> ConversationInputCheckpoint {
        let mut tx = begin_immediate_tx(self.repo.db()).await.unwrap();
        let carrier = current_input_carrier_tx(&mut tx, &self.card, &self.runtime)
            .await
            .unwrap();
        let value = carrier.checkpoint().clone();
        tx.commit().await.unwrap();
        value
    }
}

#[tokio::test]
async fn initial_keeps_pending_history_replacement_held_without_an_attempt() {
    for reverted in [false, true] {
        let fx = Fixture::new().await;
        let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
        let carrier = current_input_carrier_tx(&mut tx, &fx.card, &fx.runtime)
            .await
            .unwrap();
        let fence = RuntimeExecutionFence::from_carrier(&carrier);
        let mut header = fence.header().clone();
        header["pending_rewind"] =
            serde_json::to_value(calm_types::harness::snapshot::PersistedRewind::CodexV2 {
                thread_id: "thread".into(),
                before_turn_id: "prior".into(),
                reverted,
            })
            .unwrap();
        persist_runtime_frame_tx(&mut tx, &fx.card, &fx.runtime, &fence, header)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let before = fx.checkpoint().await;

        let Err(CalmError::Conflict(reason)) = fx.dispatch().await else {
            panic!("history replacement must remain held")
        };
        assert_eq!(
            reason,
            "canonical input cannot dispatch with pending history replacement"
        );
        assert_eq!(fx.checkpoint().await, before);
        assert!(before.attempt(&fx.submission).is_none());
        assert_eq!(fx.daemon.turn_start_count_for_test(), 0);
        assert!(fx.daemon.reverted_threads_for_test().is_empty());
    }
}

#[tokio::test]
async fn accepted_input_survives_execution_cleanup_error_and_cannot_dispatch_again() {
    let fx = Fixture::new().await;
    let hook = TurnStartReturnHook {
        entered: Arc::default(),
        release: Arc::default(),
    };
    fx.daemon
        .install_turn_start_return_hook_for_test(hook.clone());
    fx.daemon.fail_turn_interrupt_for_test(true);
    let fx = Arc::new(fx);
    let task = tokio::spawn({
        let fx = fx.clone();
        async move { fx.dispatch().await }
    });
    tokio::time::timeout(Duration::from_secs(5), hook.entered.notified())
        .await
        .unwrap();
    fx.daemon.thread_seals().seal_for_deletion("thread");
    hook.release.notify_one();
    let settled = task.await.unwrap().unwrap();
    assert!(settled.report.outcome().is_err());
    assert!(matches!(
        settled.report.delivery(),
        InputDelivery::Accepted(_)
    ));
    let checkpoint = fx.checkpoint().await;
    assert!(checkpoint.pending.is_empty());
    assert!(checkpoint.active.is_some());
    assert!(matches!(
        checkpoint.attempt(&fx.submission).unwrap().delivery,
        InputDeliveryFact::Accepted { .. }
    ));
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    let carrier = current_input_carrier_tx(&mut tx, &fx.card, &fx.runtime)
        .await
        .unwrap();
    assert!(
        !serde_json::to_string(carrier.snapshot())
            .unwrap()
            .contains(BODY)
    );
    tx.commit().await.unwrap();
    assert!(fx.dispatch().await.is_err());
    assert_eq!(fx.daemon.turn_start_count_for_test(), 1);
}

#[tokio::test]
async fn unanswered_start_keeps_exact_unknown_claim_instead_of_using_retry_policy() {
    let fx = Fixture::new().await;
    fx.daemon.fail_turn_start_for_test();
    let settled = fx.dispatch().await.unwrap();
    assert!(settled.report.outcome().is_err());
    assert!(matches!(
        settled.report.delivery(),
        InputDelivery::Unknown(_)
    ));
    let checkpoint = fx.checkpoint().await;
    assert!(checkpoint.pending.is_empty());
    assert!(checkpoint.active.is_some());
    assert!(matches!(
        checkpoint.attempt(&fx.submission).unwrap().delivery,
        InputDeliveryFact::Unknown
    ));
    // The fake counter records successful native starts, not rejected requests.
    // Let the peer accept now: an unsafe retry would become observable there.
    fx.daemon.clear_turn_start_failure_for_test();
    assert!(fx.dispatch().await.is_err());
    assert_eq!(fx.daemon.turn_start_count_for_test(), 0);
}

#[tokio::test]
async fn private_pre_send_proof_releases_only_the_original_claim_in_its_transaction() {
    let fx = Fixture::new().await;
    fx.daemon.thread_seals().seal_for_deletion("thread");
    let settled = fx.dispatch().await.unwrap();
    assert!(matches!(
        settled.report.delivery(),
        InputDelivery::NotDispatched(_)
    ));
    let checkpoint = fx.checkpoint().await;
    assert_eq!(checkpoint.pending.len(), 1);
    assert_eq!(
        checkpoint.pending[0].input_ref(),
        AcceptedInputRef::Human {
            message: fx.message.clone()
        }
    );
    assert!(checkpoint.active.is_none());
    assert_eq!(fx.daemon.turn_start_count_for_test(), 0);
}

#[tokio::test]
async fn cancellation_after_crossing_leaves_committed_unknown_and_never_resends() {
    let fx = Arc::new(Fixture::new().await);
    let hook = TurnStartReturnHook {
        entered: Arc::default(),
        release: Arc::default(),
    };
    fx.daemon
        .install_turn_start_return_hook_for_test(hook.clone());
    let task = tokio::spawn({
        let fx = fx.clone();
        async move { fx.dispatch().await }
    });
    tokio::time::timeout(Duration::from_secs(5), hook.entered.notified())
        .await
        .unwrap();
    task.abort();
    let Err(cancelled) = task.await else {
        panic!("dispatch must be cancelled")
    };
    assert!(cancelled.is_cancelled());
    let checkpoint = fx.checkpoint().await;
    assert!(checkpoint.pending.is_empty());
    assert!(matches!(
        checkpoint.attempt(&fx.submission).unwrap().delivery,
        InputDeliveryFact::CrossingUnknown
    ));
    hook.release.notify_one();
    assert!(fx.dispatch().await.is_err());
    assert_eq!(fx.daemon.turn_start_count_for_test(), 1);
}

#[tokio::test]
async fn human_only_erasure_is_rechecked_in_the_marker_transaction_before_native_effects() {
    let fx = Fixture::new().await;
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    let plan = submission_plan_tx(&mut tx, &fx.submission).await.unwrap();
    let proof_before = plan.original_digest.clone();
    // Model an already-persisted erasure fact; there is no canonical erase command.
    sqlx::query("UPDATE conversation_message_revisions SET payload_json=?1 WHERE message_id=?2")
        .bind(
            serde_json::to_string(
                &PrivateHistoryPayload::<HumanPayload>::ErasedByPrivateHistoryOperation {
                    operation_id: PrivateHistoryOperationId::parse(
                        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    )
                    .unwrap(),
                },
            )
            .unwrap(),
        )
        .bind((fx.message.message_id).as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
    assert_eq!(
        submission_plan_tx(&mut tx, &fx.submission)
            .await
            .unwrap()
            .original_digest,
        proof_before
    );
    tx.commit().await.unwrap();
    assert!(fx.dispatch().await.is_err());
    assert!(fx.checkpoint().await.attempt(&fx.submission).is_none());
    assert_eq!(fx.daemon.turn_start_count_for_test(), 0);
}

#[tokio::test]
async fn plan_hydrates_exact_revision_and_frozen_author_without_saving_another_body() {
    let fx = Fixture::new().await;
    let prepared = prepare(
        fx.repo.as_ref(),
        &fx.source,
        &fx.card,
        &fx.runtime,
        &fx.submission,
        AgentProvider::Codex,
        FrozenInputInvocation {
            model: Some("frozen-model".into()),
            effort: Some("high".into()),
            permission: PlannerPermissionMode::Never,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        prepared.proof.input_refs(),
        &[AcceptedInputRef::Human {
            message: fx.message.clone()
        }]
    );
    let InputItem::Text { text } = &prepared.items[0] else {
        panic!("text first")
    };
    assert_eq!(text, &format!("[Human \"Owner\"]\n{BODY}"));
    assert!(
        !serde_json::to_string(&prepared.proof)
            .unwrap()
            .contains(BODY)
    );
    assert!(
        !serde_json::to_string(&fx.checkpoint().await)
            .unwrap()
            .contains(BODY)
    );
    assert!(
        !serde_json::to_string(&fx.checkpoint().await)
            .unwrap()
            .contains(&fx.session)
    );
}

#[tokio::test]
async fn fresh_crossing_rechecks_actual_area_owner_instead_of_stored_provenance() {
    let fx = Fixture::new().await;
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    let area: String = sqlx::query_scalar(
        "SELECT t.area_id FROM tracks t JOIN cards c ON c.track_id=t.id WHERE c.id=?1",
    )
    .bind(&fx.card)
    .fetch_one(&mut **tx)
    .await
    .unwrap();
    crate::db::sqlite::area_membership::application_user_register(&mut tx, "alice", "Alice")
        .await
        .unwrap();
    sqlx::query("UPDATE area_members SET role='member' WHERE area_id=?1 AND role='owner'")
        .bind(&area)
        .execute(&mut **tx)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO area_members(area_id,user_id,role,joined_at) VALUES(?1,'alice','owner',0)",
    )
    .bind(&area)
    .execute(&mut **tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    assert!(fx.dispatch().await.is_err());
    assert!(fx.checkpoint().await.attempt(&fx.submission).is_none());
    assert_eq!(fx.daemon.turn_start_count_for_test(), 0);
}

#[tokio::test]
async fn claimed_work_without_current_session_cannot_begin_a_fresh_crossing() {
    let fx = Fixture::new().await;
    fx.auth.sessions.remove(&fx.session).unwrap();
    assert!(fx.dispatch().await.is_err());
    let checkpoint = fx.checkpoint().await;
    assert!(checkpoint.active.is_some());
    assert!(checkpoint.pending.is_empty());
    assert!(checkpoint.attempt(&fx.submission).is_none());
    assert_eq!(fx.daemon.turn_start_count_for_test(), 0);
}

#[tokio::test]
async fn sharing_before_a_fresh_crossing_holds_the_original_claim() {
    let fx = Fixture::new().await;
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    let area = conversation_area_tx(&mut tx, &fx.card).await.unwrap();
    sqlx::query("UPDATE areas SET conversation_history_contract='shared_history' WHERE id=?1")
        .bind(area.id.as_str())
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(fx.dispatch().await.is_err());
    let checkpoint = fx.checkpoint().await;
    assert!(checkpoint.active.is_some());
    assert!(checkpoint.attempt(&fx.submission).is_none());
    assert_eq!(fx.daemon.turn_start_count_for_test(), 0);
}

#[tokio::test]
async fn canonical_crossing_freezes_actual_invocation() {
    let fx = Fixture::new().await;
    let selection = TurnModelSelection {
        model: Some("actual-model".into()),
        effort: Some("high".into()),
    };
    initial(
        fx.repo.as_ref(),
        &fx.source,
        &fx.card,
        &fx.runtime,
        &fx.submission,
        &fx.backend,
        &selection,
        PlannerPermissionMode::Never,
    )
    .await
    .unwrap();
    assert_eq!(
        fx.daemon.started_turn_selections_for_test(),
        vec![("thread".into(), selection.clone())]
    );
    let checkpoint = fx.checkpoint().await;
    let stored = checkpoint.attempt(&fx.submission).unwrap();
    assert_eq!(
        stored.invocation,
        InputInvocation::Recorded {
            configuration: FrozenInputInvocation {
                model: selection.model,
                effort: selection.effort,
                permission: PlannerPermissionMode::Never,
            }
        }
    );
}
