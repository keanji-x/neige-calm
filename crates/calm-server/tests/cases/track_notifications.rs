//! #1829 / #1876 — the two notification sources of `kernel/track/activity`: an ask (a pending
//! `neige.ratify.request`, or a `neige.user.notify` call) and planner down (the Planner's newest
//! finished turn failed). Every row is written through its production event writer; each case
//! asserts the payload the projector computes. One test per row of the design's producer × state
//! matrix (§7).
//!
//! Fixture sources are fixed so the mutation red sets hold: only rows 3, 3b, 4a, 4b and 5 ever
//! resolve a ratify request; rows 2, 18 and 19 request one and never resolve it; rows 5–8 are
//! notify-only, and only row 5 has a resolved request. Do not add a resolution to 2, 18 or 19.
//! Rows 4b, 18, 20 and 21 (Dismiss) are in `track_notification_dismissals.rs`.

use std::time::Duration;

use calm_server::db::write_with_events_typed;
use calm_server::event::{Event, EventScope};
use calm_server::ids::{ActorId, CardId, TrackId};
use calm_server::model::{CardRole, now_ms};
use calm_server::session_projection_repo::{WorkerSessionKind, WorkerSessionState};
use calm_server::terminal_renderer::TerminalRendererRegistry;
use calm_server::track_activity::{
    ActivityItem, ActivityPayload, ActivityWake, Attention, CardState, NotificationSource,
    TrackActivityProjector,
};
use calm_types::harness::HarnessPhaseTag;
use calm_types::task_recovery::TASK_IN_TRACK_ROUTE;
use calm_types::worker::WorkerSessionId;
use serde_json::json;

use super::track_activity_fixture::{Fx, fx};

/// An open track with its one Planner card and that card's harness session.
pub(crate) struct Planner {
    pub(crate) track: String,
    pub(crate) card: String,
    pub(crate) ws: String,
}

impl Planner {
    fn actor(&self) -> ActorId {
        ActorId::AiPlannerSession(WorkerSessionId::from(self.ws.as_str()))
    }
}

async fn planner(f: &Fx, name: &str, kind: WorkerSessionKind) -> Planner {
    let track = f.track(name).await;
    let card = f
        .card(
            &track,
            &format!("card-planner-{name}"),
            "planner",
            CardRole::Planner,
        )
        .await;
    let ws = f
        .session(
            &card,
            &format!("ws-planner-{name}"),
            kind,
            WorkerSessionState::Idle,
            Some(&format!("th-{name}")),
            Some(Fx::harness_snapshot()),
            1_000,
        )
        .await;
    Planner { track, card, ws }
}

pub(crate) async fn codex_planner(f: &Fx) -> Planner {
    planner(f, "p", WorkerSessionKind::SharedPlanner).await
}

/// Every writer stamps the wall clock in milliseconds; a gap before each write keeps the order of
/// the rows strict, so no case lands on the same-millisecond boundary (`at_ms = MAX(U, L)` is closed).
async fn settle() {
    tokio::time::sleep(Duration::from_millis(3)).await;
}

/// One track-scoped event through the production event writer.
async fn emit(f: &Fx, track: &str, actor: ActorId, event: Event) {
    settle().await;
    let scope = f.track_scope(track);
    write_with_events_typed(
        f.repo_dyn.as_ref(),
        actor,
        None,
        &f.events,
        &f.write,
        move |_tx| Box::pin(async move { Ok(((), vec![(scope, event)])) }),
    )
    .await
    .unwrap();
}

/// The Planner asks for ratification with `reason` (the `ratify.requested` of `neige.ratify.request`).
pub(crate) async fn request_ratify(f: &Fx, p: &Planner, reason: &str) {
    let event = Event::RatifyRequested {
        track_id: TrackId::from(p.track.clone()),
        reason: reason.to_string(),
    };
    emit(f, &p.track, p.actor(), event).await;
}

/// The user resolves the pending request (the `ratify.resolved` of `POST /api/cards/{id}/ratify`).
pub(crate) async fn resolve_ratify(f: &Fx, p: &Planner) {
    let event = Event::RatifyResolved {
        track_id: TrackId::from(p.track.clone()),
        decision: calm_types::event::RatifyDecision::Grant,
        message: None,
    };
    emit(f, &p.track, ActorId::User, event).await;
}

/// `harness.user_message.enqueued` exactly as `POST /api/cards/{id}/planner/input` audits a send.
async fn send(f: &Fx, track: &str, card: &str, actor: ActorId) {
    settle().await;
    f.repo_dyn
        .log_pure_event(
            actor,
            EventScope::Card {
                card: CardId::from(card.to_string()),
                track: TrackId::from(track.to_string()),
                area: f.area_id.clone().into(),
            },
            None,
            &f.events,
            &f.role_cache,
            &f.area_cache,
            Event::HarnessUserMessageEnqueued {
                worker_session_id: "ws-send".into(),
                card_id: CardId::from(card.to_string()),
                track_id: TrackId::from(track.to_string()),
                char_count: 4,
            },
        )
        .await
        .unwrap();
}

/// One completed `neige.user.notify` call row of the Planner card; `error` / `status` shape a failed call.
async fn notify_call(f: &Fx, p: &Planner, uuid: &str, status: &str, error: Option<&str>) -> i64 {
    settle().await;
    let mut item = json!({
        "id": uuid, "type": "mcpToolCall", "server": "neige", "tool": "neige.user.notify",
        "status": status, "arguments": {"text": format!("  Question {uuid}?  ")},
    });
    if let Some(message) = error {
        item["error"] = json!({ "message": message });
    }
    f.transcript_item(
        &p.ws,
        &p.card,
        &p.track,
        uuid,
        "mcpToolCall",
        "item/completed",
        json!({"threadId": "th-fixture", "turnId": "turn-fixture", "item": item}),
    )
    .await
}

async fn notify(f: &Fx, p: &Planner, uuid: &str) -> i64 {
    notify_call(f, p, uuid, "completed", None).await
}

/// One `turn/completed` row of `card` through the turn outcome writer; `error` is `$.error.message`.
async fn turn_on(
    f: &Fx,
    (track, card, ws): (&str, &str, &str),
    turn_id: &str,
    status: &str,
    error: Option<&str>,
) -> i64 {
    settle().await;
    let error = error.map_or(json!(null), |message| json!({ "message": message }));
    f.turn_outcome(
        ws,
        card,
        track,
        turn_id,
        json!({"id": turn_id, "status": status, "error": error}),
    )
    .await
}

pub(crate) async fn turn(
    f: &Fx,
    p: &Planner,
    turn_id: &str,
    status: &str,
    error: Option<&str>,
) -> i64 {
    turn_on(f, (&p.track, &p.card, &p.ws), turn_id, status, error).await
}

pub(crate) fn asks(p: &ActivityPayload) -> Vec<&ActivityItem> {
    p.items
        .iter()
        .filter(|i| i.source == NotificationSource::Ask)
        .collect()
}

pub(crate) fn planner_down(p: &ActivityPayload) -> Vec<&ActivityItem> {
    p.items
        .iter()
        .filter(|i| i.source == NotificationSource::PlannerDown)
        .collect()
}

/// `(id, at)` of the track's newest `ratify.requested` event.
async fn newest_ratify_request(f: &Fx, track: &str) -> (i64, i64) {
    sqlx::query_as(
        "SELECT id, at FROM events WHERE scope_track = ?1 AND kind = 'ratify.requested' \
          ORDER BY id DESC LIMIT 1",
    )
    .bind(track)
    .fetch_one(&f.pool)
    .await
    .unwrap()
}

/// A second projector over the same repo, run as the production loop, and its in-process wake;
/// returns once its boot sweep has written a row satisfying `seeded`, so every later change can
/// only arrive by a wake-up.
pub(crate) async fn running_loop(
    f: &Fx,
    track: &str,
    seeded: impl Fn(&ActivityPayload) -> bool,
) -> (tokio::task::JoinHandle<()>, ActivityWake) {
    let looped = TrackActivityProjector::new(
        f.repo_dyn.clone(),
        f.events.clone(),
        f.write.clone(),
        f.harness.clone(),
        TerminalRendererRegistry::new(),
    )
    .expect("sqlite-backed repo");
    let wake = looped.wake();
    let task = tokio::spawn(looped.run());
    f.await_stored(track, "the boot sweep's row", seeded).await;
    (task, wake)
}

// Row 1.
#[tokio::test]
async fn pending_ratify_is_an_ask_with_its_reason() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    request_ratify(&f, &p, "Merge PR #1811 now, or hold it for the release?").await;
    let (id, at) = newest_ratify_request(&f, &p.track).await;
    let a = f.recompute(&p.track).await;
    assert_eq!(a.attention, Attention::Input, "{a:?}");
    assert_eq!(a.items.len(), 1, "{a:?}");
    assert_eq!(a.items[0].source, NotificationSource::Ask);
    assert_eq!(
        a.items[0].text,
        "Merge PR #1811 now, or hold it for the release?"
    );
    assert_eq!(a.items[0].key, format!("ask:ratify:{id}"));
    assert_eq!(a.items[0].at_ms, at);
}

// Row 2.
#[tokio::test]
async fn user_send_after_ratify_request_closes_the_ask() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    request_ratify(&f, &p, "Which region?").await;
    assert_eq!(f.recompute(&p.track).await.items.len(), 1);
    send(&f, &p.track, &p.card, ActorId::User).await;
    let a = f.recompute(&p.track).await;
    assert!(a.items.is_empty(), "the reply closes the ask: {a:?}");
    assert_eq!(a.attention, Attention::None);
}

// Row 3.
#[tokio::test]
async fn ratify_resolution_closes_the_ask() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    request_ratify(&f, &p, "Which region?").await;
    resolve_ratify(&f, &p).await;
    let a = f.recompute(&p.track).await;
    assert!(a.items.is_empty(), "{a:?}");
    assert_eq!(a.attention, Attention::None);
}

// Row 3b.
#[tokio::test]
async fn ratify_resolution_leaves_an_earlier_notify_ask_open() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    request_ratify(&f, &p, "Which region?").await;
    let call = notify(&f, &p, "call-during-request").await;
    assert_eq!(f.recompute(&p.track).await.items.len(), 2);
    resolve_ratify(&f, &p).await;
    let a = f.recompute(&p.track).await;
    assert_eq!(
        a.items.len(),
        1,
        "only the user's reply answers a notify: {a:?}"
    );
    assert_eq!(a.items[0].key, format!("ask:notify:{call}"));
}

// Row 4a.
#[tokio::test]
async fn second_ratify_request_gives_a_new_key() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    request_ratify(&f, &p, "First question?").await;
    let first = f.recompute(&p.track).await.items[0].key.clone();
    resolve_ratify(&f, &p).await;
    request_ratify(&f, &p, "Second question?").await;
    let a = f.recompute(&p.track).await;
    assert_eq!(a.items.len(), 1, "only the newest request is an ask: {a:?}");
    assert_eq!(a.items[0].text, "Second question?");
    assert_ne!(
        a.items[0].key, first,
        "the same source happening again is a new key"
    );
}

// Row 5.
#[tokio::test]
async fn notify_after_a_resolved_ratify_is_an_ask() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    request_ratify(&f, &p, "Old question?").await;
    resolve_ratify(&f, &p).await;
    let row = notify(&f, &p, "call-1").await;
    let a = f.recompute(&p.track).await;
    assert_eq!(a.attention, Attention::Input);
    assert_eq!(a.items.len(), 1, "{a:?}");
    assert_eq!(a.items[0].source, NotificationSource::Ask);
    assert_eq!(a.items[0].key, format!("ask:notify:{row}"));
    assert_eq!(
        a.items[0].text, "Question call-1?",
        "trimmed as the tool trims it"
    );
}

// Row 6.
#[tokio::test]
async fn failed_notify_call_is_no_ask() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    notify_call(&f, &p, "call-err", "completed", Some("boom")).await;
    notify_call(&f, &p, "call-failed", "failed", None).await;
    let a = f.recompute(&p.track).await;
    assert!(a.items.is_empty(), "{a:?}");
    assert_eq!(a.attention, Attention::None);
}

// Row 7.
#[tokio::test]
async fn assistant_send_does_not_close_the_ask() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    let assistant = f
        .card(&p.track, "card-assistant", "codex", CardRole::Assistant)
        .await;
    notify(&f, &p, "call-1").await;
    send(&f, &p.track, &assistant, ActorId::User).await;
    let a = f.recompute(&p.track).await;
    assert_eq!(
        asks(&a).len(),
        1,
        "a send to an assistant card is no reply: {a:?}"
    );
}

// Row 8.
#[tokio::test]
async fn ai_actor_send_does_not_close_the_ask() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    notify(&f, &p, "call-1").await;
    send(&f, &p.track, &p.card, p.actor()).await;
    let a = f.recompute(&p.track).await;
    assert_eq!(asks(&a).len(), 1, "an AI-header send is no reply: {a:?}");
}

// Row 9: the codex shape (a completed turn, then a failed one) and the Claude shape.
#[tokio::test]
async fn failed_planner_turn_is_planner_down() {
    let f = fx().await;
    let codex = codex_planner(&f).await;
    turn(&f, &codex, "turn-1", "completed", None).await;
    let row = turn(
        &f,
        &codex,
        "turn-2",
        "failed",
        Some("unexpected status 403 Forbidden"),
    )
    .await;
    let a = f.recompute(&codex.track).await;
    assert_eq!(a.attention, Attention::Failed, "{a:?}");
    assert_eq!(a.items.len(), 1);
    assert_eq!(a.items[0].source, NotificationSource::PlannerDown);
    assert_eq!(a.items[0].key, format!("planner_down:{row}"));
    assert_eq!(a.items[0].text, "unexpected status 403 Forbidden");

    let claude = planner(&f, "c", WorkerSessionKind::ClaudeCard).await;
    let row = turn(
        &f,
        &claude,
        "turn-c",
        "failed",
        Some("claude exited before the result"),
    )
    .await;
    let a = f.recompute(&claude.track).await;
    assert_eq!(a.attention, Attention::Failed, "{a:?}");
    assert_eq!(a.items[0].key, format!("planner_down:{row}"));
    assert_eq!(a.items[0].text, "claude exited before the result");
}

// Row 9b: the codex system-error order — the phase event first, the failed row after it with its one event.
#[tokio::test]
async fn system_error_row_after_the_wedged_phase_wakes_the_projector() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    let (loop_task, _) = running_loop(&f, &p.track, |a| a.items.is_empty()).await;

    f.exit_session(&p.ws, WorkerSessionState::Failed, now_ms())
        .await;
    f.events.emit_envelope_for_test(Fx::envelope(
        EventScope::System,
        Event::HarnessPhaseChanged {
            worker_session_id: p.ws.clone(),
            card_id: CardId::from(p.card.clone()),
            track_id: TrackId::from(p.track.clone()),
            old_phase: HarnessPhaseTag::TurnRunning,
            new_phase: HarnessPhaseTag::Wedged,
        },
    ));
    let wedged = f
        .await_stored(&p.track, "the wedged phase's recompute", |a| {
            a.cards
                .iter()
                .any(|c| c.card_id == p.card && c.state == CardState::Failed)
        })
        .await;
    assert!(wedged.items.is_empty(), "no failed row yet: {wedged:?}");

    turn(&f, &p, "turn-1", "failed", Some("systemError")).await;
    f.events.emit_envelope_for_test(Fx::envelope(
        EventScope::System,
        Fx::item_added(&p.track, &p.card, "turn/completed", None),
    ));
    let down = f
        .await_stored(&p.track, "planner down after the row's event", |a| {
            !planner_down(a).is_empty()
        })
        .await;
    assert_eq!(down.attention, Attention::Failed);
    loop_task.abort();
}

// Row 10.
#[tokio::test]
async fn later_completed_turn_closes_planner_down() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    turn(&f, &p, "turn-1", "failed", Some("boom")).await;
    assert_eq!(planner_down(&f.recompute(&p.track).await).len(), 1);
    turn(&f, &p, "turn-2", "completed", None).await;
    let a = f.recompute(&p.track).await;
    assert!(a.items.is_empty(), "{a:?}");
    assert_eq!(a.attention, Attention::None);
}

// Row 11.
#[tokio::test]
async fn interrupted_turn_neither_raises_nor_closes() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    let failed = turn(&f, &p, "turn-1", "failed", Some("boom")).await;
    turn(&f, &p, "turn-2", "interrupted", None).await;
    let a = f.recompute(&p.track).await;
    assert_eq!(a.items.len(), 1, "{a:?}");
    assert_eq!(a.items[0].key, format!("planner_down:{failed}"));

    let other = planner(&f, "i", WorkerSessionKind::SharedPlanner).await;
    turn(&f, &other, "turn-i", "interrupted", None).await;
    assert!(f.recompute(&other.track).await.items.is_empty());
}

// Row 12.
#[tokio::test]
async fn planner_that_never_completed_is_down() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    let row = turn(&f, &p, "turn-1", "failed", Some("sub2api 403")).await;
    let a = f.recompute(&p.track).await;
    assert_eq!(a.attention, Attention::Failed, "{a:?}");
    assert_eq!(a.items.len(), 1);
    assert_eq!(a.items[0].key, format!("planner_down:{row}"));
}

// Row 13.
#[tokio::test]
async fn assistant_failed_turn_is_not_planner_down() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    turn(&f, &p, "turn-1", "completed", None).await;
    let assistant = f
        .card(&p.track, "card-assistant", "codex", CardRole::Assistant)
        .await;
    let ws = f
        .session(
            &assistant,
            "ws-assistant",
            WorkerSessionKind::SharedPlanner,
            WorkerSessionState::Idle,
            Some("th-assistant"),
            Some(Fx::harness_snapshot()),
            1_000,
        )
        .await;
    turn_on(
        &f,
        (&p.track, &assistant, &ws),
        "turn-a",
        "failed",
        Some("boom"),
    )
    .await;
    let a = f.recompute(&p.track).await;
    assert!(a.items.is_empty(), "{a:?}");
    assert_eq!(a.attention, Attention::None);
}

// Row 14.
#[tokio::test]
async fn failed_task_is_status_not_notification() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    let worker = f.card(&p.track, "card-w", "codex", CardRole::Worker).await;
    f.session(
        &worker,
        "ws-w",
        WorkerSessionKind::CodexCard,
        WorkerSessionState::Running,
        Some("th-w"),
        None,
        1_000,
    )
    .await;
    f.plan_tasks(&p.track, &[("build", "codex", TASK_IN_TRACK_ROUTE, None)])
        .await;
    f.claim(&p.track, "build", 2_000).await;
    f.mark_running(&p.track, "build", &worker, 3_000).await;
    f.fail(&p.track, "build", &worker, 4_000).await;
    let a = f.recompute(&p.track).await;
    assert!(a.items.is_empty(), "{a:?}");
    assert_eq!(a.attention, Attention::None);
    assert!(
        a.cards
            .iter()
            .any(|c| c.card_id == worker && c.state == CardState::Failed),
        "{a:?}"
    );
}

// Row 15.
#[tokio::test]
async fn failed_session_is_status_not_notification() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    let card = f.card(&p.track, "card-i", "codex", CardRole::Worker).await;
    let ws = f
        .session(
            &card,
            "ws-i",
            WorkerSessionKind::CodexCard,
            WorkerSessionState::Running,
            Some("th-i"),
            None,
            1_000,
        )
        .await;
    f.exit_session(&ws, WorkerSessionState::Failed, 5_000).await;
    let a = f.recompute(&p.track).await;
    assert!(a.items.is_empty(), "{a:?}");
    assert_eq!(a.attention, Attention::None);
    assert!(
        a.cards
            .iter()
            .any(|c| c.card_id == card && c.state == CardState::Failed),
        "{a:?}"
    );
}

// Row 17.
#[tokio::test]
async fn planner_close_is_outcome_only() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    let closed = f
        .repo_dyn
        .track_update(
            &p.track,
            calm_server::model::TrackPatch {
                closed: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    emit(
        &f,
        &p.track,
        p.actor(),
        Event::TrackUpdated(calm_server::event::TrackUpdatedPayload::new(
            closed,
            Some("goal met".into()),
        )),
    )
    .await;
    let a = f.recompute(&p.track).await;
    assert!(a.items.is_empty(), "{a:?}");
    assert_eq!(a.attention, Attention::None);
}

/// A notify ask and a failed turn landed on a closed track.
async fn open_notifications_on(f: &Fx, p: &Planner) {
    notify(f, p, "call-late").await;
    turn(f, p, "turn-late", "failed", Some("boom")).await;
}

fn keeps_both(a: &ActivityPayload) {
    assert_eq!(asks(a).len(), 1, "{a:?}");
    assert_eq!(planner_down(a).len(), 1, "{a:?}");
    assert_eq!(a.attention, Attention::Failed);
    assert!(
        a.cards.iter().all(|c| c.state == CardState::Working),
        "the card verdicts are still filtered: {a:?}"
    );
}

// Row 17b.
#[tokio::test]
async fn closed_track_keeps_open_notifications() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    f.set_closed(&p.track, true).await;
    open_notifications_on(&f, &p).await;
    keeps_both(&f.recompute(&p.track).await);
}

// Row 19.
#[tokio::test]
async fn user_send_wakes_the_projector() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    request_ratify(&f, &p, "Which region?").await;
    let (loop_task, _) = running_loop(&f, &p.track, |a| asks(a).len() == 1).await;
    send(&f, &p.track, &p.card, ActorId::User).await;
    let a = f
        .await_stored(&p.track, "the ask closed by the send's wake-up", |a| {
            a.items.is_empty()
        })
        .await;
    assert_eq!(a.attention, Attention::None);
    loop_task.abort();
}

// Row 23.
#[tokio::test]
async fn missing_text_drops_only_that_item() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    let ask = notify(&f, &p, "call-1").await;
    turn(&f, &p, "turn-1", "failed", None).await;
    let a = f.recompute(&p.track).await;
    assert_eq!(
        a.items.len(),
        1,
        "only the textless planner down goes: {a:?}"
    );
    assert_eq!(a.items[0].key, format!("ask:notify:{ask}"));
    assert_eq!(a.attention, Attention::Input);
    assert_eq!(f.stored(&p.track).await, Some(a), "the overlay is written");
}

// Row 24.
#[tokio::test]
async fn closed_notify_still_advances_activity() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    let row = notify(&f, &p, "call-1").await;
    let at = now_ms() - 60_000;
    f.pin_transcript_row(row, at).await;
    // The reply lands while the notify's wake-up is still queued: no recompute in between.
    send(&f, &p.track, &p.card, ActorId::User).await;
    let a = f.recompute(&p.track).await;
    assert!(a.items.is_empty(), "the reply closed the ask: {a:?}");
    assert_eq!(
        a.activity_at_ms,
        Some(at),
        "E2 counts every successful notify, open or not"
    );
}

// The planner-down text is the kernel's readable form of the upstream body codex embeds.
#[tokio::test]
async fn planner_down_text_is_the_readable_error() {
    let f = fx().await;
    let p = codex_planner(&f).await;
    let body = r#"{"type":"error","status":400,"error":{"message":"Upgrade Codex."}}"#;
    turn(&f, &p, "turn-1", "failed", Some(body)).await;
    let a = f.recompute(&p.track).await;
    assert_eq!(planner_down(&a)[0].text, "400: Upgrade Codex.");
}
