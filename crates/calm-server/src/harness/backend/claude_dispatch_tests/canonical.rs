//! Real canonical transactions composed with the fake CLI at its process boundary.
mod compact;
mod control;
mod loop_fixture;
mod recovery;
mod retry;
mod settled;
use super::super::dispatch_fixture::fail_terminal_outcome;
use super::*;
use crate::auth::{AuthConfig, AuthState, SessionAuthority, SessionIdentity};
use crate::db::sqlite::begin_immediate_tx;
use crate::db::sqlite::conversation::*;
use calm_types::conversation::*;
use std::time::Duration;

pub(in crate::harness) struct Fixture {
    pub(in crate::harness) backend: PlannerBackend,
    pub(in crate::harness) session: Arc<ClaudePlannerSession>,
    dir: crate::test_support::execution::Directory,
    owner: super::super::dispatch_fixture::Owner,
    source: crate::auth::HumanInputSource,
    auth: AuthState,
    login: String,
    submission: InputSubmissionId,
    registry: crate::harness::HarnessRegistry,
}
impl Fixture {
    pub(in crate::harness) async fn new(scenario: &str) -> Self {
        Self::with_storage(scenario, FixtureStorage::Memory, true).await
    }
    async fn persistent(scenario: &str) -> Self {
        Self::with_storage(scenario, FixtureStorage::Persistent, true).await
    }
    async fn with_storage(scenario: &str, storage: FixtureStorage, boot: bool) -> Self {
        let (backend, session, dir, owner, _held) =
            fixture_full_with_storage(scenario, storage, boot).await;
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
            registry: crate::harness::HarnessRegistry::new(),
            backend,
            session,
            dir,
            owner,
            source,
            auth,
            login,
            submission: claimed.submission_id,
        }
    }
    pub(in crate::harness) async fn dispatch(
        &self,
    ) -> crate::error::Result<crate::harness::input_dispatch::CommittedInputReport> {
        self.dispatch_selection(&TurnModelSelection::inherit())
            .await
    }
    async fn dispatch_selection(
        &self,
        selection: &TurnModelSelection,
    ) -> crate::error::Result<crate::harness::input_dispatch::CommittedInputReport> {
        crate::harness::input_dispatch::initial(
            self.owner.repo.as_ref(),
            &self.source,
            &self.owner.card,
            &self.owner.worker,
            &self.submission,
            &self.backend,
            selection,
            PlannerPermissionMode::Never,
        )
        .await
    }
    pub(in crate::harness) async fn checkpoint(&self) -> ConversationInputCheckpoint {
        let mut tx = begin_immediate_tx(self.owner.repo.db()).await.unwrap();
        let carrier = current_input_carrier_tx(&mut tx, &self.owner.card, &self.owner.worker)
            .await
            .unwrap();
        let cp = carrier.checkpoint().clone();
        tx.commit().await.unwrap();
        cp
    }
    pub(in crate::harness) async fn target(&self) -> ExecutionEndTarget {
        let mut tx = begin_immediate_tx(self.owner.repo.db()).await.unwrap();
        let target = crate::harness::input_dispatch::execution_target_tx(
            &mut tx,
            &self.owner.card,
            &self.owner.worker,
            &AgentProvider::Claude,
        )
        .await
        .unwrap()
        .expect("exact local target before init");
        tx.commit().await.unwrap();
        target
    }
    pub(in crate::harness) async fn complete(&self) -> Option<RuntimeFrameAfterWrite> {
        crate::harness::input_dispatch::complete_once(
            self.owner.repo.as_ref(),
            &self.backend,
            &self.owner.card,
            &self.owner.worker,
        )
        .await
        .unwrap()
    }
    pub(in crate::harness) async fn wait_end(&self) -> RuntimeFrameAfterWrite {
        tokio::time::timeout(Duration::from_secs(10), async {
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
    fn no_stdin(&self) {
        assert!(
            !self.dir.path().join("stdin").exists(),
            "refused line reached CLI"
        );
    }
}

#[tokio::test]
async fn canonical_claude_start_checkpoints_before_first_line() {
    let fx = Fixture::new("exit").await;
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    sqlx::query(
        "CREATE TRIGGER fail_marker BEFORE UPDATE OF handle_state_json ON worker_sessions
         WHEN NEW.handle_state_json LIKE '%crossing_unknown%'
         BEGIN SELECT RAISE(ABORT,'marker failed'); END",
    )
    .execute(&mut **tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(fx.dispatch().await.is_err());
    fx.no_stdin();
    assert_eq!(fx.session.user_line_writes_for_test(), 0);
    assert!(fx.checkpoint().await.attempts.is_empty());
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_gate_rechecks_source_after_spawn_and_writer_wait() {
    // Logout is checked synchronously after the final async reads; the other
    // changes exercise the real protected transaction after writer-lock wait.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Change {
        Membership,
        Logout,
        History,
        Plan,
        Attempt,
    }
    for change in [
        Change::Logout,
        Change::Membership,
        Change::History,
        Change::Plan,
        Change::Attempt,
    ] {
        let fx = Arc::new(Fixture::new("exit").await);
        let pause = crate::claude_planner::session::SettlePause {
            entered: Arc::default(),
            release: Arc::default(),
        };
        if change == Change::Logout {
            let auth = fx.auth.clone();
            let login = fx.login.clone();
            fx.session.set_after_spawn_hook_for_test(Arc::new(move || {
                auth.sessions.remove(&login).unwrap();
            }));
        } else {
            fx.session.hold_user_writer_for_test(pause.clone());
        }
        let dispatch = tokio::spawn({
            let fx = fx.clone();
            async move { fx.dispatch().await }
        });
        if change != Change::Logout {
            tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
                .await
                .unwrap();
            let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
            match change {
                Change::Membership => {
                    let area = conversation_area_tx(&mut tx, &fx.owner.card).await.unwrap();
                    crate::db::sqlite::area_membership::application_user_register(
                        &mut tx, "alice", "Alice",
                    )
                    .await
                    .unwrap();
                    sqlx::query(
                        "UPDATE area_members SET role='member' WHERE area_id=?1 AND role='owner'",
                    )
                    .bind(area.id.as_str())
                    .execute(&mut **tx)
                    .await
                    .unwrap();
                    sqlx::query("INSERT INTO area_members(area_id,user_id,role,joined_at) VALUES(?1,'alice','owner',0)")
                        .bind(area.id.as_str()).execute(&mut **tx).await.unwrap();
                }
                Change::History => {
                    let area = conversation_area_tx(&mut tx, &fx.owner.card).await.unwrap();
                    sqlx::query("UPDATE areas SET conversation_history_contract='shared_history' WHERE id=?1")
                        .bind(area.id.as_str()).execute(&mut **tx).await.unwrap();
                }
                Change::Plan => {
                    // Frozen plans cannot be superseded by an UPDATE. The one
                    // permitted invalidation is private erasure of their source.
                    assert!(
                        sqlx::query(
                            "UPDATE conversation_input_submissions
                         SET render_plan_json=json_set(render_plan_json,
                             '$.segments[0].input.author.display_name','changed')
                         WHERE id=?1"
                        )
                        .bind(fx.submission.as_str())
                        .execute(&mut **tx)
                        .await
                        .is_err()
                    );
                    let refs = submission_plan_tx(&mut tx, &fx.submission)
                        .await
                        .unwrap()
                        .plan
                        .input_refs()
                        .unwrap();
                    let [AcceptedInputRef::Human { message }] = refs.as_slice() else {
                        panic!("human")
                    };
                    let erased =
                        PrivateHistoryPayload::<HumanPayload>::ErasedByPrivateHistoryOperation {
                            operation_id: PrivateHistoryOperationId::parse(&crate::model::new_id())
                                .unwrap(),
                        };
                    sqlx::query("UPDATE conversation_message_revisions SET payload_json=?1 WHERE message_id=?2")
                        .bind(serde_json::to_string(&erased).unwrap()).bind(message.message_id.as_str())
                        .execute(&mut **tx).await.unwrap();
                }
                Change::Attempt => {
                    let carrier =
                        current_input_fact_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
                            .await
                            .unwrap();
                    let mut frame = carrier.snapshot().clone();
                    frame["canonical_inputs"]["attempts"][0]["attempt"]["attempt_id"] =
                        serde_json::json!(crate::model::new_id());
                    crate::db::sqlite::session_set_handle_state_tx(
                        &mut tx,
                        &fx.owner.worker,
                        Some(frame),
                    )
                    .await
                    .unwrap();
                }
                Change::Logout => panic!("logout has no writer blocker"),
            }
            tx.commit().await.unwrap();
            pause.release.notify_one();
        }
        if let Ok(report) = dispatch.await.unwrap() {
            assert!(
                matches!(report.report.delivery(), InputDelivery::NotDispatched(_)),
                "change {change:?}"
            );
        }
        fx.no_stdin();
        assert_eq!(
            fx.session.user_line_writes_for_test(),
            1,
            "must reach the real writer"
        );
        fx.session.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn canonical_claude_early_finish_commits_one_original_end() {
    let fx = Arc::new(Fixture::new("exit").await);
    let pause = crate::claude_planner::session::SettlePause {
        entered: Arc::default(),
        release: Arc::default(),
    };
    fx.session.hold_dispatch_result_for_test(pause.clone());
    let mut events = fx.session.subscribe_events();
    let dispatch = tokio::spawn({
        let fx = fx.clone();
        async move { fx.dispatch().await }
    });
    tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if matches!(
                events.recv().await.unwrap().kind,
                provider::events::PlannerEventKind::TurnCompleted { .. }
            ) {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        fx.checkpoint().await.attempts[0].delivery,
        InputDeliveryFact::CrossingUnknown
    ));
    assert!(fx.complete().await.is_none());
    pause.release.notify_one();
    let report = dispatch.await.unwrap().unwrap();
    assert!(matches!(
        report.report.delivery(),
        InputDelivery::Unknown(_)
    ));
    assert!(report.report.outcome().is_ok());
    let before = fx.checkpoint().await;
    let frame = fx.wait_end().await;
    assert_eq!(frame.snapshot["phase"], "turn_completed");
    let after = fx.checkpoint().await;
    assert!(after.active.is_none());
    assert_eq!(after.attempts, before.attempts);
    assert!(matches!(
        after.attempts[0].delivery,
        InputDeliveryFact::Unknown
    ));
    assert!(fx.complete().await.is_none());
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_stop_waits_for_exact_driver_end() {
    let fx = Fixture::new("stall").await;
    let pause = crate::claude_planner::session::SettlePause {
        entered: Arc::default(),
        release: Arc::default(),
    };
    fx.session
        .set_before_turn_completed_pause_for_test(pause.clone());
    let report = fx.dispatch().await.unwrap();
    assert!(report.report.outcome().is_ok());
    let target = fx.target().await;
    fx.backend
        .interrupt_execution(
            &target,
            &crate::harness::native_admission::NativePromptAdmission::new(
                || Ok(()),
                || Box::pin(async { Ok(()) }),
            ),
        )
        .await
        .unwrap();
    assert!(fx.complete().await.is_none());
    tokio::time::timeout(Duration::from_secs(25), pause.entered.notified())
        .await
        .unwrap();
    assert!(fx.complete().await.is_none());
    assert!(fx.checkpoint().await.active.is_some());
    assert!(fx.dispatch().await.is_err());
    assert_eq!(fx.session.user_line_writes_for_test(), 1);
    pause.release.notify_one();
    fx.wait_end().await;
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_flush_failure_holds_one_attempt_without_second_spawn() {
    let fx = Arc::new(Fixture::new("exit").await);
    let pause = crate::claude_planner::session::SettlePause {
        entered: Arc::default(),
        release: Arc::default(),
    };
    fx.session.fail_user_flush_for_test(pause.clone());
    let harness = fx.start_loop().await;
    tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(line) = tokio::fs::read_to_string(fx.dir.path().join("stdin")).await
                && line.ends_with('\n')
            {
                assert!(line.contains("canonical human"));
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    pause.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(
                fx.checkpoint().await.attempts[0].delivery,
                InputDeliveryFact::Unknown
            ) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while fx.checkpoint().await.active.is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let cp = fx.checkpoint().await;
    assert_eq!(cp.attempts.len(), 1);
    assert!(cp.pending.is_empty());
    assert!(cp.active.is_none());
    assert!(fx.complete().await.is_none());
    for _ in 0..10 {
        harness.refresh_canonical_input().await.unwrap();
        assert_eq!(
            fx.checkpoint().await,
            cp,
            "END does not accept or retry Unknown"
        );
        tokio::task::yield_now().await;
    }
    assert!(fx.dispatch().await.is_err());
    assert_eq!(fx.session.user_line_writes_for_test(), 1);
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_running_and_stop_use_committed_local_target_before_init() {
    let fx = Fixture::new("stall").await;
    let harness = fx.start_loop().await;
    let running = fx.phase("turn_running").await;
    let fact = &running["canonical_inputs"]["attempts"][0];
    assert_eq!(fact["delivery"]["kind"], "unknown");
    assert!(fact["correlation"]["native_turn_id"].is_null());
    assert!(fact["correlation"]["native_session_id"].is_null());
    assert_eq!(
        running["last_turn_id"],
        fact["correlation"]["local_turn_id"]
    );
    harness.stop_canonical(fx.source.clone()).await.unwrap();
    let stopped = fx.phase("turn_completed").await;
    assert!(stopped["canonical_inputs"]["active"].is_null());
    assert_eq!(
        stopped["canonical_inputs"]["attempts"],
        running["canonical_inputs"]["attempts"]
    );
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_reply_uses_one_canonical_presentation() {
    let fx = Fixture::new("exit").await;
    let harness = fx.start_loop().await;
    fx.phase("turn_completed").await;
    // END qualification and content observation have separate consumers. Wait
    // for the actual projection, rather than treating phase as a content barrier.
    let sources = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let sources: Vec<(String, i64)> = sqlx::query_as("SELECT source_kind,count(*) FROM conversation_presentations WHERE card_id=?1 GROUP BY source_kind ORDER BY source_kind")
                .bind(&fx.owner.card).fetch_all(fx.owner.repo.pool()).await.unwrap();
            if sources.iter().any(|(kind, _count)| kind == "native_item") { break sources; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    assert_eq!(
        sources,
        vec![("human_message".into(), 1), ("native_item".into(), 1)]
    );
    let echoes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM harness_items WHERE card_id=?1 AND item_type='userMessage'",
    )
    .bind(&fx.owner.card)
    .fetch_one(fx.owner.repo.pool())
    .await
    .unwrap();
    assert_eq!(echoes, 0);
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_fact_persistence_failure_preserves_crossing_and_input() {
    let fx = Fixture::new("exit").await;
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    sqlx::query(
        "CREATE TRIGGER fail_fact BEFORE UPDATE OF handle_state_json ON worker_sessions
         WHEN json_extract(NEW.handle_state_json,
             '$.canonical_inputs.attempts[0].delivery.kind')='unknown'
         BEGIN SELECT RAISE(ABORT,'fact failed'); END",
    )
    .execute(&mut **tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(fx.dispatch().await.is_err());
    let cp = fx.checkpoint().await;
    assert!(cp.active.is_some());
    assert!(cp.pending.is_empty());
    assert!(matches!(
        cp.attempts[0].delivery,
        InputDeliveryFact::CrossingUnknown
    ));
    assert!(fx.dispatch().await.is_err());
    assert_eq!(fx.session.user_line_writes_for_test(), 1);
    assert!(fx.complete().await.is_none());
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_failed_driver_cleanup_never_qualifies_end() {
    #[derive(Clone, Copy)]
    enum Failure {
        Stop,
        Outcome,
        Instructions,
    }
    for failure in [Failure::Stop, Failure::Outcome, Failure::Instructions] {
        let fx = Fixture::new("exit").await;
        match failure {
            Failure::Stop => {
                std::fs::write(fx.dir.path().join("scenario"), "linger").unwrap();
                crate::test_support::execution::block_stops(fx.owner.repo.db()).await;
            }
            Failure::Outcome => {
                fail_terminal_outcome(fx.owner.repo.as_ref()).await;
            }
            Failure::Instructions => {
                let instructions = fx.session.host().instructions_dir.clone();
                fx.session.set_after_spawn_hook_for_test(Arc::new(move || {
                    for file in std::fs::read_dir(&instructions).unwrap() {
                        let path = file.unwrap().path();
                        if !path.is_file() {
                            continue;
                        }
                        std::fs::remove_file(&path).unwrap();
                        std::fs::create_dir(&path).unwrap();
                    }
                }));
            }
        }
        let report = fx.dispatch().await.unwrap();
        assert!(
            report.report.outcome().is_ok(),
            "{:?}",
            report.report.outcome()
        );
        assert!(!fx.session.wait_driver_for_test().await);
        assert!(
            calm_truth::db::sqlite::provider_execution::load(
                fx.owner.repo.pool(),
                &fx.owner.worker
            )
            .await
            .unwrap()
            .is_some()
        );
        assert!(fx.complete().await.is_none());
        assert!(fx.checkpoint().await.active.is_some());
    }
}

#[tokio::test]
async fn canonical_claude_resumed_session_keeps_local_identity_and_completes_current_end() {
    let mut fx = Fixture::new("exit").await;
    fx.dispatch().await.unwrap();
    let prior = fx.target().await;
    fx.wait_end().await;
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
        .await
        .unwrap();
    crate::conversation_inputs::accept_human_tx(
        &mut tx,
        &fx.source.current_principal().unwrap(),
        &mut carrier,
        HumanPayload {
            text: "second human".into(),
            bound_attachments: vec![],
        },
    )
    .await
    .unwrap();
    let plan = human_plan_for_pending_prefix_tx(
        &mut tx,
        &carrier,
        std::num::NonZeroU32::new(1).unwrap(),
        vec![],
    )
    .await
    .unwrap();
    fx.submission =
        crate::conversation_inputs::claim_initial_tx(&mut tx, &fx.source, &mut carrier, plan)
            .await
            .unwrap()
            .submission_id;
    tx.commit().await.unwrap();
    let report = fx.dispatch().await.unwrap();
    assert!(report.report.outcome().is_ok());
    assert!(matches!(
        report.report.delivery(),
        InputDelivery::Unknown(_)
    ));
    assert!(
        report
            .report
            .delivery()
            .correlation()
            .native_turn_id()
            .is_none()
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(argv) = tokio::fs::read_to_string(fx.dir.path().join("argv")).await
                && argv.lines().any(|arg| arg == "--resume")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the second actual CLI spawn must resume the validated native session");
    let current = fx.target().await;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if fx
                .backend
                .qualify_execution_end(&current)
                .await
                .unwrap()
                .is_some()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_ne!(current.scope, prior.scope);
    fx.wait_end().await;
    fx.session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_cancelled_local_start_holds_crossing_after_driver_install() {
    let fx = Arc::new(Fixture::new("exit").await);
    let pause = crate::claude_planner::session::SettlePause {
        entered: Arc::default(),
        release: Arc::default(),
    };
    fx.session.hold_dispatch_result_for_test(pause.clone());
    let dispatch = tokio::spawn({
        let fx = fx.clone();
        async move { fx.dispatch().await }
    });
    tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
        .await
        .unwrap();
    dispatch.abort();
    let Err(cancelled) = dispatch.await else {
        panic!("dispatch must be cancelled")
    };
    assert!(cancelled.is_cancelled());
    let cp = fx.checkpoint().await;
    assert!(cp.active.is_some());
    assert!(cp.pending.is_empty());
    assert!(matches!(
        cp.attempts[0].delivery,
        InputDeliveryFact::CrossingUnknown
    ));
    assert!(
        tokio::time::timeout(Duration::from_secs(10), fx.dispatch())
            .await
            .expect("a committed marker must refuse before another provider wait")
            .is_err()
    );
    assert!(fx.complete().await.is_none());
    assert_eq!(fx.session.user_line_writes_for_test(), 1);
    fx.session.shutdown().await.unwrap();
}
