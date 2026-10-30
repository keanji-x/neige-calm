//! Same worker/native identity, distinct process and fresh Human input, no resend.
use super::*;

impl Fixture {
    pub(super) async fn keep_unknown(&self, submission: &InputSubmissionId, key: &str) {
        let mut tx = begin_immediate_tx(self.owner.repo.db()).await.unwrap();
        let carrier = current_input_fact_carrier_tx(&mut tx, &self.owner.card, &self.owner.worker)
            .await
            .unwrap();
        let attempt = carrier.checkpoint().attempt(submission).unwrap();
        let request = KeepConversationInputRequest {
            carrier_id: self.owner.worker.clone(),
            submission_id: submission.as_str().into(),
            attempt_id: attempt.attempt.attempt_id.as_str().into(),
            revision: attempt.revision.to_string(),
        };
        tx.commit().await.unwrap();
        crate::conversation_inputs::resolution::keep(
            self.owner.repo.as_ref(),
            self.source.clone(),
            self.owner.card.clone(),
            key.into(),
            request,
        )
        .await
        .unwrap();
    }
    pub(super) async fn claim_successor(&self) -> InputSubmissionId {
        let mut tx = begin_immediate_tx(self.owner.repo.db()).await.unwrap();
        let mut carrier = current_input_carrier_tx(&mut tx, &self.owner.card, &self.owner.worker)
            .await
            .unwrap();
        let accepted = crate::conversation_inputs::accept_human_tx(
            &mut tx,
            &self.source.current_principal().unwrap(),
            &mut carrier,
            HumanPayload {
                text: "fresh successor".into(),
                bound_attachments: vec![],
            },
        )
        .await
        .unwrap();
        let AcceptedInputRef::Human { message } = accepted.input_ref else {
            panic!("Human");
        };
        let claimed = crate::conversation_inputs::claim_initial_tx(
            &mut tx,
            &self.source,
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
        claimed.submission_id
    }
    pub(super) async fn dispatch_submission(
        &self,
        submission: &InputSubmissionId,
    ) -> crate::harness::input_dispatch::CommittedInputReport {
        self.dispatch_submission_with_selection(
            submission,
            &TurnModelSelection {
                model: Some("fixture/model-a".into()),
                effort: Some("normal".into()),
            },
        )
        .await
    }
    pub(super) async fn dispatch_submission_with_selection(
        &self,
        submission: &InputSubmissionId,
        selection: &TurnModelSelection,
    ) -> crate::harness::input_dispatch::CommittedInputReport {
        crate::harness::input_dispatch::initial(
            self.owner.repo.as_ref(),
            &self.source,
            &self.owner.card,
            &self.owner.worker,
            submission,
            &self.backend,
            selection,
            PlannerPermissionMode::Never,
            &self.backend.handoff_for_test().await,
        )
        .await
        .unwrap()
    }
    pub(super) async fn receipt_bytes(
        &self,
        submission: &InputSubmissionId,
    ) -> (String, String, String, Option<String>) {
        sqlx::query_as("SELECT turn_id,input_json,state,outcome_json FROM acp_submissions WHERE worker_session_id=?1 AND client_id=?2").bind(&self.owner.worker).bind(submission.as_str()).fetch_one(self.owner.repo.pool()).await.unwrap()
    }
    /// Join the existing spy peer's prompt-consumed event before reading its request log.
    pub(super) async fn consume_held_prompt(
        &self,
        peer: &mut tokio::net::UnixStream,
        events: &mut tokio::sync::broadcast::Receiver<provider::events::PlannerEvent>,
    ) {
        use tokio::io::AsyncWriteExt;
        peer.write_all(b"!").await.unwrap();
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let event = events.recv().await.unwrap();
                if matches!(event.kind, provider::events::PlannerEventKind::ReplyDelta { ref delta, .. }
                    if delta == "held prompt received") {
                    assert_eq!(event.thread_id.as_deref(), Some(self.owner.thread.as_str()));
                    break;
                }
            }
        }).await.expect("the native peer must confirm prompt consumption");
    }
    pub(super) fn requests(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.dir.path().join("requests.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

#[tokio::test]
async fn canonical_acp_kept_unknown_loads_same_native_on_new_process_and_fences_again() {
    kept_unknown_loads_same_native_on_new_process(AgentProvider::OpenCode).await;
}

pub(super) async fn kept_unknown_loads_same_native_on_new_process(provider: AgentProvider) {
    let fx = Fixture::with_provider(provider, "lost", false).await;
    fx.dispatch().await.unwrap();
    fx.wait_idle().await;
    let before = fx.receipt_bytes(&fx.submission).await;
    fx.keep_unknown(&fx.submission, "keep-original").await;
    assert!(fx.backend.settle_kept_execution().await.unwrap().is_some());
    assert!(fx.backend.settle_kept_execution().await.unwrap().is_none());
    assert!(fx.complete().await.is_none());
    let successor = fx.claim_successor().await;
    fx.dispatch_submission(&successor).await;
    fx.wait_idle().await;
    assert!(fx.backend.settle_kept_execution().await.unwrap().is_none());
    assert!(
        crate::db::sqlite::acp_submission_unresolved(fx.owner.repo.pool(), &fx.owner.worker)
            .await
            .unwrap()
    );
    assert_eq!(before, fx.receipt_bytes(&fx.submission).await);
    let requests = fx.requests();
    let prompts: Vec<_> = requests
        .iter()
        .filter(|r| r["method"] == "session/prompt")
        .collect();
    assert_eq!(prompts.len(), 2);
    assert_ne!(prompts[0]["_pid"], prompts[1]["_pid"]);
    let native = &prompts[0]["params"]["sessionId"];
    assert_eq!(prompts[1]["params"]["sessionId"], *native);
    let attach = if fx.provider == AgentProvider::OpenCode {
        "session/resume"
    } else {
        "session/load"
    };
    let loaded: Vec<_> = requests.iter().filter(|r| r["method"] == attach).collect();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0]["params"]["sessionId"], *native);
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "session/new")
            .count(),
        1
    );
    // Each process negotiates afresh before its session operation: Grok authenticates every launch.
    let negotiation: &[&str] = match &fx.provider {
        AgentProvider::OpenCode => &["initialize"],
        AgentProvider::Grok => &["initialize", "authenticate"],
        AgentProvider::Codex | AgentProvider::Claude => panic!("not an ACP provider"),
    };
    for (prompt, operation) in [(prompts[0], "session/new"), (prompts[1], attach)] {
        let methods: Vec<_> = requests
            .iter()
            .filter(|r| r["_pid"] == prompt["_pid"])
            .filter_map(|r| r["method"].as_str())
            .collect();
        assert_eq!(methods[..negotiation.len()], *negotiation, "{methods:?}");
        assert_eq!(methods[negotiation.len()], operation, "{methods:?}");
    }
    // The original key still replays its immutable receipt with no third prompt.
    assert!(fx.dispatch().await.is_err());
    assert_eq!(fx.prompts(), 2);
    // A stale owner receipt cannot retire the successor's fence.
    assert!(fx.backend.settle_kept_execution().await.unwrap().is_none());
    fx.keep_unknown(&successor, "keep-successor").await;
    assert!(fx.backend.settle_kept_execution().await.unwrap().is_some());
    let third = fx.claim_successor().await;
    fx.dispatch_submission(&third).await;
    fx.wait_idle().await;
    let counts: (i64, i64) = sqlx::query_as(concat!(
        "SELECT count(*) FILTER (WHERE kept_cleanup_json IS NOT NULL), count(*) FILTER (WHERE ",
        "kept_cleanup_json IS NULL) FROM acp_submissions WHERE worker_session_id=?1",
    ))
    .bind(&fx.owner.worker)
    .fetch_one(fx.owner.repo.pool())
    .await
    .unwrap();
    assert_eq!(counts, (2, 1));
    assert_eq!(fx.prompts(), 3);
    assert_eq!(before, fx.receipt_bytes(&fx.submission).await);
    assert!(
        crate::db::sqlite::acp_submission_unresolved(fx.owner.repo.pool(), &fx.owner.worker)
            .await
            .unwrap()
    );
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_acp_keep_without_finished_cleanup_stays_held() {
    let fx = Fixture::new("lost").await;
    let pause = crate::acp_planner::test_seams::pause_after_fence(&fx.owner.worker);
    fx.dispatch().await.unwrap();
    pause.entered.notified().await;
    fx.keep_unknown(&fx.submission, "keep-unfinished").await;
    assert!(fx.backend.settle_kept_execution().await.unwrap().is_none());
    assert!(
        crate::db::sqlite::acp_submission_unresolved(fx.owner.repo.pool(), &fx.owner.worker)
            .await
            .unwrap()
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM acp_submissions WHERE kept_cleanup_json IS NOT NULL",
    )
    .fetch_one(fx.owner.repo.pool())
    .await
    .unwrap();
    assert_eq!(count, 0);
    pause.release.notify_one();
    fx.wait_idle().await;
    assert!(fx.backend.settle_kept_execution().await.unwrap().is_some());
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_acp_unsupported_load_and_registration_drift_refuse_without_new() {
    unsupported_load_and_registration_drift_refuse_without_new(AgentProvider::OpenCode).await;
}

pub(super) async fn unsupported_load_and_registration_drift_refuse_without_new(
    provider: AgentProvider,
) {
    for scenario in ["unsupported-load", "registration-drift"] {
        let fx = Fixture::with_provider(provider.clone(), "lost", false).await;
        fx.dispatch().await.unwrap();
        fx.wait_idle().await;
        fx.keep_unknown(&fx.submission, "keep").await;
        assert!(fx.backend.settle_kept_execution().await.unwrap().is_some());
        let before = fx.receipt_bytes(&fx.submission).await;
        let successor = fx.claim_successor().await;
        std::fs::write(fx.dir.path().join("scenario"), scenario).unwrap();
        if scenario == "registration-drift" {
            sqlx::query("UPDATE acp_managed_sessions SET registration_digest='different' WHERE worker_session_id=?1").bind(&fx.owner.worker).execute(fx.owner.repo.pool()).await.unwrap();
        }
        let report = fx.dispatch_submission(&successor).await;
        assert!(matches!(
            report.report.delivery(),
            InputDelivery::NotDispatched(_)
        ));
        let Err(TurnStartFailure::Refused { reader, .. }) = report.report.outcome() else {
            panic!("typed reader refusal required");
        };
        assert!(
            reader.contains(if scenario == "unsupported-load" {
                "cannot load"
            } else {
                "registration changed"
            }),
            "{reader}"
        );
        let requests = fx.requests();
        assert_eq!(
            requests
                .iter()
                .filter(|r| r["method"] == "session/new")
                .count(),
            1
        );
        assert_eq!(fx.prompts(), 1);
        assert_eq!(before, fx.receipt_bytes(&fx.submission).await);
        fx.session.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn canonical_acp_cleanup_commit_reopens_atomically_and_sql_guards_unknown() {
    let mut fx = Fixture::with_storage("lost", true).await;
    fx.dispatch().await.unwrap();
    fx.wait_idle().await;
    fx.keep_unknown(&fx.submission, "keep-reopen").await;
    let before = fx.receipt_bytes(&fx.submission).await;
    assert!(fx.backend.settle_kept_execution().await.unwrap().is_some());
    let evidence: String =
        sqlx::query_scalar("SELECT kept_cleanup_json FROM acp_submissions WHERE client_id=?1")
            .bind(fx.submission.as_str())
            .fetch_one(fx.owner.repo.pool())
            .await
            .unwrap();
    for set in [
        "kept_cleanup_json=NULL",
        "kept_cleanup_json='{}'",
        "state='completed',outcome_json='{}'",
        "turn_id='forged'",
        "input_json='{}'",
        "native_session_id='wrong'",
        "created_at_ms=0",
    ] {
        assert!(
            sqlx::query(&format!(
                "UPDATE acp_submissions SET {set} WHERE client_id=?1"
            ))
            .bind(fx.submission.as_str())
            .execute(fx.owner.repo.pool())
            .await
            .is_err(),
            "{set}"
        );
    }
    assert!(
        crate::db::sqlite::acp_submission_finish(
            fx.owner.repo.db(),
            &fx.owner.worker,
            fx.submission.as_str(),
            Some("{}")
        )
        .await
        .is_err()
    );
    // Strict reads reject malformed/future/missing/extra fields; no defaults.
    let parsed: crate::db::sqlite::AcpKeptCleanup = serde_json::from_str(&evidence).unwrap();
    let value = serde_json::to_value(parsed).unwrap();
    for key in value.as_object().unwrap().keys() {
        let mut malformed = value.clone();
        malformed.as_object_mut().unwrap().remove(key);
        assert!(
            serde_json::from_value::<crate::db::sqlite::AcpKeptCleanup>(malformed).is_err(),
            "{key}"
        );
    }
    for (key, invalid) in [("version", "future"), ("observation", "driver_finished")] {
        let mut malformed = value.clone();
        malformed[key] = serde_json::json!(invalid);
        assert!(serde_json::from_value::<crate::db::sqlite::AcpKeptCleanup>(malformed).is_err());
    }
    let mut malformed = value.clone();
    malformed["association"]["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<crate::db::sqlite::AcpKeptCleanup>(malformed).is_err());
    let mut malformed = value.clone();
    malformed["extra"] = serde_json::json!(true);
    assert!(serde_json::from_value::<crate::db::sqlite::AcpKeptCleanup>(malformed).is_err());
    fx.session.shutdown().await.unwrap();
    fx.owner.repo = Arc::new(
        crate::db::sqlite::SqlxRepo::open(&fx.owner.db_url)
            .await
            .unwrap(),
    );
    let (backend, session, _held) = session_for(&fx.owner, &fx.dir);
    fx.backend = backend;
    fx.session = session;
    fx.backend.recover_retained().await.unwrap();
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    let carrier = current_input_fact_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
        .await
        .unwrap();
    assert!(carrier.checkpoint().active.is_none());
    assert_eq!(carrier.phase().unwrap(), HarnessPhaseTag::Idle);
    tx.commit().await.unwrap();
    assert_eq!(before, fx.receipt_bytes(&fx.submission).await);
    std::fs::write(fx.dir.path().join("scenario"), "reply").unwrap();
    let successor = fx.claim_successor().await;
    fx.dispatch_submission(&successor).await;
    fx.wait_idle().await;
    assert_eq!(fx.prompts(), 2);
    let evidence_after: String =
        sqlx::query_scalar("SELECT kept_cleanup_json FROM acp_submissions WHERE client_id=?1")
            .bind(fx.submission.as_str())
            .fetch_one(fx.owner.repo.pool())
            .await
            .unwrap();
    assert_eq!(evidence, evidence_after);
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_acp_keep_after_restart_without_exact_driver_stays_held() {
    let fx = Fixture::with_storage("lost", true).await;
    fx.dispatch().await.unwrap();
    fx.wait_idle().await;
    fx.keep_unknown(&fx.submission, "keep-before-restart").await;
    fx.session.shutdown().await.unwrap();
    let (backend, session, _held) = session_for(&fx.owner, &fx.dir);
    backend.recover_retained().await.unwrap();
    assert!(backend.settle_kept_execution().await.unwrap().is_none());
    assert!(
        crate::db::sqlite::acp_submission_unresolved(fx.owner.repo.pool(), &fx.owner.worker)
            .await
            .unwrap()
    );
    assert_eq!(fx.prompts(), 1);
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_acp_keep_retires_only_its_exact_stop_intent() {
    use calm_types::harness::{
        HarnessControlIntent, HarnessInterruptionIntent, HarnessStopRequest,
    };
    for exact in [false, true] {
        let fx = Fixture::new("lost").await;
        fx.dispatch().await.unwrap();
        fx.wait_idle().await;
        fx.keep_unknown(&fx.submission, "keep-stopped").await;
        let before = fx.receipt_bytes(&fx.submission).await;
        let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
        let carrier = current_input_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
            .await
            .unwrap();
        let fence = RuntimeExecutionFence::from_carrier(&carrier);
        let mut header = fence.header().clone();
        header["phase"] = serde_json::to_value(HarnessPhaseTag::IssuingInterrupt).unwrap();
        header["last_turn_id"] = serde_json::json!(before.0);
        let intent = HarnessInterruptionIntent::Control(HarnessControlIntent::UserStopV1(
            HarnessStopRequest {
                session_id: fx.owner.worker.clone(),
                card_id: fx.owner.card.clone(),
                track_id: fx.owner.track.clone(),
                thread_id: fx.owner.thread.clone(),
                turn_id: if exact {
                    before.0.clone()
                } else {
                    uuid::Uuid::new_v4().to_string()
                },
                requested_at_ms: crate::model::now_ms(),
            },
        ));
        header["interruption_intent"] = serde_json::to_value(&intent).unwrap();
        persist_runtime_frame_tx(&mut tx, &fx.owner.card, &fx.owner.worker, &fence, header)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            fx.backend.settle_kept_execution().await.unwrap().is_some(),
            exact
        );
        let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
        let carrier = current_input_fact_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
            .await
            .unwrap();
        assert_eq!(carrier.checkpoint().active.is_none(), exact);
        if exact {
            assert_eq!(carrier.phase().unwrap(), HarnessPhaseTag::Idle);
            assert!(carrier.snapshot()["interruption_intent"].is_null());
        } else {
            assert_eq!(
                carrier.snapshot()["interruption_intent"],
                serde_json::to_value(&intent).unwrap()
            );
        }
        tx.commit().await.unwrap();
        assert_eq!(before, fx.receipt_bytes(&fx.submission).await);
        fx.session.shutdown().await.unwrap();
    }
}
