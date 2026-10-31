#![cfg(test)]
//! The application entry point must execute and recover the original accepted input.
//! Only the external provider is a protocol peer; no fixture claims or advances a turn.
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{ConnectInfo, FromRef};
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::auth::AuthState;
use crate::db::prelude::*;
use crate::db::sqlite::SqlxRepo;
use crate::harness::{BootRows, HarnessPhaseTag};
use crate::shared_codex_appserver::native_owner_tests::{
    connect, notify_thread, request as native_request, send,
};
use crate::state::AppState;
use calm_types::conversation::InputDeliveryFact;
use calm_types::harness::snapshot::CanonicalHarnessSnapshot;

/// Actual production boot refreshes the MCP catalog when the peer becomes Running.
/// Reply only to this owned maintenance protocol; execution RPCs stay observable.
async fn request(peer: &mut crate::shared_codex_appserver::native_owner_tests::Peer) -> Value {
    loop {
        let call = native_request(peer).await;
        if call["method"] != "config/mcpServer/reload" {
            return call;
        }
        assert!(
            call["params"].is_null(),
            "the actual MCP reload protocol has null params"
        );
        send(peer, json!({"id":call["id"],"result":{}})).await;
    }
}

const FIRST_BODY: &str = "Preserve this accepted input through native activation";
const THREAD: &str = "http-canonical-original-thread";
const TURN: &str = "http-canonical-original-turn";

mod archive_start_inventory_tests;
mod assistant_canonical;
mod boot_catalog_follower_tests;
#[cfg(feature = "fixtures")]
mod catalog_readiness_tests;
mod child_bootstrap_tests;
mod child_journal_tests;
mod claude_creation_tests;
mod control_tests;
mod dispatcher_inputs_tests;
mod dispatcher_report_edit_tests;
mod g4_system_creation_tests;
mod input_feedback_tests;
#[cfg(feature = "fixtures")]
mod input_source_handoff_tests;
#[cfg(feature = "fixtures")]
mod opencode_creation_tests;
mod pending_controls_tests;
#[cfg(feature = "fixtures")]
mod planner_switch_tests;
mod presentation_capability_tests;
mod presentation_read_feedback_tests;
mod resolution_tests;
mod system_assistant_tests;

struct Fixture {
    repo: Arc<SqlxRepo>,
    root: tempfile::TempDir,
    url: String,
    data_dir: Option<std::path::PathBuf>,
    workspace_root: Option<std::path::PathBuf>,
    claude_config: Option<std::path::PathBuf>,
    acp_config: Option<std::path::PathBuf>,
    templates_dir: Option<std::path::PathBuf>,
    execution: Option<crate::test_support::execution::Supervisor>,
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
            data_dir: None,
            workspace_root: None,
            claude_config: None,
            acp_config: None,
            templates_dir: None,
            execution: None,
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

    // Heap the full production bootstrap once; two sequential boots otherwise
    // make the enclosing integration-test future exceed the normal test stack.
    fn application_with_auth(
        &self,
        running: bool,
    ) -> futures::future::BoxFuture<'_, (AppState, axum::Router, String, AuthState)> {
        Box::pin(async move {
            use clap::Parser;
            let mut cfg = crate::config::Config::parse_from(["calm-server"]);
            cfg.db_url = self.url.clone();
            cfg.claude_planner_config = self.claude_config.clone();
            cfg.acp_planner_config = self.acp_config.clone();
            cfg.templates_dir = self.templates_dir.clone();
            if self.acp_config.is_some() {
                // Test binaries live in deps/, not beside the workspace's production shim.
                // Match the ACP stack fixture: never depend on a host-installed shim on PATH.
                let exe = std::env::current_exe().unwrap();
                let shim = exe
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("neige-mcp-stdio-shim");
                assert!(
                    shim.is_file(),
                    "build neige-mcp-stdio-shim before ACP tests"
                );
                cfg.mcp_stdio_shim_bin = Some(shim);
            }
            if let Some(execution) = &self.execution {
                execution.configure(&mut cfg);
            }
            cfg.data_dir = Some(
                self.data_dir
                    .clone()
                    .unwrap_or_else(|| self.root.path().join("data")),
            );
            cfg.workspace_root = Some(
                self.workspace_root
                    .clone()
                    .unwrap_or_else(|| self.root.path().join("workspaces")),
            );
            cfg.plugins_dir = Some(self.root.path().join("plugins"));
            cfg.plugins_data_dir = Some(self.root.path().join("plugins-data"));
            cfg.auth_username = Some("jian".into());
            cfg.auth_password = Some("fixture-password".into());
            cfg.auth_dev_autologin = false;
            cfg.codex_bin = self
                .root
                .path()
                .join("no-real-codex")
                .to_string_lossy()
                .into_owned();
            cfg.claude_bin = self
                .root
                .path()
                .join("no-real-claude")
                .to_string_lossy()
                .into_owned();
            let application = if running {
                AppState::boot_with_native_peer(
                &cfg,
                crate::shared_codex_appserver::SharedCodexAppServer::new_fake_running_with_pending(
                    self.repo.clone(),
                    None,
                ),
            )
            .await
            } else {
                AppState::boot(&cfg).await
            }
            .expect("actual authenticated boot and mandatory recovery");
            let (state, auth) = application.into_parts();
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
        })
    }

    async fn shutdown_process(&self, state: &AppState) {
        state.dispatcher.retire();
        if let Some(server) = &state.mcp_server {
            server.stop_listener_for_test().await;
        }
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
        let (messages, revisions, workers, submissions) = sqlx::query_as(
            "SELECT
             (SELECT count(*) FROM conversation_messages WHERE card_id=?1),
             (SELECT count(*) FROM conversation_message_revisions r
              JOIN conversation_messages m ON m.id=r.message_id WHERE m.card_id=?1),
             (SELECT count(*) FROM worker_sessions WHERE card_id=?1),
             (SELECT count(*) FROM conversation_input_submissions WHERE card_id=?1)",
        )
        .bind(card)
        .fetch_one(self.repo.pool())
        .await
        .unwrap();
        let native = self
            .transcript_count(card, None, None, Some("original-native-reply"))
            .await;
        (messages, revisions, workers, submissions, native)
    }

    async fn transcript_count(
        &self,
        card: &str,
        worker: Option<&str>,
        method: Option<&str>,
        item: Option<&str>,
    ) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM harness_items WHERE card_id=?1
             AND (?2 IS NULL OR worker_session_id=?2)
             AND (?3 IS NULL OR method=?3) AND (?4 IS NULL OR item_uuid=?4)",
        )
        .bind(card)
        .bind(worker)
        .bind(method)
        .bind(item)
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
mod hibernation_tests;
mod historical_addressability_tests;
mod historical_compatibility_tests;
mod native_attempt_recovery_tests;
mod send_boundary_tests;
mod source_readmission_tests;
mod u7_canonical;
mod u8_canonical;
mod u8b_canonical;
mod u9_canonical;
mod workspace_binding_tests;

async fn original_input_flow(
    native_initially_available: bool,
    cold_second_turn: Option<cold_recovery_tests::Scenario>,
) {
    original_input_flow_for_area(
        native_initially_available,
        cold_second_turn,
        crate::model::AreaKind::User,
    )
    .await;
}

async fn original_input_flow_for_area(
    native_initially_available: bool,
    cold_second_turn: Option<cold_recovery_tests::Scenario>,
    area_kind: crate::model::AreaKind,
) {
    let mut fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let area = post(
        &app,
        &cookie,
        match area_kind {
            crate::model::AreaKind::User => "/api/areas",
            crate::model::AreaKind::System => "/api/areas/system",
        },
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
        accepted_refs[0].accepted_input_ref().unwrap(),
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
        .map(|entry| entry.accepted_input_ref().unwrap())
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
    // A replay ACK precedes its asynchronous wake. Settle that invocation
    // before removing the harness, or it can race shutdown into cold recovery.
    let replay_wake = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "canonical-human-wake-finished",
        &card,
        replay_wake.clone(),
    );
    let replay = post(&app, &cookie, "/api/tracks", "first", create.clone()).await;
    tokio::time::timeout(Duration::from_secs(5), replay_wake.entered.notified())
        .await
        .expect("the replay wake must finish before the original harness is removed");
    replay_wake.release.notify_one();
    assert_eq!(replay["id"], created["id"]);
    assert_eq!(
        fixture.carrier(track).await.2.canonical_inputs,
        done.canonical_inputs
    );
    assert_eq!(fixture.counts(&card).await, (1, 1, 1, 1, 1));

    // A receipt replay schedules a real wake. Keep its card fence through
    // teardown so removing the handle cannot look like a cold-recovery request.
    let lifecycle_locks = crate::state::RouteState::from_ref(&state).planner_recovery_locks;
    let _lifecycle = crate::per_card_lock::lock_card(&lifecycle_locks, &card).await;
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
    fixture.shutdown_process(&state).await;
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

mod carrier_replacement_tests;
#[cfg(feature = "fixtures")]
mod unknown_recovery_tests;
async fn shutdown(fixture: &Fixture, state: &AppState) {
    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM worker_sessions")
        .fetch_all(fixture.repo.pool())
        .await
        .unwrap();
    for id in ids {
        if let Some(handle) = state.harness.remove(&id) {
            handle.shutdown().await.unwrap();
        }
    }
    fixture.shutdown_process(state).await;
}

async fn status(app: &axum::Router, cookie: &str, uri: &str, key: &str, body: Value) -> StatusCode {
    app.clone()
        .oneshot(
            Request::post(uri)
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .header("Idempotency-Key", key)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

mod u7_nondelivery;

#[cfg(feature = "fixtures")]
mod p1_workspace_upload;
