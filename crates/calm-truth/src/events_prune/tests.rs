use super::*;
use crate::db::RepoEventWrite;
use crate::db::sqlite::SqlxRepo;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

async fn repo() -> SqlxRepo {
    SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open in-memory repo")
}

pub(super) async fn insert_event(pool: &SqlitePool, kind: &str, payload: &str, at: i64) -> i64 {
    sqlx::query_scalar(
        r#"INSERT INTO events (kind, payload, actor, at, correlation)
           VALUES (?1, ?2, 'user', ?3, NULL)
           RETURNING id"#,
    )
    .bind(kind)
    .bind(payload)
    .bind(at)
    .fetch_one(pool)
    .await
    .expect("insert event")
}

pub(super) fn overlay_payload(
    plugin_id: &str,
    entity_kind: &str,
    entity_id: &str,
    kind: &str,
) -> String {
    overlay_payload_with_inner(
        plugin_id,
        entity_kind,
        entity_id,
        kind,
        serde_json::json!({"positions": {"c1": {"x": 0, "y": 0, "w": 6, "h": 12}}}),
    )
}

fn overlay_payload_with_inner(
    plugin_id: &str,
    entity_kind: &str,
    entity_id: &str,
    kind: &str,
    inner: serde_json::Value,
) -> String {
    serde_json::json!({
        "id": format!("{plugin_id}:{entity_kind}:{entity_id}:{kind}"),
        "plugin_id": plugin_id,
        "entity_kind": entity_kind,
        "entity_id": entity_id,
        "kind": kind,
        "payload": inner,
        "updated_at": 0
    })
    .to_string()
}

async fn remaining_ids(pool: &SqlitePool) -> Vec<i64> {
    sqlx::query_scalar("SELECT id FROM events ORDER BY id")
        .fetch_all(pool)
        .await
        .expect("select ids")
}

pub(super) fn old(days: i64) -> i64 {
    now_ms() - days * DAY_MS
}

#[tokio::test]
async fn keeps_exactly_max_id_per_overlay_quad() {
    let repo = repo().await;
    let pool = repo.pool();
    let quad_a = overlay_payload("kernel", "view", "w1", "layout");
    let quad_b = overlay_payload("p1", "card", "c1", "status");
    let _a1 = insert_event(pool, "overlay.set", &quad_a, old(90)).await;
    let _a2 = insert_event(pool, "overlay.set", &quad_a, old(80)).await;
    let _b1 = insert_event(pool, "overlay.set", &quad_b, old(70)).await;
    let a3 = insert_event(pool, "overlay.set", &quad_a, old(60)).await;
    let b2 = insert_event(pool, "overlay.set", &quad_b, old(50)).await;

    let pruned = prune_events_once(pool, &EventsRetentionPolicy::default())
        .await
        .expect("prune");

    assert_eq!(pruned, 3);
    assert_eq!(remaining_ids(pool).await, vec![a3, b2]);
}

#[tokio::test]
async fn keeps_rows_newer_than_horizon_even_when_superseded() {
    let repo = repo().await;
    let pool = repo.pool();
    let quad = overlay_payload("kernel", "view", "w1", "layout");
    let _old_dup = insert_event(pool, "overlay.set", &quad, old(60)).await;
    let new_dup = insert_event(pool, "overlay.set", &quad, old(1)).await;
    let new_latest = insert_event(pool, "overlay.set", &quad, now_ms()).await;
    let new_hook = insert_event(pool, "claude.hook", "{}", old(2)).await;

    let pruned = prune_events_once(pool, &EventsRetentionPolicy::default())
        .await
        .expect("prune");

    assert_eq!(pruned, 1);
    assert_eq!(
        remaining_ids(pool).await,
        vec![new_dup, new_latest, new_hook]
    );
}

#[tokio::test]
async fn deletes_old_transient_kinds_past_horizon() {
    let repo = repo().await;
    let pool = repo.pool();
    insert_event(pool, "claude.hook", "{}", old(31)).await;
    insert_event(pool, "codex.hook", "{}", old(31)).await;
    insert_event(pool, "harness.phase.changed", "{}", old(45)).await;
    insert_event(pool, "harness.item.added", "{}", old(45)).await;
    let recent_claude = insert_event(pool, "claude.hook", "{}", now_ms()).await;
    let recent_codex = insert_event(pool, "codex.hook", "{}", now_ms()).await;

    let pruned = prune_events_once(pool, &EventsRetentionPolicy::default())
        .await
        .expect("prune");

    assert_eq!(pruned, 4);
    assert_eq!(remaining_ids(pool).await, vec![recent_claude, recent_codex]);
}

#[tokio::test]
async fn never_touches_non_allowlist_kinds_or_overlay_deleted() {
    let repo = repo().await;
    let pool = repo.pool();
    let structural = insert_event(pool, "area.updated", "{}", 0).await;
    let card = insert_event(pool, "card.added", "{}", old(400)).await;
    let tombstone = insert_event(
        pool,
        "overlay.deleted",
        r#"{"plugin_id":"kernel","entity_kind":"view","entity_id":"w1","kind":"layout"}"#,
        old(400),
    )
    .await;

    let pruned = prune_events_once(pool, &EventsRetentionPolicy::default())
        .await
        .expect("prune");

    assert_eq!(pruned, 0);
    assert_eq!(remaining_ids(pool).await, vec![structural, card, tombstone]);
}

#[tokio::test]
async fn prunes_across_multiple_batches() {
    let repo = repo().await;
    let pool = repo.pool();
    for _ in 0..7 {
        insert_event(pool, "claude.hook", "{}", old(31)).await;
    }
    let policy = EventsRetentionPolicy {
        batch: 3,
        ..EventsRetentionPolicy::default()
    };

    let pruned = prune_events_once(pool, &policy).await.expect("prune");

    assert_eq!(pruned, 7);
    assert_eq!(remaining_ids(pool).await, Vec::<i64>::new());
}

#[tokio::test]
async fn zero_batch_still_makes_progress() {
    let repo = repo().await;
    let pool = repo.pool();
    insert_event(pool, "claude.hook", "{}", old(31)).await;
    insert_event(pool, "claude.hook", "{}", old(31)).await;
    let policy = EventsRetentionPolicy {
        batch: 0,
        ..EventsRetentionPolicy::default()
    };

    let pruned = prune_events_once(pool, &policy).await.expect("prune");

    assert_eq!(pruned, 2);
}

#[tokio::test]
async fn advances_durable_watermark_to_max_pruned_id() {
    let repo = repo().await;
    let pool = repo.pool();
    assert_eq!(repo.events_prune_watermark().await.expect("watermark"), 0);

    insert_event(pool, "claude.hook", "{}", old(31)).await;
    let hook2 = insert_event(pool, "claude.hook", "{}", old(31)).await;
    let structural = insert_event(pool, "area.updated", "{}", old(31)).await;

    prune_events_once(pool, &EventsRetentionPolicy::default())
        .await
        .expect("prune");
    assert_eq!(
        repo.events_prune_watermark().await.expect("watermark"),
        hook2,
        "watermark is the highest id ever pruned"
    );

    prune_events_once(pool, &EventsRetentionPolicy::default())
        .await
        .expect("second prune");
    assert_eq!(
        repo.events_prune_watermark().await.expect("watermark"),
        hook2
    );
    assert_eq!(remaining_ids(pool).await, vec![structural]);
}

#[tokio::test]
async fn freezes_quad_whose_latest_row_is_version_unsupported() {
    let repo = repo().await;
    let pool = repo.pool();
    let supported = insert_event(
        pool,
        "overlay.set",
        &overlay_payload_with_inner(
            "kernel",
            "view",
            "w1",
            "layout",
            serde_json::json!({"schemaVersion": 1, "positions": {}}),
        ),
        old(90),
    )
    .await;
    let future = insert_event(
        pool,
        "overlay.set",
        &overlay_payload_with_inner(
            "kernel",
            "view",
            "w1",
            "layout",
            serde_json::json!({"schemaVersion": 99, "positions": {}}),
        ),
        old(60),
    )
    .await;
    let quad_b = overlay_payload("p1", "card", "c1", "status");
    let _b1 = insert_event(pool, "overlay.set", &quad_b, old(80)).await;
    let b2 = insert_event(pool, "overlay.set", &quad_b, old(50)).await;

    let pruned = prune_events_once(pool, &EventsRetentionPolicy::default())
        .await
        .expect("prune");

    assert_eq!(pruned, 1, "only the control quad's superseded row goes");
    assert_eq!(remaining_ids(pool).await, vec![supported, future, b2]);
}

#[tokio::test]
async fn freeze_set_is_recomputed_at_delete_time_not_cached() {
    let repo = repo().await;
    let pool = repo.pool();
    let supported = insert_event(
        pool,
        "overlay.set",
        &overlay_payload_with_inner(
            "kernel",
            "view",
            "w1",
            "layout",
            serde_json::json!({"schemaVersion": 1, "positions": {}}),
        ),
        old(90),
    )
    .await;
    assert_eq!(
        prune_events_once(pool, &EventsRetentionPolicy::default())
            .await
            .expect("prune"),
        0
    );

    // The freeze is decided inside the delete tx, never carried over from an
    // earlier scan, so the NEXT batch must keep BOTH rows.
    let future = insert_event(
        pool,
        "overlay.set",
        &overlay_payload_with_inner(
            "kernel",
            "view",
            "w1",
            "layout",
            serde_json::json!({"schemaVersion": 99, "positions": {}}),
        ),
        old(60),
    )
    .await;
    assert_eq!(
        prune_events_once(pool, &EventsRetentionPolicy::default())
            .await
            .expect("prune"),
        0
    );
    assert_eq!(remaining_ids(pool).await, vec![supported, future]);

    let healed = insert_event(
        pool,
        "overlay.set",
        &overlay_payload_with_inner(
            "kernel",
            "view",
            "w1",
            "layout",
            serde_json::json!({"schemaVersion": 1, "positions": {"c": {"x": 0, "y": 0, "w": 1, "h": 1}}}),
        ),
        old(40),
    )
    .await;
    assert_eq!(
        prune_events_once(pool, &EventsRetentionPolicy::default())
            .await
            .expect("prune"),
        2
    );
    assert_eq!(remaining_ids(pool).await, vec![healed]);
}

#[tokio::test]
async fn malformed_overlay_payload_never_poisons_the_batch() {
    let repo = repo().await;
    let pool = repo.pool();
    // Malformed JSON: ungated, `json_extract` would RAISE and disable overlay pruning.
    let malformed = insert_event(pool, "overlay.set", "{not json", old(90)).await;
    let quad = overlay_payload("p1", "card", "c1", "status");
    let _superseded = insert_event(pool, "overlay.set", &quad, old(80)).await;
    let latest = insert_event(pool, "overlay.set", &quad, old(50)).await;
    let _hook = insert_event(pool, "claude.hook", "{}", old(40)).await;

    let pruned = prune_events_once(pool, &EventsRetentionPolicy::default())
        .await
        .expect("prune must not error on a malformed overlay payload");

    assert_eq!(pruned, 2);
    assert_eq!(remaining_ids(pool).await, vec![malformed, latest]);

    let frozen_latest = insert_event(
        pool,
        "overlay.set",
        &overlay_payload_with_inner(
            "kernel",
            "view",
            "w1",
            "layout",
            serde_json::json!({"schemaVersion": 99, "positions": {}}),
        ),
        old(30),
    )
    .await;
    let pruned = prune_events_once(pool, &EventsRetentionPolicy::default())
        .await
        .expect("second prune with frozen quads must not error either");
    assert_eq!(pruned, 0);
    assert_eq!(
        remaining_ids(pool).await,
        vec![malformed, latest, frozen_latest]
    );
}

#[test]
fn events_pruner_config_from_env_respects_disable_floor_and_defaults() {
    let saved_interval = std::env::var(EVENTS_PRUNE_INTERVAL_SECS_ENV).ok();
    let saved_retention = std::env::var(EVENTS_RETENTION_SECS_ENV).ok();
    let saved_batch = std::env::var(EVENTS_PRUNE_BATCH_ENV).ok();
    fn set(key: &str, value: &str) {
        // SAFETY: this test owns the events-pruner env vars it mutates.
        unsafe { std::env::set_var(key, value) };
    }
    fn remove(key: &str) {
        // SAFETY: see `set`.
        unsafe { std::env::remove_var(key) };
    }

    remove(EVENTS_PRUNE_INTERVAL_SECS_ENV);
    remove(EVENTS_RETENTION_SECS_ENV);
    remove(EVENTS_PRUNE_BATCH_ENV);
    assert_eq!(
        events_pruner_config_from_env(),
        Some((EVENTS_PRUNE_INTERVAL, EventsRetentionPolicy::default()))
    );

    set(EVENTS_PRUNE_INTERVAL_SECS_ENV, "0");
    assert_eq!(events_pruner_config_from_env(), None);

    set(EVENTS_PRUNE_INTERVAL_SECS_ENV, "off");
    assert_eq!(
        events_pruner_config_from_env(),
        Some((EVENTS_PRUNE_INTERVAL, EventsRetentionPolicy::default()))
    );

    set(EVENTS_PRUNE_INTERVAL_SECS_ENV, "17");
    set(EVENTS_RETENTION_SECS_ENV, "86400");
    set(EVENTS_PRUNE_BATCH_ENV, "23");
    assert_eq!(
        events_pruner_config_from_env(),
        Some((
            Duration::from_secs(17),
            EventsRetentionPolicy {
                horizon: Duration::from_secs(86400),
                batch: 23,
                ..EventsRetentionPolicy::default()
            }
        ))
    );

    set(EVENTS_RETENTION_SECS_ENV, "1");
    assert_eq!(
        events_pruner_config_from_env(),
        Some((
            Duration::from_secs(17),
            EventsRetentionPolicy {
                horizon: MIN_EVENTS_RETENTION,
                batch: 23,
                ..EventsRetentionPolicy::default()
            }
        ))
    );

    set(EVENTS_RETENTION_SECS_ENV, "0");
    set(EVENTS_PRUNE_BATCH_ENV, "0");
    assert_eq!(
        events_pruner_config_from_env(),
        Some((Duration::from_secs(17), EventsRetentionPolicy::default()))
    );
    set(EVENTS_RETENTION_SECS_ENV, "never");
    set(EVENTS_PRUNE_BATCH_ENV, "lots");
    assert_eq!(
        events_pruner_config_from_env(),
        Some((Duration::from_secs(17), EventsRetentionPolicy::default()))
    );

    match saved_interval {
        Some(value) => set(EVENTS_PRUNE_INTERVAL_SECS_ENV, &value),
        None => remove(EVENTS_PRUNE_INTERVAL_SECS_ENV),
    }
    match saved_retention {
        Some(value) => set(EVENTS_RETENTION_SECS_ENV, &value),
        None => remove(EVENTS_RETENTION_SECS_ENV),
    }
    match saved_batch {
        Some(value) => set(EVENTS_PRUNE_BATCH_ENV, &value),
        None => remove(EVENTS_PRUNE_BATCH_ENV),
    }
}

/// `harness.user_message.enqueued` is the Today bootstrap's dedup evidence;
/// pruning it would double-send after the horizon with nothing else going red.
#[tokio::test]
async fn first_message_dedup_kind_is_never_prunable() {
    assert!(
        !EVENTS_PRUNE_KINDS.contains(&"harness.user_message.enqueued"),
        "the Today bootstrap predicate reads this kind as permanent evidence; \
         pruning it re-opens a duplicate bootstrap after the horizon"
    );

    let repo = repo().await;
    let pool = repo.pool();
    let dedup_evidence = insert_event(pool, "harness.user_message.enqueued", "{}", old(400)).await;
    // A sibling allowlisted kind, so the pass provably did work rather than no-opped.
    insert_event(pool, "harness.item.added", "{}", old(400)).await;

    let pruned = prune_events_once(pool, &EventsRetentionPolicy::default())
        .await
        .expect("prune");

    assert_eq!(pruned, 1, "the allowlisted sibling must be pruned");
    assert_eq!(
        remaining_ids(pool).await,
        vec![dedup_evidence],
        "a 400-day-old harness.user_message.enqueued must survive every prune pass"
    );
}

#[test]
fn allowlist_never_contains_structural_or_tombstone_kinds() {
    assert!(!EVENTS_PRUNE_KINDS.contains(&"overlay.deleted"));
    assert!(EVENTS_PRUNE_KINDS.iter().all(|k| !k.starts_with("card.")
        && !k.starts_with("track.")
        && !k.starts_with("area.")
        && !k.starts_with("terminal.")));
}
