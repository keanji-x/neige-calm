//! #2209 U2 — a question Codex's own model put to the user (`request_user_input_async`) becomes the
//! Planner's ask. Driven from the captured app-server frame through the production mapping and
//! the run loop's item path to the stored `ask.requested` and the activity overlay.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use calm_server::card_role_cache::CardRoleCache;
use calm_server::codex_appserver::Notification;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, begin_immediate_tx, card_create_with_id_tx, session_prepare_deferred_planner_tx,
    session_start_runtime_tx,
};
use calm_server::event::{AskQuestion, Event, EventBus};
use calm_server::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessRegistry, HarnessSnapshot, PlannerHarness,
    PlannerHarnessParams,
};
use calm_server::ids::{ActorId, CardId, TrackId};
use calm_server::model::{CardRole, NewArea, NewCard, NewTrack, new_id, now_ms};
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::WriteContext;
use calm_server::terminal_renderer::TerminalRendererRegistry;
use calm_server::track_activity::{ActivityItem, Recompute, TrackActivityProjector};
use calm_server::track_area_cache::TrackAreaCache;
use calm_types::worker::WorkerSessionId;
use serde_json::{Value, json};

const THREAD: &str = "thread-native-ask";
/// The captured frame's item id: the ask's `source_item_id`.
const ITEM_ID: &str = "call_3JC4f04gLFDW8kyo92TOgOPa";
/// The captured `item/completed` params of a native question; see the `_provenance` block inside.
const FIXTURE: &str = include_str!("../fixtures/item_completed_native_question.json");

/// The captured params, on this file's thread.
fn captured_params() -> Value {
    let mut params = serde_json::from_str::<Value>(FIXTURE).unwrap()["params"].clone();
    params["threadId"] = json!(THREAD);
    params
}

fn captured_questions() -> Vec<AskQuestion> {
    let question = &captured_params()["item"]["questions"][0];
    vec![AskQuestion {
        title: question["title"].as_str().unwrap().into(),
        options: question["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|option| option.as_str().unwrap().into())
            .collect(),
    }]
}

/// Which conversation the card runs: the three profiles that share the harness.
#[derive(Clone, Copy)]
enum Profile {
    Planner,
    PlainChat,
    Assistant,
}

struct Rig {
    repo: Arc<SqlxRepo>,
    events: EventBus,
    role_cache: CardRoleCache,
    area_cache: TrackAreaCache,
    daemon: Arc<SharedCodexAppServer>,
    harness: PlannerHarness,
    session_id: String,
    card: CardId,
    track: TrackId,
}

async fn rig(profile: Profile) -> Rig {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "native-ask".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "native ask".into(),
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
    let (role, payload, kind) = match profile {
        Profile::Planner => (
            CardRole::Planner,
            json!({"schemaVersion": 1, "planner_harness": true, "planner_provider": "codex"}),
            WorkerSessionKind::SharedPlanner,
        ),
        Profile::PlainChat => (
            CardRole::Worker,
            json!({"schemaVersion": 1, "harness_profile": "plain_chat"}),
            WorkerSessionKind::CodexCard,
        ),
        Profile::Assistant => (
            CardRole::Assistant,
            json!({"schemaVersion": 1, "harness_profile": "assistant"}),
            WorkerSessionKind::CodexCard,
        ),
    };
    let mut tx = repo.pool().begin().await.unwrap();
    let card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload,
        },
        role,
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
            kind,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some(THREAD.into()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(snapshot()).unwrap()),
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let events = EventBus::new();
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let harness = run_harness(
        &repo,
        &events,
        &role_cache,
        &area_cache,
        &daemon,
        &session_id,
        &card.id,
        &track.id,
    )
    .await;
    Rig {
        repo,
        events,
        role_cache,
        area_cache,
        daemon,
        harness,
        session_id,
        card: card.id,
        track: track.id,
    }
}

fn snapshot() -> HarnessSnapshot {
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(THREAD.into());
    snapshot
}

/// A harness on the card, as boot recovery or a start installs one; ready once it listens.
#[allow(clippy::too_many_arguments)]
async fn run_harness(
    repo: &Arc<SqlxRepo>,
    events: &EventBus,
    role_cache: &CardRoleCache,
    area_cache: &TrackAreaCache,
    daemon: &Arc<SharedCodexAppServer>,
    session_id: &str,
    card: &CardId,
    track: &TrackId,
) -> PlannerHarness {
    let listening = daemon.notification_receiver_count_for_test();
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let harness = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: session_id.to_string(),
        track_id: track.clone(),
        card_id: card.clone(),
        thread_id: Some(THREAD.into()),
        repo: repo_dyn,
        events: events.clone(),
        card_role_cache: role_cache.clone(),
        track_area_cache: area_cache.clone(),
        backend: daemon.clone().into(),
        live_replies: calm_server::harness::LiveReplies::for_test(),
        config: HarnessConfig {
            debounce_min_idle: Duration::from_secs(60),
            debounce_max_wait: Duration::from_secs(60),
            ..HarnessConfig::default()
        },
        snapshot: snapshot(),
    });
    wait_for("the harness to listen", || async {
        daemon.notification_receiver_count_for_test() > listening
    })
    .await;
    harness
}

async fn wait_for<F, Fut>(what: &str, mut done: F)
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
    fn emit(&self, method: &str, params: Value) {
        self.daemon.emit_notification_for_test(Notification::Item {
            method: method.into(),
            params,
        });
    }

    /// Frames are handled in order: once a plain reply emitted after them has its row and its
    /// `harness.item.added`, every frame before it was handled, and the harness went on.
    async fn settle(&self) {
        let id = format!("sentinel-{}", new_id());
        self.emit(
            "item/completed",
            json!({ "threadId": THREAD, "turnId": "turn-sentinel",
                "item": { "id": id, "type": "agentMessage", "text": "sentinel" } }),
        );
        wait_for("the sentinel to be handled", || async {
            self.item_added(&id).await
        })
        .await;
    }

    /// Whether the item's row is stored and announced: the `harness.item.added` naming it, which
    /// the transcript reader follows, is written after the row.
    async fn item_added(&self, item_id: &str) -> bool {
        self.repo
            .events_for_track(self.track.as_str(), &["harness.item.added"], None)
            .await
            .unwrap()
            .into_iter()
            .any(|row| serde_json::to_value(&row.event).unwrap()["data"]["item_uuid"] == item_id)
    }

    /// `(actor, event)` of every `ask.requested` on the track.
    async fn asks(&self) -> Vec<(ActorId, Event)> {
        self.repo
            .events_for_track(self.track.as_str(), &["ask.requested"], None)
            .await
            .unwrap()
            .into_iter()
            .map(|row| (row.actor, row.event))
            .collect()
    }

    async fn ask_ids(&self) -> Vec<i64> {
        self.repo
            .events_for_track(self.track.as_str(), &["ask.requested"], None)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.id)
            .collect()
    }

    /// The activity items the production projector computes for the track.
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
}

/// Everything logged at WARN or above on this thread while it is held; the run loop runs on the
/// test's current-thread runtime, so its lines land here.
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logs {
    type Writer = Logs;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl Logs {
    fn capture() -> (Self, tracing::subscriber::DefaultGuard) {
        let logs = Logs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        (logs, tracing::subscriber::set_default(subscriber))
    }

    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

const REFUSED: &str = "refused to ask the user a provider question";

#[tokio::test]
async fn a_planner_native_question_becomes_one_open_ask() {
    let rig = rig(Profile::Planner).await;
    rig.emit("item/completed", captured_params());
    rig.settle().await;

    let asks = rig.asks().await;
    assert!(
        matches!(asks.as_slice(), [(ActorId::AiPlannerSession(session), Event::AskRequested {
                track_id, questions, source_item_id: Some(source) })]
            if session.as_str() == rig.session_id && track_id == &rig.track
                && questions == &captured_questions() && source == ITEM_ID),
        "{asks:?}"
    );
    // The reply is stored and announced as before: the ask is beside it, not instead of it.
    assert!(rig.item_added(ITEM_ID).await);

    let ask_id = rig.ask_ids().await[0];
    let items = rig.activity_items().await;
    assert_eq!(
        items,
        vec![ActivityItem::Ask {
            key: format!("ask:{ask_id}"),
            text: captured_questions()[0].title.clone(),
            at_ms: items[0].at_ms(),
            ask_id,
            questions: captured_questions(),
        }]
    );
    rig.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_same_item_asks_once_across_duplicate_frames_and_a_restart() {
    let (logs, _guard) = Logs::capture();
    let rig = rig(Profile::Planner).await;
    rig.emit("item/completed", captured_params());
    rig.emit("item/completed", captured_params());
    rig.settle().await;
    assert_eq!(rig.asks().await.len(), 1, "a duplicate frame asks nothing");

    // A restarted harness on the same card sees the item again, as a replay delivers it.
    rig.harness.shutdown().await.unwrap();
    let restarted = run_harness(
        &rig.repo,
        &rig.events,
        &rig.role_cache,
        &rig.area_cache,
        &rig.daemon,
        &rig.session_id,
        &rig.card,
        &rig.track,
    )
    .await;
    rig.emit("item/completed", captured_params());
    rig.settle().await;
    assert_eq!(
        rig.asks().await.len(),
        1,
        "a replay after a restart asks nothing"
    );
    assert!(
        !logs.text().contains(REFUSED),
        "an item already asked is not a refusal: {}",
        logs.text()
    );
    restarted.shutdown().await.unwrap();
}

/// The harness reads first, so a repeat is quiet; the entry's own check in the write transaction
/// is what keeps two writers racing on one item to one ask.
#[tokio::test]
async fn the_entry_asks_once_per_item_inside_its_transaction() {
    let rig = rig(Profile::Planner).await;
    rig.emit("item/completed", captured_params());
    rig.settle().await;
    assert_eq!(rig.asks().await.len(), 1);
    rig.harness.shutdown().await.unwrap();

    let mut tx = begin_immediate_tx(rig.repo.pool()).await.unwrap();
    for (item, expect_ask) in [(ITEM_ID, false), ("call-another-item", true)] {
        let asked = calm_server::ask::provider_ask_requested_tx(
            &mut tx,
            &rig.card,
            captured_questions(),
            item.to_string(),
        )
        .await
        .unwrap();
        assert_eq!(asked.is_some(), expect_ask, "{item}");
    }
    tx.rollback().await.unwrap();
}

/// A reset supersedes the old session before it stops the old harness; a question the old
/// conversation reports in between is not the card's to ask.
#[tokio::test]
async fn a_superseded_session_asks_nothing() {
    let rig = rig(Profile::Planner).await;
    let mut tx = begin_immediate_tx(rig.repo.pool()).await.unwrap();
    session_prepare_deferred_planner_tx(
        &mut tx,
        &WorkerSessionInit {
            id: new_id(),
            card_id: rig.card.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Starting,
            terminal_run_id: None,
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(
                serde_json::to_value(HarnessSnapshot::initial(0, vec![])).unwrap(),
            ),
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let old = rig
        .repo
        .session_get_by_id(&WorkerSessionId::from(rig.session_id.as_str()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.state, WorkerSessionState::Superseded);

    rig.emit("item/completed", captured_params());
    rig.settle().await;
    assert!(rig.item_added(ITEM_ID).await, "the reply is stored");
    let asks = rig.asks().await;
    assert!(asks.is_empty(), "{asks:?}");
    rig.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_started_item_asks_nothing() {
    let rig = rig(Profile::Planner).await;
    let mut params = captured_params();
    params.as_object_mut().unwrap().remove("completedAtMs");
    params["startedAtMs"] = json!(1_791_192_159_000_i64);
    rig.emit("item/started", params);
    rig.settle().await;
    assert!(rig.item_added(ITEM_ID).await, "the started row is stored");
    let asks = rig.asks().await;
    assert!(asks.is_empty(), "{asks:?}");
    rig.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn plain_chat_and_assistant_conversations_ask_nothing() {
    for profile in [Profile::PlainChat, Profile::Assistant] {
        let (logs, _guard) = Logs::capture();
        let rig = rig(profile).await;
        rig.emit("item/completed", captured_params());
        rig.settle().await;
        assert!(rig.item_added(ITEM_ID).await, "the reply is stored");
        let asks = rig.asks().await;
        assert!(asks.is_empty(), "{asks:?}");
        assert!(
            !logs.text().contains(REFUSED),
            "not a Planner conversation is not a refusal: {}",
            logs.text()
        );
        rig.harness.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn a_refused_question_is_logged_and_skipped_and_the_harness_goes_on() {
    let (logs, _guard) = Logs::capture();
    let rig = rig(Profile::Planner).await;
    let mut too_many = captured_params();
    too_many["item"]["id"] = json!("call-too-many");
    too_many["item"]["questions"] = json!(vec![json!({ "title": "Which?" }); 9]);
    let mut too_long = captured_params();
    too_long["item"]["id"] = json!("call-too-long");
    too_long["item"]["questions"] = json!([{ "title": "x".repeat(2001) }]);
    rig.emit("item/completed", too_many);
    rig.emit("item/completed", too_long);
    rig.settle().await;

    let asks = rig.asks().await;
    assert!(asks.is_empty(), "{asks:?}");
    for id in ["call-too-many", "call-too-long"] {
        assert!(
            rig.item_added(id).await,
            "{id}: the reply is stored, and a refused ask does not fail its event"
        );
        assert!(
            logs.text()
                .lines()
                .any(|line| line.contains(REFUSED) && line.contains(id)),
            "{id}: {}",
            logs.text()
        );
    }
    // And the next question is asked as usual.
    rig.emit("item/completed", captured_params());
    rig.settle().await;
    assert_eq!(rig.asks().await.len(), 1);
    rig.harness.shutdown().await.unwrap();
}
