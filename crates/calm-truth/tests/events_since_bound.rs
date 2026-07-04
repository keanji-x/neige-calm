//! #854 slice 1 — `events_since` must not permit an unbounded read.
//!
//! The events table grows for the lifetime of the deployment (214k rows /
//! 1.7 GB observed in prod), so every reader has to state its bound. These
//! tests pin the repo-layer contract: the returned window is the first
//! `limit` rows after `since_id` in id order, and no call shape can express
//! sqlite's `LIMIT -1` "no limit" sentinel.

use calm_truth::card_role_cache::CardRoleCache;
use calm_truth::db::RepoEventWrite;
use calm_truth::db::sqlite::SqlxRepo;
use calm_truth::event::{Event, EventBus, EventScope};
use calm_truth::ids::ActorId;
use calm_truth::model::{Cove, CoveKind};
use calm_truth::wave_cove_cache::WaveCoveCache;

async fn seed_cove_updates(repo: &SqlxRepo, n: usize) -> Vec<i64> {
    let bus = EventBus::new();
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        let id = repo
            .log_pure_event(
                ActorId::User,
                EventScope::System,
                None,
                &bus,
                &CardRoleCache::new(),
                &WaveCoveCache::new(),
                Event::CoveUpdated(Cove {
                    id: format!("c-{i}").into(),
                    name: "n".into(),
                    color: "#000".into(),
                    sort: 0.0,
                    kind: CoveKind::User,
                    created_at: 0,
                    updated_at: 0,
                }),
            )
            .await
            .expect("seed event");
        ids.push(id);
    }
    ids
}

#[tokio::test]
async fn events_since_enforces_caller_bound() {
    let repo = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open sqlite repo");
    let seeded = seed_cove_updates(&repo, 8).await;

    let rows = repo.events_since(0, 5).await.expect("events_since");
    assert_eq!(
        rows.len(),
        5,
        "events_since must enforce the caller-supplied bound"
    );
    let got: Vec<i64> = rows.iter().map(|(id, ..)| *id).collect();
    assert_eq!(
        got,
        seeded[..5],
        "window is the first `limit` rows in id order"
    );

    // Resume from the window's tail: pagination covers the rest.
    let rest = repo
        .events_since(seeded[4], 5)
        .await
        .expect("events_since tail");
    let got: Vec<i64> = rest.iter().map(|(id, ..)| *id).collect();
    assert_eq!(got, seeded[5..], "next page resumes past the bound");
}

#[tokio::test]
async fn events_since_non_positive_limit_returns_no_rows() {
    let repo = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open sqlite repo");
    seed_cove_updates(&repo, 3).await;

    // Negative values must clamp to empty, never fall through to sqlite's
    // `LIMIT -1` "no limit" sentinel.
    for limit in [0, -1, -100] {
        let rows = repo.events_since(0, limit).await.expect("events_since");
        assert!(rows.is_empty(), "limit {limit} must return no rows");
    }
}

/// PR #867 review — the WS replay cap decision runs on
/// `events_raw_count_since`, which must count RAW rows (including ones
/// `events_since` drops at deserialization time) and stay bounded by the
/// probe limit.
#[tokio::test]
async fn events_raw_count_since_counts_raw_rows_and_respects_probe_limit() {
    let repo = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open sqlite repo");
    let seeded = seed_cove_updates(&repo, 4).await;
    // A raw row whose kind matches no `Event` variant: invisible to
    // `events_since`, but the raw count must include it.
    sqlx::query(
        r#"INSERT INTO events (kind, payload, actor, at, event_version)
           VALUES ('test.unknown_kind', '{}', 'user', 0, 1)"#,
    )
    .execute(repo.pool())
    .await
    .expect("insert unknown-kind row");

    // 5 raw rows total; events_since only surfaces the 4 good ones.
    let filtered = repo.events_since(0, 100).await.expect("events_since");
    assert_eq!(
        filtered.len(),
        4,
        "unknown-kind row is filtered from events_since"
    );
    assert_eq!(
        repo.events_raw_count_since(0, 100)
            .await
            .expect("raw count"),
        5,
        "raw count must include rows events_since drops"
    );

    // The probe is bounded by `probe_limit`, never a full count.
    assert_eq!(
        repo.events_raw_count_since(0, 3).await.expect("raw count"),
        3
    );
    // `since_id` offsets the window like events_since does.
    assert_eq!(
        repo.events_raw_count_since(seeded[1], 100)
            .await
            .expect("raw count"),
        3,
        "two good rows + the unknown-kind row remain past seeded[1]"
    );
    // Non-positive probe limits clamp to zero (no `LIMIT -1` sentinel).
    for limit in [0, -1, -100] {
        assert_eq!(
            repo.events_raw_count_since(0, limit)
                .await
                .expect("raw count"),
            0,
            "probe limit {limit} must count zero rows"
        );
    }
}
