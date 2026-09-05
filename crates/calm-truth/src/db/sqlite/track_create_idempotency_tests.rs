//! #1384 — the `Idempotency-Key` → track binding, at the layer where its two
//! load-bearing properties actually live: the transaction it commits in, and
//! the primary key that makes it exclusive.
//!
//! Route-level behaviour (which arm a request takes, what it answers) is
//! pinned in `calm-server/tests/cases/track_create_first_message.rs`. These two
//! cases are the substrate that behaviour rests on, and neither is observable
//! from up there: a route test cannot roll a transaction back at the seam, and
//! it cannot construct the cross-process race the primary key exists for
//! (`conversation_first_message_locks` serializes both requests before either
//! reaches the INSERT — see the design's KNOWN GAP 3).

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

/// #1426 — the same claim for a create that carried no `first_message`. The
/// `None` is the whole difference and it is what selects fingerprint version 2.
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

/// T-BIND-1 / design §4.3 FP3 — the binding and the id are the SAME commit, so
/// an in-transaction failure leaves neither.
///
/// This is the entire reason the table exists rather than the `operations` row
/// being made to work harder: `insert_operation` runs on a pooled connection
/// after `adapter.validate`, so between the track's commit and the operation's
/// there is an interval where the track exists and nothing remembers who owns
/// it. Variant 4 is that interval widened by a daemon outage.
///
/// Mutation that must redden it: write the binding on a second connection (the
/// pool) instead of on `tx`. The rollback then takes the track and leaves the
/// binding behind, and the `None` assertion below fires.
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
    // Whatever fails after the mint — the folder claim, an unknown recipe, a
    // report projection — takes the whole transaction with it.
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

/// T-BIND-2 — the primary key is the wall, not a check somebody remembers to
/// write.
///
/// The in-process claim (`conversation_first_message_locks`) serializes two
/// same-key creates inside one server, so the second one takes the `Resume`
/// arm and never reaches this INSERT. That map is in-process only, which is
/// what makes this constraint the thing that actually holds on a second
/// instance — and the only place it can be exercised is here, because the
/// route cannot get past its own lock to construct the race.
///
/// Mutation that must redden it: widen the primary key to
/// `(area_id, idempotency_key, track_id)`. The DDL stays valid under
/// `WITHOUT ROWID`, so exactly this test goes red rather than every test that
/// boots a database.
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

    // And the surviving binding is the first claimant's, not the loser's.
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
    // The same key under a DIFFERENT area is a different binding: the area is
    // in the primary key, which is what makes `area_id` need no separate check
    // in the payload digest.
    assert_eq!(
        track_create_idempotency_get_pool(repo.pool(), area.id.as_str(), "one-key")
            .await
            .expect("read back"),
        None,
    );
}

/// T-BIND-3 (#1426) — a message-less claim round-trips as its **own** variant,
/// and the CHECK constraint admits it.
///
/// Both halves matter. If the read side folded version 2 into `V1` with an
/// empty digest, the route could no longer tell a message-less binding from a
/// message-carrying one, and `ensure_binding_create_matches`'s shape check —
/// the thing that stops a `first_message` create from replaying a message-less
/// binding — would compare a fabricated value. If the migration's CHECK did not
/// admit version 2, the INSERT below would fail inside the mint transaction and
/// take the track with it.
///
/// Mutation that must redden it: make `track_create_idempotency_claim_tx` write
/// version 1 unconditionally. The INSERT then violates the CHECK (version 1
/// requires a message digest) and the `expect` below fires.
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

    // And the primary key does not care which shape wrote the row: one
    // (area, key) still names one track, so a `first_message` create cannot
    // claim a key a message-less create already bound.
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
