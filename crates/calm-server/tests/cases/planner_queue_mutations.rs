//! #1505 PR2 — the queue write port, end to end.
//!
//! Four things are pinned here that no unit test can see: the actor guard on a
//! real request, the compare-and-swap answering with the body the client needs
//! to retry with, the debounce being re-armed from what is left in the queue,
//! and the delete-versus-drain race having two outcomes rather than three.

use axum::http::StatusCode;
use calm_server::harness::{Observation, QueueEntry};
use serde_json::{Value, json};

use crate::support::planner_queue_fixture::{
    Boot, Issuance, boot_with, boot_with_issuance, get, idle_snapshot, post_input, send_json,
};

fn entry_uri(card_id: &str, entry_id: &str) -> String {
    format!("/api/cards/{card_id}/planner/input/{entry_id}")
}

async fn patch_entry(
    boot: &Boot,
    entry_id: &str,
    text: &str,
    if_entry_rev: u32,
) -> (StatusCode, Value) {
    send_json(
        boot.app.clone(),
        "PATCH",
        entry_uri(boot.planner_card.id.as_str(), entry_id),
        "user",
        json!({"text": text, "if_entry_rev": if_entry_rev}),
    )
    .await
}

async fn delete_entry(boot: &Boot, entry_id: &str, if_entry_rev: u32) -> (StatusCode, Value) {
    send_json(
        boot.app.clone(),
        "DELETE",
        entry_uri(boot.planner_card.id.as_str(), entry_id),
        "user",
        json!({"if_entry_rev": if_entry_rev}),
    )
    .await
}

/// Queue one message through the production send route and return its id.
async fn queue_one(boot: &Boot, text: &str) -> String {
    let (status, posted) = post_input(boot.app.clone(), boot.planner_card.id.as_str(), text).await;
    assert_eq!(status, StatusCode::OK, "body={posted}");
    posted["entry_id"]
        .as_str()
        .expect("a live harness acks with an id")
        .to_string()
}

async fn pending(boot: &Boot) -> Vec<Value> {
    let (status, run) = get(
        boot.app.clone(),
        format!("/api/cards/{}/planner/run", boot.planner_card.id.as_str()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={run}");
    run["pending"].as_array().cloned().unwrap_or_default()
}

// ---------------------------------------------------------------------------
// The happy paths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_edit_rewrites_the_entry_bumps_its_rev_and_keeps_its_id() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let entry_id = queue_one(&boot, "look at the raport").await;

    let (status, body) = patch_entry(&boot, &entry_id, "look at the report", 0).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["entry_id"], json!(entry_id));
    assert_eq!(body["text"], json!("look at the report"));
    assert_eq!(
        body["rev"],
        json!(1),
        "a rewrite invalidates a stale editor"
    );

    let listed = pending(&boot).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed[0]["entry_id"],
        json!(entry_id),
        "editing must not re-mint the id the client is holding"
    );
    assert_eq!(listed[0]["text"], json!("look at the report"));
    assert_eq!(listed[0]["rev"], json!(1));
}

#[tokio::test]
async fn a_delete_removes_the_entry_and_a_second_delete_answers_404() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let entry_id = queue_one(&boot, "never mind").await;

    let (status, body) = delete_entry(&boot, &entry_id, 0).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["entry_id"], json!(entry_id));
    assert_eq!(body["text"], Value::Null, "a delete has no text to echo");
    assert!(pending(&boot).await.is_empty());

    // Not idempotent, and the 404 says only "it is not in the queue" — which
    // is why the frontend contract is "re-read", not "retry".
    let (status, body) = delete_entry(&boot, &entry_id, 0).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body={body}");
    assert_eq!(body["code"], json!("not_found"));
}

#[tokio::test]
async fn an_unknown_entry_id_is_404_and_leaves_the_queue_alone() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let kept = queue_one(&boot, "still mine").await;

    let (status, body) = delete_entry(&boot, "no-such-entry", 0).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body={body}");

    let listed = pending(&boot).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["entry_id"], json!(kept));
}

/// A dispatcher observation has no id, so nothing can address it — including
/// an attacker who guesses. There is no id to guess: `QueueEntry::System`
/// has no id field.
#[tokio::test]
async fn a_system_observation_cannot_be_addressed_at_all() {
    let boot = boot_with(idle_snapshot(vec![
        QueueEntry::system(
            Observation::TrackGoal {
                text: "the track goal".into(),
            },
            None,
        )
        .expect("a track goal is a system entry"),
    ]))
    .await;

    let (status, _) = delete_entry(&boot, "anything", 0).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        boot.harness.snapshot().await.pending_entries().len(),
        1,
        "the system entry is untouched"
    );
}

// ---------------------------------------------------------------------------
// §11.3 #5 / #5b — the actor guard
// ---------------------------------------------------------------------------

/// The criterion is `Actor::as_str() == "user"`, and `ai:claude` is the case
/// that proves it has to be: `Actor::to_actor_id()` folds `ai:claude` into
/// `ActorId::User` by a defensive default, so a guard written on the id would
/// let exactly this request through while looking correct.
#[tokio::test]
async fn an_agent_actor_is_refused_and_the_human_is_not() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let entry_id = queue_one(&boot, "the human's own sentence").await;
    let uri = entry_uri(boot.planner_card.id.as_str(), &entry_id);

    for agent in ["ai:claude", "ai:codex", "ai:planner"] {
        let (status, body) = send_json(
            boot.app.clone(),
            "DELETE",
            uri.clone(),
            agent,
            json!({"if_entry_rev": 0}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "actor={agent} body={body}");
        let message = body["error"].as_str().unwrap_or_default();
        assert!(
            message.contains("planner input"),
            "the 403 must name the subsystem that refused: {message}"
        );
        assert!(
            !message.contains("track-report"),
            "and must not inherit another subsystem's copy: {message}"
        );
    }
    assert_eq!(pending(&boot).await.len(), 1, "nothing was deleted");

    // The green half: the same request as the human succeeds.
    let (status, body) = delete_entry(&boot, &entry_id, 0).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
}

// The paired green example — that parameterising the criterion did not change
// what the track-report port says — is
// `routes::track_report_blocks::tests::each_write_port_names_itself_in_its_403`,
// which is in-crate because the function is `pub(crate)`.

// ---------------------------------------------------------------------------
// §11.3 #6 / #7 — compare and swap
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_stale_if_entry_rev_is_409_carrying_the_current_text_and_rev() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let entry_id = queue_one(&boot, "first draft").await;

    // Somebody else's edit moves the entry to rev 1.
    let (status, _) = patch_entry(&boot, &entry_id, "second draft", 0).await;
    assert_eq!(status, StatusCode::OK);

    // The first tab still holds rev 0.
    let (status, body) = patch_entry(&boot, &entry_id, "a third, blind draft", 0).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(body["code"], json!("planner_input_stale"));
    assert_eq!(body["entry_id"], json!(entry_id));
    assert_eq!(
        body["text"],
        json!("second draft"),
        "the 409 must carry what it collided with, or the client has to guess"
    );
    assert_eq!(body["rev"], json!(1));

    // And the collision changed nothing.
    let listed = pending(&boot).await;
    assert_eq!(listed[0]["text"], json!("second draft"));

    // The green half: resending with the rev the 409 handed back succeeds.
    let (status, body) = patch_entry(&boot, &entry_id, "a third, blind draft", 1).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["rev"], json!(2));
}

#[tokio::test]
async fn a_delete_with_a_stale_if_entry_rev_is_refused() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let entry_id = queue_one(&boot, "keep me").await;
    let (status, _) = patch_entry(&boot, &entry_id, "no, keep this instead", 0).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = delete_entry(&boot, &entry_id, 0).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "deleting what you last read is the same precondition as editing it: {body}"
    );
    assert_eq!(body["code"], json!("planner_input_stale"));
    assert_eq!(pending(&boot).await.len(), 1);

    let (status, _) = delete_entry(&boot, &entry_id, 1).await;
    assert_eq!(status, StatusCode::OK);
}

/// §11.3 #7 — a fold is a rewrite, so it invalidates a stale editor the same
/// way an edit does. Without the `rev` bump on the fold path, an edit issued
/// against the pre-fold text would silently discard the folded-in message.
#[tokio::test]
async fn a_fold_invalidates_an_editor_holding_the_pre_fold_rev() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    // Fill the queue one short of the cap with system observations, so the
    // user message queued next becomes the TAIL of a full queue — which is the
    // only position a later send folds into.
    for index in 0..calm_server::harness::MAX_PENDING_QUEUE_LEN - 1 {
        boot.harness
            .observe(Observation::TrackGoal {
                text: format!("filler {index}"),
            })
            .expect("a system observation is accepted");
    }
    for _ in 0..2_000 {
        if boot.harness.snapshot().await.pending_entries().len()
            == calm_server::harness::MAX_PENDING_QUEUE_LEN - 1
        {
            break;
        }
        tokio::task::yield_now().await;
    }

    let tail = queue_one(&boot, "first half").await;
    assert_eq!(
        boot.harness.snapshot().await.pending_entries().len(),
        calm_server::harness::MAX_PENDING_QUEUE_LEN,
        "the queue is now full and its tail is the entry under test"
    );

    let (status, folded) = post_input(
        boot.app.clone(),
        boot.planner_card.id.as_str(),
        "second half",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={folded}");
    assert_eq!(
        folded["entry_id"],
        json!(tail),
        "the ack names the survivor, not the id that was discarded"
    );

    let (status, body) = patch_entry(&boot, &tail, "only the first half", 0).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "the body this editor read no longer exists: {body}"
    );
    assert_eq!(body["rev"], json!(1));
    assert_eq!(body["text"], json!("first half\n\nsecond half"));

    // The green half: the rev the 409 handed back still works.
    let (status, body) = patch_entry(&boot, &tail, "only the first half", 1).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
}

// ---------------------------------------------------------------------------
// §11.3 #9 / #14 — debounce re-arming
// ---------------------------------------------------------------------------

/// Deleting the only hard-fire entry must disarm the queue. Without the
/// recompute, a queue holding nothing but soft observations would keep the
/// arming the deleted message gave it and fire a turn nobody asked for.
#[tokio::test]
async fn deleting_the_only_user_entry_disarms_a_queue_of_system_observations() {
    let boot = boot_with(idle_snapshot(vec![
        QueueEntry::system(
            Observation::TrackGoal {
                text: "a soft observation".into(),
            },
            None,
        )
        .expect("system entry"),
    ]))
    .await;
    let entry_id = queue_one(&boot, "a hard-fire message").await;
    assert!(
        boot.harness.debounce_hard_fire_for_test().await,
        "the send armed the queue"
    );

    let (status, _) = delete_entry(&boot, &entry_id, 0).await;
    assert_eq!(status, StatusCode::OK);

    assert!(
        !boot.harness.debounce_hard_fire_for_test().await,
        "nothing hard-fire is left, so the arming must be gone"
    );
    let (first, last) = boot.harness.debounce_timestamps_set_for_test().await;
    assert!(
        first && last,
        "the queue is NOT empty, so the surviving observation keeps the arming \
         window it was enqueued with"
    );
}

#[tokio::test]
async fn deleting_the_last_entry_clears_the_debounce_window() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let entry_id = queue_one(&boot, "the only one").await;

    let (status, _) = delete_entry(&boot, &entry_id, 0).await;
    assert_eq!(status, StatusCode::OK);

    assert!(!boot.harness.debounce_hard_fire_for_test().await);
    let (first, last) = boot.harness.debounce_timestamps_set_for_test().await;
    assert!(
        !first && !last,
        "an empty queue has no pending window to keep"
    );
}

/// The other direction, so the recompute cannot be read as "any delete
/// disarms": with a second user message still queued, the arming stays.
#[tokio::test]
async fn deleting_one_of_two_user_entries_keeps_the_queue_armed() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let first = queue_one(&boot, "one").await;
    let _second = queue_one(&boot, "two").await;

    let (status, _) = delete_entry(&boot, &first, 0).await;
    assert_eq!(status, StatusCode::OK);

    assert!(
        boot.harness.debounce_hard_fire_for_test().await,
        "a user message is still waiting"
    );
    let (first_set, last_set) = boot.harness.debounce_timestamps_set_for_test().await;
    assert!(first_set && last_set);
}

/// An edit is not a departure, so it must not touch the window at all —
/// otherwise a user could postpone their own turn indefinitely by retyping.
#[tokio::test]
async fn an_edit_does_not_move_the_debounce_window() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let entry_id = queue_one(&boot, "before").await;
    let before = boot
        .harness
        .debounce_first_pending_elapsed_ms_for_test()
        .await;

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let (status, _) = patch_entry(&boot, &entry_id, "after", 0).await;
    assert_eq!(status, StatusCode::OK);

    let after = boot
        .harness
        .debounce_first_pending_elapsed_ms_for_test()
        .await;
    assert!(
        after >= before,
        "the window must keep running, not restart: before={before} after={after}"
    );
    assert!(
        boot.harness.debounce_hard_fire_for_test().await,
        "and the entry is still hard-fire"
    );
}

// ---------------------------------------------------------------------------
// The event
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_mutation_emits_harness_queue_changed_naming_the_entry_and_the_actor() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let entry_id = queue_one(&boot, "queued").await;

    let (status, _) = patch_entry(&boot, &entry_id, "edited", 0).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = delete_entry(&boot, &entry_id, 1).await;
    assert_eq!(status, StatusCode::OK);

    let changes = boot.event_payloads("harness.queue.changed").await;
    assert_eq!(changes.len(), 2, "one per applied mutation: {changes:?}");

    assert_eq!(
        changes[0],
        json!({
            "worker_session_id": boot.worker_session_id,
            "card_id": boot.planner_card.id.as_str(),
            "track_id": boot.planner_card.track_id.as_str(),
            "entry_id": entry_id,
            "change": "edited",
            "actor": {"kind": "User"},
        }),
        "the stored payload is the wire payload"
    );
    assert_eq!(changes[1]["change"], json!("deleted"));
    assert_eq!(changes[1]["entry_id"], json!(entry_id));
}

#[tokio::test]
async fn a_refused_mutation_emits_nothing() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let entry_id = queue_one(&boot, "queued").await;

    let (status, _) = patch_entry(&boot, &entry_id, "blind", 7).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = delete_entry(&boot, "no-such-entry", 0).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    assert!(
        boot.event_payloads("harness.queue.changed")
            .await
            .is_empty(),
        "a refusal changed nothing, so announcing a change would be a lie"
    );
}

// ---------------------------------------------------------------------------
// §11.2 — the race, asserted as a mechanism
// ---------------------------------------------------------------------------

/// A delete submitted while the run loop is free to issue a turn has exactly
/// two outcomes, and this test names both and denies a third.
///
/// It asserts on the pair `(HTTP status, what the daemon was handed)` and never
/// on timing. The property is not "the delete wins" or "issuance wins" — it is
/// that one of them completely precedes the other:
///
/// - 200 ⇒ the entry left the queue before issuance looked at it, so the
///   daemon must never have seen its text.
/// - 404 ⇒ issuance drained it first, so the daemon must have seen it.
///
/// The third state — accepted AND delivered, or refused AND never delivered —
/// is the only thing this test can fail on.
///
/// WHAT CARRIES IT, measured rather than assumed. Two things, and neither is
/// the one the design named:
///
///  1. `queue::apply_mutation` locates, checks `rev` and writes inside a single
///     hold of `Inner::pending_queue`.
///  2. `maybe_issue_turn` EMPTIES that queue, under the same lock, before it
///     calls `turn/start`. So "left the queue" and "reached the daemon" are
///     ordered by the lock, not by which task is running.
///
/// Mutating (2) — read the entries and remove them only after `turn/start`
/// returns, which is a plausible implementation and one edit — reddens this
/// test and the deterministic one below.
///
/// The design predicted the mutation "handle `Mutate` on the caller's task
/// instead of the run loop's `select!`". That was tried and this test stayed
/// GREEN, correctly: `pending_queue` is a `tokio::Mutex`, so moving the
/// mutation to another task changes who waits, not what is atomic. The
/// `select!` still matters for latency and ordering against the rest of a tick
/// — see the second test below — but it is not what makes the disjunction
/// true, and a comment claiming it was would have been a comfortable
/// falsehood.
///
/// The varying delay is a WINDOW EXPLORER, not part of any assertion: the run
/// loop's tick is 50ms, so a delete issued immediately after the POST would
/// win every single round and the 404 half of the disjunction would never be
/// reached. Nothing here asserts what a given delay produces. The 404 half is
/// also pinned deterministically, without any clock, by
/// `a_delete_arriving_while_turn_start_is_in_flight_is_404_and_was_delivered`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delete_racing_issuance_never_lands_in_a_third_state() {
    const TEXT: &str = "the racing sentence";
    const ROUNDS: u64 = 24;
    let mut deleted_before_issue = 0;
    let mut issued_before_delete = 0;

    for round in 0..ROUNDS {
        let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
        let entry_id = queue_one(&boot, TEXT).await;
        tokio::time::sleep(std::time::Duration::from_millis(round * 3 % 55)).await;

        let (status, body) = delete_entry(&boot, &entry_id, 0).await;

        // Let an issuance that was already in flight finish before reading the
        // daemon. This decides nothing — both branches are asserted below
        // whichever way it goes — it only stops the read from racing the write
        // it is reading.
        for _ in 0..2_000 {
            if boot.harness.snapshot().await.pending_entries().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        let delivered = boot
            .daemon
            .started_turns_for_test()
            .iter()
            .any(|(_, items)| format!("{items:?}").contains(TEXT));

        match status {
            StatusCode::OK => {
                assert!(
                    !delivered,
                    "round {round}: the delete was accepted, so the text must never have \
                     reached the daemon — accepted AND delivered is the third state this test \
                     denies (body={body})"
                );
                deleted_before_issue += 1;
            }
            StatusCode::NOT_FOUND => {
                assert!(
                    delivered,
                    "round {round}: the delete answered 404, so the entry had already left the \
                     queue — the only way out is issuance, so the daemon must hold it \
                     (body={body})"
                );
                issued_before_delete += 1;
            }
            other => panic!("round {round}: unexpected status {other} body={body}"),
        }
    }

    assert_eq!(
        deleted_before_issue + issued_before_delete,
        ROUNDS,
        "every round landed in one of the two states"
    );
    // For the record, and not asserted: on the machine this was written on the
    // split is a stable 17 accepted / 7 refused. Asserting a split would be
    // asserting a schedule, which is the thing this test is built not to do.
}

/// The 404 half of the disjunction above, reached without a clock.
///
/// The fake daemon is told to block inside `turn/start` after it has recorded
/// what it was handed. So at the moment the delete is submitted, the entry has
/// provably left the queue and provably reached the daemon — and the delete
/// must say 404 rather than claim to have removed something already on its way.
///
/// It also shows GAP-L from the other side: the delete cannot be answered while
/// issuance is in flight, because both are arms of the same `select!`. That is
/// why the request is spawned and the daemon released before it is awaited —
/// awaiting it first would hang until the 30s request timeout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delete_arriving_while_turn_start_is_in_flight_is_404_and_was_delivered() {
    const TEXT: &str = "already on its way";
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    let entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    boot.daemon.install_turn_start_return_hook_for_test(
        calm_server::shared_codex_appserver::TurnStartReturnHook {
            entered: entered.clone(),
            release: release.clone(),
        },
    );

    let entry_id = queue_one(&boot, TEXT).await;
    entered.notified().await;

    assert!(
        boot.daemon
            .started_turns_for_test()
            .iter()
            .any(|(_, items)| format!("{items:?}").contains(TEXT)),
        "the daemon is holding this text right now"
    );

    let app = boot.app.clone();
    let uri = entry_uri(boot.planner_card.id.as_str(), &entry_id);
    let pending_delete = tokio::spawn(async move {
        send_json(app, "DELETE", uri, "user", json!({"if_entry_rev": 0})).await
    });
    // Nothing can answer it until the run loop's tick arm returns.
    release.notify_one();
    let (status, body) = pending_delete.await.expect("the delete task finishes");

    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the entry is gone from the queue and on its way to the model; reporting \
         a successful delete would be a lie the user acts on (body={body})"
    );
}
