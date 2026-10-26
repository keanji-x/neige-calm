//! Creator allocation through its real route, with an authenticated, executing Planner.
use super::*;
use crate::error::CalmError;
use crate::ids::ActorId;
use crate::mcp_server::tools::track_add::{TrackAddArgs, TrackAddRequest, TrackCreator};
use crate::routes::tracks::RouteTrackCreator;
use calm_truth::db::RepoRead;

fn input(flow: &Flow, key: &str) -> TrackAddRequest {
    TrackAddRequest {
        creator_track_id: flow.track.clone(),
        area_id: flow.area.clone(),
        create_actor: ActorId::AiPlannerSession(flow.worker.clone().into()),
        start_actor: ActorId::AiPlanner(flow.card.clone().into()),
        planner_provider: crate::session_projection_repo::AgentProvider::Codex,
        args: TrackAddArgs {
            recipe_id: None,
            template_id: None,
            template_input: None,
            cwd: None,
            attach_folder: None,
            allow_cross_area_cwd: None,
            title: key.into(),
            idempotency_key: key.into(),
            text: "Full creator input\nwith a second line and exact trailing whitespace.  ".into(),
            message: "delegate".into(),
        },
    }
}

pub(in crate::routes::canonical_flow_tests) async fn attached_mcp_selection() {
    let mut flow = Flow::running_with_admission(false, true).await;
    flow.state.dispatcher.abort_event_listener_for_test();
    let path = super::super::u8b_canonical::repo(&flow.fixture);
    let args = json!({"template_id":"small-change", "title":"Agent added",
        "cwd":path,"attach_folder":true,"idempotency_key":"agent-add",
        "text":FIRST_BODY,"message":"Create an attached track"});
    let created = super::u9_canonical::tool(&flow, "neige_track_add", args.clone())
        .await
        .unwrap();
    let id = created["track_id"].as_str().unwrap();
    let saved = flow.fixture.repo.track_get(id).await.unwrap().unwrap();
    assert_eq!(saved.creator_track_id.as_deref(), Some(flow.track.as_str()));
    assert_eq!(saved.workspace.path, path.to_str().unwrap());
    assert!(std::path::Path::new(saved.workspace.agent_cwd()).is_dir());
    flow.state.dispatcher.reconcile_tick_for_test().await;
    deliver(&flow.fixture, &flow.state, &mut flow.peer, id, FIRST_BODY).await;
    assert_eq!(
        super::u9_canonical::tool(&flow, "neige_track_add", args)
            .await
            .unwrap(),
        created
    );
    // Agent input has no Human message; deliver checks the System source and once contract.
    assert_eq!(counts(&flow.fixture, id).await, (1, 1, 1, 0, 1));
    stop_creator(&mut flow).await;
}

#[tokio::test]
async fn u13_historical_allocation_materialization_failure_refuses_boot() {
    let mut flow = Flow::running_with_admission(false, true).await;
    flow.state.dispatcher.abort_event_listener_for_test();
    let mut state = crate::state::RouteState::from_ref(&flow.state);
    let blocked = flow.fixture.root.path().join("blocked-workspace-root");
    std::fs::write(&blocked, b"not a directory").unwrap();
    state.workspace_root = blocked;
    let creator = RouteTrackCreator::new(state, 2);
    assert!(
        creator
            .historical_receipt_for_test(input(&flow, "failed-materialize"))
            .await
            .is_err()
    );
    let facts: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM track_create_idempotency b JOIN tracks t ON t.id=b.track_id WHERE t.creator_key='failed-materialize' AND b.first_message_sha256 IS NOT NULL),
         (SELECT count(*) FROM cards c JOIN tracks t ON t.id=c.track_id WHERE t.creator_key='failed-materialize' AND c.role='planner' AND c.session_id IS NULL),
         (SELECT count(*) FROM worker_sessions ws JOIN tracks t ON t.id=ws.track_id WHERE t.creator_key='failed-materialize')")
        .fetch_one(flow.fixture.repo.pool()).await.unwrap();
    assert_eq!(facts, (1, 1, 0));
    stop_creator(&mut flow).await;
    assert!(matches!(SqlxRepo::open(&flow.fixture.url).await,
        Err(calm_truth::error::CalmError::Internal(message)) if message.starts_with("conversation upgrade:")));
}

#[tokio::test]
async fn u13_creator_canonical_before_dispatch_allows_reopen() {
    let mut flow = Flow::running_with_admission(false, true).await;
    flow.state.dispatcher.abort_event_listener_for_test();
    let created = route(&flow, 2)
        .add(input(&flow, "boot-receipt"))
        .await
        .unwrap();
    assert_eq!(
        counts(&flow.fixture, created.id.as_str()).await,
        (1, 1, 0, 0, 0)
    );
    let human: i64 = sqlx::query_scalar("SELECT count(*) FROM conversation_human_track_creations a JOIN track_create_idempotency b USING(area_id,idempotency_key) WHERE b.track_id=?1")
        .bind(created.id.as_str()).fetch_one(flow.fixture.repo.pool()).await.unwrap();
    assert_eq!(human, 0);
    // Allocation and its keyed System source committed; dispatcher has not
    // accepted any input. A restart here is a normal canonical crash boundary.
    stop_creator(&mut flow).await;
    let reopened = SqlxRepo::open(&flow.fixture.url)
        .await
        .expect("canonical creator allocation");
    reopened.db().close().await;
}

#[tokio::test]
async fn u9_canonical_creator_system_full_text() {
    let mut flow = Flow::running_with_admission(false, true).await;
    flow.state.dispatcher.abort_event_listener_for_test();
    let creator = RouteTrackCreator::new(crate::state::RouteState::from_ref(&flow.state), 2);
    let request = input(&flow, "raw/key");
    let created = tokio::time::timeout(Duration::from_secs(3), creator.add(request.clone()))
        .await
        .expect("creator allocation must commit without a native start operation")
        .unwrap();
    let (_, _, snapshot) = flow.fixture.carrier(created.id.as_str()).await;
    assert!(snapshot.canonical_inputs.pending.is_empty());
    let (id, body, hop): (String, String, i64) =
        sqlx::query_as("SELECT id,text,hop FROM mails WHERE to_track_id=?1")
            .bind(created.id.as_str())
            .fetch_one(flow.fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(body, request.args.text);
    assert_eq!(hop, 1);
    let payload: String = sqlx::query_scalar("SELECT payload FROM events WHERE kind='track.wake_requested' AND json_extract(payload,'$.key')=?1")
        .bind(&id).fetch_one(flow.fixture.repo.pool()).await.unwrap();
    let payload: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(payload["text"], request.args.text);
    assert_eq!(payload["source"], calm_types::observation::MAIL_WAKE_SOURCE);
    assert_eq!(created.creator_key.as_deref(), Some("raw/key"));
    assert_eq!(
        created.creator_track_id.as_deref(),
        Some(flow.track.as_str())
    );
    assert!(created.admin_granted_at.is_none());
    flow.state.dispatcher.reconcile_tick_for_test().await;
    deliver(
        &flow.fixture,
        &flow.state,
        &mut flow.peer,
        created.id.as_str(),
        &request.args.text,
    )
    .await;
    let replay = creator.add(request).await.unwrap();
    assert_eq!(replay.id, created.id);
    assert_eq!(
        counts(&flow.fixture, created.id.as_str()).await,
        (1, 1, 1, 0, 1)
    );
    flow.end(TURN).await;
    flow.fixture
        .wait_phase(&flow.track, HarnessPhaseTag::TurnCompleted)
        .await;
    assert!(matches!(
        creator.add(input(&flow, "no-active-turn")).await,
        Err(crate::mcp_server::tools::track_add::TrackAddRefusal::Create(CalmError::Conflict(_)))
    ));
    assert_eq!(
        counts(&flow.fixture, created.id.as_str()).await,
        (1, 1, 1, 0, 1)
    );
    flow.close().await;
}

fn route(flow: &Flow, cap: u32) -> RouteTrackCreator {
    RouteTrackCreator::new(crate::state::RouteState::from_ref(&flow.state), cap)
}

async fn independent(
    flow: &Flow,
    cap: u32,
    gate: Option<Arc<crate::routes::tracks::TrackCreateMintGate>>,
) -> RouteTrackCreator {
    let mut state = crate::state::RouteState::from_ref(&flow.state);
    state.repo = Arc::new(SqlxRepo::open(&flow.fixture.url).await.unwrap());
    state.area_delete_locks = crate::per_card_lock::new_keyed_locks();
    state.conversation_first_message_locks = crate::per_card_lock::new_per_card_locks();
    state.track_create_mint_rendezvous = gate;
    RouteTrackCreator::new(state, cap)
}

async fn stop_creator(flow: &mut Flow) {
    flow.end(TURN).await;
    flow.fixture
        .wait_phase(&flow.track, HarnessPhaseTag::TurnCompleted)
        .await;
    flow.close().await;
}

async fn counts(fixture: &Fixture, track: &str) -> (i64, i64, i64, i64, i64) {
    sqlx::query_as("SELECT
        (SELECT count(*) FROM mails WHERE to_track_id=?1),
        (SELECT count(*) FROM events WHERE scope_track=?1 AND kind='track.wake_requested'),
        (SELECT count(*) FROM conversation_presentations p JOIN cards c ON c.id=p.card_id WHERE c.track_id=?1 AND p.source_kind='system_observation'),
        (SELECT count(*) FROM conversation_messages m JOIN cards c ON c.id=m.card_id WHERE c.track_id=?1),
        (SELECT count(*) FROM conversation_input_submissions s JOIN cards c ON c.id=s.card_id WHERE c.track_id=?1)")
        .bind(track).fetch_one(fixture.repo.pool()).await.unwrap()
}

#[tokio::test]
async fn u9_canonical_creator_raw_key_replay_order_and_race() {
    let mut flow = Flow::running_with_admission(false, true).await;
    flow.state.dispatcher.abort_event_listener_for_test();
    let mut request = input(&flow, "raw/key");
    let recipe = post(
        &flow.app,
        &flow.cookie,
        "/api/track-recipes",
        "recipe",
        json!({"title":"delegated recipe","body":"Original frozen report"}),
    )
    .await;
    request.args.recipe_id = Some(recipe["id"].as_str().unwrap().into());
    let held = Arc::new(crate::routes::tracks::TrackCreateMintGate::new());
    let loser = independent(&flow, 1, Some(held.clone())).await;
    let winner = independent(&flow, 1, None).await;
    let concurrent = {
        let request = request.clone();
        tokio::spawn(async move { loser.add(request).await })
    };
    tokio::time::timeout(Duration::from_secs(5), held.reached.wait())
        .await
        .unwrap();
    let first = winner.add(request.clone()).await.unwrap();
    held.released.wait().await;
    let other = concurrent.await.unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        serde_json::to_value(&other).unwrap()
    );
    let baseline = counts(&flow.fixture, first.id.as_str()).await;
    assert_eq!(baseline, (1, 1, 0, 0, 0));
    let deleted = flow
        .app
        .clone()
        .oneshot(
            Request::delete(format!(
                "/api/track-recipes/{}",
                recipe["id"].as_str().unwrap()
            ))
            .header(header::COOKIE, &flow.cookie)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    // Current cap/readiness/catalog cannot change a receipt. The fingerprint
    // deliberately excludes inherited provider, but preserves every raw argument.
    let mut unavailable = request.clone();
    unavailable.planner_provider = crate::session_projection_repo::AgentProvider::Claude;
    let replay = route(&flow, 0).add(unavailable).await.unwrap();
    assert_eq!(replay.id, first.id);
    for field in ["text", "title", "message", "template_id"] {
        let mut changed = request.clone();
        match field {
            "text" => changed.args.text.push('!'),
            "title" => changed.args.title.push('!'),
            "message" => changed.args.message.push('!'),
            _ => changed.args.template_id = Some("missing-template".into()),
        }
        assert!(matches!(
            route(&flow, 0).add(changed).await,
            Err(
                crate::mcp_server::tools::track_add::TrackAddRefusal::Create(
                    crate::error::CalmError::IdempotencyKeyReused(_)
                )
            )
        ));
    }
    let (a, b) = tokio::join!(winner.add(request.clone()), winner.add(request));
    assert_eq!(a.unwrap().id, first.id);
    assert_eq!(b.unwrap().id, first.id);
    assert_eq!(counts(&flow.fixture, first.id.as_str()).await, baseline);
    stop_creator(&mut flow).await;
}

#[tokio::test]
async fn u9_canonical_creator_cap_cross_instance() {
    let mut flow = Flow::running_with_admission(false, true).await;
    flow.state.dispatcher.abort_event_listener_for_test();
    route(&flow, 2).add(input(&flow, "occupied")).await.unwrap();
    let held = Arc::new(crate::routes::tracks::TrackCreateMintGate::new());
    let loser = independent(&flow, 2, Some(held.clone())).await;
    let winner = independent(&flow, 2, None).await;
    let req = input(&flow, "loser");
    let adding = tokio::spawn(async move { loser.add(req).await });
    tokio::time::timeout(Duration::from_secs(5), held.reached.wait())
        .await
        .unwrap();
    let won = winner.add(input(&flow, "winner")).await.unwrap();
    held.released.wait().await;
    assert!(matches!(
        adding.await.unwrap(),
        Err(crate::mcp_server::tools::track_add::TrackAddRefusal::OpenCap { cap: 2, .. })
    ));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM tracks WHERE creator_track_id=?1")
        .bind(&flow.track)
        .fetch_one(flow.fixture.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 2);
    assert_eq!(
        counts(&flow.fixture, won.id.as_str()).await,
        (1, 1, 0, 0, 0)
    );
    close_track(&flow, won.id.as_str()).await;
    route(&flow, 2)
        .add(input(&flow, "after-close"))
        .await
        .unwrap();
    stop_creator(&mut flow).await;
}

#[tokio::test]
async fn u9_canonical_creator_mail_and_event_failure_roll_back() {
    let mut flow = Flow::running_with_admission(false, true).await;
    flow.state.dispatcher.abort_event_listener_for_test();
    // Keep the DDL lifecycle on one connection: another pooled connection can
    // still cache the dropped trigger's schema when preparing the next CREATE.
    let mut ddl = flow.fixture.repo.pool().acquire().await.unwrap();
    for (table, condition) in [
        ("mails", "1"),
        ("events", "NEW.kind='track.wake_requested'"),
    ] {
        let before: (i64,i64,i64,i64,i64,i64) = sqlx::query_as("SELECT (SELECT count(*) FROM tracks),(SELECT count(*) FROM cards),(SELECT count(*) FROM \
            worker_sessions),(SELECT count(*) FROM track_create_idempotency),(SELECT count(*) FROM \
            mails),(SELECT count(*) FROM events WHERE scope_track<>?1)")
            .bind(&flow.track).fetch_one(flow.fixture.repo.pool()).await.unwrap();
        sqlx::query(&format!(
            "CREATE TRIGGER fail_creator BEFORE INSERT ON {table} WHEN {condition} BEGIN SELECT \
            RAISE(ABORT,'{table} injected fault'); END"
        ))
        .execute(&mut *ddl)
        .await
        .unwrap();
        let request = input(&flow, table);
        assert!(matches!(
            route(&flow, 1).add(request.clone()).await,
            Err(crate::mcp_server::tools::track_add::TrackAddRefusal::Create(
                CalmError::Db(sqlx::Error::Database(error))
            )) if error.code().as_deref() == Some("1811")
                && error.message() == format!("{table} injected fault")
        ));
        let after = sqlx::query_as::<_, (i64,i64,i64,i64,i64,i64)>("SELECT (SELECT count(*) FROM tracks),(SELECT count(*) FROM cards),(SELECT count(*) FROM \
            worker_sessions),(SELECT count(*) FROM track_create_idempotency),(SELECT count(*) FROM \
            mails),(SELECT count(*) FROM events WHERE scope_track<>?1)")
            .bind(&flow.track).fetch_one(flow.fixture.repo.pool()).await.unwrap();
        assert_eq!(before, after);
        sqlx::query("DROP TRIGGER fail_creator")
            .execute(&mut *ddl)
            .await
            .unwrap();
    }
    drop(ddl);
    for key in ["mails", "events"] {
        let track = route(&flow, 2).add(input(&flow, key)).await.unwrap();
        assert_eq!(
            counts(&flow.fixture, track.id.as_str()).await,
            (1, 1, 0, 0, 0)
        );
    }
    stop_creator(&mut flow).await;
}

const CHILD_THREAD: &str = "creator-child-native-thread";
const CHILD_TURN: &str = "creator-child-native-turn";

#[cfg(test)]
async fn deliver(fixture: &Fixture, state: &AppState, peer: &mut Peer, track: &str, text: &str) {
    let call = tokio::time::timeout(Duration::from_secs(8), async {
        let mut starts = 0;
        loop {
            let call = request(peer).await;
            let result = match call["method"].as_str().unwrap() {
                "config/read" => json!({"config":{}}),
                "thread/start" => {
                    starts += 1;
                    json!({"thread":{"id":CHILD_THREAD}})
                }
                "turn/start" => {
                    assert_eq!(starts, 1);
                    break call;
                }
                method => panic!("unexpected creator activation: {method}"),
            };
            send(peer, json!({"id":call["id"],"result":result})).await;
        }
    })
    .await
    .expect("durable creator input activates through the production Dispatcher");
    assert_eq!(call["params"]["threadId"], CHILD_THREAD);
    assert!(
        call["params"]["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["text"].as_str().is_some_and(|s| s.contains(text)))
    );
    let (card, worker, crossing) = fixture.carrier(track).await;
    assert_eq!(crossing.canonical_inputs.attempts.len(), 1);
    let id = &crossing
        .canonical_inputs
        .active
        .as_ref()
        .unwrap()
        .submission_ids[0];
    let mut tx = fixture.repo.pool().begin().await.unwrap();
    let plan = crate::db::sqlite::conversation::submission_plan_tx(&mut tx, id)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    use calm_types::conversation::{
        PrivateHistoryPayload, SubmissionRenderInput, SubmissionRenderSegment,
    };
    let systems: Vec<_> = plan
        .plan
        .segments()
        .iter()
        .filter_map(|segment| match segment {
            SubmissionRenderSegment::Input {
                input:
                    SubmissionRenderInput::System {
                        payload: PrivateHistoryPayload::Present { value },
                        ..
                    },
                ..
            } => Some(value),
            SubmissionRenderSegment::Input {
                input: SubmissionRenderInput::Human { .. },
                ..
            } => panic!("agent content cannot acquire Human attribution"),
            _ => None,
        })
        .collect();
    let (mail, envelope): (String, i64) = sqlx::query_as(
        "SELECT m.id,e.id FROM mails m JOIN events e ON json_extract(e.payload,'$.key')=m.id WHERE \
            m.to_track_id=?1 AND e.kind='track.wake_requested'",
    )
    .bind(track)
    .fetch_one(fixture.repo.pool())
    .await
    .unwrap();
    let accepted_envelope: i64 = sqlx::query_scalar("SELECT envelope_id FROM conversation_submission_inputs WHERE submission_id=?1 AND input_kind='system' AND effective=1")
        .bind(id.as_str()).fetch_one(fixture.repo.pool()).await.unwrap();
    assert_eq!(accepted_envelope, envelope);
    assert_eq!(systems.len(), 1);
    assert!(
        matches!(systems[0].observation(), calm_types::observation::Observation::TrackWake{source,key,text:body,..}
        if source==calm_types::observation::MAIL_WAKE_SOURCE && body==text && key==&mail)
    );
    assert_eq!(counts(fixture, track).await, (1, 1, 1, 0, 1));
    send(
        peer,
        json!({"id":call["id"],"result":{"turn":{"id":CHILD_TURN}}}),
    )
    .await;
    notify_thread(
        &state.shared_codex_appserver,
        peer,
        CHILD_THREAD,
        "turn/started",
        json!({"id":CHILD_TURN,"status":"inProgress"}),
    )
    .await;
    fixture
        .wait_phase(track, HarnessPhaseTag::TurnRunning)
        .await;
    notify_thread(
        &state.shared_codex_appserver,
        peer,
        CHILD_THREAD,
        "turn/completed",
        json!({"id":CHILD_TURN,"status":"completed"}),
    )
    .await;
    let done = fixture
        .wait_phase(track, HarnessPhaseTag::TurnCompleted)
        .await;
    state.dispatcher.reconcile_tick_for_test().await;
    state.dispatcher.reconcile_tick_for_test().await;
    let route = crate::state::RouteState::from_ref(state);
    let _fence = crate::per_card_lock::lock_card(&route.planner_recovery_locks, &card).await;
    assert_eq!(
        fixture.carrier(track).await.2.canonical_inputs,
        done.canonical_inputs
    );
    assert_eq!(counts(fixture, track).await, (1, 1, 1, 0, 1));
    assert_eq!(done.canonical_inputs.attempts.len(), 1);
    if let Some(handle) = state.harness.remove(&worker) {
        let mut stopping = tokio::spawn(async move { handle.shutdown().await });
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                result = &mut stopping => result.unwrap().unwrap(),
                call = request(peer) => {
                    assert_eq!(call["method"],"turn/interrupt");
                    send(peer,json!({"id":call["id"],"result":{}})).await;
                    stopping.await.unwrap().unwrap();
                }
            }
        })
        .await
        .unwrap();
    }
}

async fn cold_creator(accepted: bool, changed_source: bool) {
    let mut flow = Flow::running_with_admission(false, true).await;
    flow.state.dispatcher.abort_event_listener_for_test();
    let request = input(&flow, "cold/raw");
    let track = route(&flow, 1).add(request.clone()).await.unwrap();
    let (card, worker, empty) = flow.fixture.carrier(track.id.as_str()).await;
    assert!(empty.canonical_inputs.pending.is_empty());
    let route_state = crate::state::RouteState::from_ref(&flow.state);
    let fence = crate::per_card_lock::lock_card(&route_state.planner_recovery_locks, &card).await;
    if accepted {
        flow.state.dispatcher.reconcile_tick_for_test().await;
        let frame = flow.fixture.carrier(track.id.as_str()).await.2;
        assert!(matches!(
            frame.canonical_inputs.pending.as_slice(),
            [PendingInput::System { .. }]
        ));
        assert!(frame.canonical_inputs.attempts.is_empty());
        assert_eq!(
            counts(&flow.fixture, track.id.as_str()).await,
            (1, 1, 1, 0, 0)
        );
    }
    stop_creator(&mut flow).await;
    drop(fence);
    // Drain the real pending wake after producer retirement before dropping it.
    let drain = crate::per_card_lock::lock_card(&route_state.planner_recovery_locks, &card).await;
    drop(drain);
    drop(route_state);
    let before = flow.fixture.carrier(track.id.as_str()).await.2;
    if changed_source {
        sqlx::query("UPDATE events SET payload=json_set(payload,'$.text','changed source') WHERE scope_track=?1 \
            AND kind='track.wake_requested'")
            .bind(track.id.as_str()).execute(flow.fixture.repo.pool()).await.unwrap();
    }
    let Flow {
        mut fixture,
        state,
        app,
        _client,
        peer,
        ..
    } = flow;
    drop((state, app, _client, peer));
    fixture.repo.pool().close().await;
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    let (state, app, cookie) = fixture.application().await;
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    state.dispatcher.reconcile_tick_for_test().await;
    if changed_source {
        assert_eq!(
            fixture.carrier(track.id.as_str()).await.2.canonical_inputs,
            before.canonical_inputs
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(200), super::super::request(&mut peer))
                .await
                .is_err()
        );
    } else {
        deliver(
            &fixture,
            &state,
            &mut peer,
            track.id.as_str(),
            &request.args.text,
        )
        .await;
        let replay = RouteTrackCreator::new(crate::state::RouteState::from_ref(&state), 0)
            .add(request)
            .await
            .unwrap();
        assert_eq!(replay.id, track.id);
        assert_eq!(counts(&fixture, track.id.as_str()).await, (1, 1, 1, 0, 1));
    }
    assert_eq!(fixture.carrier(track.id.as_str()).await.1, worker);
    drop((app, cookie));
    fixture.shutdown_process(&state).await;
    peer.close(None).await.unwrap();
}

#[tokio::test]
async fn u9_canonical_creator_commit_dispatch_cold_once() {
    cold_creator(false, false).await;
}
#[tokio::test]
async fn u9_canonical_creator_accepted_before_crossing_cold_once() {
    cold_creator(true, false).await;
}
#[tokio::test]
async fn u9_canonical_creator_changed_durable_source_is_refused() {
    cold_creator(true, true).await;
}

#[tokio::test]
async fn u9_canonical_creator_source_and_admin_refusals() {
    let mut flow = Flow::running_with_admission(false, false).await;
    flow.state.dispatcher.abort_event_listener_for_test();
    let creator = route(&flow, 2);
    assert!(matches!(
        creator.add(input(&flow, "non-admin")).await,
        Err(crate::mcp_server::tools::track_add::TrackAddRefusal::NotAdmin)
    ));
    for fact in ["session", "card", "area"] {
        let mut req = input(&flow, fact);
        match fact {
            "session" => req.create_actor = ActorId::AiPlannerSession("wrong-session".into()),
            "card" => req.start_actor = ActorId::AiPlanner("wrong-card".into()),
            _ => req.area_id = "wrong-area".into(),
        }
        assert!(matches!(
            creator.add(req).await,
            Err(
                crate::mcp_server::tools::track_add::TrackAddRefusal::Create(CalmError::Forbidden(
                    _
                ))
            )
        ));
    }
    let allocations: i64 =
        sqlx::query_scalar("SELECT count(*) FROM tracks WHERE creator_track_id=?1")
            .bind(&flow.track)
            .fetch_one(flow.fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(allocations, 0);
    stop_creator(&mut flow).await;
}

#[tokio::test]
async fn u9_canonical_creator_system_turn_uses_real_hops() {
    let mut flow = Flow::running_with_admission(false, true).await;
    flow.state.dispatcher.abort_event_listener_for_test();
    let first = route(&flow, 3)
        .add(input(&flow, "human-turn"))
        .await
        .unwrap();
    flow.report(SYSTEM_BODY).await;
    flow.end(TURN).await;
    let call = flow.next().await.unwrap();
    flow.accept(&call, "creator-system-turn").await;
    let incoming = super::u9_canonical::incoming(&flow, first.id.as_str(), 3).await;
    super::u9_canonical::tool(&flow, "neige_mail_cat", json!({"mail_id":incoming}))
        .await
        .unwrap();
    let second = route(&flow, 3)
        .add(input(&flow, "system-hop-four"))
        .await
        .unwrap();
    let hop: i64 = sqlx::query_scalar("SELECT hop FROM mails WHERE to_track_id=?1")
        .bind(second.id.as_str())
        .fetch_one(flow.fixture.repo.pool())
        .await
        .unwrap();
    assert_eq!(hop, 4);
    let incoming = super::u9_canonical::incoming(&flow, first.id.as_str(), 6).await;
    super::u9_canonical::tool(&flow, "neige_mail_cat", json!({"mail_id":incoming}))
        .await
        .unwrap();
    assert!(matches!(
        route(&flow, 3).add(input(&flow, "seventh-hop")).await,
        Err(crate::mcp_server::tools::track_add::TrackAddRefusal::Create(CalmError::Conflict(_)))
    ));
    let allocated: i64 =
        sqlx::query_scalar("SELECT count(*) FROM tracks WHERE creator_track_id=?1")
            .bind(&flow.track)
            .fetch_one(flow.fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(allocated, 2);
    flow.end("creator-system-turn").await;
    flow.fixture
        .wait_phase(&flow.track, HarnessPhaseTag::TurnCompleted)
        .await;
    flow.close().await;
}

#[tokio::test]
async fn u9_canonical_creator_historical_receipt_replays_factually() {
    let mut flow = Flow::running_with_admission(false, true).await;
    flow.state.dispatcher.abort_event_listener_for_test();
    let creator = route(&flow, 1);
    let input = input(&flow, "historical-key");
    let create = creator.historical_receipt_for_test(input.clone());
    let native = async {
        loop {
            let call = request(&mut flow.peer).await;
            let result = match call["method"].as_str().unwrap() {
                "config/read" => json!({"config":{}}),
                "thread/start" => json!({"thread":{"id":CHILD_THREAD}}),
                "turn/start" => json!({"turn":{"id":CHILD_TURN}}),
                method => panic!("historical fixture native protocol: {method}"),
            };
            send(&mut flow.peer, json!({"id":call["id"],"result":result})).await;
            if call["method"] == "turn/start" {
                break;
            }
        }
    };
    let (track, ()) = tokio::join!(create, native);
    let track = track.unwrap();
    let snapshot: String = sqlx::query_scalar(concat!(
        "SELECT ws.handle_state_json FROM worker_sessions ws JOIN cards c ON c.session_id=ws.id ",
        "WHERE c.track_id=?1 AND c.role='planner'"
    ))
    .bind(track.id.as_str())
    .fetch_one(flow.fixture.repo.pool())
    .await
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&snapshot).unwrap()["schema_version"],
        crate::harness::snapshot::HARNESS_SNAPSHOT_SCHEMA_VERSION
    );
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM operations")
        .fetch_one(flow.fixture.repo.pool())
        .await
        .unwrap();
    let replay = creator.add(input).await.unwrap();
    assert_eq!(replay.id, track.id);
    assert_eq!(
        counts(&flow.fixture, track.id.as_str()).await,
        (0, 0, 0, 0, 0)
    );
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM operations")
        .fetch_one(flow.fixture.repo.pool())
        .await
        .unwrap();
    assert_eq!(before, after);
    stop_creator(&mut flow).await;
}

async fn close_track(flow: &Flow, track: &str) {
    let response = flow
        .app
        .clone()
        .oneshot(
            Request::patch(format!("/api/tracks/{track}"))
                .header(header::COOKIE, &flow.cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"closed":true}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
