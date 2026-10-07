//! #2348 A1 — the provider requests a Planner's running turn is paused on. A test adapter pushes
//! `Open` / `Gone` / `ConnectionLost` into the harness's held-request channel, as a provider
//! adapter would; the answers go through the production answer route; the asks are read from the
//! events table and the production activity projector.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::codex_appserver::Notification;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, card_create_with_id_tx, session_start_runtime_tx};
use calm_server::event::{AskAnswer, AskDelivery, AskQuestion, Event, EventBus};
use calm_server::harness::held_requests::{
    ConnectionId, HeldRequestMessage, HeldResponder, RequestKey,
};
use calm_server::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessRegistry, HarnessSnapshot, HarnessState, Observation,
    PlannerHarness, PlannerHarnessParams,
};
use calm_server::ids::{ActorId, CardId, TrackId};
use calm_server::model::{CardRole, NewArea, NewCard, NewTrack, new_id, now_ms};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient, WriteContext};
use calm_server::terminal_renderer::TerminalRendererRegistry;
use calm_server::track_activity::{ActivityItem, Recompute, TrackActivityProjector};
use calm_server::track_area_cache::TrackAreaCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tower::ServiceExt;

const THREAD: &str = "thread-hold-ask";
const CONNECTION: &str = "connection-1";

/// What the provider request was told.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Answered(usize),
    Refused,
}

/// The test adapter's responder: an answer is recorded; a drop without one is a refusal, as a
/// provider adapter's `Drop` refuses its request.
struct TestResponder {
    outcome: mpsc::UnboundedSender<Outcome>,
    answered: bool,
}

impl HeldResponder for TestResponder {
    fn respond(mut self: Box<Self>, option: usize) {
        self.answered = true;
        let _ = self.outcome.send(Outcome::Answered(option));
    }
}

impl Drop for TestResponder {
    fn drop(&mut self) {
        if !self.answered {
            let _ = self.outcome.send(Outcome::Refused);
        }
    }
}

/// One paused request as the test adapter holds it.
struct Request1 {
    outcome: mpsc::UnboundedReceiver<Outcome>,
}

impl Request1 {
    async fn told(&mut self) -> Outcome {
        tokio::time::timeout(Duration::from_secs(5), self.outcome.recv())
            .await
            .expect("the request was never told anything")
            .expect("the responder is gone without an outcome")
    }

    fn told_nothing_yet(&mut self) -> bool {
        matches!(
            self.outcome.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        )
    }
}

/// A Planner harness on its own track and session, with the answer route mounted. `planner_codex_approvals`
/// (#2348 A2) builds one over a live fake daemon.
pub(super) struct Rig {
    pub(super) repo: Arc<SqlxRepo>,
    events: EventBus,
    role_cache: CardRoleCache,
    area_cache: TrackAreaCache,
    pub(super) daemon: Arc<SharedCodexAppServer>,
    pub(super) harness: PlannerHarness,
    registry: HarnessRegistry,
    app: axum::Router,
    session_id: String,
    card: CardId,
    track: TrackId,
    thread: &'static str,
}

fn snapshot(thread: &str) -> HarnessSnapshot {
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(thread.into());
    snapshot
}

async fn rig() -> Rig {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    rig_on(repo, daemon, THREAD, "never").await
}

/// The rig over `daemon`, the harness on `thread`, its Planner card in `permission_mode`.
pub(super) async fn rig_on(
    repo: Arc<SqlxRepo>,
    daemon: Arc<SharedCodexAppServer>,
    thread: &'static str,
    permission_mode: &str,
) -> Rig {
    let area = repo
        .area_create(NewArea {
            name: "hold-ask".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "hold ask".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let role_cache = CardRoleCache::new();
    let area_cache = TrackAreaCache::new();
    area_cache.insert(track.id.clone(), area.id);
    let mut tx = repo.pool().begin().await.unwrap();
    let card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "planner_harness": true, "planner_provider": "codex", "permission_mode": permission_mode}),
        },
        CardRole::Planner,
        true,
        &role_cache,
    )
    .await
    .unwrap();
    let session_id = new_id();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: session_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some(thread.into()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(snapshot(thread)).unwrap()),
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let events = EventBus::new();
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let host = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        repo_dyn.clone(),
        PathBuf::new(),
        std::env::temp_dir().join("calm-plugins-data-hold-ask"),
        Vec::new(),
        EventBus::new(),
        WriteContext::new(role_cache.clone(), area_cache.clone()),
    ));
    let state = AppState::from_parts(
        repo_dyn,
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        host,
        Arc::new(CodexClient::new_stub()),
        Some(role_cache.clone()),
        Some(area_cache.clone()),
    );
    let registry = state.harness.clone();
    let app = calm_server::routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);
    let harness = run_harness(
        &repo,
        &events,
        &role_cache,
        &area_cache,
        &daemon,
        &registry,
        &session_id,
        &card.id,
        &track.id,
        thread,
    )
    .await;
    Rig {
        repo,
        events,
        role_cache,
        area_cache,
        daemon,
        harness,
        registry,
        app,
        session_id,
        card: card.id,
        track: track.id,
        thread,
    }
}

/// A harness on the session, as boot recovery, a respawn or a system-error recovery builds one;
/// installed in the registry the answer route reads, and listening.
#[allow(clippy::too_many_arguments)]
async fn run_harness(
    repo: &Arc<SqlxRepo>,
    events: &EventBus,
    role_cache: &CardRoleCache,
    area_cache: &TrackAreaCache,
    daemon: &Arc<SharedCodexAppServer>,
    registry: &HarnessRegistry,
    session_id: &str,
    card: &CardId,
    track: &TrackId,
    thread: &str,
) -> PlannerHarness {
    let listening = daemon.notification_receiver_count_for_test();
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let harness = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: session_id.to_string(),
        track_id: track.clone(),
        card_id: card.clone(),
        thread_id: Some(thread.into()),
        repo: repo_dyn,
        events: events.clone(),
        card_role_cache: role_cache.clone(),
        track_area_cache: area_cache.clone(),
        backend: daemon.clone().into(),
        live_replies: calm_server::harness::LiveReplies::for_test(),
        config: HarnessConfig::default(),
        snapshot: snapshot(thread),
    });
    wait_for("the harness to listen", || async {
        daemon.notification_receiver_count_for_test() > listening
    })
    .await;
    registry.insert(session_id.to_string(), harness.clone());
    harness
}

pub(super) async fn wait_for<F, Fut>(what: &str, mut done: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

impl Rig {
    async fn run_harness(&self) -> PlannerHarness {
        run_harness(
            &self.repo,
            &self.events,
            &self.role_cache,
            &self.area_cache,
            &self.daemon,
            &self.registry,
            &self.session_id,
            &self.card,
            &self.track,
            self.thread,
        )
        .await
    }

    /// Push `Open` for one request on `connection`, as an adapter does, and wait for its ask.
    async fn open(&self, key: &str, connection: &str) -> (i64, Request1) {
        let before = self.asks().await.len();
        let (outcome, rx) = mpsc::unbounded_channel();
        self.harness
            .held_request_sender()
            .send(HeldRequestMessage::Open {
                request_key: RequestKey(key.into()),
                connection: ConnectionId(connection.into()),
                questions: vec![question()],
                responder: Box::new(TestResponder {
                    outcome,
                    answered: false,
                }),
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let ask_id = loop {
            let asks = self.asks().await;
            if asks.len() > before {
                let id = asks.last().unwrap().0;
                if self.harness.held_requests().contains(id) {
                    break id;
                }
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the hold ask"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        (ask_id, Request1 { outcome: rx })
    }

    fn send(&self, message: HeldRequestMessage) {
        self.harness.held_request_sender().send(message).unwrap();
    }

    /// `(id, actor, event)` of every `ask.requested`.
    pub(super) async fn asks(&self) -> Vec<(i64, ActorId, Event)> {
        self.rows("ask.requested").await
    }

    async fn rows(&self, kind: &str) -> Vec<(i64, ActorId, Event)> {
        self.repo
            .events_for_track(self.track.as_str(), &[kind], None)
            .await
            .unwrap()
            .into_iter()
            .map(|row| (row.id, row.actor, row.event))
            .collect()
    }

    pub(super) async fn withdrawn(&self, ask_id: i64) -> bool {
        self.rows("ask.withdrawn")
            .await
            .iter()
            .any(|(_, _, event)| matches!(event, Event::AskWithdrawn { ask_id: id, .. } if *id == ask_id))
    }

    pub(super) async fn answered(&self, ask_id: i64) -> bool {
        self.rows("ask.answered").await.iter().any(
            |(_, _, event)| matches!(event, Event::AskAnswered { ask_id: id, .. } if *id == ask_id),
        )
    }

    async fn activity_items(&self) -> Vec<ActivityItem> {
        let repo: Arc<dyn Repo> = self.repo.clone();
        let projector = TrackActivityProjector::new(
            repo,
            self.events.clone(),
            WriteContext::new(self.role_cache.clone(), self.area_cache.clone()),
            HarnessRegistry::new(),
            TerminalRendererRegistry::new(),
        )
        .expect("sqlite-backed repo");
        match projector
            .recompute_track(self.track.as_str())
            .await
            .unwrap()
        {
            Recompute::NoTrack => panic!("the track vanished"),
            Recompute::Unchanged(payload) | Recompute::Written(payload) => payload.items,
        }
    }

    async fn post(&self, uri: String, body: Value) -> (StatusCode, Value) {
        let resp = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    pub(super) async fn answer(&self, ask_id: i64, answers: Value) -> (StatusCode, Value) {
        self.post(
            format!("/api/tracks/{}/asks/{ask_id}/answer", self.track),
            json!({ "answers": answers }),
        )
        .await
    }

    /// Start a turn the way any input does, and return its id once the harness runs it.
    async fn start_turn(&self) -> String {
        self.start_turn_with("Read the track goal.").await
    }

    /// [`Self::start_turn`] with the turn's input text.
    pub(super) async fn start_turn_with(&self, text: &str) -> String {
        self.harness
            .observe(Observation::TrackGoal { text: text.into() })
            .unwrap();
        wait_for("the turn to run", || {
            let harness = self.harness.clone();
            async move {
                matches!(
                    harness.state_for_test().await,
                    HarnessState::TurnRunning { .. }
                )
            }
        })
        .await;
        match self.harness.state_for_test().await {
            HarnessState::TurnRunning { turn_id, .. } => turn_id,
            other => panic!("expected a running turn, got {other:?}"),
        }
    }

    async fn set_session_state(&self, state: &str) {
        sqlx::query("UPDATE worker_sessions SET state = ?1 WHERE id = ?2")
            .bind(state)
            .bind(&self.session_id)
            .execute(self.repo.pool())
            .await
            .unwrap();
    }
}

fn question() -> AskQuestion {
    AskQuestion {
        title: "Run `cargo test` (cwd /work)?".into(),
        options: vec!["Allow".into(), "Deny".into()],
    }
}

/// An `Open` is a `hold` ask by the harness's own session, shown as a paused-turn item; the
/// chosen option goes to the request, and the answer closes the ask.
#[tokio::test]
async fn an_open_request_is_a_hold_ask_and_its_answer_goes_to_the_request() {
    let rig = rig().await;
    let (ask_id, mut request) = rig.open("req-1", CONNECTION).await;
    let asks = rig.asks().await;
    assert!(
        matches!(asks.as_slice(), [(_, ActorId::AiPlannerSession(session), Event::AskRequested {
                delivery: AskDelivery::Hold, questions, source_item_id: None, .. })]
            if session.as_str() == rig.session_id && questions == &vec![question()]),
        "{asks:?}"
    );
    let items = rig.activity_items().await;
    assert!(
        matches!(items.as_slice(), [ActivityItem::Ask { ask_id: id, delivery: AskDelivery::Hold, .. }] if *id == ask_id),
        "{items:?}"
    );

    let (status, body) = rig.answer(ask_id, json!([{ "option": 1 }])).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(request.told().await, Outcome::Answered(1));
    assert!(!rig.harness.held_requests().contains(ask_id));
    let answered = rig.rows("ask.answered").await;
    assert!(
        matches!(answered.as_slice(), [(_, ActorId::User, Event::AskAnswered { answers, .. })]
            if answers == &vec![AskAnswer::Option(1)]),
        "{answered:?}"
    );
    assert!(
        rig.activity_items().await.is_empty(),
        "the answer closes it"
    );
    rig.harness.shutdown().await.unwrap();
}

/// A paused request takes an option only: typed text is refused and the request stays held.
#[tokio::test]
async fn a_hold_ask_refuses_a_text_answer() {
    let rig = rig().await;
    let (ask_id, mut request) = rig.open("req-1", CONNECTION).await;
    let (status, body) = rig.answer(ask_id, json!([{ "text": "Allow" }])).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|m| m.contains("answer with one of its options")),
        "{body}"
    );
    let (status, body) = rig.answer(ask_id, json!([{ "option": 2 }])).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an option it does not have: {body}"
    );
    assert!(!rig.answered(ask_id).await);
    assert!(rig.harness.held_requests().contains(ask_id));
    assert!(request.told_nothing_yet());
    rig.harness.shutdown().await.unwrap();
}

/// The provider settled the request itself: the ask is withdrawn and the request let go.
#[tokio::test]
async fn open_then_gone_withdraws_the_ask() {
    let rig = rig().await;
    let (ask_id, mut request) = rig.open("req-1", CONNECTION).await;
    rig.send(HeldRequestMessage::Gone {
        request_key: RequestKey("req-1".into()),
    });
    assert_eq!(request.told().await, Outcome::Refused);
    wait_for("the withdrawal", || rig.withdrawn(ask_id)).await;
    assert!(!rig.harness.held_requests().contains(ask_id));
    assert!(rig.activity_items().await.is_empty());
    let (status, body) = rig.answer(ask_id, json!([{ "option": 0 }])).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(!rig.answered(ask_id).await);
    rig.harness.shutdown().await.unwrap();
}

/// A connection that ends takes only its own requests: a request a newer connection asked again
/// before the old one's end arrived stays open.
#[tokio::test]
async fn connection_lost_keeps_the_ask_of_a_newer_connection() {
    let rig = rig().await;
    let (old_ask, mut old_request) = rig.open("conn-1/req-7", "conn-1").await;
    let (new_ask, mut new_request) = rig.open("conn-2/req-7", "conn-2").await;
    rig.send(HeldRequestMessage::ConnectionLost {
        connection: ConnectionId("conn-1".into()),
    });
    assert_eq!(old_request.told().await, Outcome::Refused);
    wait_for("the old ask's withdrawal", || rig.withdrawn(old_ask)).await;
    assert!(!rig.withdrawn(new_ask).await);
    assert!(rig.harness.held_requests().contains(new_ask));
    assert!(new_request.told_nothing_yet());
    let items = rig.activity_items().await;
    assert!(
        matches!(items.as_slice(), [ActivityItem::Ask { ask_id, .. }] if *ask_id == new_ask),
        "{items:?}"
    );
    let (status, body) = rig.answer(new_ask, json!([{ "option": 0 }])).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(new_request.told().await, Outcome::Answered(0));
    rig.harness.shutdown().await.unwrap();
}

/// The database decides between an answer and a withdrawal: once one is stored the other is not,
/// whichever comes first, even when the answer got past the route's table check before the
/// request went away.
#[tokio::test]
async fn answered_and_withdrawn_are_mutually_exclusive() {
    let rig = rig().await;

    // Withdrawn first: an answer that already passed the table check is refused at the commit.
    let (withdrawn, mut request) = rig.open("req-1", CONNECTION).await;
    rig.send(HeldRequestMessage::Gone {
        request_key: RequestKey("req-1".into()),
    });
    assert_eq!(request.told().await, Outcome::Refused);
    wait_for("the withdrawal", || rig.withdrawn(withdrawn)).await;
    let late = answer_in_tx(&rig, withdrawn).await;
    assert!(
        matches!(late, Err(calm_server::error::CalmError::Conflict(_))),
        "{late:?}"
    );
    assert!(!rig.answered(withdrawn).await);

    // Answered first: the request's end withdraws nothing.
    let (answered, mut request) = rig.open("req-2", CONNECTION).await;
    answer_in_tx(&rig, answered).await.unwrap();
    rig.send(HeldRequestMessage::Gone {
        request_key: RequestKey("req-2".into()),
    });
    assert_eq!(request.told().await, Outcome::Refused);
    // A later request's ask is the witness that the `Gone` was handled.
    let (_witness, _request) = rig.open("req-3", CONNECTION).await;
    assert!(rig.answered(answered).await);
    assert!(!rig.withdrawn(answered).await);
    rig.harness.shutdown().await.unwrap();
}

/// The answer transaction the route commits, without the route's table check before it.
async fn answer_in_tx(rig: &Rig, ask_id: i64) -> calm_server::error::Result<()> {
    let track = rig.track.clone();
    calm_server::db::write_with_actor_events_typed::<(), _>(
        rig.repo.as_ref(),
        None,
        &rig.events,
        &WriteContext::new(rig.role_cache.clone(), rig.area_cache.clone()),
        move |tx| {
            Box::pin(async move {
                let (scope, events) = calm_server::ask::ask_answered_tx(
                    tx,
                    &track,
                    ask_id,
                    vec![AskAnswer::Option(0)],
                )
                .await?;
                Ok((
                    (),
                    events
                        .into_iter()
                        .map(|event| (ActorId::User, scope.clone(), event))
                        .collect(),
                ))
            })
        },
    )
    .await
    .map(|_| ())
}

/// A turn that ends waits on none of its requests any more.
#[tokio::test]
async fn a_turn_end_withdraws_its_held_asks() {
    let rig = rig().await;
    let turn = rig.start_turn().await;
    let (ask_id, mut request) = rig.open("req-1", CONNECTION).await;
    rig.daemon
        .emit_notification_for_test(Notification::TurnCompleted {
            thread_id: THREAD.into(),
            turn: json!({ "id": turn, "status": "interrupted" }),
        });
    assert_eq!(request.told().await, Outcome::Refused);
    wait_for("the withdrawal", || rig.withdrawn(ask_id)).await;
    assert!(rig.activity_items().await.is_empty());
    rig.harness.shutdown().await.unwrap();
}

/// A turn that starts withdraws every open ask whose request no table holds: here one whose
/// entry left the table without the run loop seeing it go.
#[tokio::test]
async fn a_turn_start_withdraws_an_ask_no_table_holds() {
    let rig = rig().await;
    let (ask_id, mut request) = rig.open("req-1", CONNECTION).await;
    drop(rig.harness.held_requests().take(ask_id));
    assert_eq!(request.told().await, Outcome::Refused);
    assert!(
        !rig.withdrawn(ask_id).await,
        "nothing has swept since the entry left"
    );
    assert_eq!(rig.activity_items().await.len(), 1);

    rig.start_turn().await;
    wait_for("the turn-start sweep", || rig.withdrawn(ask_id)).await;
    assert!(rig.activity_items().await.is_empty());
    rig.harness.shutdown().await.unwrap();
}

/// A session that failed and came back idle under the same id still has the ask its old harness
/// held; the harness built for it holds no request, so its construction sweep withdraws the ask.
/// While the session is failed the ask is hidden, not closed.
#[tokio::test]
async fn the_sweep_closes_a_leftover_ask_after_failed_to_idle_recovery() {
    let mut rig = rig().await;
    let (ask_id, mut request) = rig.open("req-1", CONNECTION).await;
    rig.harness.shutdown().await.unwrap();
    assert_eq!(request.told().await, Outcome::Refused);
    assert!(
        !rig.withdrawn(ask_id).await,
        "a stopping harness writes nothing"
    );

    rig.set_session_state("failed").await;
    assert!(
        rig.activity_items().await.is_empty(),
        "a failed session's ask is hidden"
    );
    rig.set_session_state("idle").await;
    assert_eq!(
        rig.activity_items().await.len(),
        1,
        "the leftover ask shows again"
    );

    rig.harness = rig.run_harness().await;
    wait_for("the construction sweep", || rig.withdrawn(ask_id)).await;
    assert!(rig.activity_items().await.is_empty());
    let (status, body) = rig.answer(ask_id, json!([{ "option": 0 }])).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    rig.harness.shutdown().await.unwrap();
}

/// A dismissal does not close a paused turn's ask.
#[tokio::test]
async fn a_dismissal_does_not_close_a_hold_ask() {
    let rig = rig().await;
    let (ask_id, _request) = rig.open("req-1", CONNECTION).await;
    let (status, body) = rig
        .post(
            format!("/api/tracks/{}/activity/dismissals", rig.track),
            json!({ "key": format!("ask:{ask_id}") }),
        )
        .await;
    assert!(status.is_success(), "{status}: {body}");
    let items = rig.activity_items().await;
    assert!(
        matches!(items.as_slice(), [ActivityItem::Ask { ask_id: id, .. }] if *id == ask_id),
        "{items:?}"
    );
    rig.harness.shutdown().await.unwrap();
}

/// Waiting on the user is not turn time: approving after 31 minutes of waiting does not trip the
/// 30-minute turn watchdog, neither while waiting nor after the answer.
#[tokio::test]
async fn approving_after_31_minutes_of_waiting_is_not_interrupted() {
    let rig = rig().await;
    let turn = rig.start_turn().await;
    let (ask_id, mut request) = rig.open("req-1", CONNECTION).await;
    wait_for("a watchdog tick to see the held request", || {
        rig.harness.watchdog_saw_held_request_for_test()
    })
    .await;
    rig.harness
        .rewind_turn_clock_for_test(Duration::from_secs(31 * 60))
        .await;
    let ticks = || tokio::time::sleep(Duration::from_millis(300));
    ticks().await;
    assert!(
        matches!(rig.harness.state_for_test().await, HarnessState::TurnRunning { turn_id, .. } if turn_id == turn),
        "{:?}",
        rig.harness.state_for_test().await
    );

    let (status, body) = rig.answer(ask_id, json!([{ "option": 0 }])).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(request.told().await, Outcome::Answered(0));
    ticks().await;
    assert!(
        matches!(rig.harness.state_for_test().await, HarnessState::TurnRunning { turn_id, .. } if turn_id == turn),
        "{:?}",
        rig.harness.state_for_test().await
    );
    assert!(rig.daemon.interrupted_turns_for_test().is_empty());
    rig.harness.shutdown().await.unwrap();
}
