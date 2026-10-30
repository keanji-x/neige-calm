//! Recover native identity from real, validated replay evidence after a lost init bind.
use super::*;

#[tokio::test]
async fn canonical_claude_identity_recovery_resumes_after_bind_failure_and_restart() {
    let mut fx = Fixture::persistent("exit").await;
    // Fail the production init bind without blocking the independent replay acknowledgement.
    sqlx::query(
        "CREATE TRIGGER fail_native_bind BEFORE UPDATE OF agent_session_id ON worker_sessions
        WHEN NEW.agent_session_id IS NOT NULL
        BEGIN SELECT RAISE(ABORT,'bind failed'); END",
    )
    .execute(fx.owner.repo.pool())
    .await
    .unwrap();
    fx.dispatch().await.unwrap();
    fx.wait_end().await;
    let before = fx.checkpoint().await;
    assert!(matches!(&before.attempts[0].delivery,
        InputDeliveryFact::Accepted { basis, .. } if basis == "claude_user_replay"));
    let bound: Option<String> =
        sqlx::query_scalar("SELECT agent_session_id FROM worker_sessions WHERE id=?")
            .bind(&fx.owner.worker)
            .fetch_one(fx.owner.repo.pool())
            .await
            .unwrap();
    assert_eq!(bound, None);
    fx.session.shutdown().await.unwrap();
    sqlx::query("DROP TRIGGER fail_native_bind")
        .execute(fx.owner.repo.pool())
        .await
        .unwrap();
    let (backend, session) = super::recovery::reopen_session(&fx, fx.owner.repo.clone()).await;
    fx.backend = backend;
    fx.session = session;
    assert_eq!(
        fx.checkpoint().await,
        before,
        "identity recovery does not alter delivery facts"
    );

    next_input(&mut fx).await;
    fx.dispatch().await.unwrap();
    fx.wait_end().await;
    let argv = std::fs::read_to_string(fx.dir.path().join("argv")).unwrap();
    assert!(argv.lines().any(|arg| arg == "--resume"), "{argv}");
    assert!(!argv.lines().any(|arg| arg == "--session-id"), "{argv}");
    let bound: Option<String> =
        sqlx::query_scalar("SELECT agent_session_id FROM worker_sessions WHERE id=?")
            .bind(&fx.owner.worker)
            .fetch_one(fx.owner.repo.pool())
            .await
            .unwrap();
    assert_eq!(bound.as_deref(), Some(fx.owner.thread.as_str()));
    fx.session.shutdown().await.unwrap();
}

async fn next_input(fx: &mut Fixture) {
    let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
    let mut carrier = current_input_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
        .await
        .unwrap();
    let accepted = crate::conversation_inputs::accept_human_tx(
        &mut tx,
        &fx.source.current_principal().unwrap(),
        &mut carrier,
        HumanPayload {
            text: "following human input".into(),
            bound_attachments: vec![],
        },
    )
    .await
    .unwrap();
    let AcceptedInputRef::Human { message } = accepted.input_ref else {
        panic!("Human")
    };
    let claimed = crate::conversation_inputs::claim_initial_tx(
        &mut tx,
        &fx.source,
        &mut carrier,
        SubmissionRenderPlan::V1 {
            segments: vec![SubmissionRenderSegment::Input {
                input_position: 0,
                input: SubmissionRenderInput::Human {
                    message,
                    author: FrozenHumanAttribution {
                        registered_user_id: calm_types::organization::LEGACY_OWNER_USER_ID.into(),
                        display_name: "Owner".into(),
                    },
                },
            }],
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    fx.submission = claimed.submission_id;
}

pub(super) async fn binding(fx: &Fixture) -> Option<String> {
    sqlx::query_scalar("SELECT agent_session_id FROM worker_sessions WHERE id=?")
        .bind(&fx.owner.worker)
        .fetch_one(fx.owner.repo.pool())
        .await
        .unwrap()
}

async fn fail_bind(fx: &Fixture) {
    sqlx::query(
        "CREATE TRIGGER fail_native_bind BEFORE UPDATE OF agent_session_id ON worker_sessions
        WHEN NEW.agent_session_id IS NOT NULL
        BEGIN SELECT RAISE(ABORT,'bind failed'); END",
    )
    .execute(fx.owner.repo.pool())
    .await
    .unwrap();
}

#[tokio::test]
async fn canonical_claude_identity_recovery_preserves_later_unknown_without_retry() {
    let mut fx = Fixture::persistent("exit").await;
    fail_bind(&fx).await;
    fx.dispatch().await.unwrap();
    fx.wait_end().await;
    next_input(&mut fx).await;
    std::fs::write(fx.dir.path().join("scenario"), "no-input-replay").unwrap();
    fx.dispatch().await.unwrap();
    fx.wait_end().await;
    let before = fx.checkpoint().await;
    assert!(matches!(
        before.attempts[0].delivery,
        InputDeliveryFact::Accepted { .. }
    ));
    assert!(matches!(
        before.attempts[1].delivery,
        InputDeliveryFact::Unknown
    ));
    assert_eq!(binding(&fx).await, None);
    fx.session.shutdown().await.unwrap();
    sqlx::query("DROP TRIGGER fail_native_bind")
        .execute(fx.owner.repo.pool())
        .await
        .unwrap();
    let spawns = std::fs::read_to_string(fx.dir.path().join("spawns")).unwrap();
    let (_backend, session) = super::recovery::reopen_session(&fx, fx.owner.repo.clone()).await;
    assert_eq!(
        binding(&fx).await.as_deref(),
        Some(fx.owner.thread.as_str())
    );
    assert_eq!(fx.checkpoint().await, before);
    assert_eq!(
        std::fs::read_to_string(fx.dir.path().join("spawns")).unwrap(),
        spawns,
        "identity recovery must not retry uncertain input"
    );
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_claude_identity_recovery_requires_verified_replay() {
    for scenario in [
        "no-input-replay",
        "wrong-input-replay",
        "receipt-before-init",
        "wrong-replay-session",
    ] {
        let fx = Fixture::persistent(scenario).await;
        fail_bind(&fx).await;
        fx.dispatch().await.unwrap();
        fx.wait_end().await;
        let before = fx.checkpoint().await;
        assert!(
            matches!(before.attempts[0].delivery, InputDeliveryFact::Unknown),
            "{scenario}"
        );
        assert_eq!(binding(&fx).await, None);
        fx.session.shutdown().await.unwrap();
        sqlx::query("DROP TRIGGER fail_native_bind")
            .execute(fx.owner.repo.pool())
            .await
            .unwrap();
        let (_backend, session) = super::recovery::reopen_session(&fx, fx.owner.repo.clone()).await;
        assert_eq!(binding(&fx).await, None, "{scenario}");
        assert_eq!(fx.checkpoint().await, before, "{scenario}");
        session.shutdown().await.unwrap();
    }
}

/// How the original launch was left unconfirmed.
#[derive(Clone, Copy)]
enum LostBind {
    Failed,
    TimedOut,
    /// A replay preceded init: the reader stopped at a protocol failure without reading on.
    ReceiptBeforeInit,
    /// The CLI read the line and named nothing before a stop ended the turn.
    Stopped,
    /// The CLI exited without naming the session while a child kept its stdout open, so the
    /// reader was cut off after the exit instead of reaching the end of the stream.
    OrphanedStdout,
    SilentExit,
    CreatedBeforeInitDeath,
    InterruptEof,
}

#[tokio::test]
async fn canonical_claude_identity_unconfirmed_failed_bind_retry_stays_held() {
    unconfirmed_launch(LostBind::Failed, "retry").await;
}

#[tokio::test]
async fn canonical_claude_identity_unconfirmed_failed_bind_keep_holds_next_input() {
    unconfirmed_launch(LostBind::Failed, "keep").await;
}

#[tokio::test]
async fn canonical_claude_identity_unconfirmed_timed_out_bind_retry_stays_held() {
    unconfirmed_launch(LostBind::TimedOut, "retry").await;
}

#[tokio::test]
async fn canonical_claude_identity_unconfirmed_receipt_before_init_retry_stays_held() {
    unconfirmed_launch(LostBind::ReceiptBeforeInit, "retry").await;
}

#[tokio::test]
async fn canonical_claude_identity_unconfirmed_stopped_without_init_retry_stays_held() {
    unconfirmed_launch(LostBind::Stopped, "retry").await;
}

#[tokio::test]
async fn canonical_claude_identity_unconfirmed_orphaned_stdout_retry_stays_held() {
    unconfirmed_launch(LostBind::OrphanedStdout, "retry").await;
}

#[tokio::test]
async fn canonical_claude_identity_unconfirmed_silent_exit_retry_stays_held() {
    unconfirmed_launch(LostBind::SilentExit, "retry").await;
}
#[tokio::test]
async fn canonical_claude_identity_unconfirmed_created_before_init_death_retry_stays_held() {
    unconfirmed_launch(LostBind::CreatedBeforeInitDeath, "retry").await;
}
#[tokio::test]
async fn canonical_claude_identity_unconfirmed_interrupt_eof_retry_stays_held() {
    unconfirmed_launch(LostBind::InterruptEof, "retry").await;
}

/// #3318: no bind and no accepted replay confirm the launch: init was observed but its bind
/// was lost, or the reader ended before it read the CLI's whole output. After a restart
/// nothing proves whether the launch created the native session, so neither Human
/// resolution may launch the same `--session-id` again.
async fn unconfirmed_launch(lost: LostBind, action: &str) {
    let mut fx = Fixture::persistent(match lost {
        LostBind::Failed | LostBind::TimedOut => "no-input-replay",
        LostBind::ReceiptBeforeInit => "receipt-before-init",
        LostBind::Stopped => "read-then-stall",
        LostBind::OrphanedStdout => "orphan-stdout",
        LostBind::SilentExit => "silent-exit",
        LostBind::CreatedBeforeInitDeath => "created-before-init-death",
        LostBind::InterruptEof => "interrupt-before-init-eof",
    })
    .await;
    let pause = match lost {
        LostBind::Failed => {
            fail_bind(&fx).await;
            None
        }
        LostBind::TimedOut => Some(super::c5d::Pause::arm("c4-bind-session", &fx.owner.worker)),
        LostBind::ReceiptBeforeInit
        | LostBind::Stopped
        | LostBind::OrphanedStdout
        | LostBind::SilentExit
        | LostBind::CreatedBeforeInitDeath
        | LostBind::InterruptEof => None,
    };
    let mut events = fx.session.subscribe_events();
    // Retry requires the frozen invocation of the original attempt.
    fx.dispatch_selection(&TurnModelSelection {
        model: Some("claude-frozen".into()),
        effort: Some("high".into()),
    })
    .await
    .unwrap();
    if let Some(pause) = &pause {
        pause.entered().await;
    }
    if matches!(lost, LostBind::Stopped | LostBind::InterruptEof) {
        let stdin = fx.dir.path().join("stdin");
        tokio::time::timeout(Duration::from_secs(10), async {
            while !stdin.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        if matches!(lost, LostBind::InterruptEof) {
            let turn = fx
                .session
                .active_turn_id_for_thread(&fx.owner.thread)
                .expect("active turn");
            assert!(matches!(
                fx.session.turn_interrupt(&fx.owner.thread, &turn).await,
                provider::control::InterruptOutcome::LocalSignalDelivered
            ));
        } else {
            fx.session.shutdown().await.unwrap();
        }
    }
    // The loop, not this test, consumes END: its phase leaves issuing_turn only then.
    tokio::time::timeout(Duration::from_secs(20), async {
        while !matches!(
            events.recv().await.unwrap().kind,
            provider::events::PlannerEventKind::TurnCompleted { .. }
        ) {}
    })
    .await
    .unwrap();
    drop(pause);
    if matches!(lost, LostBind::InterruptEof) {
        assert!(
            std::fs::read_to_string(fx.dir.path().join("stdin"))
                .unwrap()
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .any(|line| line["request"]["subtype"] == "interrupt")
        );
    }
    let before = fx.checkpoint().await;
    assert!(matches!(
        before.attempts[0].delivery,
        InputDeliveryFact::Unknown
    ));
    assert_eq!(binding(&fx).await, None);
    fx.session.shutdown().await.unwrap();
    if matches!(lost, LostBind::Failed) {
        sqlx::query("DROP TRIGGER fail_native_bind")
            .execute(fx.owner.repo.pool())
            .await
            .unwrap();
    }
    let (backend, session) = super::recovery::reopen_session(&fx, fx.owner.repo.clone()).await;
    fx.backend = backend;
    fx.session = session;
    assert_eq!(binding(&fx).await, None, "no evidence names the session");
    assert_eq!(fx.checkpoint().await, before);
    let dir = fx.dir.path().to_path_buf();
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap();
    let (spawns, argv, stdin) = (read("spawns"), read("argv"), read("stdin"));
    // A relaunch would now succeed visibly.
    std::fs::write(fx.dir.path().join("scenario"), "exit").unwrap();
    let (state, app) = super::settled::application(&fx).await;
    let harness = fx.start_loop_in(state.harness.clone()).await;
    super::retry::action(&fx, &app, action, "resolve-unconfirmed").await;
    if action == "keep" {
        tokio::time::timeout(Duration::from_secs(25), async {
            while fx.checkpoint().await.receipts.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut tx = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
        let mut carrier = current_input_carrier_tx(&mut tx, &fx.owner.card, &fx.owner.worker)
            .await
            .unwrap();
        let accepted = crate::conversation_inputs::accept_human_tx(
            &mut tx,
            &fx.source.current_principal().unwrap(),
            &mut carrier,
            HumanPayload {
                text: "following kept input".into(),
                bound_attachments: vec![],
            },
        )
        .await
        .unwrap();
        let AcceptedInputRef::Human { message } = accepted.input_ref else {
            panic!("Human")
        };
        tx.commit().await.unwrap();
        harness
            .observe_canonical_source(message, fx.source.clone())
            .await
            .unwrap();
    }
    // Bounded: a held loop records no further attempt, a relaunch changes `spawns`.
    let _ = tokio::time::timeout(Duration::from_secs(10), async {
        while read("spawns") == spawns && fx.checkpoint().await.attempts.len() < 2 {
            harness.refresh_canonical_input().await.unwrap();
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(read("spawns"), spawns, "nothing was launched again");
    assert_eq!(read("argv"), argv);
    assert_eq!(read("stdin"), stdin, "nothing was sent");
    let after = fx.checkpoint().await;
    assert_eq!(after.attempts[0].delivery, before.attempts[0].delivery);
    assert!(
        after.attempts[1..].iter().all(|attempt| !matches!(
            attempt.delivery,
            InputDeliveryFact::Unknown
                | InputDeliveryFact::CrossingUnknown
                | InputDeliveryFact::Accepted { .. }
        )),
        "{:?}",
        after.attempts
    );
    assert_eq!(binding(&fx).await, None);
    let reader = fx.backend.issuance_hold().expect("identity hold");
    assert!(
        !reader.contains("fresh session") && !reader.contains("restart"),
        "canonical hold names no action: {reader}"
    );
    harness.shutdown().await.unwrap();
    state.dispatcher.retire();
}

#[tokio::test]
async fn canonical_claude_identity_unconfirmed_refuses_edit_preflight_without_spawn() {
    let mut fx = Fixture::persistent("no-input-replay").await;
    fail_bind(&fx).await;
    fx.dispatch().await.unwrap();
    fx.wait_end().await;
    fx.session.shutdown().await.unwrap();
    sqlx::query("DROP TRIGGER fail_native_bind")
        .execute(fx.owner.repo.pool())
        .await
        .unwrap();
    let (backend, session) = super::recovery::reopen_session(&fx, fx.owner.repo.clone()).await;
    fx.backend = backend;
    fx.session = session;
    let before = fx.checkpoint().await;
    let result = fx.session.prepare_edit_for_test(&fx.owner.thread).await;
    assert!(
        matches!(result, Err(crate::error::CalmError::Conflict(_))),
        "unconfirmed identity refuses an edit before native preparation"
    );
    assert!(
        !fx.dir.path().join("checks").exists(),
        "no native edit probe"
    );
    assert_eq!(fx.checkpoint().await, before);
    fx.session.shutdown().await.unwrap();
}
