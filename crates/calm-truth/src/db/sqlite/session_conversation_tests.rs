//! What a fresh start would lose, on the real schema (#2184).

use super::session_conversation::card_conversation;
use crate::card_role_cache::CardRoleCache;
use crate::db::sqlite::{SqlxRepo, area_create_tx, card_create_tx, track_create_tx};
use crate::model::{NewArea, NewCard, NewTrack, RequestTheme};
use crate::session_projection_repo::CardConversation;
use serde_json::json;

async fn card(repo: &SqlxRepo) -> (String, String) {
    let mut tx = repo.pool().begin().await.unwrap();
    let area = area_create_tx(
        &mut tx,
        NewArea {
            name: "history".into(),
            color: "#101010".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let track = track_create_tx(
        &mut tx,
        NewTrack {
            area_id: area.id,
            title: "history".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        None,
        &crate::db::sqlite::TrackWorkspacePlan::AttachedFromCwd,
        None,
        repo.track_area_cache(),
    )
    .await
    .unwrap();
    let card = card_create_tx(
        &mut tx,
        NewCard {
            track_id: track.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: json!({}),
            title: None,
        },
        &CardRoleCache::new(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (card.id.to_string(), track.id.to_string())
}

async fn conversation(repo: &SqlxRepo, card_id: &str) -> CardConversation {
    card_conversation(repo.pool(), card_id).await.unwrap()
}

#[tokio::test]
async fn a_fresh_start_is_allowed_only_when_no_carrier_holds_a_thread_and_no_transcript_exists() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let (card_id, _) = card(&repo).await;
    let (other_card, _) = card(&repo).await;
    let (transcript_card, transcript_track) = card(&repo).await;
    sqlx::query(
        "INSERT INTO harness_items (worker_session_id, card_id, track_id, thread_id, method, \
           params, created_at_ms) VALUES ('gone', ?1, ?2, 't', 'item/completed', '{}', 0)",
    )
    .bind(&transcript_card)
    .bind(&transcript_track)
    .execute(repo.pool())
    .await
    .unwrap();
    assert_eq!(
        conversation(&repo, &transcript_card).await,
        CardConversation::ThreadToPreserve,
        "a transcript without any row is a conversation"
    );
    assert_eq!(
        conversation(&repo, &card_id).await,
        CardConversation::NoThreadToPreserve
    );

    sqlx::query(
        "INSERT INTO operations (id, operation_key, kind, payload_hash, target_type, \
           target_json, payload_json, phase, created_at_ms, updated_at_ms) \
         VALUES ('op', 'op', 'planner-harness-start', 'h', 'track', '{}', ?1, 'pending', 0, 0)",
    )
    .bind(json!({ "spec_card_id": card_id }).to_string())
    .execute(repo.pool())
    .await
    .unwrap();
    assert_eq!(
        conversation(&repo, &card_id).await,
        CardConversation::StartInFlight
    );
    assert_eq!(
        conversation(&repo, &other_card).await,
        CardConversation::NoThreadToPreserve,
        "another card's start is not this card's"
    );
    for finished in ["succeeded", "failed", "stuck"] {
        sqlx::query("UPDATE operations SET phase = ?1 WHERE id = 'op'")
            .bind(finished)
            .execute(repo.pool())
            .await
            .unwrap();
        assert_eq!(
            conversation(&repo, &card_id).await,
            CardConversation::NoThreadToPreserve,
            "a {finished} start is not in flight"
        );
    }
    sqlx::query("UPDATE operations SET phase = 'pending' WHERE id = 'op'")
        .execute(repo.pool())
        .await
        .unwrap();

    sqlx::query("UPDATE operations SET phase = 'failed' WHERE id = 'op'")
        .execute(repo.pool())
        .await
        .unwrap();
    let mut tx = repo.pool().begin().await.unwrap();
    crate::db::sqlite::session_start_runtime_tx(
        &mut tx,
        crate::session_projection_repo::WorkerSessionInit::shared_planner(
            "row".into(),
            card_id.clone(),
            crate::session_projection_repo::AgentProvider::Codex,
            calm_types::worker::WorkerSessionState::Idle,
            None,
            json!({ "last_thread_id": null }),
            0,
        ),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let shape = |sql: &'static str| {
        let pool = repo.pool().clone();
        async move { sqlx::query(sql).execute(&pool).await.unwrap() }
    };
    assert_eq!(
        conversation(&repo, &card_id).await,
        CardConversation::ThreadToPreserve,
        "a live row is a conversation even before its thread exists"
    );
    // What a failed start's compensation leaves behind.
    shape("UPDATE worker_sessions SET state = 'failed', completed_at_ms = 1 WHERE id = 'row'")
        .await;
    assert_eq!(
        conversation(&repo, &card_id).await,
        CardConversation::NoThreadToPreserve,
        "a failed start that never got a thread preserves nothing"
    );
    for (holding, restore, why) in [
        (
            "UPDATE worker_sessions SET thread_id = 'thread-1' WHERE id = 'row'",
            "UPDATE worker_sessions SET thread_id = ' ' WHERE id = 'row'",
            "a failed row with a thread",
        ),
        (
            "UPDATE worker_sessions SET handle_state_json = '{\"last_thread_id\":\"thread-1\"}' WHERE id = 'row'",
            "UPDATE worker_sessions SET handle_state_json = NULL WHERE id = 'row'",
            "a failed row whose snapshot names a thread",
        ),
        (
            "UPDATE worker_sessions SET handle_state_json = 'not json' WHERE id = 'row'",
            "UPDATE worker_sessions SET handle_state_json = '{}' WHERE id = 'row'",
            "a failed row whose snapshot cannot be read",
        ),
        (
            "UPDATE worker_sessions SET completed_at_ms = NULL WHERE id = 'row'",
            "UPDATE worker_sessions SET completed_at_ms = 1 WHERE id = 'row'",
            "a failed row not completed (recoverable)",
        ),
        (
            "UPDATE worker_sessions SET state = 'exited' WHERE id = 'row'",
            "UPDATE worker_sessions SET state = 'failed' WHERE id = 'row'",
            "a retired row",
        ),
    ] {
        shape(holding).await;
        assert_eq!(
            conversation(&repo, &card_id).await,
            CardConversation::ThreadToPreserve,
            "{why} is a conversation"
        );
        shape(restore).await;
        assert_eq!(
            conversation(&repo, &card_id).await,
            CardConversation::NoThreadToPreserve,
            "restored after: {why}"
        );
    }
}
