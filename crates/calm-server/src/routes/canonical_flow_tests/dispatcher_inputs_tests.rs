//! Mixed sources keep their own real invocations while the sole checkpoint owns order.
use super::*;
use crate::db::TrackEvent;
use crate::shared_codex_appserver::native_owner_tests::Peer;
use calm_types::conversation::PendingInput;

mod child_results;
mod knowledge;
mod phase_trace;
#[cfg(all(test, feature = "fixtures"))]
mod planner_switch_tests;
mod qualification;
mod recovery;
mod reliable_results;
mod retry_read;
mod u9_canonical;
pub(super) mod u9_creator;

const SYSTEM_BODY: &str = "System report work queued during the original Human turn";
const HUMAN_BODY: &str = "A later Human input must continue without another POST";

async fn read(app: &axum::Router, cookie: &str, path: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::get(path)
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(status, StatusCode::OK, "{path}: {body}");
    body
}

async fn human(app: &axum::Router, cookie: &str, card: &str) -> Value {
    let pause = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test("canonical-human-wake-finished", card, pause.clone());
    let accepted = post(
        app,
        cookie,
        &format!("/api/cards/{card}/planner/input"),
        "human-2",
        json!({"text":HUMAN_BODY}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(4), pause.entered.notified())
        .await
        .expect("the real HTTP wake must finish before native restoration or the preceding END");
    pause.release.notify_one();
    accepted
}

struct Flow {
    trace: phase_trace::Trace,
    fixture: Fixture,
    state: AppState,
    app: axum::Router,
    cookie: String,
    area: String,
    track: String,
    card: String,
    worker: String,
    _client: Arc<crate::codex_appserver::CodexAppServer>,
    peer: Peer,
}
impl Flow {
    async fn running() -> Self {
        Self::running_with_model(false).await
    }
    async fn running_with_model(frozen: bool) -> Self {
        Self::running_with_admission(frozen, false).await
    }
    async fn running_with_admission(frozen: bool, admin: bool) -> Self {
        let trace = phase_trace::Trace::new();
        let running = trace.phase("running");
        let setup = trace.phase("running.fixture_application");
        let fixture = Fixture::new().await;
        let (state, app, cookie) = fixture.application().await;
        setup.complete();
        let area = post(
            &app,
            &cookie,
            "/api/areas",
            "area",
            json!({"name":"Mixed original Planner","color":"#123456"}),
        )
        .await;
        let connection = trace.phase("running.native_connect");
        let (client, mut peer) = connect(&state.shared_codex_appserver).await;
        connection.complete();
        let mut body = json!({"area_id":area["id"],"title":"","planner_provider":"codex",
            "theme":{"fg":[255,255,255],"bg":[0,0,0]},"first_message":FIRST_BODY});
        body["admin"] = json!(admin);
        if frozen {
            body["model"] = json!("retry-model");
            body["reasoning_effort"] = json!("high");
        }
        let create = async {
            let phase = trace.phase("running.http_create");
            let created = post(&app, &cookie, "/api/tracks", "first", body).await;
            phase.complete();
            created
        };
        let native = async {
            let phase = trace.phase("running.initial_handshake");
            if frozen {
                let catalog = request(&mut peer).await;
                assert_eq!(catalog["method"], "model/list");
                send(&mut peer, json!({"id":catalog["id"],"error":{"code":-32603,"message":"catalog unavailable"}})).await;
            }
            let call = super::native_attempt_recovery_tests::initial_request(&mut peer).await;
            phase.complete();
            call
        };
        let (created, call) = tokio::join!(create, native);
        let track = created["id"].as_str().unwrap().to_owned();
        let (card, worker, _) = fixture.carrier(&track).await;
        let receipt = trace.phase("running.turn_response_notification");
        send(
            &mut peer,
            json!({"id":call["id"],"result":{"turn":{"id":TURN}}}),
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
        receipt.complete();
        let wait = trace.phase("running.wait_turn_running");
        fixture
            .wait_phase(&track, HarnessPhaseTag::TurnRunning)
            .await;
        wait.complete();
        running.complete();
        Self {
            trace,
            fixture,
            state,
            app,
            cookie,
            area: area["id"].as_str().unwrap().into(),
            track,
            card,
            worker,
            _client: client,
            peer,
        }
    }
    async fn report(&self, text: &str) -> TrackEvent {
        let report = read(
            &self.app,
            &self.cookie,
            &format!("/api/tracks/{}/report", self.track),
        )
        .await;
        post(&self.app, &self.cookie, &format!("/api/tracks/{}/report/blocks", self.track), "report", json!({
            "kind":"prose","markdown":text,"payload":null,"ifDocRev":report["docRev"],"position":null
        })).await;
        let event = crate::db::RepoEventWrite::events_for_track(self.fixture.repo.as_ref(), &self.track, &["track.report_edited"], None).await.unwrap()
            .into_iter().find(|row| matches!(&row.event, crate::event::Event::TrackReportEdited { body_after, .. } if body_after.contains(text))).unwrap();
        self.state
            .dispatcher
            .catch_up_push(self.track.clone().into(), event.event.clone(), event.id)
            .await;
        event
    }
    async fn human(&self) -> Value {
        human(&self.app, &self.cookie, &self.card).await
    }
    async fn next(&mut self) -> Option<Value> {
        let phase = self.trace.phase("next");
        let result = tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                let request_phase = self.trace.phase("next.request");
                let call = request(&mut self.peer).await;
                request_phase.complete();
                match call["method"].as_str().unwrap() {
                    "config/read" => {
                        send(
                            &mut self.peer,
                            json!({"id":call["id"],"result":{"config":{}}}),
                        )
                        .await
                    }
                    "turn/start" => {
                        assert_eq!(call["params"]["threadId"], THREAD);
                        assert!(
                            !call["params"]["input"].to_string().contains(FIRST_BODY),
                            "the first accepted Human input must never repeat"
                        );
                        break call;
                    }
                    method => panic!(
                        "continuing this same carrier must not restart its native thread: {method}"
                    ),
                }
            }
        })
        .await
        .ok();
        phase.finish(if result.is_some() {
            "complete"
        } else {
            "timeout"
        });
        result
    }
    async fn accept(&mut self, call: &Value, turn: &str) {
        send(
            &mut self.peer,
            json!({"id":call["id"],"result":{"turn":{"id":turn}}}),
        )
        .await;
        notify_thread(
            &self.state.shared_codex_appserver,
            &mut self.peer,
            THREAD,
            "turn/started",
            json!({"id":turn,"status":"inProgress"}),
        )
        .await;
        self.fixture
            .wait_phase(&self.track, HarnessPhaseTag::TurnRunning)
            .await;
    }
    async fn end(&mut self, turn: &str) {
        notify_thread(
            &self.state.shared_codex_appserver,
            &mut self.peer,
            THREAD,
            "turn/completed",
            json!({"id":turn,"status":"completed"}),
        )
        .await;
    }
    async fn close(&mut self) {
        let phase = self.trace.phase("close");
        let handle = self.state.harness.remove(&self.worker).unwrap();
        let mut stopping = tokio::spawn(async move { handle.shutdown().await });
        let mut extra = vec![];
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                tokio::select! {
                    result = &mut stopping => { result.unwrap().unwrap(); break; }
                    call = request(&mut self.peer) => {
                        let result = match call["method"].as_str().unwrap() {
                            "config/read" => json!({"config":{}}),
                            "turn/interrupt" => json!({}),
                            "turn/start" => { extra.push(call.clone()); json!({"turn":{"id":"cleanup-turn"}}) },
                            method => panic!("unexpected execution during normal cleanup: {method}"),
                        };
                        send(&mut self.peer, json!({"id":call["id"],"result":result})).await;
                        if call["method"] == "turn/start" {
                            notify_thread(&self.state.shared_codex_appserver, &mut self.peer, THREAD, "turn/started", json!({"id":"cleanup-turn","status":"inProgress"})).await;
                            notify_thread(&self.state.shared_codex_appserver, &mut self.peer, THREAD, "turn/completed", json!({"id":"cleanup-turn","status":"completed"})).await;
                        }
                    }
                }
            }
        }).await.expect("normal original lifecycle cleanup");
        let shutdown = self.trace.phase("close.shutdown_process");
        self.fixture.shutdown_process(&self.state).await;
        shutdown.complete();
        let peer = self.trace.phase("close.peer");
        self.peer.close(None).await.unwrap();
        peer.complete();
        assert!(
            extra.is_empty(),
            "all accepted input must continue before cleanup: {extra:?}"
        );
        phase.complete();
    }
}

async fn mixed_order(system_first: bool) {
    let mut flow = Flow::running().await;
    let receipt = flow.fixture.receipt(&flow.area).await;
    let (event, human) = if system_first {
        let event = flow.report(SYSTEM_BODY).await;
        (event, flow.human().await)
    } else {
        let human = flow.human().await;
        (flow.report(SYSTEM_BODY).await, human)
    };
    let (_, _, frame) = flow.fixture.carrier(&flow.track).await;
    assert_eq!(frame.canonical_inputs.pending.len(), 2);
    assert_eq!(frame.header.last_turn_id.as_deref(), Some(TURN));
    let system = frame
        .canonical_inputs
        .pending
        .iter()
        .find(|input| matches!(input, PendingInput::System { .. }))
        .unwrap();
    assert_eq!(system.envelope_id(), Some(event.id));
    let run = read(
        &flow.app,
        &flow.cookie,
        &format!(
            "/api/cards/{}/planner/run?contract=conversation_v1",
            flow.card
        ),
    )
    .await;
    let expected_kinds = if system_first {
        vec!["system", "human"]
    } else {
        vec!["human", "system"]
    };
    assert_eq!(
        run["pending_inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|input| input["kind"].as_str().unwrap())
            .collect::<Vec<_>>(),
        expected_kinds
    );
    let page = read(
        &flow.app,
        &flow.cookie,
        &format!(
            "/api/cards/{}/harness/items?contract=conversation_v1",
            flow.card
        ),
    )
    .await;
    let display = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| &item["source"])
        .find(|source| source["kind"] == "system")
        .unwrap()
        .clone();
    let system_text = match system {
        PendingInput::System { observation, .. } => observation.observation().to_turn_text(),
        _ => unreachable!(),
    };
    assert_eq!(display["text"], system_text);
    assert_ne!(display["presentation"], "user");
    assert_eq!(display["created_at_ms"], system.queued_at_ms());
    assert_eq!(flow.fixture.carrier(&flow.track).await.1, flow.worker);
    flow.end(TURN).await;
    let expected = if system_first {
        [system_text.as_str(), HUMAN_BODY]
    } else {
        [HUMAN_BODY, system_text.as_str()]
    };
    let mut observed = vec![];
    for (index, body) in expected.iter().enumerate() {
        let Some(call) = flow.next().await else {
            break;
        };
        assert!(
            call["params"]["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|input| input["text"]
                    .as_str()
                    .is_some_and(|text| text.contains(*body)))
        );
        observed.push(*body);
        let turn = format!("mixed-turn-{index}");
        flow.accept(&call, &turn).await;
        flow.end(&turn).await;
    }
    let after = flow.fixture.carrier(&flow.track).await;
    let receipt_after = flow.fixture.receipt(&flow.area).await;
    let page_after = read(
        &flow.app,
        &flow.cookie,
        &format!(
            "/api/cards/{}/harness/items?contract=conversation_v1",
            flow.card
        ),
    )
    .await;
    let display_after = page_after["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| &item["source"])
        .find(|source| source["kind"] == "system")
        .unwrap();
    assert_eq!(
        display_after, &display,
        "claim preserves the once-accepted body, presentation and time"
    );
    flow.close().await;
    assert_eq!((&after.0, &after.1), (&flow.card, &flow.worker));
    assert_eq!(receipt_after, receipt);
    assert_eq!(human["worker_session_id"], flow.worker);
    assert_eq!(
        observed, expected,
        "both already accepted sources must run in order without another POST or event"
    );
}

#[tokio::test]
async fn real_system_then_human_tail_continues_after_both_preceding_end_events() {
    mixed_order(true).await;
}
#[tokio::test]
async fn real_human_then_system_tail_continues_after_both_preceding_end_events() {
    mixed_order(false).await;
}

pub(super) async fn retry_system_via_http() {
    let mut flow = Flow::running_with_model(true).await;
    flow.report(SYSTEM_BODY).await;
    flow.end(TURN).await;
    let call = flow.next().await.expect("real Dispatcher input must cross");
    assert!(
        call["params"]["input"]
            .to_string()
            .contains("Report knowledge delta")
    );
    send(
        &mut flow.peer,
        json!({"id":call["id"],"result":{"turn":{}}}),
    )
    .await;
    let before = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let frame = flow.fixture.carrier(&flow.track).await.2;
            if frame.canonical_inputs.attempts.len() == 2
                && matches!(
                    frame.canonical_inputs.attempts[1].delivery,
                    InputDeliveryFact::Unknown
                )
            {
                break frame;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    flow.state
        .harness
        .get(&flow.worker)
        .unwrap()
        .pause_issuance_for_dev();
    let display = retry_read::system_display(&flow.app, &flow.cookie, &flow.card).await;
    let predecessor = &before.canonical_inputs.attempts[1];
    let uri = format!("/api/cards/{}/conversation/input/retry", flow.card);
    let result = post(&flow.app,&flow.cookie,&uri,"system-repeat",json!({"carrier_id":flow.worker,
        "submission_id":predecessor.attempt.submission_id,"attempt_id":predecessor.attempt.attempt_id,"revision":predecessor.revision.to_string()})).await;
    let id =
        calm_types::conversation::RetryIntentId::parse(result["retry_intent_id"].as_str().unwrap())
            .unwrap();
    let mut tx = crate::db::sqlite::begin_immediate_tx(flow.fixture.repo.db())
        .await
        .unwrap();
    let carrier = crate::db::sqlite::conversation::current_input_fact_carrier_tx(
        &mut tx,
        &flow.card,
        &flow.worker,
    )
    .await
    .unwrap();
    let intent = crate::db::sqlite::conversation::retry_intent_tx(&mut tx, &carrier, &id)
        .await
        .unwrap();
    let original = crate::db::sqlite::conversation::submission_plan_tx(
        &mut tx,
        &predecessor.attempt.submission_id,
    )
    .await
    .unwrap();
    assert_eq!(intent.plan, original);
    assert!(original.plan.segments().iter().any(|segment| matches!(
        segment,
        calm_types::conversation::SubmissionRenderSegment::Input {
            input: calm_types::conversation::SubmissionRenderInput::System { .. },
            ..
        }
    )));
    assert!(original.plan.segments().iter().all(|segment| !matches!(
        segment,
        calm_types::conversation::SubmissionRenderSegment::Input {
            input: calm_types::conversation::SubmissionRenderInput::Human { .. },
            ..
        }
    )));
    tx.commit().await.unwrap();
    let after = flow.fixture.carrier(&flow.track).await.2;
    assert_eq!(
        after.canonical_inputs.attempts[1].attempt,
        predecessor.attempt
    );
    assert_eq!(
        after.canonical_inputs.attempts[1].delivery,
        predecessor.delivery
    );
    assert_eq!(after.canonical_inputs.pending.len(), 1);
    retry_read::queued_restore_reopen(flow, id, display).await;
}
