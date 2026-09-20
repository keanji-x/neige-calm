//! The `Idempotency-Key` → track binding: the transaction it commits in and
//! the primary key that makes it exclusive.

use super::{
    SqlxRepo, TrackCreateBinding, TrackCreateBindingClaim, TrackCreateRequestFingerprint,
    area_create_tx, track_create_idempotency_claim_tx, track_create_idempotency_get_pool,
    track_create_tx,
};
use crate::model::{NewArea, NewTrack, RequestTheme};

fn new_track(area_id: &crate::ids::AreaId, title: &str) -> NewTrack {
    NewTrack {
        area_id: area_id.clone(),
        title: title.into(),
        sort: None,
        cwd: "/tmp".into(),
        template_id: None,
        plugin_scope: None,
        template_input: None,
        attach_folder: false,
        theme: RequestTheme::default_dark(),
    }
}

fn claim(track_id: impl Into<String>, planner: &str, report: &str) -> TrackCreateBindingClaim {
    TrackCreateBindingClaim {
        track_id: track_id.into(),
        planner_card_id: planner.into(),
        report_card_id: report.into(),
        create_request_sha256: "a".repeat(64),
        first_message_sha256: Some("b".repeat(64)),
    }
}

/// The `None` is what selects fingerprint version 2.
fn message_less_claim(
    track_id: impl Into<String>,
    planner: &str,
    report: &str,
) -> TrackCreateBindingClaim {
    TrackCreateBindingClaim {
        first_message_sha256: None,
        ..claim(track_id, planner, report)
    }
}

fn stored_binding(track_id: impl Into<String>, planner: &str, report: &str) -> TrackCreateBinding {
    TrackCreateBinding {
        track_id: track_id.into(),
        planner_card_id: planner.into(),
        report_card_id: report.into(),
        request_fingerprint: TrackCreateRequestFingerprint::V1 {
            create_request_sha256: "a".repeat(64),
            first_message_sha256: "b".repeat(64),
        },
    }
}

#[tokio::test]
async fn the_binding_and_the_track_commit_together() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open repo");
    let mut tx = repo.pool().begin().await.expect("begin tx");
    let area = area_create_tx(
        &mut tx,
        NewArea {
            name: "binding commits with the mint".into(),
            color: "#202020".into(),
            sort: None,
        },
    )
    .await
    .expect("create area");
    tx.commit().await.expect("commit the area");

    let mut tx = repo.pool().begin().await.expect("begin the create tx");
    let track = track_create_tx(
        &mut tx,
        new_track(&area.id, "rolled back"),
        None,
        &super::TrackWorkspacePlan::AttachedFromCwd,
        None,
        repo.track_area_cache(),
    )
    .await
    .expect("mint the track");
    track_create_idempotency_claim_tx(
        &mut tx,
        area.id.as_str(),
        "key-rolled-back",
        &claim(track.id.to_string(), "planner-1", "report-1"),
    )
    .await
    .expect("claim the key");
    tx.rollback().await.expect("roll the create back");

    let surviving: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tracks")
        .fetch_one(repo.pool())
        .await
        .expect("count tracks");
    assert_eq!(surviving, 0, "premise: the rollback took the track");
    assert_eq!(
        track_create_idempotency_get_pool(repo.pool(), area.id.as_str(), "key-rolled-back")
            .await
            .expect("read the binding back"),
        None,
        "the binding must not survive a transaction its track did not — a row pointing at a \
         track that was rolled back poisons that key forever"
    );
}

/// The in-process lock is per server, so the primary key is what actually
/// holds on a second instance.
#[tokio::test]
async fn the_database_refuses_two_tracks_under_one_area_and_key() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open repo");
    let mut tx = repo.pool().begin().await.expect("begin tx");
    let area = area_create_tx(
        &mut tx,
        NewArea {
            name: "one key one track".into(),
            color: "#202020".into(),
            sort: None,
        },
    )
    .await
    .expect("create area");
    let first = track_create_tx(
        &mut tx,
        new_track(&area.id, "first"),
        None,
        &super::TrackWorkspacePlan::AttachedFromCwd,
        None,
        repo.track_area_cache(),
    )
    .await
    .expect("mint the first track");
    let second = track_create_tx(
        &mut tx,
        new_track(&area.id, "second"),
        None,
        &super::TrackWorkspacePlan::AttachedFromCwd,
        None,
        repo.track_area_cache(),
    )
    .await
    .expect("mint the second track");
    assert_ne!(first.id, second.id, "premise: two distinct minted ids");

    track_create_idempotency_claim_tx(
        &mut tx,
        area.id.as_str(),
        "one-key",
        &claim(first.id.to_string(), "planner-1", "report-1"),
    )
    .await
    .expect("the first claim wins");
    let refused = track_create_idempotency_claim_tx(
        &mut tx,
        area.id.as_str(),
        "one-key",
        &claim(second.id.to_string(), "planner-2", "report-2"),
    )
    .await;
    let error = refused.expect_err(
        "a second track under one (area, Idempotency-Key) must be refused by the database",
    );
    let message = error.to_string();
    assert!(
        message.contains("UNIQUE constraint failed"),
        "the refusal must come from the primary key, not from something incidental: {message}"
    );

    drop(tx);
    let mut tx = repo.pool().begin().await.expect("begin a clean tx");
    let area2 = area_create_tx(
        &mut tx,
        NewArea {
            name: "one key one track, committed".into(),
            color: "#202020".into(),
            sort: None,
        },
    )
    .await
    .expect("create area");
    let winner = track_create_tx(
        &mut tx,
        new_track(&area2.id, "winner"),
        None,
        &super::TrackWorkspacePlan::AttachedFromCwd,
        None,
        repo.track_area_cache(),
    )
    .await
    .expect("mint");
    track_create_idempotency_claim_tx(
        &mut tx,
        area2.id.as_str(),
        "one-key",
        &claim(winner.id.to_string(), "planner-w", "report-w"),
    )
    .await
    .expect("claim");
    tx.commit().await.expect("commit");
    assert_eq!(
        track_create_idempotency_get_pool(repo.pool(), area2.id.as_str(), "one-key")
            .await
            .expect("read back"),
        Some(stored_binding(
            winner.id.to_string(),
            "planner-w",
            "report-w"
        )),
        "the read side must return the three ids the mint wrote"
    );
    // The area is in the primary key, so `area_id` needs no check in the payload digest.
    assert_eq!(
        track_create_idempotency_get_pool(repo.pool(), area.id.as_str(), "one-key")
            .await
            .expect("read back"),
        None,
    );
}

#[tokio::test]
async fn a_message_less_claim_round_trips_as_its_own_fingerprint_variant() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open repo");
    let mut tx = repo.pool().begin().await.expect("begin tx");
    let area = area_create_tx(
        &mut tx,
        NewArea {
            name: "message-less binding".into(),
            color: "#202020".into(),
            sort: None,
        },
    )
    .await
    .expect("create area");
    let track = track_create_tx(
        &mut tx,
        new_track(&area.id, "message-less"),
        None,
        &super::TrackWorkspacePlan::AttachedFromCwd,
        None,
        repo.track_area_cache(),
    )
    .await
    .expect("mint the track");
    track_create_idempotency_claim_tx(
        &mut tx,
        area.id.as_str(),
        "key-message-less",
        &message_less_claim(track.id.to_string(), "planner-m", "report-m"),
    )
    .await
    .expect("a claim with no message digest must be accepted by the CHECK constraint");
    tx.commit().await.expect("commit");

    assert_eq!(
        track_create_idempotency_get_pool(repo.pool(), area.id.as_str(), "key-message-less")
            .await
            .expect("read back"),
        Some(TrackCreateBinding {
            track_id: track.id.to_string(),
            planner_card_id: "planner-m".into(),
            report_card_id: "report-m".into(),
            request_fingerprint: TrackCreateRequestFingerprint::V2MessageLess {
                create_request_sha256: "a".repeat(64),
            },
        }),
        "a message-less binding must read back as V2MessageLess, never as a V1 with a \
         fabricated message digest — the route's create-shape check compares exactly this"
    );

    let mut tx = repo.pool().begin().await.expect("begin tx");
    let second = track_create_tx(
        &mut tx,
        new_track(&area.id, "second"),
        None,
        &super::TrackWorkspacePlan::AttachedFromCwd,
        None,
        repo.track_area_cache(),
    )
    .await
    .expect("mint");
    let refused = track_create_idempotency_claim_tx(
        &mut tx,
        area.id.as_str(),
        "key-message-less",
        &claim(second.id.to_string(), "planner-2", "report-2"),
    )
    .await;
    assert!(
        refused
            .expect_err("one (area, key) names one track, whatever shape claimed it")
            .to_string()
            .contains("UNIQUE constraint failed"),
    );
}
