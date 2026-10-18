//! The application entry point must execute and recover the original accepted input.
//! Only the external provider is a protocol peer; no fixture claims or advances a turn.
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::auth::{AuthConfig, AuthState};
use crate::db::prelude::*;
use crate::db::sqlite::SqlxRepo;
use crate::event::EventBus;
use crate::harness::{BootRows, HarnessPhaseTag};
use crate::plugin_host::{PluginHost, PluginRegistry};
use crate::shared_codex_appserver::native_owner_tests::{connect, notify_thread, request, send};
use crate::state::{AppState, CodexClient, DaemonClient, WriteContext};
use calm_types::conversation::InputDeliveryFact;
use calm_types::harness::snapshot::CanonicalHarnessSnapshot;

const FIRST_BODY: &str = "Preserve this accepted input through native activation";
const THREAD: &str = "http-canonical-original-thread";
const TURN: &str = "http-canonical-original-turn";

mod control_tests;

struct Fixture {
    repo: Arc<SqlxRepo>,
    root: tempfile::TempDir,
    url: String,
}

impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            root.path().join("canonical-flow.db").display()
        );
        Self {
            repo: Arc::new(SqlxRepo::open(&url).await.unwrap()),
            root,
            url,
        }
    }

    async fn application(&self) -> (AppState, axum::Router, String) {
        self.application_with_historical_native(false).await
    }

    async fn application_with_historical_native(
        &self,
        running: bool,
    ) -> (AppState, axum::Router, String) {
        let (state, app, cookie, _) = self.application_with_auth(running).await;
        (state, app, cookie)
    }

    async fn application_with_auth(
        &self,
        running: bool,
    ) -> (AppState, axum::Router, String, AuthState) {
        let repo: Arc<dyn Repo> = self.repo.clone();
        let events = EventBus::new();
        let roles = crate::card_role_cache::CardRoleCache::new();
        let areas = crate::track_area_cache::TrackAreaCache::new();
        self.repo.seed_card_role_cache(&roles).await.unwrap();
        self.repo.seed_track_area_cache(&areas).await.unwrap();
        let plugin = Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            std::path::PathBuf::new(),
            self.root.path().join("plugins-data"),
            vec![],
            events.clone(),
            WriteContext::new(roles.clone(), areas.clone()),
        ));
        let mut state = AppState::from_parts(
            repo,
            events,
            Arc::new(DaemonClient::new_stub()),
            plugin,
            Arc::new(CodexClient::new_stub()),
            Some(roles),
            Some(areas),
        )
        .with_workspace_root(self.root.path().join("workspaces"));
        if running {
            state = state.with_shared_codex_appserver(
                crate::shared_codex_appserver::SharedCodexAppServer::new_fake_running_with_pending(
                    self.repo.clone(),
                    None,
                ),
            );
        }
        let auth = AuthState::new(AuthConfig {
            username: Some("jian".into()),
            password: Some("fixture-password".into()),
            dev_autologin: false,
            display_name: "Jian".into(),
        });
        let app = super::application_router(state.clone(), auth.clone());
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/login")
                    .extension(ConnectInfo("127.0.0.1:4444".parse::<SocketAddr>().unwrap()))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({"username":"jian","password":"fixture-password"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        (state, app, cookie, auth)
    }

    async fn carrier(&self, track: &str) -> (String, String, CanonicalHarnessSnapshot) {
        let (card, worker, snapshot): (String, String, String) = sqlx::query_as(
            "SELECT c.id, ws.id, ws.handle_state_json
             FROM cards c JOIN worker_sessions ws ON c.session_id=ws.id
             WHERE c.track_id=?1 AND c.role='planner'",
        )
        .bind(track)
        .fetch_one(self.repo.pool())
        .await
        .unwrap();
        (card, worker, serde_json::from_str(&snapshot).unwrap())
    }

    async fn receipt(
        &self,
        area: &str,
    ) -> crate::db::sqlite::conversation::HumanTrackCreationRecord {
        let mut tx = self.repo.pool().begin().await.unwrap();
        let receipt =
            crate::db::sqlite::conversation::human_track_creation_get_tx(&mut tx, area, "first")
                .await
                .unwrap()
                .expect("original committed creation receipt");
        tx.rollback().await.unwrap();
        receipt
    }

    async fn counts(&self, card: &str) -> (i64, i64, i64, i64, i64) {
        sqlx::query_as(
            "SELECT
             (SELECT count(*) FROM conversation_messages WHERE card_id=?1),
             (SELECT count(*) FROM conversation_message_revisions r
              JOIN conversation_messages m ON m.id=r.message_id WHERE m.card_id=?1),
             (SELECT count(*) FROM worker_sessions WHERE card_id=?1),
             (SELECT count(*) FROM conversation_input_submissions WHERE card_id=?1),
             (SELECT count(*) FROM harness_items
              WHERE card_id=?1 AND item_uuid='original-native-reply')",
        )
        .bind(card)
        .fetch_one(self.repo.pool())
        .await
        .unwrap()
    }

    async fn wait_phase(&self, track: &str, phase: HarnessPhaseTag) -> CanonicalHarnessSnapshot {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let (_, _, frame) = self.carrier(track).await;
                if frame.header.phase == phase {
                    return frame;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the real application must advance the original execution record")
    }

    async fn wait_for_stored_reply(&self, card: &str) {
        // END may be consumed by tick before queued content observations. Wait
        // for the independent persisted body/presentation fact as well.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let payload: Option<String> = sqlx::query_scalar(
                    "SELECT i.params FROM conversation_presentations p
                     JOIN harness_items i ON i.id=p.native_item_row_id
                     WHERE p.card_id=?1 AND p.source_kind='native_item'
                       AND p.erased_by_operation_id IS NULL
                       AND i.item_uuid='original-native-reply'",
                )
                .bind(card)
                .fetch_optional(self.repo.pool())
                .await
                .unwrap();
                if let Some(payload) = payload {
                    let item: Value = serde_json::from_str(&payload).unwrap();
                    assert_eq!(item["item"]["text"], "Original execution result");
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the actual native reply and its presentation must both be committed");
    }
}

async fn post(app: &axum::Router, cookie: &str, uri: &str, key: &str, body: Value) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .header("Idempotency-Key", key)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(status.is_success(), "{uri}: {status} {body}");
    body
}

mod cold_recovery_tests;
mod historical_addressability_tests;
mod historical_compatibility_tests;
mod native_attempt_recovery_tests;
mod source_readmission_tests;

async fn original_input_flow(
    native_initially_available: bool,
    cold_second_turn: Option<cold_recovery_tests::Scenario>,
) {
    let mut fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "area",
        json!({"name":"Original carrier flow","color":"#123456"}),
    )
    .await;
    let create = json!({
        "area_id":area["id"],"title":"","planner_provider":"codex",
        "theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY
    });
    let native = if native_initially_available {
        Some(connect(&state.shared_codex_appserver).await)
    } else {
        None
    };
    // Acceptance must finish independently of any provider request/response.
    let created = tokio::time::timeout(
        Duration::from_secs(5),
        post(&app, &cookie, "/api/tracks", "first", create.clone()),
    )
    .await
    .expect("a committed message cannot wait for native startup");
    let track = created["id"].as_str().unwrap();
    let (card, worker, before) = fixture.carrier(track).await;
    let accepted_refs = before.canonical_inputs.pending.clone();
    assert_eq!(accepted_refs.len(), 1);
    let area_id = area["id"].as_str().unwrap();
    let original_receipt = fixture.receipt(area_id).await;
    let crate::db::sqlite::conversation::HumanTrackCreationAck::V1 { first_message, .. } =
        &original_receipt.acknowledgement;
    assert_eq!(
        accepted_refs[0].input_ref(),
        calm_types::conversation::AcceptedInputRef::Human {
            message: first_message.clone()
        }
    );
    let (client, mut peer) = match native {
        Some(native) => native,
        None => {
            assert!(before.canonical_inputs.active.is_none());
            assert!(before.canonical_inputs.attempts.is_empty());
            let native = connect(&state.shared_codex_appserver).await;
            let replay = post(&app, &cookie, "/api/tracks", "first", create.clone()).await;
            assert_eq!(replay["id"], created["id"]);
            native
        }
    };
    let turn_start = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let call = request(&mut peer).await;
            let result = match call["method"].as_str().unwrap() {
                "config/read" => json!({"config":{}}),
                "thread/start" => json!({"thread":{"id":THREAD}}),
                "turn/start" => break call,
                method => panic!("unexpected native call during original activation: {method}"),
            };
            send(&mut peer, json!({"id":call["id"],"result":result})).await;
        }
    })
    .await
    .expect("the production post-commit wake must activate the accepted carrier");
    assert_eq!(turn_start["params"]["threadId"], THREAD);
    assert!(
        turn_start["params"]["input"]
            .to_string()
            .contains(FIRST_BODY)
    );
    let crossing = fixture.carrier(track).await;
    assert_eq!((&crossing.0, &crossing.1), (&card, &worker));
    assert!(crossing.2.canonical_inputs.pending.is_empty());
    assert_eq!(crossing.2.canonical_inputs.attempts.len(), 1);
    assert!(matches!(
        crossing.2.canonical_inputs.attempts[0].delivery,
        InputDeliveryFact::CrossingUnknown
    ));
    let original_refs = accepted_refs
        .iter()
        .map(calm_types::conversation::PendingInput::input_ref)
        .collect::<Vec<_>>();
    let mut tx = fixture.repo.pool().begin().await.unwrap();
    let submission = crate::db::sqlite::conversation::submission_plan_tx(
        &mut tx,
        &crossing.2.canonical_inputs.attempts[0]
            .attempt
            .submission_id,
    )
    .await
    .unwrap();
    assert_eq!(submission.plan.input_refs().unwrap(), original_refs);
    let identity = crate::db::sqlite::conversation::submission_identity_tx(
        &mut tx,
        &crossing.2.canonical_inputs.attempts[0]
            .attempt
            .submission_id,
    )
    .await
    .unwrap();
    let active = crossing.2.canonical_inputs.active.as_ref().unwrap();
    assert_eq!(identity.card_id, card);
    assert_eq!(identity.ordinal, 0, "the original Initial submission");
    assert_eq!(identity.logical_turn_id, active.logical_turn_id);
    assert_eq!(
        active.submission_ids,
        vec![
            crossing.2.canonical_inputs.attempts[0]
                .attempt
                .submission_id
                .clone()
        ]
    );
    tx.rollback().await.unwrap();
    send(
        &mut peer,
        json!({"id":turn_start["id"],"result":{"turn":{"id":TURN}}}),
    )
    .await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/started",
        json!({"id":TURN,"status":"inProgress"}),
    )
    .await;
    let running = fixture
        .wait_phase(track, HarnessPhaseTag::TurnRunning)
        .await;
    assert!(matches!(
        running.canonical_inputs.attempts[0].delivery,
        InputDeliveryFact::Accepted { .. }
    ));
    send(&mut peer, json!({"method":"item/completed","params":{
        "threadId":THREAD,"turnId":TURN,
        "item":{"id":"original-native-reply","type":"agentMessage","text":"Original execution result"}
    }})).await;
    notify_thread(
        &state.shared_codex_appserver,
        &mut peer,
        THREAD,
        "turn/completed",
        json!({"id":TURN,"status":"completed"}),
    )
    .await;
    let done = fixture
        .wait_phase(track, HarnessPhaseTag::TurnCompleted)
        .await;
    assert!(done.canonical_inputs.active.is_none());
    assert_eq!(
        done.canonical_inputs.attempts,
        running.canonical_inputs.attempts
    );
    fixture.wait_for_stored_reply(&card).await;
    let replay = post(&app, &cookie, "/api/tracks", "first", create.clone()).await;
    assert_eq!(replay["id"], created["id"]);
    assert_eq!(
        fixture.carrier(track).await.2.canonical_inputs,
        done.canonical_inputs
    );
    assert_eq!(fixture.counts(&card).await, (1, 1, 1, 1, 1));

    let harness = state
        .harness
        .remove(&worker)
        .expect("actual installed harness");
    let mut stopping = tokio::spawn(async move { harness.shutdown().await });
    tokio::select! {
        result = &mut stopping => result.unwrap().unwrap(),
        call = request(&mut peer) => {
            assert_eq!(call["method"],"turn/interrupt");
            send(&mut peer,json!({"id":call["id"],"result":{}})).await;
            stopping.await.unwrap().unwrap();
        }
    }
    drop((client, peer, state, app));
    fixture.repo.pool().close().await;
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    let (restarted, app, cookie) = fixture.application().await;
    // Invoke the actual boot owner with no restored Human source. Recovery may
    // reload facts; it cannot invent another native execution from old authors.
    restarted
        .recover_harnesses_on_boot(BootRows::All)
        .await
        .unwrap();
    if cold_second_turn.is_none() {
        // The original replay-only cases exercise this post-reopen wake. Cold
        // scenarios instead issue one new message below: a second authorized
        // POST may independently re-admit its then-current pending head.
        let replay = post(&app, &cookie, "/api/tracks", "first", create).await;
        assert_eq!(replay["id"], created["id"]);
    }
    let after = fixture.carrier(track).await;
    assert_eq!((&after.0, &after.1), (&card, &worker));
    assert_eq!(after.2.canonical_inputs, done.canonical_inputs);
    assert_eq!(after.2.header.phase, HarnessPhaseTag::TurnCompleted);
    assert_eq!(after.2.header.last_thread_id.as_deref(), Some(THREAD));
    assert_eq!(after.2.header.last_turn_id.as_deref(), Some(TURN));
    assert_eq!(fixture.receipt(area_id).await, original_receipt);
    assert_eq!(fixture.counts(&card).await, (1, 1, 1, 1, 1));
    if let Some(scenario) = cold_second_turn {
        cold_recovery_tests::second_turn(
            &fixture,
            &restarted,
            &app,
            &cookie,
            (track, &card, &worker),
            scenario,
        )
        .await;
        assert_eq!(fixture.receipt(area_id).await, original_receipt);
    }
}

#[tokio::test]
async fn production_http_executes_original_input_and_reopens_its_completed_record() {
    original_input_flow(true, None).await;
}

#[tokio::test]
async fn production_http_retry_after_native_unavailability_resumes_original_head() {
    original_input_flow(false, None).await;
}
