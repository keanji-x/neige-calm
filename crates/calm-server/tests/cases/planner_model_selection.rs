//! Choosing the model a planner conversation's turns run with, end to end: the REST write reaches the payload, the
//! payload reaches every `turn/start`, the actor guard stands on a real request, and an unresolvable selection stops the conversation.

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

/// Wait for the run loop to have issued `count` turns and return what each one told codex about the model; the deadline is a failure ceiling.
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

/// Tell the harness the in-flight turn finished: the fixtures fake never ends a turn, so a second turn is otherwise unreachable.
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

/// This is a PUT, and the stored selection has no third state for an omitted key to mean.
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
    // Positional, not merely present: `require_rest_user_actor_for` takes two `&str`s, so a swap compiles; the
    // substring spans the boundary between the subject slot and the fixed rule text.
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

/// The catalog can be the bundled presets of a signed-out daemon, so an unknown slug is reported, never refused; with no daemon there is no catalog to judge against.
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

/// The card is turned into a non-codex one by changing its `kind` in place rather than minting a second card: the
/// role cache the route consults is populated by the fixture's creation path, so a card inserted around it would answer 404 at `verify_role`.
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

/// Codex's override is sticky, so sending the model only on the turn after a change would look correct on turn one; a daemon respawn or resume silently drops the sticky value.
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

/// `model_ever_set = true, model = null`: every turn needs codex to say what the default is, and the fixtures daemon
/// answers no RPC, exactly a codex restart. The turn must not go out under a model nobody chose AND the conversation must stay alive.
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

    // Any phase outside `can_issue_turn` here is a dead conversation whatever the reason string says.
    let phase_now = phase(&boot).await;
    assert!(
        matches!(
            phase_now,
            HarnessPhaseTag::Idle | HarnessPhaseTag::TurnCompleted
        ),
        "a transient resolution failure must leave the harness able to issue; phase={phase_now:?}"
    );

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

/// Driven by making the payload resolvable again from underneath the harness, the same observable the daemon coming back produces.
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

    // Repair the payload behind the harness's back — no REST call, so only its own next tick can pick this up.
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

/// The selection is read at the moment the batch is handed over. The drain-race hook parks the run loop after the
/// early `card_get` and before the resolve, so the change lands while the loop is held and the frame must carry it.
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

/// Without a pace, a re-buffered batch re-arms `hard_fire` every 50 ms tick. Attempts are counted: `config/read` is
/// unanswerable against the fixtures daemon, so every attempt refuses.
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

    // Sampled over seven seconds against a two-second pace: a window barely longer than the interval turned a correct
    // implementation red on a busy runner; the upper bound is what catches the defect.
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

/// Driven through the unreadable-payload state rather than null-config: the fixtures daemon answers no RPC and so
/// cannot produce "codex answered, and the answer named no model". The two share this arm (`resolve_model_selection`).
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

    let (status, run) = get(
        boot.app.clone(),
        format!("/api/cards/{}/planner/run", boot.planner_card.id.as_str()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={run}");
    assert_eq!(run["blocked_reason"], json!(reason));

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

/// A field that lights up for conditions the reader can do nothing about is one they learn to ignore.
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

    // "Early" is defined by the retry pace: refusals are two seconds apart, so at 400 ms exactly one has happened. Shortening the pace must shorten this too.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        boot.harness.issuance_block().await,
        None,
        "codex refusing one turn is not something the reader can act on, and a notice for it \
         would train them to ignore the field"
    );

    // `PUT /planner/model` stores a slug codex does not know BY DESIGN, so an unpaced arm here is the 50 ms tick issuing RPCs forever.
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

/// `config.model = null` is a supported codex state: the daemon answers, and the answer names no model. Asserted through the endpoint the composer reads, not the enum.
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

    put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": null}),
    )
    .await;
    let seen = selections_after(&boot, 1).await;
    assert_eq!(seen[0].model.as_deref(), Some("gpt-5"));
}

/// Pacing bounds the retry's rate, not its duration; past `transient_silence_budget` the conversation says it is waiting, and the notice clears itself.
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

    // "Brief" is the BUDGET (5 s), deliberately longer than the retry pace (2 s), so by three seconds several refusals have happened.
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

/// `PUT /planner/model` stores an unknown slug BY DESIGN, so this is a menu click away; the pill's own `unknown_model`
/// warning is ephemeral React state, so this notice is the only thing left on screen after a reload.
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

    // The reader is told at once, not after the silence budget the retryable arm waits out.
    assert!(
        Instant::now() < deadline,
        "a refusal must not wait out the silence budget"
    );

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

/// With `config/read` answering (no effort set) and `model/list` unavailable, a degraded catalog read of `None` is indistinguishable from "the catalog has no default".
#[tokio::test]
async fn a_catalog_outage_is_not_reported_as_a_selection_the_reader_must_fix() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    // Codex answers the config read, naming a model but no effort, and cannot answer `model/list`, which the fixtures daemon never can.
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

/// This path reads the card, awaits, then reads the track; a `track_delete_tx` committing in between leaves a harness
/// whose `inner.track_id` names a track that is gone. Driven, not raced: the cwd hook parks the run loop between the two reads.
#[tokio::test]
async fn a_track_deleted_between_the_card_read_and_the_workspace_read_refuses() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    // Codex would name a model, so a fallback to the global layers WOULD send a turn, which is what must not happen.
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

    // The track goes through the SAME function a real delete uses (`track_delete_tx`), so every referencing table is cleared the way production clears it.
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

    // Nothing may go out under a model resolved from a scope this conversation is not in.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let sent = boot.daemon.started_turn_selections_for_test();
    assert!(
        sent.is_empty(),
        "a workspace that cannot be read must refuse, not fall back to the global config layers; \
         sent {sent:?}"
    );
    // A weak corroboration: `track_delete_tx` cascades the card away, so the next tick also refuses at the card-existence check. The discriminator is `sent.is_empty()` above.
    assert!(
        boot.harness.refused_issuances_for_test() >= 1,
        "the loop must have refused at least once"
    );
}

/// A refused `config/read` (codex answering no, as for a removed workspace directory) maps to `NeedsAChoice`, not
/// `Rejected`: a choice can remove the need for the read. The read is entered by a disjunction, so an explicit model does not by itself skip it.
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
    // No `reject_config_read_for_test`: the fixtures daemon simply cannot be asked, which is the outage case.
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

/// The third codex read on this path, classified at its own call site: a mutation flipping only this site would not be caught by the `config/read` test.
#[tokio::test]
async fn a_refused_model_list_is_not_sold_as_a_wait() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    // config/read answers and names a model but no effort, so resolving the effort needs the catalog.
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
    boot.daemon.reject_model_list_for_test();
    post_input(boot.app.clone(), boot.planner_card.id.as_str(), "hello").await;

    let deadline = Instant::now() + Duration::from_secs(8);
    let reason = loop {
        if let Some(reason) = boot.harness.issuance_block().await {
            break reason;
        }
        assert!(
            Instant::now() < deadline,
            "codex refusing the catalog is an answer, and must be said at once"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(
        !reason.contains("will be sent when it answers"),
        "codex answered — waiting for it to answer is not the remedy: {reason}"
    );
    assert!(
        reason.contains("has not been sent") && reason.contains("Pick a reasoning effort"),
        "it must name the choice that removes the need for the catalog read: {reason}"
    );

    put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": "high"}),
    )
    .await;
    let seen = selections_after(&boot, 1).await;
    assert_eq!(seen[0].effort.as_deref(), Some("high"));
    assert_eq!(boot.harness.issuance_block().await, None);
}

/// `config/read` is entered by a disjunction (model follows the default, or effort does, or both), so the assertion is on WHICH word appears, per construction.
#[tokio::test]
async fn the_remedy_names_the_half_that_actually_follows_the_default() {
    // (a) the model follows the default, the effort is explicit.
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": "high"}),
    )
    .await;
    put_model(
        &boot,
        "user",
        json!({"model": null, "reasoning_effort": "high"}),
    )
    .await;
    boot.daemon.reject_config_read_for_test();
    post_input(boot.app.clone(), boot.planner_card.id.as_str(), "hello").await;
    let reason = block_reason_within(&boot, Duration::from_secs(8)).await;
    assert!(
        reason.contains("Pick a model explicitly"),
        "the model is what follows the default here: {reason}"
    );
    assert!(
        !reason.contains("reasoning effort"),
        "and the effort is explicit, so naming it would send the reader to re-pick what they \
         already have: {reason}"
    );

    // (b) the mirror image: the model is explicit, the EFFORT follows the default.
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
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
    boot.daemon.reject_config_read_for_test();
    post_input(boot.app.clone(), boot.planner_card.id.as_str(), "hello").await;
    let reason = block_reason_within(&boot, Duration::from_secs(8)).await;
    assert!(
        reason.contains("Pick a reasoning effort explicitly"),
        "the effort is what follows the default here; naming the model would leave the reader \
         re-picking a model they already have while the disjunct stays true: {reason}"
    );

    // (c) both — fixing one still leaves the other entering the same branch, so both are named.
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    put_model(
        &boot,
        "user",
        json!({"model": "gpt-5", "reasoning_effort": "high"}),
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
    let reason = block_reason_within(&boot, Duration::from_secs(8)).await;
    assert!(
        reason.contains("Pick a model and a reasoning effort explicitly"),
        "both halves follow the default, so naming one leaves the reader stuck after doing what \
         they were told: {reason}"
    );
}

/// Wait for a reader-visible block and return it.
async fn block_reason_within(boot: &Boot, budget: Duration) -> String {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(reason) = boot.harness.issuance_block().await {
            return reason;
        }
        assert!(
            Instant::now() < deadline,
            "a refusal codex answered must reach the reader"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
