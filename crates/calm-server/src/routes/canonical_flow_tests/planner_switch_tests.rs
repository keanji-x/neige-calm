//! Production switch entry with authenticated Human ingress and fake native only.
use super::*;
use crate::actor::Actor;
use crate::auth::HumanInputSource;
use crate::planner_switch::{self, PlannerSwitchError, SwitchPlannerRequest};
use crate::shared_codex_appserver::native_owner_tests::Peer;
use crate::state::RouteState;
use calm_truth::db::sqlite::track_planner::PlannerRevision;
use calm_truth::db::{RepoEventWrite, RepoRead};
use calm_truth::session_repo::SessionRepo;

mod claude;
mod http;

struct Pair {
    fixture: Fixture,
    state: AppState,
    app: axum::Router,
    cookie: String,
    human: HumanInputSource,
    peer: Peer,
    _client: Arc<provider::codex::CodexAppServer>,
    track: String,
    cards: [String; 2],
    workers: [String; 2],
}

async fn complete(state: &AppState, peer: &mut Peer, thread: &str, turn: &str) {
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let call = request(peer).await;
            let result = match call["method"].as_str().unwrap() {
                "config/read" => json!({"config":{}}),
                "thread/start" => json!({"thread":{"id":thread}}),
                "turn/start" => break call,
                method => panic!("unexpected activation: {method}"),
            };
            send(peer, json!({"id":call["id"],"result":result})).await;
        }
    })
    .await;
    let call = match result {
        Ok(call) => call,
        Err(_) => {
            let db = state.mcp_context().sqlite_db.clone().unwrap();
            let rows: Vec<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
                "SELECT c.role,ws.state,ws.thread_id,ws.handle_state_json
                FROM cards c JOIN worker_sessions ws ON c.session_id=ws.id
                WHERE ws.mode='resumable'",
            )
            .fetch_all(db.read_pool())
            .await
            .unwrap();
            panic!("native activation timed out: {thread}: {rows:?}");
        }
    };
    send(peer, json!({"id":call["id"],"result":{"turn":{"id":turn}}})).await;
    notify_thread(
        &state.shared_codex_appserver,
        peer,
        thread,
        "turn/started",
        json!({"id":turn,"status":"inProgress"}),
    )
    .await;
    send(peer,json!({"method":"item/completed","params":{"threadId":thread,"turnId":turn,"item":{"id":"original-native-reply","type":"agentMessage","text":"Original execution result"}}})).await;
    notify_thread(
        &state.shared_codex_appserver,
        peer,
        thread,
        "turn/completed",
        json!({"id":turn,"status":"completed"}),
    )
    .await;
}

impl Pair {
    async fn new() -> Self {
        Self::with_provider("codex").await
    }
    async fn with_provider(provider: &str) -> Self {
        let mut fixture = Fixture::new().await;
        if provider == "claude" {
            claude_creation_tests::assistant::configure(&mut fixture, "exit");
        }
        let (state, app, cookie, auth) = fixture.application_with_auth(false).await;
        state.dispatcher.abort_event_listener_for_test();
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(header::COOKIE, cookie.parse().unwrap());
        let (_, human) = crate::auth::authenticated_source(auth, headers).await;
        let (client, mut peer) = connect(&state.shared_codex_appserver).await;
        let area = post(
            &app,
            &cookie,
            "/api/areas",
            "area",
            json!({"name":"Switch pair","color":"#123456"}),
        )
        .await;
        let created = post(&app,&cookie,"/api/tracks","planner",json!({"area_id":area["id"],"title":"Preserved track","planner_provider":"codex","theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY})).await;
        let track = created["id"].as_str().unwrap().to_owned();
        complete(&state, &mut peer, THREAD, TURN).await;
        fixture
            .wait_phase(&track, HarnessPhaseTag::TurnCompleted)
            .await;
        let (planner, old, _) = fixture.carrier(&track).await;
        fixture.wait_for_stored_reply(&planner).await;
        let assistant = post(
            &app,
            &cookie,
            &format!("/api/tracks/{track}/conversations"),
            "assistant",
            json!({"provider":provider,"text":"Assistant history"}),
        )
        .await;
        let target = assistant["id"].as_str().unwrap().to_owned();
        if provider == "claude" {
            claude_creation_tests::assistant::wait_frame(&fixture, &target, |f| {
                f.idle_for_lifecycle() && f.canonical_inputs.attempts.len() == 1
            })
            .await;
        } else {
            complete(&state, &mut peer, "switch-assistant", "assistant-turn").await;
        }
        let (worker, _) = assistant_canonical::carrier(&fixture, &target).await;
        let handle = state.harness.get(&worker).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if handle.idle_carrier_owner().await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        if provider == "codex" {
            fixture.wait_for_stored_reply(&target).await;
        }
        Self {
            fixture,
            state,
            app,
            cookie,
            human: human.unwrap(),
            peer,
            _client: client,
            track,
            cards: [planner, target],
            workers: [old, worker],
        }
    }
    fn request(&self) -> SwitchPlannerRequest {
        SwitchPlannerRequest {
            track_id: self.track.clone().into(),
            expected_planner_card_id: self.cards[0].clone().into(),
            target_assistant_card_id: self.cards[1].clone().into(),
            expected_revision: PlannerRevision(0),
        }
    }
    async fn switch(&mut self) -> Result<planner_switch::PlannerSwitchReceipt, PlannerSwitchError> {
        let route = RouteState::from_ref(&self.state);
        let human = self.human.clone();
        let switch_request = self.request();
        let actor = Actor("user".into());
        let switch = planner_switch::switch_planner(&route, &actor, &human, switch_request);
        tokio::pin!(switch);
        tokio::time::timeout(Duration::from_secs(10),async {
            loop { tokio::select! {
                result = &mut switch => break result,
                call = request(&mut self.peer) => {
                    assert!(matches!(call["method"].as_str(),Some("thread/unsubscribe"|"turn/interrupt"|"config/read")),"{call}");
                    send(&mut self.peer,json!({"id":call["id"],"result":{}})).await;
                }
            }}
        }).await.unwrap()
    }
    async fn unchanged(&self) {
        let current: Vec<(String,String)> = sqlx::query_as("SELECT id,session_id FROM cards WHERE track_id=?1 AND role IN ('planner','assistant') ORDER BY role DESC").bind(&self.track).fetch_all(self.fixture.repo.pool()).await.unwrap();
        assert_eq!(
            current,
            self.cards
                .iter()
                .cloned()
                .zip(self.workers.iter().cloned())
                .collect::<Vec<_>>()
        );
        for worker in &self.workers {
            let hash: Option<String> =
                sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?1")
                    .bind(worker)
                    .fetch_one(self.fixture.repo.pool())
                    .await
                    .unwrap();
            assert!(hash.is_some(), "credentials not revoked on refusal");
        }
    }
    async fn close(self) {
        self.fixture.shutdown_process(&self.state).await;
    }
}
fn code(error: &PlannerSwitchError) -> Option<&'static str> {
    match error {
        PlannerSwitchError::Refused { reason, .. } => Some(reason.code()),
        PlannerSwitchError::Failed(_) => None,
    }
}

#[tokio::test]
async fn planner_switch_pair_commit_root_history_and_next_send() {
    for stale_mode in [false, true] {
        let mut p = Pair::new().await;
        let target = p.fixture.repo.card_get(&p.cards[1]).await.unwrap().unwrap();
        assert!(target.payload.get("permission_mode").is_none());
        if stale_mode {
            sqlx::query(
                "UPDATE cards SET payload=json_set(payload,'$.permission_mode','full') WHERE id=?1",
            )
            .bind(&p.cards[1])
            .execute(p.fixture.repo.pool())
            .await
            .unwrap();
        }
        sqlx::query("UPDATE cards SET payload=json_set(payload,
            '$.permission_mode','ask','$.model','preserved-planner-model','$.model_ever_set',json('true')),
            title='Original Planner title' WHERE id=?1").bind(&p.cards[0]).execute(p.fixture.repo.pool()).await.unwrap();
        sqlx::query("UPDATE cards SET title='Original Assistant title' WHERE id=?1")
            .bind(&p.cards[1])
            .execute(p.fixture.repo.pool())
            .await
            .unwrap();

        let before: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT id,payload,COALESCE(title,'') FROM cards WHERE track_id=?1 ORDER BY id",
        )
        .bind(&p.track)
        .fetch_all(p.fixture.repo.pool())
        .await
        .unwrap();
        let facts: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT id,initiating_runtime_id,initial_submission_id FROM conversation_turns ORDER BY id",
    )
    .fetch_all(p.fixture.repo.pool())
    .await
    .unwrap();
        let messages: Vec<(String, i64, String)> = sqlx::query_as("SELECT message_id,revision,payload_json FROM conversation_message_revisions ORDER BY message_id,revision").fetch_all(p.fixture.repo.pool()).await.unwrap();
        let receipt = p.switch().await.unwrap();
        assert_eq!(receipt.revision, PlannerRevision(1));
        let root: String = sqlx::query_scalar("SELECT root_session_id FROM tracks WHERE id=?1")
            .bind(&p.track)
            .fetch_one(p.fixture.repo.pool())
            .await
            .unwrap();
        assert_eq!(root, receipt.promoted.new_worker.id.as_str());
        let roles = p
            .state
            .card_role_cache
            .roles_snapshot(&p.cards.iter().cloned().map(Into::into).collect::<Vec<_>>());
        assert_eq!(
            roles[&p.cards[0].clone().into()],
            crate::model::CardRole::Assistant
        );
        assert_eq!(
            roles[&p.cards[1].clone().into()],
            crate::model::CardRole::Planner
        );
        let after: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT id,payload,COALESCE(title,'') FROM cards WHERE track_id=?1 ORDER BY id",
        )
        .bind(&p.track)
        .fetch_all(p.fixture.repo.pool())
        .await
        .unwrap();
        for (a, b) in before.iter().zip(after.iter()) {
            assert_eq!((&a.0, &a.2), (&b.0, &b.2));
            let mut old: Value = serde_json::from_str(&a.1).unwrap();
            let mut new: Value = serde_json::from_str(&b.1).unwrap();
            if a.0 == p.cards[1] {
                assert_eq!(new["permission_mode"], json!("ask"));
                old.as_object_mut().unwrap().remove("permission_mode");
                new.as_object_mut().unwrap().remove("permission_mode");
            }
            for value in [&mut old, &mut new] {
                for key in [
                    "planner_provider",
                    "assistant_provider",
                    "harness_profile",
                    "planner_harness",
                ] {
                    value.as_object_mut().unwrap().remove(key);
                }
            }
            assert_eq!(old, new, "preferences remain on their own card");
        }
        let after_facts: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT id,initiating_runtime_id,initial_submission_id FROM conversation_turns ORDER BY id",
    )
    .fetch_all(p.fixture.repo.pool())
    .await
    .unwrap();
        assert_eq!(facts, after_facts);
        let after_messages: Vec<(String, i64, String)> = sqlx::query_as("SELECT message_id,revision,payload_json FROM conversation_message_revisions ORDER BY message_id,revision").fetch_all(p.fixture.repo.pool()).await.unwrap();
        assert_eq!(messages, after_messages);

        for replacement in [&receipt.demoted, &receipt.promoted] {
            assert_ne!(replacement.old_worker.id, replacement.new_worker.id);
            let stored = p
                .fixture
                .repo
                .session_get(&replacement.old_worker.id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(stored.contract, replacement.old_worker.contract);
            assert_eq!(
                stored.parent_session_id,
                replacement.old_worker.parent_session_id
            );
            assert_eq!(
                stored.requester_session_id,
                replacement.old_worker.requester_session_id
            );
            assert_eq!(stored.thread_id, replacement.old_worker.thread_id);

            let hash: Option<String> =
                sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?1")
                    .bind(replacement.old_worker.id.as_str())
                    .fetch_one(p.fixture.repo.pool())
                    .await
                    .unwrap();
            assert!(hash.is_none());
            assert!(
                p.state
                    .harness
                    .get(&replacement.old_worker.id.to_string())
                    .is_none()
            );
            let snapshot: String =
                sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id=?1")
                    .bind(replacement.new_worker.id.as_str())
                    .fetch_one(p.fixture.repo.pool())
                    .await
                    .unwrap();
            let snapshot: Value = serde_json::from_str(&snapshot).unwrap();
            assert_eq!(snapshot["push_watermark"], replacement.watermark);
        }
        let sent = post(
            &p.app,
            &p.cookie,
            &format!("/api/cards/{}/planner/input", p.cards[1]),
            "next",
            json!({"text":"New Planner turn"}),
        )
        .await;
        assert_eq!(
            sent["worker_session_id"],
            receipt.promoted.new_worker.id.as_str()
        );
        complete(&p.state, &mut p.peer, "new-planner", "new-turn").await;
        p.fixture
            .wait_phase(&p.track, HarnessPhaseTag::TurnCompleted)
            .await;
        p.close().await;
    }
}

#[tokio::test]
async fn planner_switch_nonhuman_actor_refused() {
    let p = Pair::new().await;
    let result = planner_switch::switch_planner(
        &RouteState::from_ref(&p.state),
        &Actor("ai:codex".into()),
        &p.human,
        p.request(),
    )
    .await;
    assert_eq!(code(&result.err().unwrap()), Some("planner_user_required"));
    p.unchanged().await;
    p.close().await;
}

#[tokio::test]
async fn planner_switch_session_proof_unavailable_refused() {
    let mut p = Pair::new().await;
    p.state.harness.remove(&p.workers[1]);
    let error = p.switch().await.err().unwrap();
    assert_eq!(code(&error), Some("planner_session_unavailable"));
    p.unchanged().await;
    p.close().await;
}

#[tokio::test]
async fn planner_switch_open_wake_refused_before_revoke() {
    let mut p = Pair::new().await;
    // Persist through the actual Planner ask and plural event owner.
    let mut tx = crate::db::sqlite::begin_immediate_tx(p.fixture.repo.db())
        .await
        .unwrap();
    let (scope, event) = crate::ask::ask_requested_tx(
        &mut tx,
        &p.cards[0].clone().into(),
        vec![crate::event::AskQuestion {
            title: "Open wake".into(),
            options: vec!["Yes".into()],
        }],
    )
    .await
    .unwrap();
    crate::db::sqlite::append_decision_event_in_tx(
        &mut tx,
        &crate::ids::ActorId::AiPlannerSession(p.workers[0].clone().into()),
        &scope,
        None,
        &event,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let error = p.switch().await.err().unwrap();
    assert_eq!(code(&error), Some("planner_open_ask"));
    p.unchanged().await;
    p.close().await;
}

#[tokio::test]
async fn planner_switch_pending_event_refused() {
    let mut p = Pair::new().await;
    let area = p
        .fixture
        .repo
        .track_get(&p.track)
        .await
        .unwrap()
        .unwrap()
        .area_id;
    p.fixture
        .repo
        .log_pure_event(
            crate::ids::ActorId::User,
            crate::event::EventScope::Track {
                track: p.track.clone().into(),
                area,
            },
            None,
            &p.state.events,
            &p.state.card_role_cache,
            &p.state.track_area_cache,
            crate::event::Event::TrackReportEdited {
                track_id: p.track.clone().into(),
                card_id: p.cards[0].clone().into(),
                author: crate::event::EditAuthor::User,
                author_plugin_id: None,
                edit_id: "pending".into(),
                summary_before: String::new(),
                summary_after: String::new(),
                body_before: String::new(),
                body_after: "pending report".into(),
                agent_message: None,
            },
        )
        .await
        .unwrap();
    let error = p.switch().await.err().unwrap();
    assert_eq!(code(&error), Some("planner_pending_observations"));
    p.unchanged().await;
    p.close().await;
}

#[tokio::test]
async fn planner_switch_owner_predicate_failure_is_failed() {
    let mut p = Pair::new().await;
    // Break the operation owner's SQL read, after the fixture is fully idle.
    sqlx::query("ALTER TABLE operations RENAME TO unavailable_operations")
        .execute(p.fixture.repo.pool())
        .await
        .unwrap();
    let error = p.switch().await.err().unwrap();
    assert!(matches!(error, PlannerSwitchError::Failed(_)), "{error:?}");
    p.unchanged().await;
    sqlx::query("ALTER TABLE unavailable_operations RENAME TO operations")
        .execute(p.fixture.repo.pool())
        .await
        .unwrap();
    p.close().await;
}

#[tokio::test]
async fn planner_switch_malformed_permission_mode_refused() {
    let mut p = Pair::new().await;
    for expression in [
        "json_remove(payload,'$.permission_mode')",
        "json_set(payload,'$.permission_mode','invalid')",
    ] {
        sqlx::query(&format!(
            "UPDATE cards SET payload={expression} WHERE id=?1"
        ))
        .bind(&p.cards[0])
        .execute(p.fixture.repo.pool())
        .await
        .unwrap();
        let error = p.switch().await.err().unwrap();
        assert_eq!(code(&error), Some("planner_binding_unsupported"));
        p.unchanged().await;
    }
    p.close().await;
}

#[tokio::test]
async fn planner_switch_reconcile_fresh_planner_without_broadcast() {
    let mut p = Pair::new().await;
    let receipt = p.switch().await.unwrap();
    let worker = receipt.promoted.new_worker.id.to_string();
    let native: (Option<String>, Option<String>) =
        sqlx::query_as("SELECT thread_id,spawn_op_id FROM worker_sessions WHERE id=?1")
            .bind(&worker)
            .fetch_one(p.fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(native, (None, None));
    let area = p
        .fixture
        .repo
        .track_get(&p.track)
        .await
        .unwrap()
        .unwrap()
        .area_id;
    let id = p
        .fixture
        .repo
        .log_pure_event(
            crate::ids::ActorId::User,
            crate::event::EventScope::Track {
                track: p.track.clone().into(),
                area,
            },
            None,
            &p.state.events,
            &p.state.card_role_cache,
            &p.state.track_area_cache,
            crate::event::Event::TrackReportEdited {
                track_id: p.track.clone().into(),
                card_id: p.cards[1].clone().into(),
                author: crate::event::EditAuthor::User,
                author_plugin_id: None,
                edit_id: "pending".into(),
                summary_before: String::new(),
                summary_after: String::new(),
                body_before: String::new(),
                body_after: "pending report".into(),
                agent_message: None,
            },
        )
        .await
        .unwrap();

    assert!(id > receipt.promoted.watermark);
    p.state.dispatcher.reconcile_tick_for_test().await;
    let (_, current_worker, frame) = p.fixture.carrier(&p.track).await;
    assert_eq!(current_worker, worker);
    assert_eq!(
        frame.header.push_watermark, id,
        "reconcile must accept the missed event on the fresh Planner"
    );
    complete(
        &p.state,
        &mut p.peer,
        "reconciled-planner",
        "reconciled-turn",
    )
    .await;
    p.fixture
        .wait_phase(&p.track, HarnessPhaseTag::TurnCompleted)
        .await;
    let (_, current_worker, frame) = p.fixture.carrier(&p.track).await;
    assert_eq!(current_worker, worker);
    assert!(frame.canonical_inputs.pending.is_empty());
    assert_eq!(
        frame.canonical_inputs.attempts.len(),
        1,
        "the normal wake must execute the accepted System input once"
    );
    p.close().await;
}
