use super::*;
use crate::error::CalmError;
use crate::state::RouteState;

// Control requests share the application's frozen bootstrap and exact boot binding.
#[derive(Clone)]
pub(super) struct Requests {
    issuer: crate::dispatcher::system_assistant::SystemAssistantIssuer,
    route: RouteState,
    track: String,
}
impl Requests {
    pub(super) fn new(state: &AppState, track: &str) -> Self {
        Self {
            issuer: state.today_summary_issuer.clone().unwrap(),
            route: RouteState::from_ref(state),
            track: track.into(),
        }
    }
    pub(super) async fn submit(
        &self,
        subject: &str,
        text: &str,
    ) -> crate::error::Result<calm_types::system_assistant::SystemAssistantReceipt> {
        self.issuer
            .submit(
                &self.route,
                self.track.clone().into(),
                BOOTSTRAP.into(),
                subject.into(),
                text.into(),
            )
            .await
    }
}

#[tokio::test]
async fn original_subject_replay_is_evidence_and_retirement_revokes_the_issuer() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = assistant_canonical::track(&app, &cookie).await;
    let requests = Requests::new(&state, &track);
    let issuer = state.today_summary_issuer.as_ref().unwrap();
    let route = RouteState::from_ref(&state);
    let original = requests.submit("original", "frozen body").await.unwrap();
    let before = frame(&fixture, &original.card_id).await;
    let receipts = recovery::receipts(&fixture, &original.card_id).await;
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
    let replay = requests.submit("original", "frozen body").await.unwrap();
    assert_eq!(replay, original);
    assert_eq!(frame(&fixture, &original.card_id).await, before);
    assert_eq!(
        recovery::receipts(&fixture, &original.card_id).await,
        receipts
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM events")
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap(),
        events
    );
    assert!(matches!(
        requests.submit("original", "changed").await,
        Err(CalmError::IdempotencyKeyReused(_))
    ));
    assert!(matches!(
        issuer
            .submit(
                &route,
                track.clone().into(),
                "changed bootstrap".into(),
                "next".into(),
                "body".into()
            )
            .await,
        Err(CalmError::IdempotencyKeyReused(_))
    ));
    state.dispatcher.retire();
    assert!(matches!(
        requests.submit("retired", "body").await,
        Err(CalmError::Unauthorized)
    ));
    assert_eq!(
        recovery::receipts(&fixture, &original.card_id).await,
        receipts
    );
    shutdown(&fixture, &state).await;
}

#[tokio::test]
async fn outer_error_and_cancel_rollback_the_entire_acceptance_before_publication() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = assistant_canonical::track(&app, &cookie).await;
    let requests = Requests::new(&state, &track);
    let card =
        crate::conversation_keys::derive_track_conversation_keys(&track, "today-summary").card_id;
    let mut events = state.events.subscribe();
    sqlx::query("CREATE TRIGGER reject_system_receipt BEFORE INSERT ON conversation_system_assistant_receipts WHEN NEW.bootstrap=0 BEGIN SELECT RAISE(ABORT,'reject original receipt'); END").execute(fixture.repo.pool()).await.unwrap();
    assert!(requests.submit("error", "body").await.is_err());
    assert_empty(&fixture, &card).await;
    sqlx::query("DROP TRIGGER reject_system_receipt")
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    let pause = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "system-assistant-before-commit",
        &card,
        pause.clone(),
    );
    let task = tokio::spawn({
        let requests = requests.clone();
        async move { requests.submit("cancel", "body").await }
    });
    tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
        .await
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    // Acquiring another real IMMEDIATE writer waits for SQLx's queued rollback cleanup.
    let writer = crate::state::WorkerState::from_ref(&state).repo;
    let database = writer.sqlite_db().unwrap();
    let tx = crate::db::sqlite::begin_immediate_tx(&database)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert_empty(&fixture, &card).await;
    while let Ok(event) = events.try_recv() {
        assert_ne!(
            event.scope.card_id().map(|id| id.as_str()),
            Some(card.as_str())
        );
    }
    requests.submit("control", "body").await.unwrap();
    assert_eq!(recovery::receipts(&fixture, &card).await.len(), 2);
    shutdown(&fixture, &state).await;
}
async fn assert_empty(fixture: &Fixture, card: &str) {
    let counts:(i64,i64,i64,i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM cards WHERE id=?1),(SELECT count(*) FROM worker_sessions WHERE card_id=?1),(SELECT count(*) FROM events WHERE scope_card=?1),(SELECT count(*) FROM conversation_presentations WHERE card_id=?1),(SELECT count(*) FROM conversation_system_assistant_entries WHERE card_id=?1),(SELECT count(*) FROM conversation_system_assistant_receipts WHERE card_id=?1)")
        .bind(card).fetch_one(fixture.repo.pool()).await.unwrap();
    assert_eq!(counts, (0, 0, 0, 0, 0, 0));
}
#[tokio::test]
async fn current_binding_and_area_reject_new_acceptance_on_original_target() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let track = assistant_canonical::track(&app, &cookie).await;
    let requests = Requests::new(&state, &track);
    let issuer = state.today_summary_issuer.as_ref().unwrap();
    let original = requests.submit("original", "body").await.unwrap();
    let (worker, before) = frame(&fixture, &original.card_id).await;
    let changes = [
        (
            "UPDATE worker_sessions SET state='failed' WHERE id=(SELECT session_id FROM cards WHERE id=?1)",
            "UPDATE worker_sessions SET state='idle' WHERE id=(SELECT session_id FROM cards WHERE id=?1)",
        ),
        (
            "UPDATE cards SET role='worker' WHERE id=?1",
            "UPDATE cards SET role='assistant' WHERE id=?1",
        ),
        (
            "UPDATE cards SET payload=json_set(payload,'$.harness_profile','plain_chat') WHERE id=?1",
            "UPDATE cards SET payload=json_set(payload,'$.harness_profile','assistant') WHERE id=?1",
        ),
        (
            "UPDATE cards SET payload=json_set(payload,'$.assistant_provider','claude') WHERE id=?1",
            "UPDATE cards SET payload=json_set(payload,'$.assistant_provider','codex') WHERE id=?1",
        ),
        (
            "UPDATE cards SET session_id=NULL WHERE id=?1",
            "UPDATE cards SET session_id=?2 WHERE id=?1",
        ),
        (
            "UPDATE tracks SET closed_at=1 WHERE id=(SELECT track_id FROM cards WHERE id=?1)",
            "UPDATE tracks SET closed_at=NULL WHERE id=(SELECT track_id FROM cards WHERE id=?1)",
        ),
    ];
    for (change, restore) in changes {
        sqlx::query(change)
            .bind(&original.card_id)
            .execute(fixture.repo.pool())
            .await
            .unwrap();
        let result = requests.submit(&crate::model::new_id(), "body").await;
        assert!(
            matches!(
                result,
                Err(CalmError::Forbidden(_) | CalmError::Conflict(_))
            ),
            "{change}: {result:?}"
        );
        let mut query = sqlx::query(restore).bind(&original.card_id);
        if restore.contains("?2") {
            query = query.bind(&worker);
        }
        query.execute(fixture.repo.pool()).await.unwrap();
        assert_eq!(
            frame(&fixture, &original.card_id).await.1.canonical_inputs,
            before.canonical_inputs
        );
        assert_eq!(
            recovery::receipts(&fixture, &original.card_id).await.len(),
            2
        );
    }
    assert_eq!(
        status(
            &app,
            &cookie,
            &format!("/api/tracks/{track}/conversations"),
            "today-summary",
            json!({"provider":"codex","text":"cannot adopt System input as Human"})
        )
        .await,
        StatusCode::CONFLICT
    );
    let other = Fixture::new().await;
    let (other_state, _app, _cookie) = other.application().await;
    assert!(matches!(
        issuer
            .submit(
                &RouteState::from_ref(&other_state),
                track.into(),
                BOOTSTRAP.into(),
                "wrong-repo".into(),
                "body".into()
            )
            .await,
        Err(CalmError::Forbidden(_))
    ));
    shutdown(&other, &other_state).await;
    sqlx::query("UPDATE areas SET conversation_history_contract='shared_history' WHERE id=(SELECT t.area_id FROM cards c JOIN tracks t ON t.id=c.track_id WHERE c.id=?1)")
        .bind(&original.card_id).execute(fixture.repo.pool()).await.unwrap();
    assert!(matches!(
        requests.submit("shared-history", "body").await,
        Err(CalmError::Forbidden(_))
    ));
    assert_eq!(
        frame(&fixture, &original.card_id).await.1.canonical_inputs,
        before.canonical_inputs
    );
    shutdown(&fixture, &state).await;
}

async fn native_ack_revocation(corrupt_source: bool) {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    activity(&app, &cookie).await;
    let accepted = summary(&state, &app, &cookie, "first", &body(1)).await;
    let card = accepted["card_id"].as_str().unwrap();
    let before = frame(&fixture, card).await;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    state.dispatcher.reconcile_tick_for_test().await;
    let operation = loop {
        let call = request(&mut peer).await;
        match call["method"].as_str().unwrap() {
            "config/read" => send(&mut peer, json!({"id":call["id"],"result":{"config":{}}})).await,
            "thread/start" => {
                let operation: String =
                    sqlx::query_scalar("SELECT spawn_op_id FROM worker_sessions WHERE id=?1")
                        .bind(&before.0)
                        .fetch_one(fixture.repo.pool())
                        .await
                        .unwrap();
                if corrupt_source {
                    let original = recovery::receipts(&fixture, card).await;
                    sqlx::query("UPDATE events SET actor=?2 WHERE id=?1")
                        .bind(original[0].event_id)
                        .bind(serde_json::to_string(&crate::ids::ActorId::User).unwrap())
                        .execute(fixture.repo.pool())
                        .await
                        .unwrap();
                } else {
                    sqlx::query("UPDATE areas SET conversation_history_contract='shared_history' WHERE id=(SELECT t.area_id FROM cards c JOIN tracks t ON t.id=c.track_id WHERE c.id=?1)")
                    .bind(card).execute(fixture.repo.pool()).await.unwrap();
                }
                send(
                    &mut peer,
                    json!({"id":call["id"],"result":{"thread":{"id":THREAD}}}),
                )
                .await;
                break operation;
            }
            method => panic!("unexpected pre-turn RPC {method}"),
        }
    };
    tokio::time::timeout(
        Duration::from_secs(5),
        RouteState::from_ref(&state)
            .operation_runtime
            .wait(&operation),
    )
    .await
    .expect("the original native ACK owner must settle after revocation")
    .unwrap();
    drain_owners(&state, card, &before.0, None, &mut peer).await;
    let after = frame(&fixture, card).await;
    assert_eq!(after.0, before.0);
    assert_eq!(after.1.canonical_inputs, before.1.canonical_inputs);
    shutdown(&fixture, &state).await;
}

#[tokio::test]
async fn area_revocation_during_native_thread_ack_blocks_the_original_turn() {
    native_ack_revocation(false).await;
}

#[tokio::test]
async fn original_source_tamper_during_native_thread_ack_blocks_the_original_turn() {
    native_ack_revocation(true).await;
}

#[tokio::test]
async fn historical_summary_key_requires_explicit_migration_without_erasing_original_work() {
    use crate::operation::planner_harness_start_adapter::OpeningBriefing;
    use crate::routes::track_conversations::*;
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application_with_historical_native(true).await;
    activity(&app, &cookie).await;
    let launchpad = post(
        &app,
        &cookie,
        "/api/today/launchpad/ensure",
        "ensure",
        json!(null),
    )
    .await;
    let track = launchpad["track_id"].as_str().unwrap();
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("Idempotency-Key", "today-summary".parse().unwrap());
    create_track_conversation_inner(
        RouteState::from_ref(&state),
        crate::state::WorkerState::from_ref(&state),
        crate::actor::Actor::server_send(),
        headers,
        track.into(),
        NewTrackConversationBody {
            provider: AssistantProviderRequest::LegacyCodex,
            side: None,
            text: "legacy sentinel".into(),
            model: None,
            reasoning_effort: None,
        },
        OpeningBriefing::CallerSuppliesItsOwn,
        AssistantCreationEntry::InternalHistorical,
    )
    .await
    .unwrap();
    let card =
        crate::conversation_keys::derive_track_conversation_keys(track, "today-summary").card_id;
    let worker: String = sqlx::query_scalar("SELECT session_id FROM cards WHERE id=?1")
        .bind(&card)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap();
    let items: Vec<(i64, String)> =
        sqlx::query_as("SELECT id,params FROM harness_items WHERE card_id=?1 ORDER BY id")
            .bind(&card)
            .fetch_all(fixture.repo.pool())
            .await
            .unwrap();
    assert!(matches!(
        Requests::new(&state, track).submit("new", "body").await,
        Err(CalmError::IdempotencyKeyExhausted(_))
    ));
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT session_id FROM cards WHERE id=?1")
            .bind(&card)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap(),
        worker
    );
    assert!(recovery::receipts(&fixture, &card).await.is_empty());
    for (id, params) in items {
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT params FROM harness_items WHERE id=?1")
                .bind(id)
                .fetch_one(fixture.repo.pool())
                .await
                .unwrap(),
            params
        );
    }
    shutdown(&fixture, &state).await;
}

// Stop and join the actual owners while serving only recovery of the original
// thread. The sink barrier is meaningful only after those owners have returned.
async fn drain_owners(
    state: &AppState,
    card: &str,
    worker: &str,
    completed: Option<&str>,
    peer: &mut Peer,
) {
    let settled = async {
        let route = RouteState::from_ref(state);
        // Match the actual lifecycle lock order and wait for prior retained
        // wakes, including recovery that creates no operation of its own.
        let card = crate::operation::planner_start_fence::CardStartFence::lock(
            &route.planner_recovery_locks,
            &route.repo,
            &state.operation_runtime,
            &card.into(),
        )
        .await;
        let operations = state.operation_runtime.lock_for_track_delete().await;
        if let Some(handle) = state.harness.get(&worker.to_owned()) {
            handle.shutdown().await.unwrap();
        }
        (card, operations)
    };
    tokio::pin!(settled);
    let _held = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            tokio::select! {
                guards = &mut settled => break guards,
                call = request(peer) => {
                    let result = match call["method"].as_str().unwrap() {
                        "config/read" => json!({"config":{}}),
                        "thread/read" | "thread/resume" => {
                            assert_eq!(call["params"]["threadId"], THREAD);
                            json!({"thread":{"id":THREAD,"status":{"type":"idle"},"turns":[]}})
                        }
                        "turn/interrupt" if completed.is_some() => {
                            // Managed shutdown may interrupt its last known turn
                            // even after END; it cannot name the refused body.
                            assert_eq!(call["params"]["threadId"], THREAD);
                            assert_eq!(call["params"]["turnId"], completed.unwrap());
                            json!({})
                        }
                        method => panic!("revoked input crossed native boundary: {method}"),
                    };
                    send(peer, json!({"id":call["id"],"result":result})).await;
                }
            }
        }
    })
    .await
    .unwrap();
    crate::shared_codex_appserver::native_owner_tests::assert_no_request(peer).await;
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Revocation {
    Role,
    Worker,
    Profile,
    Provider,
    Producer,
}
impl Revocation {
    pub(super) async fn apply(self, fixture: &Fixture, state: &AppState, card: &str) {
        let query = match self {
            Self::Role => "UPDATE cards SET role='worker' WHERE id=?1",
            Self::Worker => {
                "UPDATE worker_sessions SET state='failed' WHERE id=(SELECT session_id FROM cards WHERE id=?1)"
            }
            Self::Profile => {
                "UPDATE cards SET payload=json_set(payload,'$.harness_profile','plain_chat') WHERE id=?1"
            }
            Self::Provider => {
                "UPDATE cards SET payload=json_set(payload,'$.assistant_provider','claude') WHERE id=?1"
            }
            Self::Producer => {
                state.dispatcher.retire();
                return;
            }
        };
        sqlx::query(query)
            .bind(card)
            .execute(fixture.repo.pool())
            .await
            .unwrap();
    }
}
pub(super) const REVOCATIONS: [Revocation; 5] = [
    Revocation::Role,
    Revocation::Worker,
    Revocation::Profile,
    Revocation::Provider,
    Revocation::Producer,
];

#[tokio::test]
async fn accepted_system_input_rechecks_authority_after_the_actual_outbound_sink_wait() {
    for change in REVOCATIONS {
        let fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application().await;
        let accepted = summary(&state, &app, &cookie, "original", "original frozen body").await;
        let card = accepted["card_id"].as_str().unwrap();
        let receipts = recovery::receipts(&fixture, card).await;
        let (client, mut peer) = connect(&state.shared_codex_appserver).await;
        state.dispatcher.reconcile_tick_for_test().await;
        let bootstrap = next(&mut peer, true).await;
        Native {
            fixture: &fixture,
            state: &state,
            card,
        }
        .ack(&mut peer, bootstrap, BOOTSTRAP, "gate-bootstrap")
        .await;
        let before = frame(&fixture, card).await;
        // The accepted body must queue at the actual final native sink, after
        // bootstrap END; activation/recovery RPCs have already completed.
        let sink = client.lock_outbound_sink_for_test().await;
        notify_thread(
            &state.shared_codex_appserver,
            &mut peer,
            THREAD,
            "turn/completed",
            json!({"id":"gate-bootstrap","status":"completed"}),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let current = frame(&fixture, card).await.1;
                if current
                    .canonical_inputs
                    .attempts
                    .last()
                    .is_some_and(|a| a.delivery == InputDeliveryFact::CrossingUnknown)
                    && client.pending_requests_for_test() > 0
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("original dispatch must queue its native RPC behind the sink");
        let crossing = frame(&fixture, card).await.1;
        let original_attempt = crossing.canonical_inputs.attempts.last().unwrap();
        change.apply(&fixture, &state, card).await;
        drop(sink);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let current = frame(&fixture, card).await.1;
                if let Some(attempt) = current.canonical_inputs.attempts.last()
                    && matches!(attempt.delivery, InputDeliveryFact::NotDispatched { .. })
                {
                    assert_eq!(attempt.attempt, original_attempt.attempt);
                    assert_eq!(
                        attempt.correlation.thread_id,
                        original_attempt.correlation.thread_id
                    );
                    assert!(attempt.correlation.rpc_request_id.is_some());
                    assert_eq!(
                        attempt.delivery,
                        InputDeliveryFact::NotDispatched {
                            basis: "transport_before_send".into()
                        }
                    );
                    assert_eq!(
                        current.canonical_inputs.pending,
                        before.1.canonical_inputs.pending
                    );
                    assert!(current.canonical_inputs.active.is_none());
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|e| panic!("{change:?}: original dispatch did not settle: {e}"));
        drain_owners(&state, card, &before.0, Some("gate-bootstrap"), &mut peer).await;
        assert_eq!(frame(&fixture, card).await.0, before.0);
        assert_eq!(recovery::receipts(&fixture, card).await, receipts);
        fixture.shutdown_process(&state).await;
    }
}
