//! #1505 S4-3 — choosing the model a planner conversation's turns run with,
//! end to end.
//!
//! The rule table itself (which of `model` / `null` / "never set" produces
//! which frame member) is pinned by unit tests in
//! `calm_server::planner_model`, and the shape of the frame by unit tests in
//! `calm_server::codex_appserver`. What can only be seen from out here is the
//! wiring between them: that a REST write reaches the payload, that the
//! payload reaches every `turn/start` the run loop issues, that the actor
//! guard stands on a real request, and that a selection nobody can resolve
//! stops the conversation instead of quietly running under something else.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use calm_server::codex_appserver::CodexConfig;
use calm_server::codex_appserver::Notification;
use calm_server::db::sqlite::track_delete_tx;
use calm_server::db::write_in_tx_typed;
use calm_server::harness::HarnessPhaseTag;
use calm_server::harness::run_loop::{
    PlannerHarnessCwdRaceHook, PlannerHarnessDrainRaceHook,
    install_planner_harness_cwd_race_hook_for_test,
    install_planner_harness_drain_race_hook_for_test,
};
use calm_server::planner_model::TurnModelSelection;
use calm_server::track_area_cache::TrackAreaCache;
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::support::planner_queue_fixture::{
    Boot, Issuance, SEED_THREAD_ID, boot_with, boot_with_issuance, get, idle_snapshot, post_input,
    send_json,
};

fn model_uri(card_id: &str) -> String {
    format!("/api/cards/{card_id}/planner/model")
}

async fn put_model(boot: &Boot, actor: &str, body: Value) -> (StatusCode, Value) {
    send_json(
        boot.app.clone(),
        "PUT",
        model_uri(boot.planner_card.id.as_str()),
        actor,
        body,
    )
    .await
}

/// The card's payload as it stands in the database.
async fn payload(boot: &Boot) -> Value {
    let (text,): (String,) = sqlx::query_as("SELECT payload FROM cards WHERE id = ?1")
        .bind(boot.planner_card.id.as_str())
        .fetch_one(boot.repo.pool())
        .await
        .expect("the planner card row");
    serde_json::from_str(&text).expect("a card payload is JSON")
}

/// Wait for the run loop to have issued `count` turns and return what each one
/// told codex about the model.
///
/// Polls rather than sleeps a fixed time: the deadline is a failure ceiling so
/// a turn that never issues fails with the selections seen so far, not by
/// hanging.
async fn selections_after(boot: &Boot, count: usize) -> Vec<TurnModelSelection> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let seen = boot.daemon.started_turn_selections_for_test();
        if seen.len() >= count {
            return seen.into_iter().map(|(_thread, sel)| sel).collect();
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {count} turn(s); saw {seen:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn phase(boot: &Boot) -> HarnessPhaseTag {
    boot.harness.snapshot().await.phase
}

/// Tell the harness the in-flight turn finished, so it can issue the next one.
///
/// The fixtures fake acknowledges `turn/start` and emits `turn/started`, but
/// nothing ends the turn — a real daemon would. Without this the run loop sits
/// in `TurnRunning` forever and a second turn is unreachable, which would make
/// "does the model ride the SECOND turn too" untestable rather than passing.
async fn complete_the_running_turn(boot: &Boot) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let turn_id = loop {
        if let Some(id) = boot.harness.snapshot().await.last_turn_id.clone() {
            break id;
        }
        assert!(Instant::now() < deadline, "no turn ever started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    boot.daemon
        .emit_notification_for_test(Notification::TurnCompleted {
            thread_id: SEED_THREAD_ID.to_string(),
            turn: json!({ "id": turn_id, "status": "completed" }),
        });
    loop {
        if phase(boot).await == HarnessPhaseTag::TurnCompleted {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the harness never left TurnRunning"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ---------------------------------------------------------------------------
// The write port
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_selection_is_stored_on_the_card_payload() {
    let boot = boot_with(idle_snapshot(vec![])).await;

    let (status, body) = put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": "high"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["model"], json!("gpt-5"));
    assert_eq!(body["reasoning_effort"], json!("high"));

    let stored = payload(&boot).await;
    assert_eq!(stored["model"], json!("gpt-5"));
    assert_eq!(stored["reasoning_effort"], json!("high"));
    assert_eq!(stored["model_ever_set"], json!(true));
    assert_eq!(stored["reasoning_effort_ever_set"], json!(true));
    assert_eq!(
        stored["planner_harness"],
        json!(true),
        "the write merges into the payload; it does not replace it"
    );
    assert_eq!(stored["schemaVersion"], json!(1));
}

/// The whole point of the monotone marker: choosing the default back again
/// stores a `null` and keeps the record that a value was once chosen.
#[tokio::test]
async fn choosing_the_default_again_stores_null_and_keeps_the_marker() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": "high"}),
    )
    .await;

    let (status, body) = put_model(
        &boot,
        "user",
        json!({"model": null, "reasoning_effort": null}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["model"], Value::Null);

    let stored = payload(&boot).await;
    assert_eq!(stored["model"], Value::Null);
    assert_eq!(stored["reasoning_effort"], Value::Null);
    assert_eq!(
        stored["model_ever_set"],
        json!(true),
        "clearing this marker would let the thread keep running the old model \
         while the picker said `default`"
    );
    assert_eq!(stored["reasoning_effort_ever_set"], json!(true));
}

/// Both keys are required. A body that omits one is refused outright rather
/// than read as "leave that half alone" — this is a PUT, and the stored
/// selection has no third state for the request to mean.
#[tokio::test]
async fn omitting_a_key_is_refused_and_changes_nothing() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let before = payload(&boot).await;

    let (status, _) = put_model(&boot, "user", json!({"model": "gpt-5"})).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a body missing `reasoning_effort` must not be read as null"
    );

    let (status, _) = put_model(&boot, "user", json!({"reasoning_effort": "high"})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, _) = put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": "high", "surprise": 1}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "an unknown key is a client that thinks it is setting something"
    );

    assert_eq!(
        payload(&boot).await,
        before,
        "a refused request must leave the payload byte-identical"
    );
}

#[tokio::test]
async fn an_agent_actor_is_refused_and_the_human_is_not() {
    let boot = boot_with(idle_snapshot(vec![])).await;

    let (status, body) = put_model(
        &boot,
        "ai:codex",
        json!({"model": "gpt-5", "reasoning_effort": null}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body={body}");
    // Positional, not merely present. `require_rest_user_actor_for` formats
    // `"{subject}: … {redirect}"` from two `&str` parameters, so swapping them
    // compiles and still leaves the subject somewhere in the sentence — an
    // assertion that only asked whether the subject appeared passed on the
    // swap, and the 403 went out naming the wrong subsystem, which is a false
    // statement in the audit log. The substring below spans the boundary
    // between the subject slot and the fixed rule text, so only a subject
    // actually in that slot satisfies it.
    let message = body["error"].as_str().unwrap_or_default();
    assert!(
        message.contains("planner model selection: only `X-Calm-Actor: user`"),
        "the subject must sit in the SUBJECT slot, immediately before the rule; check the \
         argument order of require_rest_user_actor_for: {message}"
    );
    assert!(
        message.ends_with("agents have no write path to it."),
        "and close with its own redirect: {message}"
    );
    assert!(
        !message.contains("track-report") && !message.contains("planner input edit"),
        "wrong subsystem entirely: {message}"
    );
    assert_eq!(
        payload(&boot).await["model"],
        Value::Null,
        "the refused write must not have landed"
    );

    let (status, _) = put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": null}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// The write goes through the event spine, so a reader that already listens
/// for `card.updated` sees the new selection without a new event kind — and
/// the audit row names who chose it.
#[tokio::test]
async fn the_write_emits_card_updated_attributed_to_the_human() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": null}),
    )
    .await;

    let events = boot.event_payloads("card.updated").await;
    let last = events.last().expect("a card.updated event was persisted");
    assert_eq!(
        last["payload"]["model"],
        json!("gpt-5"),
        "the event carries the whole card row, selection included: {last}"
    );

    let (actor,): (String,) = sqlx::query_as(
        "SELECT actor FROM events WHERE kind = 'card.updated' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(boot.repo.pool())
    .await
    .expect("the event row");
    let actor: Value = serde_json::from_str(&actor).expect("the actor column is JSON");
    assert_eq!(
        actor["kind"],
        json!("User"),
        "the choice was a person's; a write attributed to the kernel would hide who made it"
    );
}

/// A slug the catalog does not list is reported, never refused: the catalog
/// can be the bundled presets of a signed-out daemon, and a 400 would then
/// block a model this account can really run.
///
/// With no daemon connection there is no catalog to judge against, so the flag
/// stays false — "we could not ask" is not evidence of absence.
#[tokio::test]
async fn an_unreadable_catalog_never_claims_a_model_is_unknown() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let (status, body) = put_model(
        &boot,
        "user",
        json!({"model": "a-model-nobody-has", "reasoning_effort": "high"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["unknown_model"], json!(false));
    assert_eq!(body["effort_adjusted"], json!(false));
    assert_eq!(
        payload(&boot).await["model"],
        json!("a-model-nobody-has"),
        "the choice is stored either way"
    );
}

#[tokio::test]
async fn a_card_id_nobody_has_is_a_404() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let (status, _) = send_json(
        boot.app.clone(),
        "PUT",
        model_uri("card-that-does-not-exist"),
        "user",
        json!({"model": null, "reasoning_effort": null}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// #1505 S4 review — the `card_runs_headless_harness` guard, which nothing
/// used to reach.
///
/// The 404 above walks a different branch entirely (`card_get` returns
/// `None`), so deleting the guard left the whole suite green. What the
/// deletion buys, concretely: a terminal or worker card accepts and stores a
/// selection that `turn/start` will never read, and `GET /planner/run` then
/// reports a model that cannot possibly run — a card claiming a setting it
/// does not have.
///
/// The card is turned into a non-codex one by changing its `kind` in place
/// rather than by minting a second card: the role cache the route consults is
/// populated by the fixture's own creation path, so a card inserted around it
/// would answer 404 at `verify_role` and walk the branch above again instead
/// of the one under test.
#[tokio::test]
async fn a_card_that_is_not_a_planner_codex_card_is_refused() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    sqlx::query("UPDATE cards SET kind = 'terminal' WHERE id = ?1")
        .bind(boot.planner_card.id.as_str())
        .execute(boot.repo.pool())
        .await
        .expect("retarget the card kind");

    let (status, body) = put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": null}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body={body}");
    assert_eq!(
        payload(&boot).await["model"],
        Value::Null,
        "a refused card must not be left holding a selection nothing will read"
    );
}

// ---------------------------------------------------------------------------
// The downlink
// ---------------------------------------------------------------------------

/// The frame is asserted for the FIRST and the SECOND turn. Codex's override
/// is sticky, so an implementation that sent the model only on the turn after
/// a change would look correct on turn one and be wrong from turn two — and it
/// would be wrong in the direction that matters, because a daemon respawn or a
/// resume in between silently drops the sticky value.
#[tokio::test]
async fn a_chosen_model_rides_every_turn_not_just_the_first() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    let (status, body) = put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": "high"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    post_input(boot.app.clone(), boot.planner_card.id.as_str(), "one").await;
    let first = selections_after(&boot, 1).await;
    assert_eq!(first[0].model.as_deref(), Some("gpt-5"));
    assert_eq!(first[0].effort.as_deref(), Some("high"));

    complete_the_running_turn(&boot).await;
    post_input(boot.app.clone(), boot.planner_card.id.as_str(), "two").await;
    let both = selections_after(&boot, 2).await;
    assert_eq!(
        both[1].model.as_deref(),
        Some("gpt-5"),
        "stickiness is codex's, not ours to assume"
    );
    assert_eq!(both[1].effort.as_deref(), Some("high"));
}

/// A card whose picker was never touched must send exactly what this kernel
/// sent before #1505: no `model` key, no `effort` key.
#[tokio::test]
async fn a_card_that_never_chose_anything_sends_neither_key() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    post_input(boot.app.clone(), boot.planner_card.id.as_str(), "hello").await;

    let seen = selections_after(&boot, 1).await;
    assert_eq!(
        seen[0],
        TurnModelSelection::inherit(),
        "an untouched card must not start sending a model"
    );
}

/// #1505 S4 review — the BLOCKER. This is the state THIS feature invents:
/// the person picked a model and then picked "Default" again, so the card is
/// `model_ever_set = true, model = null` and every turn now needs codex to say
/// what the default is. The fixtures daemon answers no RPC, which is exactly a
/// codex restart.
///
/// Two things have to hold, and the first one alone used to be all that was
/// asserted — which is why the defect shipped. The turn must not go out under
/// a model nobody chose, AND the conversation must still be alive afterwards.
/// The first cut wedged, and `HarnessState::Wedged` has no exit in this tree:
/// `can_issue_turn` admits only `Idle | TurnCompleted`, every assignment back
/// to `Idle` is guarded on some other phase, and a snapshot restore rehydrates
/// `Wedged` as `Wedged`. A transient outage therefore ended the conversation
/// for good.
#[tokio::test]
async fn an_unresolvable_default_defers_the_turn_and_recovers_when_it_becomes_resolvable() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": null}),
    )
    .await;
    put_model(
        &boot,
        "user",
        json!({"model": null, "reasoning_effort": null}),
    )
    .await;

    post_input(boot.app.clone(), boot.planner_card.id.as_str(), "hello").await;

    // Nothing is sent, and the message is still the person's.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        assert!(
            boot.daemon.started_turn_selections_for_test().is_empty(),
            "a turn went out under a model nobody could name"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        boot.harness.snapshot().await.pending_entries().len(),
        1,
        "the refused turn must leave the person's message queued, not drop it"
    );

    // And the conversation is still able to issue. This is the half the first
    // cut failed: any phase outside `can_issue_turn` here is a dead
    // conversation whatever the reason string says.
    let phase_now = phase(&boot).await;
    assert!(
        matches!(
            phase_now,
            HarnessPhaseTag::Idle | HarnessPhaseTag::TurnCompleted
        ),
        "a transient resolution failure must leave the harness able to issue; phase={phase_now:?}"
    );

    // The recovery, driven rather than asserted about: name a model, and the
    // message that was waiting goes out under it.
    let (status, body) = put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": null}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    let seen = selections_after(&boot, 1).await;
    assert_eq!(
        seen[0].model.as_deref(),
        Some("gpt-5"),
        "the queued message must be sent once the selection can be resolved again"
    );
}

/// The other half of the same fix, on the other trigger: the card is left
/// alone and codex becomes reachable. Nothing about the card changes — only
/// the daemon — and the queued message must still go out.
///
/// Driven by making the payload itself resolvable again from underneath the
/// harness, which is the same observable the daemon coming back produces: the
/// next attempt resolves where the previous one did not. It proves the retry
/// exists at all, which is what a wedge removed.
#[tokio::test]
async fn a_deferred_turn_retries_without_anyone_touching_the_rest_port() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": null}),
    )
    .await;
    put_model(
        &boot,
        "user",
        json!({"model": null, "reasoning_effort": null}),
    )
    .await;
    post_input(boot.app.clone(), boot.planner_card.id.as_str(), "hello").await;

    // Let at least one attempt fail.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(boot.daemon.started_turn_selections_for_test().is_empty());

    // Repair the payload behind the harness's back — no REST call, so nothing
    // pokes the run loop. Only its own next tick can pick this up.
    sqlx::query("UPDATE cards SET payload = json_set(payload, '$.model', 'gpt-5') WHERE id = ?1")
        .bind(boot.planner_card.id.as_str())
        .execute(boot.repo.pool())
        .await
        .expect("repair the selection");

    let seen = selections_after(&boot, 1).await;
    assert_eq!(
        seen[0].model.as_deref(),
        Some("gpt-5"),
        "the run loop must re-read the card and retry on its own"
    );
}

/// #1505 S4 review round 2 (MAJOR) — the selection is read at the moment the
/// batch is handed over, not from the row fetched before the transcript
/// refresh and the diff.
///
/// The first attempt at this test did not discriminate: it changed the model
/// and THEN queued, so the early `card_get` already saw the new value and the
/// mutation "resolve from the early row" stayed green. A test whose doc
/// describes an experiment its body does not perform is worse than no test.
///
/// This one performs it, using the #1449 drain-race hook — which parks the run
/// loop after the early `card_get` and the diff, and before the drain and the
/// resolve. That is exactly the window, so the park makes the race an
/// ordering: the early row is read with `gpt-5`, the change to `gpt-5-codex`
/// lands while the loop is held, and the frame must carry the second one.
#[tokio::test]
async fn a_change_landing_after_the_early_read_still_ships_on_that_turn() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": null}),
    )
    .await;

    let hook = PlannerHarnessDrainRaceHook {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    install_planner_harness_drain_race_hook_for_test(&boot.worker_session_id, hook.clone());

    post_input(boot.app.clone(), boot.planner_card.id.as_str(), "one").await;
    tokio::time::timeout(Duration::from_secs(5), hook.entered.notified())
        .await
        .expect("the run loop must reach the drain window");

    // Held here, with `gpt-5` already read by the early `card_get`.
    let (status, body) = put_model(
        &boot,
        "user",
        json!({"model": "gpt-5-codex", "reasoning_effort": null}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    hook.release.notify_one();

    let seen = selections_after(&boot, 1).await;
    assert_eq!(
        seen[0].model.as_deref(),
        Some("gpt-5-codex"),
        "the turn must run under the selection as it stood when the batch left, not as it stood \
         before the transcript refresh"
    );
}

/// #1505 S4 review round 2 — the retry is PACED.
///
/// Without a pace, a re-buffered batch re-arms `hard_fire` and the next 50 ms
/// tick tries again, which is roughly twenty codex calls and forty persist
/// writes a second for as long as the condition lasts. Nothing tested it: the
/// other tests wait on an outcome with a five-second deadline, which absorbs
/// any interval smaller than itself, so deleting the constant left them green.
///
/// This counts attempts instead of waiting for one. `config/read` is
/// unanswerable against the fixtures daemon, so every attempt refuses; over a
/// second the paced loop can have made at most a couple, and the unpaced one
/// makes tens.
#[tokio::test]
async fn a_refused_turn_is_retried_on_a_pace_rather_than_every_tick() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": null}),
    )
    .await;
    put_model(
        &boot,
        "user",
        json!({"model": null, "reasoning_effort": null}),
    )
    .await;
    post_input(boot.app.clone(), boot.planner_card.id.as_str(), "hello").await;

    // Sampled over three seconds because the pace under test is two: a window
    // shorter than the interval cannot tell "paced" from "stopped".
    // Sampled over seven seconds against a two-second pace. The window is wide
    // on purpose: at three seconds the lower bound had barely a second of
    // slack, so a busy runner starving the run loop for that long turned a
    // correct implementation RED. A false red costs more here than a blunt
    // bound — the upper bound is what catches the defect, and it is unaffected.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let before = boot.harness.refused_issuances_for_test();
    tokio::time::sleep(Duration::from_secs(7)).await;
    let attempts = boot.harness.refused_issuances_for_test() - before;

    assert!(
        attempts >= 1,
        "the retry must still happen — a harness that stops trying is the wedge whose absence of \
         an exit was an earlier round's BLOCKER"
    );
    assert!(
        attempts <= 12,
        "a paced retry makes about one attempt every two seconds; saw {attempts} in seven \
         seconds, which is the 50 ms tick running unthrottled"
    );
}

/// #1505 S4 review round 2 (MAJOR) — a refusal nobody can wait out is SAID, not
/// merely retried.
///
/// The round-1 fix removed the wedge, which removed the lie — and put silence
/// in its place. The two states that reach this arm — codex's config naming no
/// model, and a stored selection that cannot be read — are both ones no amount
/// of waiting changes, so the conversation retried forever while the person's
/// sentence rendered as `queued` with nothing on screen to say why. Invisible
/// and unrecoverable is not an improvement on visible and unrecoverable.
///
/// Driven through the unreadable-payload state rather than the null-config
/// one, because the fixtures daemon answers no RPC at all and so cannot
/// produce "codex answered, and the answer named no model" — with it, every
/// read is a transient outage instead. The two share this arm by construction
/// (`resolve_model_selection`), and which of them a given card is in changes
/// nothing about what the reader is told.
#[tokio::test]
async fn a_refusal_that_cannot_clear_itself_tells_the_reader_what_to_do() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    sqlx::query("UPDATE cards SET payload = json_set(payload, '$.model', 42) WHERE id = ?1")
        .bind(boot.planner_card.id.as_str())
        .execute(boot.repo.pool())
        .await
        .expect("store a selection nothing can read");
    post_input(boot.app.clone(), boot.planner_card.id.as_str(), "hello").await;

    let deadline = Instant::now() + Duration::from_secs(5);
    let reason = loop {
        if let Some(reason) = boot.harness.issuance_block().await {
            break reason;
        }
        assert!(
            Instant::now() < deadline,
            "a refusal a person has to act on must reach them"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(
        reason.contains("Pick a model"),
        "the message must name the one action that works: {reason}"
    );
    assert!(
        !reason.contains("codex is reachable") && !reason.contains("retry"),
        "it must not offer an action that does nothing, which is what the first cut did: {reason}"
    );

    // And it reaches them through the read the conversation already makes.
    let (status, run) = get(
        boot.app.clone(),
        format!("/api/cards/{}/planner/run", boot.planner_card.id.as_str()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={run}");
    assert_eq!(run["blocked_reason"], json!(reason));

    // Acting on it clears the message and sends the sentence that was waiting.
    put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": null}),
    )
    .await;
    let seen = selections_after(&boot, 1).await;
    assert_eq!(seen[0].model.as_deref(), Some("gpt-5"));
    assert_eq!(
        boot.harness.issuance_block().await,
        None,
        "a block that outlives its cause is the next false message"
    );
}

/// A codex restart is nobody's problem to act on, so it must NOT produce a
/// message. A field that lights up for conditions the reader can do nothing
/// about is one they learn to ignore.
#[tokio::test]
async fn a_transient_refusal_says_nothing_to_the_reader() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": "high"}),
    )
    .await;
    boot.daemon.fail_turn_start_for_test();
    post_input(boot.app.clone(), boot.planner_card.id.as_str(), "hello").await;

    // Read early, and "early" is defined by the retry pace rather than by the
    // silence budget: the notice is computed ON a refusal, and refusals are two
    // seconds apart, so at 400 ms exactly one has happened and its own run of
    // failures is milliseconds old. A future reader shortening the pace must
    // shorten this too.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        boot.harness.issuance_block().await,
        None,
        "codex refusing one turn is not something the reader can act on, and a notice for it \
         would train them to ignore the field"
    );

    // And it is paced. This arm predates #1505 and was unpaced, which was
    // survivable only while reaching it took an operator: `PUT /planner/model`
    // stores a slug codex does not know BY DESIGN, so the picker turned "every
    // turn/start fails" into a menu click. Unpaced that is the 50 ms tick
    // issuing RPCs and persist writes at roughly twenty a second, forever.
    let before = boot.harness.refused_issuances_for_test();
    tokio::time::sleep(Duration::from_secs(7)).await;
    let attempts = boot.harness.refused_issuances_for_test() - before;
    assert!(
        attempts >= 1,
        "it must keep trying — codex dropping one turn is not a reason to abandon the message"
    );
    assert!(
        attempts <= 12,
        "an undelivered turn/start must be retried on the same pace as a refused resolution; saw \
         {attempts} in seven seconds, which is the tick running unthrottled"
    );
}

/// #1505 S4 review round 2 — the OTHER `NeedsAChoice` state, driven end to end.
///
/// `config.model = null` is an explicitly supported codex state: the daemon
/// answers, and the answer names no model. It is the case the enum split
/// exists for, and it is NOT the same code path as an unreadable payload — it
/// only exists once codex has replied, which is why the fixtures daemon had to
/// learn to reply at all before this could be written.
///
/// What the reader gets is asserted through the endpoint their composer reads,
/// not through the enum: an internal state no surface renders is the same
/// silence with more code in it.
#[tokio::test]
async fn a_config_that_names_no_model_reaches_the_reader_through_planner_run() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    // Codex answers, and its answer names nothing. Not an outage.
    boot.daemon.set_config_read_for_test(CodexConfig::default());
    put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": null}),
    )
    .await;
    put_model(
        &boot,
        "user",
        json!({"model": null, "reasoning_effort": null}),
    )
    .await;
    post_input(boot.app.clone(), boot.planner_card.id.as_str(), "hello").await;

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (status, run) = get(
            boot.app.clone(),
            format!("/api/cards/{}/planner/run", boot.planner_card.id.as_str()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={run}");
        if let Some(reason) = run["blocked_reason"].as_str() {
            assert!(
                reason.contains("does not name one") && reason.contains("Pick a model"),
                "the reader must be told what is wrong and the one thing that fixes it: {reason}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "codex answering `model: null` must reach the reader, not just the enum"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert!(
        boot.daemon.started_turn_selections_for_test().is_empty(),
        "and nothing may go out under a model nobody named"
    );

    // Doing the one thing the message asks for sends the waiting sentence.
    put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": null}),
    )
    .await;
    let seen = selections_after(&boot, 1).await;
    assert_eq!(seen[0].model.as_deref(), Some("gpt-5"));
}

/// #1505 S4 review round 2 — a transient refusal has a CEILING on its silence.
///
/// Pacing the retry bounds its rate, not its duration: an outage that lasts an
/// hour left the sentence sitting as `queued` for an hour, politely. Past
/// `transient_silence_budget` the conversation says it is waiting — and keeps
/// waiting, so the notice clears itself rather than becoming the next thing
/// that outlives its cause.
#[tokio::test]
async fn a_transient_refusal_stops_being_silent_once_it_stops_being_brief() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": null}),
    )
    .await;
    boot.daemon.fail_turn_start_for_test();
    post_input(boot.app.clone(), boot.planner_card.id.as_str(), "hello").await;

    // Silent while the outage is still brief — and "brief" is the BUDGET, not
    // merely "the first refusal". The fixture budget (5 s) is deliberately
    // longer than the retry pace (2 s), so by three seconds several refusals
    // have happened and a notice here would mean the budget is being ignored.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        boot.harness.refused_issuances_for_test() >= 2,
        "the window under test must actually contain more than one refusal, or it cannot tell \
         `budget` apart from `notify from the second refusal`"
    );
    assert_eq!(
        boot.harness.issuance_block().await,
        None,
        "inside the silence budget the conversation says nothing, however many refusals it has \
         already made"
    );

    let deadline = Instant::now() + Duration::from_secs(20);
    let reason = loop {
        if let Some(reason) = boot.harness.issuance_block().await {
            break reason;
        }
        assert!(
            Instant::now() < deadline,
            "a wait long enough to look like a hang must stop being silent"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(
        reason.contains("Waiting for codex") && reason.contains("still queued"),
        "it must say the message is still coming, and name no action, because there is none: \
         {reason}"
    );

    // And it goes away on its own — nobody has to acknowledge it.
    boot.daemon.clear_turn_start_failure_for_test();
    let seen = selections_after(&boot, 1).await;
    assert_eq!(seen[0].model.as_deref(), Some("gpt-5"));
    assert_eq!(
        boot.harness.issuance_block().await,
        None,
        "a notice that outlives its cause is the next false message"
    );
}

/// #1505 S4 review r3 (MAJOR) — a slug codex will never accept must not be sold
/// as "still queued and will be sent".
///
/// `PUT /planner/model` stores an unknown slug BY DESIGN (`unknown_model` is a
/// hint, not a refusal), so this is a menu click away. Classifying the refusal
/// as retryable produced a permanent stall behind a sentence promising
/// delivery — round 1's invisible stall made visibly reassuring, which is
/// worse, because a person who reads it waits indefinitely. The pill's own
/// `unknown_model` warning is ephemeral React state and is gone after a
/// reload, so this notice is the only thing left on screen.
#[tokio::test]
async fn a_turn_codex_refuses_never_promises_the_message_will_be_sent() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    put_model(
        &boot,
        "user",
        json!({"model": "a-model-codex-will-not-run", "reasoning_effort": null}),
    )
    .await;
    // Codex ANSWERS, and the answer is no. Not an outage.
    boot.daemon.reject_turn_start_for_test();
    post_input(boot.app.clone(), boot.planner_card.id.as_str(), "hello").await;

    let deadline = Instant::now() + Duration::from_secs(8);
    let reason = loop {
        if let Some(reason) = boot.harness.issuance_block().await {
            break reason;
        }
        assert!(
            Instant::now() < deadline,
            "a refusal codex actually answered is said at once — there is no window in which \
             staying quiet about it is honest"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    assert!(
        reason.contains("has not been sent"),
        "it must say the message did NOT go: {reason}"
    );
    assert!(
        !reason.contains("will be sent"),
        "it must not promise a delivery that cannot happen — this is the exact sentence that \
         made a permanent stall look reassuring: {reason}"
    );
    assert!(
        reason.contains("try another"),
        "and it must name the one lever the reader has: {reason}"
    );

    // The reader is told at once, not after the silence budget the retryable
    // arm waits out.
    assert!(
        Instant::now() < deadline,
        "a refusal must not wait out the silence budget"
    );

    // Changing the model clears it and the waiting sentence goes out.
    boot.daemon.clear_turn_start_failure_for_test();
    put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": null}),
    )
    .await;
    let seen = selections_after(&boot, 1).await;
    assert_eq!(seen[0].model.as_deref(), Some("gpt-5"));
    assert_eq!(boot.harness.issuance_block().await, None);
}

/// #1505 S4 review r3 (MAJOR) — a `model/list` outage is an OUTAGE, not a
/// selection the reader has to fix.
///
/// The construction is a state this feature adds: the person picked an effort
/// and then picked "Default effort" again, so resolving needs the catalog. With
/// `config/read` answering (the common config leaves `model_reasoning_effort`
/// unset) and `model/list` unavailable, the old code degraded the catalog read
/// to `None`, which is indistinguishable from "the catalog has no default" —
/// so a hiccup was reported as "Pick a reasoning effort to start it again" and
/// the retry slowed from two seconds to thirty.
#[tokio::test]
async fn a_catalog_outage_is_not_reported_as_a_selection_the_reader_must_fix() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    // Codex answers the config read, naming a model but no effort — the common
    // configuration — and cannot answer `model/list`, which the fixtures
    // daemon never can.
    boot.daemon.set_config_read_for_test(CodexConfig {
        model: Some("gpt-5".into()),
        model_reasoning_effort: None,
    });
    put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": "high"}),
    )
    .await;
    put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": null}),
    )
    .await;
    post_input(boot.app.clone(), boot.planner_card.id.as_str(), "hello").await;

    // Long enough for several refusals, and inside the silence budget.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        boot.harness.refused_issuances_for_test() >= 2,
        "the resolution must actually be failing, or this proves nothing"
    );
    assert_eq!(
        boot.harness.issuance_block().await,
        None,
        "a catalog that could not be asked is an outage: it must not be dressed up as a choice \
         the reader has to make"
    );
    assert!(
        boot.daemon.started_turn_selections_for_test().is_empty(),
        "and nothing may go out under an effort nobody resolved"
    );
}

/// #1505 S4 review r4 — the workspace read is a SECOND instant, and the
/// foreign key does not reach across it.
///
/// An earlier round deleted this test on the argument that `cards` holds a
/// foreign key to `tracks`, so "card present, track absent" is not a
/// representable state. That is true of any single database instant and
/// irrelevant here: this path reads the card, awaits, and then reads the
/// track. A `track_delete_tx` committing in between — taking the card with it
/// — leaves a harness whose `inner.track_id` names a track that is gone, and
/// the track read answers `Ok(None)`. The claim that the card check "has
/// already returned" by then was backwards: its having returned is the window.
///
/// Driven, not raced: the cwd hook parks the run loop between the two reads.
#[tokio::test]
async fn a_track_deleted_between_the_card_read_and_the_workspace_read_refuses() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    // Codex would answer, and would name a model — so a fallback to the global
    // layers WOULD send a turn, which is what must not happen.
    boot.daemon.set_config_read_for_test(CodexConfig {
        model: Some("gpt-5-from-the-wrong-scope".into()),
        model_reasoning_effort: None,
    });
    put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": null}),
    )
    .await;
    put_model(
        &boot,
        "user",
        json!({"model": null, "reasoning_effort": null}),
    )
    .await;

    let hook = PlannerHarnessCwdRaceHook {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    install_planner_harness_cwd_race_hook_for_test(&boot.worker_session_id, hook.clone());

    post_input(boot.app.clone(), boot.planner_card.id.as_str(), "hello").await;
    tokio::time::timeout(Duration::from_secs(5), hook.entered.notified())
        .await
        .expect("the run loop must reach the workspace read");

    // The card read has already succeeded. Now the track goes, through the
    // SAME function a real delete uses (`track_delete_tx`) rather than
    // hand-rolled SQL — one transaction, so the foreign key holds throughout,
    // and every table that references the track is cleared the way production
    // clears it.
    let track_id = boot.planner_card.track_id.to_string();
    let area_cache = TrackAreaCache::new();
    write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            track_delete_tx(tx, &track_id, &area_cache)
                .await
                .map_err(calm_server::error::CalmError::from)
        })
    })
    .await
    .expect("delete the track the way production does");
    hook.release.notify_one();

    // Nothing may go out under a model resolved from a scope this conversation
    // is not in.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let sent = boot.daemon.started_turn_selections_for_test();
    assert!(
        sent.is_empty(),
        "a workspace that cannot be read must refuse, not fall back to the global config layers; \
         sent {sent:?}"
    );
    assert!(
        boot.harness.refused_issuances_for_test() >= 1,
        "and it must actually have refused, or this proves nothing"
    );
}

/// #1505 S4 review r4 (MAJOR) — `CodexRefused` was minted for `turn/start` and
/// then consulted only there, so the same false promise survived one call
/// above it.
///
/// A refused `config/read` — codex answering, and answering no, as it does for
/// a workspace directory that has been removed — was classified retryable, so
/// past the silence budget the reader was told "your message is still queued
/// and will be sent when it answers" about a turn that could not go out until
/// a person acted. That is the exact sentence `CodexRefused` exists to end.
///
/// It maps to `NeedsAChoice` rather than `Rejected` because a choice genuinely
/// removes the need for the read: a card carrying an explicit model never
/// calls `config/read` at all.
#[tokio::test]
async fn a_refused_config_read_is_not_sold_as_a_wait() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": null}),
    )
    .await;
    put_model(
        &boot,
        "user",
        json!({"model": null, "reasoning_effort": null}),
    )
    .await;
    boot.daemon.reject_config_read_for_test();
    post_input(boot.app.clone(), boot.planner_card.id.as_str(), "hello").await;

    let deadline = Instant::now() + Duration::from_secs(8);
    let reason = loop {
        if let Some(reason) = boot.harness.issuance_block().await {
            break reason;
        }
        assert!(
            Instant::now() < deadline,
            "a refusal codex answered must be said at once, not after the silence budget"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(
        !reason.contains("will be sent when it answers"),
        "codex answered — waiting for it to answer is not the remedy: {reason}"
    );
    assert!(
        reason.contains("has not been sent") && reason.contains("Pick a model"),
        "it must say the message did not go, and name the choice that removes the need for the \
         read: {reason}"
    );

    // And the choice it names works.
    put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": null}),
    )
    .await;
    let seen = selections_after(&boot, 1).await;
    assert_eq!(seen[0].model.as_deref(), Some("gpt-5"));
    assert_eq!(boot.harness.issuance_block().await, None);
}

/// The other half of the same split: codex being UNREACHABLE for the same read
/// still says nothing at first, because that one does clear itself.
#[tokio::test]
async fn an_unreachable_config_read_still_waits_quietly() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": null}),
    )
    .await;
    put_model(
        &boot,
        "user",
        json!({"model": null, "reasoning_effort": null}),
    )
    .await;
    // No `reject_config_read_for_test`: the fixtures daemon simply cannot be
    // asked, which is the outage case.
    post_input(boot.app.clone(), boot.planner_card.id.as_str(), "hello").await;

    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        boot.harness.refused_issuances_for_test() >= 2,
        "the read must actually be failing, or this proves nothing"
    );
    assert_eq!(
        boot.harness.issuance_block().await,
        None,
        "an unreachable codex is not a choice the reader has to make, and inside the silence \
         budget it is not said at all"
    );
}
