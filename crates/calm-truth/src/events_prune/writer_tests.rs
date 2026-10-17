//! Real public retention phases and counters; no alternate retention implementation.
use super::tests::{insert_event, old, overlay_payload};
use super::*;
use crate::db::{RepoEventWrite, sqlite::SqlxRepo};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Phase {
    Frontier,
    Interpret,
    BeforeWrite,
}
struct Pause {
    entered: Notify,
    release: Notify,
    steps: Vec<Phase>,
    next: AtomicUsize,
}
#[derive(Default)]
struct Counts {
    initial: AtomicUsize,
    tail: AtomicUsize,
    writes: AtomicUsize,
}
tokio::task_local! { static DISCOVERY_PAUSE: Arc<Pause>; }
tokio::task_local! { static COUNTS: Arc<Counts>; }
pub(super) fn count_observation(initial: bool) {
    let _ = COUNTS.try_with(|counts| {
        if initial {
            counts.initial.fetch_add(1, Ordering::SeqCst);
        } else {
            counts.tail.fetch_add(1, Ordering::SeqCst);
        }
    });
}
pub(super) async fn at_phase(phase: Phase) {
    if phase == Phase::BeforeWrite {
        let _ = COUNTS.try_with(|counts| counts.writes.fetch_add(1, Ordering::SeqCst));
    }
    if let Ok(pause) = DISCOVERY_PAUSE.try_with(Arc::clone) {
        let next = pause.next.load(Ordering::SeqCst);
        if pause.steps.get(next) == Some(&phase) {
            pause.next.fetch_add(1, Ordering::SeqCst);
            pause.entered.notify_one();
            pause.release.notified().await;
        }
    }
}
pub(super) async fn pause_before_overlay_interpretation() {
    at_phase(Phase::Interpret).await;
}
impl Pause {
    async fn entered(&self) {
        tokio::time::timeout(Duration::from_secs(10), self.entered.notified())
            .await
            .unwrap();
    }
    fn release(&self) {
        self.release.notify_one();
    }
}
fn policy(batch: i64) -> EventsRetentionPolicy {
    EventsRetentionPolicy {
        batch,
        rules: vec![RetentionRule {
            kinds: vec!["overlay.set"],
            keep_latest_per_overlay_key: true,
        }],
        ..EventsRetentionPolicy::default()
    }
}
fn start(
    pool: SqlitePool,
    policy: EventsRetentionPolicy,
    steps: Vec<Phase>,
) -> (Arc<Pause>, tokio::task::JoinHandle<Result<u64>>) {
    let pause = Arc::new(Pause {
        entered: Notify::new(),
        release: Notify::new(),
        steps,
        next: AtomicUsize::new(0),
    });
    let task = tokio::spawn(DISCOVERY_PAUSE.scope(pause.clone(), async move {
        prune_events_once(&pool, &policy).await
    }));
    (pause, task)
}
async fn finish(task: tokio::task::JoinHandle<Result<u64>>) -> u64 {
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}
async fn file_repo() -> (tempfile::TempDir, SqlxRepo) {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("events-writer.db");
    let repo = SqlxRepo::open(&format!("sqlite://{}?mode=rwc", database.display()))
        .await
        .unwrap();
    (directory, repo)
}
async fn ids(pool: &SqlitePool) -> Vec<i64> {
    sqlx::query_scalar("SELECT id FROM events ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap()
}
fn payload(key: &str) -> String {
    overlay_payload("kernel", "view", key, "layout")
}
fn future(key: &str) -> String {
    let mut value: serde_json::Value = serde_json::from_str(&payload(key)).unwrap();
    value["payload"]["schemaVersion"] = serde_json::json!(99);
    value.to_string()
}
async fn pair(pool: &SqlitePool, key: &str) -> (i64, i64) {
    let older = insert_event(pool, "overlay.set", &payload(key), old(60)).await;
    let latest = insert_event(pool, "overlay.set", &payload(key), old(50)).await;
    (older, latest)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overlay_history_discovery_does_not_hold_database_writer_without_candidates() {
    let (_directory, repo) = file_repo().await;
    let retained = insert_event(
        repo.pool(),
        "overlay.set",
        &payload("writer-probe"),
        old(60),
    )
    .await;
    sqlx::query("CREATE TABLE unrelated_event_writer_probe (value INTEGER NOT NULL)")
        .execute(repo.pool())
        .await
        .unwrap();
    let (pause, pruning) = start(repo.pool().clone(), policy(16), vec![Phase::Interpret]);
    pause.entered().await;
    let pool = repo.pool().clone();
    let mut writer = tokio::spawn(async move {
        let mut tx = begin_immediate_tx(&pool).await?;
        sqlx::query("INSERT INTO unrelated_event_writer_probe(value) VALUES (1)")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok::<_, crate::error::CalmError>(())
    });
    let committed = tokio::time::timeout(Duration::from_secs(1), &mut writer).await;
    pause.release();
    let pruned = finish(pruning).await;
    if committed.is_err() {
        tokio::time::timeout(Duration::from_secs(10), writer)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    assert_eq!(pruned, 0);
    assert_eq!(repo.events_prune_watermark().await.unwrap(), 0);
    assert_eq!(ids(repo.pool()).await, vec![retained]);
    assert!(
        matches!(committed, Ok(Ok(Ok(())))),
        "overlay discovery/interpretation retained the writer with no candidates: {committed:?}"
    );
    let writes: i64 = sqlx::query_scalar("SELECT count(*) FROM unrelated_event_writer_probe")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(writes, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overlay_append_after_frontier_capture_is_not_skipped_by_discovery() {
    let (_directory, repo) = file_repo().await;
    let (older, latest) = pair(repo.pool(), "capture-race").await;
    let (pause, pruning) = start(repo.pool().clone(), policy(1), vec![Phase::Frontier]);
    pause.entered().await;
    let future = insert_event(repo.pool(), "overlay.set", &future("capture-race"), old(40)).await;
    pause.release();
    assert_eq!(finish(pruning).await, 0);
    assert_eq!(ids(repo.pool()).await, vec![older, latest, future]);
    assert_eq!(repo.events_prune_watermark().await.unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overlay_future_tail_freezes_quad_without_stopping_other_candidate_progress() {
    let (_directory, repo) = file_repo().await;
    let (a1, a2) = pair(repo.pool(), "freeze-a").await;
    let (b1, b2) = pair(repo.pool(), "control-b").await;
    let (pause, pruning) = start(repo.pool().clone(), policy(1), vec![Phase::BeforeWrite]);
    pause.entered().await;
    let a3 = insert_event(repo.pool(), "overlay.set", &future("freeze-a"), old(40)).await;
    pause.release();
    assert_eq!(
        finish(pruning).await,
        1,
        "a consumed zero-delete batch must not end the whole overlay pass"
    );
    assert_eq!(ids(repo.pool()).await, vec![a1, a2, b2, a3]);
    assert_eq!(repo.events_prune_watermark().await.unwrap(), b1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overlay_missing_latest_witness_defers_after_sibling_public_retention() {
    let (_directory, repo) = file_repo().await;
    let older = insert_event(repo.pool(), "overlay.set", &payload("witness"), old(50)).await;
    let latest = insert_event(repo.pool(), "overlay.set", &payload("witness"), old(60)).await;
    let (pause, pruning) = start(repo.pool().clone(), policy(1), vec![Phase::BeforeWrite]);
    pause.entered().await;
    let sibling = EventsRetentionPolicy {
        horizon: Duration::from_secs(55 * 24 * 60 * 60),
        batch: 1,
        rules: vec![RetentionRule {
            kinds: vec!["overlay.set"],
            keep_latest_per_overlay_key: false,
        }],
    };
    assert_eq!(prune_events_once(repo.pool(), &sibling).await.unwrap(), 1);
    pause.release();
    assert_eq!(finish(pruning).await, 0);
    assert_eq!(ids(repo.pool()).await, vec![older]);
    assert_eq!(repo.events_prune_watermark().await.unwrap(), latest);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overlay_unrelated_append_does_not_invalidate_overlay_frontier() {
    let (_directory, repo) = file_repo().await;
    let (_, latest) = pair(repo.pool(), "unrelated").await;
    let (pause, pruning) = start(repo.pool().clone(), policy(1), vec![Phase::BeforeWrite]);
    pause.entered().await;
    let structural = insert_event(repo.pool(), "track.updated", "{}", old(40)).await;
    pause.release();
    assert_eq!(finish(pruning).await, 1);
    assert_eq!(ids(repo.pool()).await, vec![latest, structural]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overlay_candidate_age_and_kind_are_rechecked_in_delete_transaction() {
    for change in ["age", "kind"] {
        let (_directory, repo) = file_repo().await;
        let (older, latest) = pair(repo.pool(), change).await;
        let (pause, pruning) = start(repo.pool().clone(), policy(1), vec![Phase::BeforeWrite]);
        pause.entered().await;
        if change == "age" {
            sqlx::query("UPDATE events SET at=?1 WHERE id=?2")
                .bind(now_ms())
                .bind(older)
                .execute(repo.pool())
                .await
                .unwrap();
        } else {
            sqlx::query("UPDATE events SET kind='track.updated' WHERE id=?1")
                .bind(older)
                .execute(repo.pool())
                .await
                .unwrap();
        }
        pause.release();
        assert_eq!(finish(pruning).await, 0);
        assert_eq!(ids(repo.pool()).await, vec![older, latest]);
        assert_eq!(repo.events_prune_watermark().await.unwrap(), 0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overlay_repeated_tail_churn_defers_without_spinning_or_deleting() {
    let (_directory, repo) = file_repo().await;
    pair(repo.pool(), "churn").await;
    let (pause, pruning) = start(repo.pool().clone(), policy(1), vec![Phase::BeforeWrite; 3]);
    let mut final_latest = 0;
    for _ in 0..3 {
        pause.entered().await;
        final_latest = insert_event(repo.pool(), "overlay.set", &payload("churn"), old(40)).await;
        pause.release();
    }
    assert_eq!(finish(pruning).await, 0);
    assert_eq!(ids(repo.pool()).await.len(), 5);
    assert_eq!(
        prune_events_once(repo.pool(), &policy(16)).await.unwrap(),
        4
    );
    assert_eq!(ids(repo.pool()).await, vec![final_latest]);
}

#[tokio::test]
async fn overlay_sparse_candidate_pages_share_one_history_discovery_and_actual_batch() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let mut retained = Vec::new();
    let mut old_ids = std::collections::BTreeSet::new();
    for n in 0..1024 {
        let key = format!("sparse-{n}");
        if n % 64 == 0 {
            old_ids.insert(insert_event(repo.pool(), "overlay.set", &payload(&key), old(60)).await);
        }
        retained.push(insert_event(repo.pool(), "overlay.set", &payload(&key), old(50)).await);
    }
    let all = ids(repo.pool()).await;
    let pages: std::collections::BTreeSet<_> = all
        .iter()
        .enumerate()
        .filter(|(_, id)| old_ids.contains(*id))
        .map(|(rank, _)| rank / 256)
        .collect();
    assert!(
        pages.len() >= 3,
        "real candidates must span multiple protected-history pages"
    );
    let counts = Arc::new(Counts::default());
    let deleted = COUNTS
        .scope(counts.clone(), prune_events_once(repo.pool(), &policy(16)))
        .await
        .unwrap();
    assert_eq!(deleted, 16);
    assert_eq!(
        counts.initial.load(Ordering::SeqCst),
        1,
        "history GROUP must not repeat per sparse page/batch"
    );
    assert_eq!(counts.tail.load(Ordering::SeqCst), 0);
    assert_eq!(
        counts.writes.load(Ordering::SeqCst),
        1,
        "one actual candidate batch, not one writer per sparse page"
    );
    assert_eq!(ids(repo.pool()).await, retained);
}

#[tokio::test]
async fn overlay_sql_text_quad_aliases_array_and_literal_string() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let mut unusual: serde_json::Value = serde_json::from_str(&payload("sql-keys")).unwrap();
    unusual["plugin_id"] = serde_json::json!([]);
    let older = insert_event(repo.pool(), "overlay.set", &unusual.to_string(), old(60)).await;
    unusual["plugin_id"] = serde_json::json!("[]");
    let latest = insert_event(repo.pool(), "overlay.set", &unusual.to_string(), old(50)).await;
    assert_eq!(
        prune_events_once(repo.pool(), &policy(16)).await.unwrap(),
        1
    );
    assert_eq!(ids(repo.pool()).await, vec![latest]);
    assert_eq!(repo.events_prune_watermark().await.unwrap(), older);
}

#[tokio::test]
async fn overlay_incompatible_numeric_quad_key_remains_fail_closed() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let mut numeric: serde_json::Value = serde_json::from_str(&payload("numeric")).unwrap();
    numeric["plugin_id"] = serde_json::json!(1);
    let numeric = insert_event(repo.pool(), "overlay.set", &numeric.to_string(), old(60)).await;
    let (older, latest) = pair(repo.pool(), "control").await;
    assert_eq!(
        prune_events_once(repo.pool(), &policy(16)).await.unwrap(),
        0
    );
    assert_eq!(ids(repo.pool()).await, vec![numeric, older, latest]);
}

#[tokio::test]
async fn retention_delete_and_watermark_remain_atomic_when_watermark_write_fails() {
    for kind in ["overlay.set", "claude.hook"] {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        if kind == "overlay.set" {
            pair(repo.pool(), "atomic").await;
        } else {
            insert_event(repo.pool(), kind, "{}", old(60)).await;
        }
        let retained = ids(repo.pool()).await;
        sqlx::query(include_str!("reject_watermark.sql"))
            .execute(repo.pool())
            .await
            .unwrap();
        let policy = EventsRetentionPolicy {
            rules: vec![RetentionRule {
                kinds: vec![kind],
                keep_latest_per_overlay_key: true,
            }],
            ..policy(16)
        };
        assert_eq!(prune_events_once(repo.pool(), &policy).await.unwrap(), 0);
        assert_eq!(ids(repo.pool()).await, retained);
        assert_eq!(repo.events_prune_watermark().await.unwrap(), 0);
    }
}

#[tokio::test]
async fn overlay_null_missing_non_overlay_and_duplicate_fields_freeze_sql_quads() {
    for malformed_shape in ["missing", "null", "duplicate"] {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let mut older: serde_json::Value = serde_json::from_str(&payload("shape")).unwrap();
        let latest = if malformed_shape == "null" {
            older["plugin_id"] = serde_json::Value::Null;
            let mut missing = older.clone();
            missing.as_object_mut().unwrap().remove("plugin_id");
            missing.to_string()
        } else if malformed_shape == "missing" {
            let mut missing = older.clone();
            missing.as_object_mut().unwrap().remove("id");
            missing.to_string()
        } else {
            format!("{{\"plugin_id\":\"kernel\",{}", &older.to_string()[1..])
        };
        let a1 = insert_event(repo.pool(), "overlay.set", &older.to_string(), old(60)).await;
        let a2 = insert_event(repo.pool(), "overlay.set", &latest, old(50)).await;
        let (_, control) = pair(repo.pool(), "shape-control").await;
        assert_eq!(
            prune_events_once(repo.pool(), &policy(16)).await.unwrap(),
            1
        );
        assert_eq!(
            ids(repo.pool()).await,
            vec![a1, a2, control],
            "{malformed_shape}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overlay_malformed_tail_advances_frontier_without_poisoning_current_state() {
    let (_directory, repo) = file_repo().await;
    let (_, latest) = pair(repo.pool(), "malformed-tail").await;
    let (pause, pruning) = start(repo.pool().clone(), policy(1), vec![Phase::BeforeWrite]);
    pause.entered().await;
    let malformed = insert_event(repo.pool(), "overlay.set", "{not json", old(40)).await;
    pause.release();
    assert_eq!(finish(pruning).await, 1);
    assert_eq!(ids(repo.pool()).await, vec![latest, malformed]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overlay_healed_tail_preserves_replay_and_next_pass_reclaims_skipped_history() {
    let (_directory, repo) = file_repo().await;
    let old_a = insert_event(repo.pool(), "overlay.set", &payload("healed"), old(60)).await;
    let unsupported = insert_event(repo.pool(), "overlay.set", &future("healed"), old(50)).await;
    let (_, control) = pair(repo.pool(), "heal-control").await;
    let (pause, pruning) = start(repo.pool().clone(), policy(1), vec![Phase::BeforeWrite]);
    pause.entered().await;
    let healed = insert_event(repo.pool(), "overlay.set", &payload("healed"), old(40)).await;
    pause.release();
    assert_eq!(finish(pruning).await, 1);
    // Frozen rows skipped before the append remain conservative until the next bounded pass.
    assert_eq!(
        ids(repo.pool()).await,
        vec![old_a, unsupported, control, healed]
    );
    assert_eq!(
        prune_events_once(repo.pool(), &policy(16)).await.unwrap(),
        2
    );
    assert_eq!(ids(repo.pool()).await, vec![control, healed]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overlay_concurrent_default_pruner_removing_candidate_is_harmless() {
    let (_directory, repo) = file_repo().await;
    let (older, latest) = pair(repo.pool(), "two-pruners").await;
    let (pause, pruning) = start(repo.pool().clone(), policy(1), vec![Phase::BeforeWrite]);
    pause.entered().await;
    assert_eq!(
        prune_events_once(repo.pool(), &policy(16)).await.unwrap(),
        1
    );
    pause.release();
    assert_eq!(
        finish(pruning).await,
        0,
        "report actual RETURNING rows, not planned candidates"
    );
    assert_eq!(ids(repo.pool()).await, vec![latest]);
    assert_eq!(repo.events_prune_watermark().await.unwrap(), older);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overlay_later_tail_decode_failure_preserves_prior_committed_count_and_watermark() {
    let (_directory, repo) = file_repo().await;
    let (a1, a2) = pair(repo.pool(), "partial-a").await;
    let (b1, b2) = pair(repo.pool(), "partial-b").await;
    let (pause, pruning) = start(repo.pool().clone(), policy(1), vec![Phase::BeforeWrite; 2]);
    pause.entered().await;
    pause.release();
    pause.entered().await;
    let mut invalid: serde_json::Value = serde_json::from_str(&payload("new-numeric")).unwrap();
    invalid["plugin_id"] = serde_json::json!(1);
    let numeric = insert_event(repo.pool(), "overlay.set", &invalid.to_string(), old(40)).await;
    pause.release();
    assert_eq!(finish(pruning).await, 1);
    assert_eq!(ids(repo.pool()).await, vec![a2, b1, b2, numeric]);
    assert_eq!(repo.events_prune_watermark().await.unwrap(), a1);
}
