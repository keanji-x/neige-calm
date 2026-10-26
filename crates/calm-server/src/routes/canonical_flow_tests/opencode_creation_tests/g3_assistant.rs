//! G3 uses authenticated HTTP, the shared acceptance owner and a local Grok ACP peer.
use super::*;
use crate::routes::canonical_flow_tests::assistant_canonical::{carrier, initial, track};

fn calls(fixture: &Fixture) -> Vec<Value> {
    std::fs::read_to_string(peer_root(fixture, &AgentProvider::Grok).join("requests.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

async fn facts(fixture: &Fixture, card: &str) -> (i64, i64, i64, i64, i64, i64) {
    sqlx::query_as("SELECT
        (SELECT count(*) FROM cards WHERE id=?1),
        (SELECT count(*) FROM worker_sessions WHERE card_id=?1),
        (SELECT count(*) FROM conversation_messages WHERE card_id=?1),
        (SELECT count(*) FROM conversation_message_revisions r JOIN conversation_messages m ON m.id=r.message_id WHERE m.card_id=?1),
        (SELECT count(*) FROM conversation_presentations WHERE card_id=?1 AND source_kind='human_message'),
        (SELECT count(*) FROM conversation_assistant_creations WHERE card_id=?1)")
        .bind(card).fetch_one(fixture.repo.pool()).await.unwrap()
}

async fn idle(fixture: &Fixture, card: &str, attempts: usize) -> CanonicalHarnessSnapshot {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (_, frame) = carrier(fixture, card).await;
            if frame.idle_for_lifecycle()
                && frame.canonical_inputs.pending.is_empty()
                && frame.canonical_inputs.attempts.len() == attempts
            {
                return frame;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|error| panic!("{error}: requests={:#?}", calls(fixture)))
}

async fn no_wake(
    app: &axum::Router,
    cookie: &str,
    uri: &str,
    key: &str,
    body: Value,
    card: &str,
) -> Value {
    let seam = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test("canonical-human-wake-finished", card, seam.clone());
    let replay = post(app, cookie, uri, key, body).await;
    let woke = tokio::time::timeout(Duration::from_secs(1), seam.entered.notified())
        .await
        .is_ok();
    // Release even a faulty replay before asserting, and leave a permit for a later fresh wake.
    seam.release.notify_one();
    assert!(!woke, "original creation key must not schedule a wake");
    replay
}

async fn source(fixture: &Fixture, track: &str) -> String {
    sqlx::query_scalar("SELECT id FROM cards WHERE track_id=?1 AND role='planner'")
        .bind(track)
        .fetch_one(fixture.repo.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn g3_grok_assistant_normal_atomic_first_send() {
    atomic(false).await;
}

#[tokio::test]
async fn g3_grok_assistant_side_atomic_first_send() {
    atomic(true).await;
}

async fn atomic(side: bool) {
    let mut fixture = Fixture::new().await;
    configure(&mut fixture);
    let (state, app, cookie) = fixture.application().await;
    let track = track(&app, &cookie).await;
    let uri = format!("/api/tracks/{track}/conversations");
    let card = crate::conversation_keys::derive_track_conversation_keys(&track, "atomic").card_id;
    let locks = crate::state::RouteState::from_ref(&state).planner_recovery_locks;
    let guard = crate::per_card_lock::lock_card(&locks, &card).await;
    let mut body = json!({"provider":"grok","text":FIRST_BODY});
    if side {
        body["side"] = json!({"source_card_id":source(&fixture, &track).await,"context":"Frozen Grok excerpt"});
    }
    let created = tokio::time::timeout(
        Duration::from_secs(5),
        post(&app, &cookie, &uri, "atomic", body.clone()),
    )
    .await
    .expect("canonical acceptance cannot wait for the held execution fence");
    assert_eq!(created["id"], card);
    assert_eq!(facts(&fixture, &card).await, (1, 1, 1, 1, 1, 1));
    let (worker, frame) = carrier(&fixture, &card).await;
    assert_eq!(frame.canonical_inputs.pending.len(), 1);
    assert!(frame.canonical_inputs.active.is_none());
    assert!(frame.canonical_inputs.attempts.is_empty());
    let receipt = &created["initialInput"]["receipt"];
    crate::routes::canonical_flow_tests::assistant_canonical::assert_initial_carrier(
        receipt, &worker,
    );
    let stored: String = sqlx::query_scalar(
        "SELECT r.payload_json FROM conversation_message_revisions r
         JOIN conversation_messages m ON m.id=r.message_id WHERE m.card_id=?1",
    )
    .bind(&card)
    .fetch_one(fixture.repo.pool())
    .await
    .unwrap();
    let stored: calm_types::conversation::PrivateHistoryPayload<
        calm_types::conversation::HumanPayload,
    > = serde_json::from_str(&stored).unwrap();
    let calm_types::conversation::PrivateHistoryPayload::Present { value } = stored else {
        panic!("accepted Human payload must remain readable");
    };
    assert_eq!(value.text, FIRST_BODY);
    assert!(value.bound_attachments.is_empty());
    assert_eq!(
        frame.canonical_inputs.pending[0]
            .accepted_input_ref()
            .unwrap(),
        calm_types::conversation::AcceptedInputRef::Human {
            message: calm_types::conversation::MessageRevisionRef {
                message_id: calm_types::conversation::ConversationMessageId::parse(
                    receipt["entryId"].as_str().unwrap()
                )
                .unwrap(),
                revision: std::num::NonZeroU32::new(1).unwrap(),
            }
        }
    );
    let binding: (String, String, String) =
        sqlx::query_as("SELECT provider,mode,contract FROM worker_sessions WHERE id=?1")
            .bind(&worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        binding,
        ("grok".into(), "resumable".into(), "assistant".into())
    );
    assert_eq!(prompts(&fixture, &AgentProvider::Grok), 0);
    drop(guard);
    let done = idle(&fixture, &card, 1).await;
    assert!(done.canonical_inputs.pending.is_empty());
    assert_eq!(carrier(&fixture, &card).await.0, worker);
    assert_eq!(facts(&fixture, &card).await, (1, 1, 1, 1, 1, 1));
    let sent: Vec<_> = calls(&fixture)
        .into_iter()
        .filter(|c| c["method"] == "session/prompt")
        .collect();
    assert_eq!(sent.len(), 1);
    let text = sent[0]["params"]["prompt"].to_string();
    assert_eq!(text.matches(FIRST_BODY).count(), 1);
    if side {
        assert!(text.find("Frozen Grok excerpt").unwrap() < text.find(FIRST_BODY).unwrap());
    }
    let replay = no_wake(&app, &cookie, &uri, "atomic", body.clone(), &card).await;
    assert_eq!(replay["initialInput"], created["initialInput"]);
    assert_eq!(
        initial(&app, &cookie, &track, &card, "atomic").await["receipt"],
        *receipt
    );
    for (field, value) in [
        ("text", json!("changed")),
        ("provider", json!("opencode")),
        ("model", json!("fixture/model-b")),
        (
            "side",
            json!({"source_card_id":source(&fixture,&track).await,"context":"changed"}),
        ),
    ] {
        let mut changed = body.clone();
        changed[field] = value;
        assert_eq!(
            status(&app, &cookie, &uri, "atomic", changed).await,
            StatusCode::CONFLICT
        );
    }
    assert_eq!(
        carrier(&fixture, &card).await.1.canonical_inputs,
        done.canonical_inputs
    );
    assert_eq!(facts(&fixture, &card).await, (1, 1, 1, 1, 1, 1));
    assert_eq!(prompts(&fixture, &AgentProvider::Grok), 1);
    fixture.shutdown_process(&state).await;
}

#[tokio::test]
async fn g3_grok_assistant_compaction_contract() {
    let mut fixture = Fixture::new().await;
    configure(&mut fixture);
    let (state, app, cookie) = fixture.application().await;
    let planner_body = body(&app, &cookie, &AgentProvider::Grok).await;
    let created_track = post(&app, &cookie, "/api/tracks", "compact-track", planner_body).await;
    let track = created_track["id"].as_str().unwrap();
    let planner = source(&fixture, track).await;
    idle(&fixture, &planner, 1).await;
    let planner_run =
        super::super::presentation_read_feedback_tests::read_run(&app, &cookie, &planner).await;
    assert_eq!(planner_run["manual_compact_supported"], true);
    let created = post(
        &app,
        &cookie,
        &format!("/api/tracks/{track}/conversations"),
        "compact-assistant",
        json!({"provider":"grok","text":FIRST_BODY}),
    )
    .await;
    let card = created["id"].as_str().unwrap();
    idle(&fixture, card, 1).await;
    let worker = carrier(&fixture, card).await.0;
    let native: String =
        sqlx::query_scalar("SELECT agent_session_id FROM worker_sessions WHERE id=?1")
            .bind(&worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    let run = super::super::presentation_read_feedback_tests::read_run(&app, &cookie, card).await;
    // Offer native compact on the next process attachment; refusal must come from the contract.
    std::fs::write(
        peer_root(&fixture, &AgentProvider::Grok).join("scenario"),
        "grok-compact-empty",
    )
    .unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::post(format!("/api/cards/{card}/planner/compact"))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let response_status = response.status();
    let response_body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let native_compacts = calls(&fixture)
        .iter()
        .filter(|c| {
            c["method"] == "_x.ai/compact_conversation"
                || (c["method"] == "session/prompt"
                    && c["params"]["prompt"].to_string().contains("/compact"))
        })
        .count();
    assert_eq!(
        (
            run["manual_compact_supported"].clone(),
            response_status,
            native_compacts
        ),
        (json!(false), StatusCode::CONFLICT, 0),
        "{response_body}"
    );
    let (current, after_frame) = carrier(&fixture, card).await;
    assert_eq!(current, worker);
    assert_eq!(
        after_frame.header.compact_outcome.as_ref().unwrap().status,
        crate::harness::compact_outcome::CompactStatus::Refused
    );
    let after: String =
        sqlx::query_scalar("SELECT agent_session_id FROM worker_sessions WHERE id=?1")
            .bind(&worker)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(after, native);
    fixture.shutdown_process(&state).await;
}

mod boundaries;
mod lifecycle;
