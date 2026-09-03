//! #1255 S3 — `thread/tokenUsage/updated` end to end.
//!
//! The arithmetic itself is unit-tested next to the code it protects
//! (`calm-server/src/harness/token_usage.rs`). What this file pins is the
//! wiring the unit tests cannot see: that a real notification reaches the run
//! loop's arm, that the reading rides the runtime snapshot out to
//! `worker_sessions.handle_state` and back in through the constructor, and
//! that `GET /api/cards/{id}/planner/run` reports it — derived from `last`, with
//! the lifetime `total` nowhere on the wire.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::codex_appserver::Notification;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, card_create_with_id_tx, session_start_runtime_tx};
use calm_server::event::EventBus;
use calm_server::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessSnapshot, PlannerHarness, PlannerHarnessParams,
    TokenUsage,
};
use calm_server::model::{Card, CardRole, NewArea, NewCard, NewTrack, new_id};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::track_area_cache::TrackAreaCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// The thread id the harness in this file is seeded with. Notifications must
/// carry it: `on_notification`'s prologue drops any frame whose `threadId`
/// does not match, and it does so silently. Fixtures hold a placeholder; the
/// test substitutes this in.
const SEED_THREAD_ID: &str = "thread-token-usage";

/// A thread id that is emphatically not this harness's. See
/// [`token_usage_from_a_foreign_thread_is_ignored`].
const FOREIGN_THREAD_ID: &str = "thread-belonging-to-another-card";

/// The `thread/tokenUsage/updated` payload — a **real capture** out of
/// `~/.codex/sessions` on this box, cross-checked against the deployed
/// binary's own `generate-json-schema` output. See the `_provenance` block
/// inside the file for the source frame, the redactions, and the command.
/// Because the numbers were not curated, they are more damning than any
/// invented ones: a `total`-derived percentage would be ~26607%.
const USAGE_FIXTURE: &str = include_str!("../fixtures/thread_token_usage_updated.json");

/// `last.totalTokens` in the captured frame.
const CAPTURED_USED: i64 = 113_356;
/// `total.totalTokens` in the captured frame — 253.8x the window.
const CAPTURED_TOTAL: i64 = 65_570_537;
/// `modelContextWindow` in the captured frame.
const CAPTURED_WINDOW: i64 = 258_400;

/// The fixture's wire `params`, with the placeholder `threadId` replaced by
/// the seeded one.
fn fixture_params() -> Value {
    let fixture: Value = serde_json::from_str(USAGE_FIXTURE).unwrap();
    let mut params = fixture
        .get("params")
        .expect("fixture must carry the wire params under `params`")
        .clone();
    params["threadId"] = json!(SEED_THREAD_ID);
    params
}

/// A frame built by editing the captured frame's counts, so every test in
/// this file still exercises the real field layout rather than a hand-rolled
/// object that happens to have the two keys the parser reads.
fn frame_with(last_total: i64, window: Value) -> Value {
    let mut params = fixture_params();
    params["tokenUsage"]["last"]["totalTokens"] = json!(last_total);
    params["tokenUsage"]["modelContextWindow"] = window;
    params
}

/// The same frame, addressed to a thread this harness does not own. Built
/// through `fixture_params` and then re-addressed, so the ONLY difference
/// from a frame this harness must accept is the `threadId`.
fn foreign_frame(last_total: i64, window: Value) -> Value {
    let mut params = frame_with(last_total, window);
    params["threadId"] = json!(FOREIGN_THREAD_ID);
    params
}

struct Boot {
    app: axum::Router,
    repo: Arc<SqlxRepo>,
    daemon: Arc<SharedCodexAppServer>,
    harness: PlannerHarness,
    planner_card: Card,
    runtime_id: String,
}

/// A planner card with a *live, registered* harness behind it.
///
/// `GET /planner/run` resolves through the registry (active runtime row →
/// `s.harness.get`), so a harness that merely exists is not enough — it has to
/// be installed under the same runtime id as the row.
async fn boot() -> Boot {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "token-usage".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "token usage".into(),
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
    let track_area_cache = TrackAreaCache::new();
    track_area_cache.insert(track.id.clone(), area.id);

    let mut tx = repo.pool().begin().await.unwrap();
    let planner_card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "planner_harness": true}),
        },
        CardRole::Planner,
        false,
        &role_cache,
    )
    .await
    .unwrap();

    let runtime_id = new_id();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(SEED_THREAD_ID.to_string());
    session_start_runtime_tx(
        &mut tx,
        calm_server::session_projection_repo::WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: planner_card.id.to_string(),
            kind: calm_server::session_projection_repo::WorkerSessionKind::SharedPlanner,
            agent_provider: Some(calm_server::session_projection_repo::AgentProvider::Codex),
            status: calm_server::session_projection_repo::WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some(SEED_THREAD_ID.to_string()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
            spawn_op_id: None,
            now_ms: calm_server::model::now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let events = EventBus::new();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-token-usage"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(role_cache.clone(), track_area_cache.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(role_cache.clone()),
        Some(track_area_cache.clone()),
    );

    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let harness = PlannerHarness::run(PlannerHarnessParams {
        runtime_id: runtime_id.clone(),
        track_id: planner_card.track_id.clone(),
        card_id: planner_card.id.clone(),
        thread_id: Some(SEED_THREAD_ID.to_string()),
        repo: repo_dyn,
        events,
        card_role_cache: role_cache,
        track_area_cache,
        daemon: daemon.clone(),
        config: HarnessConfig {
            debounce_min_idle: Duration::from_secs(60),
            debounce_max_wait: Duration::from_secs(60),
            ..HarnessConfig::default()
        },
        snapshot,
    });
    state.harness.insert(runtime_id.clone(), harness.clone());

    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);

    Boot {
        app,
        repo,
        daemon,
        harness,
        planner_card,
        runtime_id,
    }
}

async fn get(app: axum::Router, uri: String) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

async fn wait_for_notification_receiver(daemon: &SharedCodexAppServer) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if daemon.notification_receiver_count_for_test() > 0 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for harness notification receiver"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Emit a frame and wait until the harness's own snapshot reflects it.
///
/// Polling the snapshot rather than sleeping: notification handling is
/// asynchronous on the run loop's `select!`, so the alternative is a race that
/// passes on a quiet box and flakes on a loaded one.
async fn emit_and_wait(boot: &Boot, params: Value, expect_used: i64) -> TokenUsage {
    boot.daemon.emit_notification_for_test(Notification::Other {
        method: "thread/tokenUsage/updated".into(),
        params,
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(usage) = boot.harness.snapshot().await.token_usage
            && usage.used_tokens == expect_used
        {
            return usage;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for token usage with used_tokens={expect_used}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// THE central-trap regression at the integration layer, and the one to read
/// first if this file goes red.
///
/// The captured frame's `total.totalTokens` is 65_570_537 against a 258_400
/// window — 253.8x over, which is what a long real thread looks like. The
/// response must report the `last`-derived occupancy (113_356 → ~41.13%) and
/// must not be anywhere near the ~26607% that `total` produces. It must also
/// not ship `total_tokens` at all: the reading is stored with it, and the wire
/// type drops it precisely so no client can divide by the wrong number.
#[tokio::test]
async fn planner_run_reports_percent_derived_from_last_not_the_lifetime_total() {
    let boot = boot().await;
    wait_for_notification_receiver(&boot.daemon).await;

    let usage = emit_and_wait(&boot, fixture_params(), CAPTURED_USED).await;
    assert_eq!(
        usage.total_tokens, CAPTURED_TOTAL,
        "the lifetime total is stored — it is just never the numerator"
    );

    let (status, body) = get(
        boot.app.clone(),
        format!("/api/cards/{}/planner/run", boot.planner_card.id.as_str()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["runtime_id"], json!(boot.runtime_id));
    assert_eq!(body["phase"], json!("idle"));

    let wire = &body["token_usage"];
    assert_eq!(wire["used_tokens"], json!(CAPTURED_USED));
    assert_eq!(wire["context_window"], json!(CAPTURED_WINDOW));

    let percent = wire["percent"].as_f64().expect("a renderable percentage");
    let expected = 101_356.0 / 246_400.0 * 100.0;
    assert!(
        (percent - expected).abs() < 1e-9,
        "percent must be (last - 12000) / (window - 12000); got {percent}"
    );
    assert!(
        percent < 100.0,
        "a total-derived percentage would be ~26607%; got {percent}"
    );

    // #1255 S3 review — `at_ms` ships. Without it a reading rehydrated from a
    // months-old snapshot is indistinguishable on the wire from a live one,
    // and the field's own doc comment claims exactly that distinguishability.
    assert_eq!(
        wire["at_ms"].as_i64(),
        Some(usage.at_ms),
        "the frame's wall clock must reach the client; got {wire}"
    );
    assert!(
        wire["at_ms"].as_i64().is_some_and(|at| at > 0),
        "at_ms must be the real ingest clock, not a placeholder; got {wire}"
    );

    assert!(
        wire.get("total_tokens").is_none(),
        "the lifetime total must not cross the wire — a client holding both \
         numbers is a client that can pick the wrong one; got {wire}"
    );

    boot.harness.shutdown().await.unwrap();
}

/// A later frame with `modelContextWindow: null` updates the counts and keeps
/// the window. Asserted through the REST response, because the consequence
/// that matters is the meter not blinking out mid-turn.
#[tokio::test]
async fn planner_run_keeps_a_known_context_window_across_a_null_frame() {
    let boot = boot().await;
    wait_for_notification_receiver(&boot.daemon).await;

    emit_and_wait(&boot, fixture_params(), CAPTURED_USED).await;
    emit_and_wait(&boot, frame_with(80_000, Value::Null), 80_000).await;

    let (status, body) = get(
        boot.app.clone(),
        format!("/api/cards/{}/planner/run", boot.planner_card.id.as_str()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let wire = &body["token_usage"];
    assert_eq!(
        wire["context_window"],
        json!(CAPTURED_WINDOW),
        "a null modelContextWindow must not erase the window we already know"
    );
    assert_eq!(
        wire["used_tokens"],
        json!(80_000),
        "counts still come from the newest frame"
    );
    let percent = wire["percent"].as_f64().expect("still renderable");
    let expected = 68_000.0 / 246_400.0 * 100.0;
    assert!((percent - expected).abs() < 1e-9, "got {percent}");

    boot.harness.shutdown().await.unwrap();
}

/// `used > window`: the raw count ships, the percentage does not, and it is
/// NOT clamped to 100. See `TokenUsage::percent` for why a clamp would be the
/// dishonest choice — it would render our proxy failing as a plausible "the
/// context is full".
#[tokio::test]
async fn planner_run_omits_the_percentage_when_usage_exceeds_the_window() {
    let boot = boot().await;
    wait_for_notification_receiver(&boot.daemon).await;

    emit_and_wait(
        &boot,
        frame_with(CAPTURED_WINDOW + 1, json!(CAPTURED_WINDOW)),
        CAPTURED_WINDOW + 1,
    )
    .await;

    let (status, body) = get(
        boot.app.clone(),
        format!("/api/cards/{}/planner/run", boot.planner_card.id.as_str()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let wire = &body["token_usage"];
    assert_eq!(
        wire["used_tokens"],
        json!(CAPTURED_WINDOW + 1),
        "the raw count is the evidence and must still ship"
    );
    assert_eq!(wire["context_window"], json!(CAPTURED_WINDOW));
    assert_eq!(
        wire["percent"],
        Value::Null,
        "an impossible occupancy is reported as no percentage, never as 100"
    );

    boot.harness.shutdown().await.unwrap();
}

/// The reading survives `snapshot_for` → `handle_state` → the constructor.
///
/// This is the reboot / lazy-recovery path, and it is a genuine round trip:
/// the second harness is built from the JSON the first one actually wrote to
/// `worker_sessions.handle_state`, so it exercises serialization,
/// `from_value_strict` (including `assert_known_schema` against the
/// *unbumped* version — the field is `#[serde(default)]`, which is what makes
/// the bump unnecessary) and `inner_from_params`'s rehydration in one pass.
/// Drop the rehydration line and the value is written to disk and silently
/// discarded on the way back: codex re-pushes usage only on the next model
/// response, so a resumed-but-idle thread would show no context reading at all.
#[tokio::test]
async fn token_usage_round_trips_through_the_persisted_runtime_snapshot() {
    let boot = boot().await;
    wait_for_notification_receiver(&boot.daemon).await;
    let live = emit_and_wait(&boot, fixture_params(), CAPTURED_USED).await;
    boot.harness.shutdown().await.unwrap();

    let row = boot
        .repo
        .session_projection_active_for_card(&boot.planner_card.id.to_string())
        .await
        .unwrap()
        .expect("the active runtime row");
    let stored = row.handle_state_json.expect("persisted handle state");
    assert_eq!(
        stored["token_usage"]["used_tokens"],
        json!(CAPTURED_USED),
        "the reading must be in the runtime snapshot on disk, not only in memory"
    );

    let snapshot = HarnessSnapshot::from_value_strict(stored);
    let rehydrated = PlannerHarness::run(PlannerHarnessParams {
        runtime_id: boot.runtime_id.clone(),
        track_id: boot.planner_card.track_id.clone(),
        card_id: boot.planner_card.id.clone(),
        thread_id: Some(SEED_THREAD_ID.to_string()),
        repo: boot.repo.clone(),
        events: EventBus::new(),
        card_role_cache: CardRoleCache::new(),
        track_area_cache: TrackAreaCache::new(),
        daemon: SharedCodexAppServer::new_fake_running_with_pending(boot.repo.clone(), None),
        config: HarnessConfig::default(),
        snapshot,
    });

    let recovered = rehydrated
        .snapshot()
        .await
        .token_usage
        .expect("token usage must survive the rebuild");
    assert_eq!(
        recovered, live,
        "every field must round-trip, not just the count"
    );

    rehydrated.shutdown().await.unwrap();
}

/// THE cross-thread gate (#1255 S3 review). Card A must never show card B's
/// context, and this is the only test that holds it.
///
/// `PlannerHarness::run` subscribes to the daemon's *global* notification
/// broadcast, so every harness on the box receives every
/// `thread/tokenUsage/updated` frame from every thread. The sole filter is
/// `on_notification`'s prologue comparing `notif.thread_id()` against the
/// harness's own. Every other test in this file goes through `fixture_params`,
/// which overwrites `threadId` with the seeded one — so none of them can see
/// this. Its failure mode is not a crash or an error: it is a meter showing a
/// plausible number that belongs to a different conversation.
///
/// The assertion is on the **window**, not the count, and that is deliberate.
/// An ignored notification returns before `persist_snapshot`, so it leaves no
/// trace to poll for — there is no barrier that says "the foreign frame has
/// been processed and discarded". Sequencing a legitimate frame after it gives
/// that barrier (the run loop handles the broadcast in order), but a count
/// would then simply be overwritten and the test would pass either way. The
/// window survives: `sticky_merge` keeps a window a previous frame established.
/// So a foreign frame carrying a window, followed by a legitimate frame
/// carrying none, leaves exactly one distinguishable state.
///
/// Mutation: delete the two-line `thread_id` guard at the top of
/// `on_notification` and this goes red on `context_window`.
#[tokio::test]
async fn token_usage_from_a_foreign_thread_is_ignored() {
    let boot = boot().await;
    wait_for_notification_receiver(&boot.daemon).await;

    // Another card's harness, mid-conversation, on the same daemon.
    boot.daemon.emit_notification_for_test(Notification::Other {
        method: "thread/tokenUsage/updated".into(),
        params: foreign_frame(200_000, json!(400_000)),
    });

    // Our own next frame: it carries no window of its own, so any window on
    // the wire afterwards can only have come from the foreign frame.
    let usage = emit_and_wait(&boot, frame_with(55_555, Value::Null), 55_555).await;

    assert_eq!(
        usage.context_window, None,
        "a frame addressed to another thread must not establish this \
         harness's context window — the harness subscribes to a GLOBAL \
         notification broadcast and the thread id is the only filter"
    );
    assert_eq!(
        usage.used_tokens, 55_555,
        "and its counts must not land either"
    );

    let (status, body) = get(
        boot.app.clone(),
        format!("/api/cards/{}/planner/run", boot.planner_card.id.as_str()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let wire = &body["token_usage"];
    assert_eq!(
        wire["context_window"],
        Value::Null,
        "the foreign window must not reach a client either; got {wire}"
    );
    assert_eq!(
        wire["percent"],
        Value::Null,
        "no window, no percentage — a foreign window would have produced one"
    );

    boot.harness.shutdown().await.unwrap();
}
