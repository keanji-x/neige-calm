//! Canonical ACP entry exercises the real claim, receipt and native process owners.
#![cfg(test)]
mod carrier;
mod cleanup;
mod control;
mod failures;
mod recovery;
mod recovery_windows;
mod resolution;
mod transport;
use super::*;
use crate::auth::{AuthConfig, AuthState, SessionAuthority, SessionIdentity};
use crate::db::sqlite::{begin_immediate_tx, conversation::*};
use calm_types::conversation::*;
use std::time::Duration;

struct Fixture {
    backend: PlannerBackend,
    session: Arc<AcpPlannerSession>,
    owner: super::super::dispatch_fixture::Owner,
    dir: tempfile::TempDir,
    source: crate::auth::HumanInputSource,
    auth: AuthState,
    login: String,
    submission: InputSubmissionId,
}
impl Fixture {
    async fn new(scenario: &str) -> Self {
        Self::with_storage(scenario, false).await
    }
    async fn with_storage(scenario: &str, persistent: bool) -> Self {
        let (backend, session, owner, dir, _held) = fixture_storage(persistent).await;
        std::fs::write(dir.path().join("scenario"), scenario).unwrap();
        let auth = AuthState::new(AuthConfig {
            username: Some("Owner".into()),
            password: Some("fixture-password".into()),
            dev_autologin: false,
            display_name: "Owner".into(),
        });
        let login = auth.sessions.create(
            SessionIdentity::owner(&auth.config),
            SessionAuthority::PasswordLogin,
        );
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            format!("{}={login}", crate::auth::SESSION_COOKIE)
                .parse()
                .unwrap(),
        );
        let (_, source) = crate::auth::authenticated_source(auth.clone(), headers).await;
        let source = source.unwrap();
        let mut tx = begin_immediate_tx(owner.repo.db()).await.unwrap();
        let mut frame = crate::harness::snapshot::CanonicalHarnessSnapshot::dormant(0);
        frame.header.last_thread_id = Some(owner.thread.clone());
        crate::db::sqlite::session_set_handle_state_tx(
            &mut tx,
            &owner.worker,
            Some(serde_json::to_value(frame).unwrap()),
        )
        .await
        .unwrap();
        let mut carrier = current_input_carrier_tx(&mut tx, &owner.card, &owner.worker)
            .await
            .unwrap();
        let accepted = crate::conversation_inputs::accept_human_tx(
            &mut tx,
            &source.current_principal().unwrap(),
            &mut carrier,
            HumanPayload {
                text: "canonical human".into(),
                bound_attachments: vec![],
            },
        )
        .await
        .unwrap();
        let AcceptedInputRef::Human { message } = accepted.input_ref else {
            panic!("human")
        };
        let claimed = crate::conversation_inputs::claim_initial_tx(
            &mut tx,
            &source,
            &mut carrier,
            SubmissionRenderPlan::V1 {
                segments: vec![SubmissionRenderSegment::Input {
                    input_position: 0,
                    input: SubmissionRenderInput::Human {
                        message,
                        author: FrozenHumanAttribution {
                            registered_user_id: calm_types::organization::LEGACY_OWNER_USER_ID
                                .into(),
                            display_name: "Owner".into(),
                        },
                    },
                }],
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        Self {
            backend,
            session,
            owner,
            dir,
            source,
            auth,
            login,
            submission: claimed.submission_id,
        }
    }
    async fn dispatch(
        &self,
    ) -> crate::error::Result<crate::harness::input_dispatch::CommittedInputReport> {
        crate::harness::input_dispatch::initial(
            self.owner.repo.as_ref(),
            &self.source,
            &self.owner.card,
            &self.owner.worker,
            &self.submission,
            &self.backend,
            &TurnModelSelection::inherit(),
            PlannerPermissionMode::Never,
        )
        .await
    }
    async fn complete(&self) -> Option<RuntimeFrameAfterWrite> {
        crate::harness::input_dispatch::complete_once(
            self.owner.repo.as_ref(),
            &self.backend,
            &self.owner.card,
            &self.owner.worker,
        )
        .await
        .unwrap()
    }
    async fn wait_idle(&self) {
        tokio::time::timeout(Duration::from_secs(20), async {
            while self
                .session
                .active_turn_id_for_thread(&self.owner.thread)
                .is_some()
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    async fn wait_end(&self) -> RuntimeFrameAfterWrite {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Some(frame) = self.complete().await {
                    return frame;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }
    fn prompts(&self) -> usize {
        std::fs::read_to_string(self.dir.path().join("requests.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["method"]
                    == "session/prompt"
            })
            .count()
    }
}
#[tokio::test]
async fn canonical_acp_executes_through_body_free_receipt_and_exact_end() {
    for scenario in ["reply", "prompt-error", "lost"] {
        let fx = Fixture::new(scenario).await;
        let report = fx.dispatch().await.unwrap();
        assert!(
            matches!(report.report.delivery(), InputDelivery::Unknown(_)),
            "{scenario}"
        );
        assert!(report.report.outcome().is_ok());
        fx.wait_idle().await;
        let receipt: (String, String) = sqlx::query_as(
            "SELECT input_json,state FROM acp_submissions WHERE worker_session_id=?1",
        )
        .bind(&fx.owner.worker)
        .fetch_one(fx.owner.repo.pool())
        .await
        .unwrap();
        let input: serde_json::Value = serde_json::from_str(&receipt.0).unwrap();
        assert_eq!(input["version"], "canonical_v1");
        assert!(!receipt.0.contains("canonical human"));
        assert!(!receipt.0.contains("prompt"));
        assert_eq!(fx.prompts(), 1);
        if scenario == "lost" {
            assert_eq!(receipt.1, "unknown");
            assert!(fx.complete().await.is_none());
        } else {
            assert_eq!(receipt.1, "completed");
            if scenario == "prompt-error" {
                let outcome: String =
                    sqlx::query_scalar("SELECT outcome_json FROM acp_submissions")
                        .fetch_one(fx.owner.repo.pool())
                        .await
                        .unwrap();
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&outcome).unwrap()["status"],
                    "failed"
                );
            }
            fx.wait_end().await;
            assert!(fx.complete().await.is_none());
        }
        fx.session.shutdown().await.unwrap();
    }
}
#[tokio::test]
async fn canonical_acp_completed_receipt_cannot_end_before_cleanup_and_join() {
    let fx = Fixture::new("reply").await;
    let pause = crate::acp_planner::test_seams::pause_settlement(&fx.owner.worker);
    let report = fx.dispatch().await.unwrap();
    assert!(matches!(
        report.report.delivery(),
        InputDelivery::Unknown(_)
    ));
    tokio::time::timeout(Duration::from_secs(20), pause.entered.notified())
        .await
        .unwrap();
    assert!(fx.complete().await.is_none());
    pause.release.notify_one();
    fx.wait_idle().await;
    fx.wait_end().await;
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_acp_cancel_each_issue_handoff_retains_cleanup_and_never_resends() {
    use crate::acp_planner::test_seams::{IssueStage, pause_issue};
    for stage in [
        IssueStage::BeforeMint,
        IssueStage::AfterSpawn,
        IssueStage::BeforeReceipt,
        IssueStage::AfterReceipt,
        IssueStage::DriverInstalled,
    ] {
        let fx = Arc::new(Fixture::new("reply").await);
        let pause = pause_issue(&fx.owner.worker, stage);
        let mut dispatch = tokio::spawn({
            let fx = fx.clone();
            async move { fx.dispatch().await }
        });
        tokio::select! {
            _ = pause.entered.notified() => {},
            result = &mut dispatch => panic!("issue did not retain handoff {stage:?}: {}", result.unwrap().unwrap().report.outcome().is_ok()),
            _ = tokio::time::sleep(Duration::from_secs(20)) => panic!("missing handoff {stage:?}"),
        }
        dispatch.abort();
        match dispatch.await {
            Err(error) => assert!(error.is_cancelled()),
            Ok(_) => panic!("dispatch survived cancellation"),
        }
        fx.session.shutdown().await.unwrap();
        let before = fx.prompts();
        assert!(before <= 1);
        assert!(fx.dispatch().await.is_err());
        assert_eq!(fx.prompts(), before);
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM acp_submissions")
            .fetch_one(fx.owner.repo.pool())
            .await
            .unwrap();
        assert!(count <= 1);
        assert!(fx.complete().await.is_none());
    }
}

#[tokio::test]
async fn canonical_acp_replay_is_receipt_owned_for_completed_sending_and_unknown() {
    for scenario in ["reply", "lost", "hold"] {
        let fx = Fixture::new(scenario).await;
        let report = fx.dispatch().await.unwrap();
        assert!(matches!(
            report.report.delivery(),
            InputDelivery::Unknown(_)
        ));
        if scenario != "hold" {
            fx.wait_idle().await;
        }
        let receipt = crate::db::sqlite::acp_submission_get(
            fx.owner.repo.pool(),
            &fx.owner.worker,
            fx.submission.as_str(),
        )
        .await
        .unwrap()
        .unwrap();
        let input: serde_json::Value = serde_json::from_str(&receipt.input_json).unwrap();
        let logical: LogicalTurnId =
            serde_json::from_value(input["association"]["logical_turn_id"].clone()).unwrap();
        let proof: SubmissionDispatchProof =
            serde_json::from_value(input["association"]["proof"].clone()).unwrap();
        let before = fx.prompts();
        for changed in [false, true] {
            let mut proof = proof.clone();
            if changed {
                let SubmissionDispatchProof::V2 { attempt, .. } = &mut proof;
                attempt.attempt_id = InputAttemptId::parse(&crate::model::new_id()).unwrap();
            }
            let replay = fx
                .backend
                .dispatch_input(
                    None,
                    &logical,
                    proof.attempt(),
                    &fx.owner.thread,
                    &proof,
                    vec![InputItem::text("must never be sent")],
                    &TurnModelSelection::inherit(),
                    PlannerPermissionMode::Never,
                    &crate::harness::native_admission::NativePromptAdmission::new(
                        || Ok(()),
                        || Box::pin(async { Ok(()) }),
                    ),
                )
                .await;
            if changed {
                assert!(replay.outcome().is_err());
            } else {
                assert!(matches!(replay.delivery(), InputDelivery::Unknown(_)));
                assert_eq!(
                    replay.outcome().as_ref().unwrap().turn_id(),
                    receipt.turn_id
                );
            }
            assert_eq!(fx.prompts(), before);
        }
        fx.session.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn canonical_acp_fake_completed_receipt_does_not_replace_join_and_cleanup() {
    let fx = Fixture::new("reply").await;
    let pause = crate::acp_planner::test_seams::pause_after_fence(&fx.owner.worker);
    let report = fx.dispatch().await.unwrap();
    assert!(matches!(
        report.report.delivery(),
        InputDelivery::Unknown(_)
    ));
    tokio::time::timeout(Duration::from_secs(20), pause.entered.notified())
        .await
        .unwrap();
    let receipt = crate::db::sqlite::acp_submission_get(
        fx.owner.repo.pool(),
        &fx.owner.worker,
        fx.submission.as_str(),
    )
    .await
    .unwrap()
    .unwrap();
    crate::db::sqlite::acp_submission_finish(
        fx.owner.repo.db(),
        &fx.owner.worker,
        fx.submission.as_str(),
        Some(&serde_json::json!({"id":receipt.turn_id,"status":"completed"}).to_string()),
    )
    .await
    .unwrap();
    assert!(fx.complete().await.is_none());
    let admission = crate::harness::native_admission::NativePromptAdmission::new(
        || Ok(()),
        || Box::pin(async { Ok(()) }),
    );
    assert!(matches!(
        fx.backend
            .settled_activity(&fx.owner.card, &fx.owner.thread, &admission)
            .await
            .unwrap(),
        SettledActivity::Held
    ));
    pause.release.notify_one();
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_acp_source_refusal_before_receipt_has_no_native_effect() {
    let (backend, session, owner, dir) = fixture().await;
    let checked = std::sync::atomic::AtomicBool::new(false);
    let proof = super::super::dispatch_tests::proof();
    let report = backend
        .dispatch_input(
            None,
            &super::super::dispatch_tests::logical_turn_id(),
            proof.attempt(),
            &owner.thread,
            &proof,
            vec![InputItem::text("refused")],
            &TurnModelSelection::inherit(),
            PlannerPermissionMode::Never,
            &crate::harness::native_admission::NativePromptAdmission::new(
                || Ok(()),
                || {
                    Box::pin(async {
                        checked.store(true, std::sync::atomic::Ordering::SeqCst);
                        Err(CalmError::Forbidden("source revoked".into()))
                    })
                },
            ),
        )
        .await;
    assert!(checked.load(std::sync::atomic::Ordering::SeqCst));
    assert!(matches!(report.delivery(), InputDelivery::NotDispatched(_)));
    assert!(!dir.path().join("requests.jsonl").exists());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM acp_submissions")
        .fetch_one(owner.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_acp_receipt_revalidates_claim_and_final_source_after_commit() {
    use crate::acp_planner::test_seams::{IssueStage, pause_issue};
    for after_receipt in [false, true] {
        let fx = Arc::new(Fixture::new("reply").await);
        let pause = pause_issue(
            &fx.owner.worker,
            if after_receipt {
                IssueStage::AfterReceipt
            } else {
                IssueStage::BeforeReceipt
            },
        );
        let mut dispatch = tokio::spawn({
            let fx = fx.clone();
            async move { fx.dispatch().await }
        });
        tokio::select! {
            _ = pause.entered.notified() => {},
            _ = &mut dispatch => panic!("canonical issue refused before reaching receipt boundary"),
            _ = tokio::time::sleep(Duration::from_secs(20)) => panic!("receipt boundary missing"),
        }
        if after_receipt {
            fx.auth.sessions.remove(&fx.login).unwrap();
        } else {
            let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
            let mut carrier = current_input_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
                .await
                .unwrap();
            let prior = carrier
                .checkpoint()
                .attempt(&fx.submission)
                .unwrap()
                .clone();
            record_input_attempt_fact_tx(
                &mut tx,
                &mut carrier,
                InputAttemptFact {
                    attempt: prior.attempt.clone(),
                    correlation: prior.correlation,
                    delivery: InputDeliveryFact::NotDispatched {
                        basis: "concurrent factual refusal".into(),
                    },
                },
            )
            .await
            .unwrap();
            release_submission_tx(&mut tx, &mut carrier, &prior.attempt)
                .await
                .unwrap();
            tx.commit().await.unwrap();
        }
        pause.release.notify_one();
        let report = dispatch.await.unwrap();
        if after_receipt {
            assert!(matches!(
                report.unwrap().report.delivery(),
                InputDelivery::Unknown(_)
            ));
            fx.wait_idle().await;
            let state: String = sqlx::query_scalar("SELECT state FROM acp_submissions")
                .fetch_one(fx.owner.repo.pool())
                .await
                .unwrap();
            assert_eq!(state, "unknown");
        } else if let Ok(report) = report {
            assert!(matches!(
                report.report.delivery(),
                InputDelivery::NotDispatched(_)
            ));
        }
        assert_eq!(fx.prompts(), 0);
        if !after_receipt {
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM acp_submissions")
                .fetch_one(fx.owner.repo.pool())
                .await
                .unwrap();
            assert_eq!(count, 0);
        }
        assert!(fx.complete().await.is_none());
        fx.session.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn canonical_acp_reopened_receipt_never_resends_lost_report_or_certifies_missing_driver() {
    use crate::acp_planner::test_seams::{IssueStage, pause_issue};
    for lost_report in [false, true] {
        let fx = Arc::new(Fixture::with_storage("reply", true).await);
        if lost_report {
            let pause = pause_issue(&fx.owner.worker, IssueStage::AfterReceipt);
            let dispatch = tokio::spawn({
                let fx = fx.clone();
                async move { fx.dispatch().await }
            });
            tokio::time::timeout(Duration::from_secs(20), pause.entered.notified())
                .await
                .unwrap();
            dispatch.abort();
            match dispatch.await {
                Err(error) => assert!(error.is_cancelled()),
                Ok(_) => panic!("dispatch survived cancellation"),
            }
        } else {
            let report = fx.dispatch().await.unwrap();
            assert!(matches!(
                report.report.delivery(),
                InputDelivery::Unknown(_)
            ));
            fx.wait_idle().await;
        }
        fx.session.shutdown().await.unwrap();
        let repo = Arc::new(
            crate::db::sqlite::SqlxRepo::open(&fx.owner.db_url)
                .await
                .unwrap(),
        );
        let receipt = crate::db::sqlite::acp_submission_get(
            repo.pool(),
            &fx.owner.worker,
            fx.submission.as_str(),
        )
        .await
        .unwrap()
        .unwrap();
        let crate::db::sqlite::AcpSubmissionInput::Canonical(input) =
            crate::db::sqlite::AcpSubmissionInput::decode(&receipt.input_json).unwrap()
        else {
            panic!("canonical receipt")
        };
        let owner = super::super::dispatch_fixture::Owner {
            repo,
            card: fx.owner.card.clone(),
            track: fx.owner.track.clone(),
            worker: fx.owner.worker.clone(),
            thread: fx.owner.thread.clone(),
            db_url: fx.owner.db_url.clone(),
        };
        let (backend, session, _held) = session_for(&owner, &fx.dir);
        let before = fx.prompts();
        let report = backend
            .dispatch_input(
                None,
                &input.logical_turn_id,
                input.proof.attempt(),
                &owner.thread,
                &input.proof,
                vec![InputItem::text("never resend")],
                &TurnModelSelection::inherit(),
                PlannerPermissionMode::Never,
                &crate::harness::native_admission::NativePromptAdmission::new(
                    || Ok(()),
                    || Box::pin(async { Ok(()) }),
                ),
            )
            .await;
        assert!(matches!(report.delivery(), InputDelivery::Unknown(_)));
        assert_eq!(
            report.outcome().as_ref().unwrap().turn_id(),
            receipt.turn_id
        );
        let target = ExecutionEndTarget {
            card_id: owner.card.clone(),
            track_id: owner.track.clone(),
            worker_session_id: owner.worker.clone(),
            logical_turn_id: input.logical_turn_id,
            proof: input.proof,
            scope: ExecutionEndScope::ReceiptProcess {
                thread_id: uuid::Uuid::parse_str(&owner.thread).unwrap(),
                local_turn_id: uuid::Uuid::parse_str(&receipt.turn_id).unwrap(),
                native_session_id: receipt.native_session_id.clone(),
            },
        };
        assert!(!backend.owns_local_execution(&target).await);
        assert!(
            backend
                .qualify_execution_end(&target)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(fx.prompts(), before);
        let stored = crate::db::sqlite::acp_submission_get(
            owner.repo.pool(),
            &owner.worker,
            fx.submission.as_str(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(stored.input_json, receipt.input_json);
        assert_eq!(stored.turn_id, receipt.turn_id);
        session.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn canonical_acp_rejects_wrong_owner_proof_local_turn_and_native_session() {
    let fx = Fixture::new("reply").await;
    fx.dispatch().await.unwrap();
    fx.wait_idle().await;
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    let target = crate::harness::input_dispatch::execution_target_tx(
        &mut tx,
        &fx.owner.card,
        &fx.owner.worker,
        &AgentProvider::OpenCode,
    )
    .await
    .unwrap()
    .unwrap();
    tx.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while fx
            .backend
            .qualify_execution_end(&target)
            .await
            .unwrap()
            .is_none()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    #[derive(Clone, Copy, Debug)]
    enum Field {
        Card,
        Track,
        Worker,
        Logical,
        Proof,
        Thread,
        Turn,
        Native,
    }
    for field in [
        Field::Card,
        Field::Track,
        Field::Worker,
        Field::Logical,
        Field::Proof,
        Field::Thread,
        Field::Turn,
        Field::Native,
    ] {
        let mut wrong = target.clone();
        match field {
            Field::Card => wrong.card_id = "wrong".into(),
            Field::Track => wrong.track_id = "wrong".into(),
            Field::Worker => wrong.worker_session_id = "wrong".into(),
            Field::Logical => {
                wrong.logical_turn_id = LogicalTurnId::parse(&crate::model::new_id()).unwrap()
            }
            Field::Proof => {
                let SubmissionDispatchProof::V2 { attempt, .. } = &mut wrong.proof;
                attempt.attempt_id = InputAttemptId::parse(&crate::model::new_id()).unwrap();
            }
            Field::Thread => {
                let ExecutionEndScope::ReceiptProcess { thread_id, .. } = &mut wrong.scope else {
                    panic!("local")
                };
                *thread_id = uuid::Uuid::new_v4();
            }
            Field::Turn => {
                let ExecutionEndScope::ReceiptProcess { local_turn_id, .. } = &mut wrong.scope
                else {
                    panic!("local")
                };
                *local_turn_id = uuid::Uuid::new_v4();
            }
            Field::Native => {
                let ExecutionEndScope::ReceiptProcess {
                    native_session_id, ..
                } = &mut wrong.scope
                else {
                    panic!("receipt process")
                };
                *native_session_id = "wrong-native".into();
            }
        }
        assert!(
            fx.backend
                .qualify_execution_end(&wrong)
                .await
                .unwrap()
                .is_none(),
            "{field:?}"
        );
    }
    sqlx::query("UPDATE worker_sessions SET agent_session_id='wrong-native' WHERE id=?1")
        .bind(&fx.owner.worker)
        .execute(fx.owner.repo.pool())
        .await
        .unwrap();
    assert!(
        fx.backend
            .qualify_execution_end(&target)
            .await
            .unwrap()
            .is_none()
    );
    assert!(fx.complete().await.is_none());
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_acp_historical_entry_cannot_execute_canonical_storage() {
    let fx = Fixture::new("reply").await;
    let result = fx
        .session
        .start_turn(
            &fx.owner.thread,
            provider::control::TurnRequest {
                input: provider::control::InputRequest {
                    items: vec![InputItem::text("historical")],
                    correlation: provider::control::CorrelationKey("historical-key".into()),
                },
                selection: TurnModelSelection::inherit(),
                permission: PlannerPermissionMode::Never,
                edit: provider::control::TurnEdit::NoEdit,
            },
            &[],
        )
        .await;
    assert!(result.is_err());
    assert_eq!(fx.prompts(), 0);
    assert!(!fx.dir.path().join("requests.jsonl").exists());
    fx.session.shutdown().await.unwrap();
}
