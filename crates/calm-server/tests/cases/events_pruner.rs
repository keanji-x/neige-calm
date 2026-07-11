//! Integration tests for the events retention pruner (#854 slice 2).
//!
//! The must-have regression is the card-status-dot incident pinned as a
//! test: pruning `overlay.set` history whose LATEST write per overlay quad
//! is OLDER than the retention horizon must leave the last-writer-wins
//! layout fold (`derive_layout_positions` / `fold_layout_positions`)
//! byte-identical, and must never touch structural events.
//!
//! Also pins the test-suite seeding invariant the pruner's age-based
//! predicate relies on (design §5): no suite outside this file seeds an
//! ALLOWLISTED kind into the `events` table with a literal (non-`now_ms`)
//! `at`. Background pruners are only spawned by `AppState::new` (main.rs)
//! — no test fixture path spawns them — but the scan keeps the invariant
//! from rotting silently if that ever changes.

use std::sync::Arc;

use calm_server::db::sqlite::{SqlxRepo, card_create_tx, cove_create_tx, wave_create_tx};
use calm_server::db::{RepoEventWrite, write_with_event_typed};
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::events_prune::{EVENTS_PRUNE_KINDS, EventsRetentionPolicy, prune_events_once};
use calm_server::ids::ActorId;
use calm_server::model::{NewCard, NewCove, NewWave};
use calm_server::replay::derive_layout_positions;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis() as i64
}

fn old(days: i64) -> i64 {
    now_ms() - days * DAY_MS
}

async fn insert_event(pool: &sqlx::SqlitePool, kind: &str, payload: &str, at: i64) -> i64 {
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

fn layout_overlay_payload(wave_id: &str, positions: serde_json::Value) -> String {
    serde_json::json!({
        "id": format!("kernel:view:{wave_id}:layout"),
        "plugin_id": "kernel",
        "entity_kind": "view",
        "entity_id": wave_id,
        "kind": "layout",
        "payload": {"schemaVersion": 1, "positions": positions},
        "updated_at": 0
    })
    .to_string()
}

fn status_overlay_payload(card_id: &str, status: &str) -> String {
    serde_json::json!({
        "id": format!("p1:card:{card_id}:status"),
        "plugin_id": "p1",
        "entity_kind": "card",
        "entity_id": card_id,
        "kind": "status",
        "payload": {"status": status},
        "updated_at": 0
    })
    .to_string()
}

async fn count_where(pool: &sqlx::SqlitePool, predicate: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT COUNT(*) FROM events WHERE {predicate}"))
        .fetch_one(pool)
        .await
        .expect("count events")
}

async fn event_exists(pool: &sqlx::SqlitePool, id: i64) -> bool {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE id = ?1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("count by id");
    n == 1
}

/// Seed a real cove + wave + card through the production write path
/// (structural events stamped `at = now_ms()`), returning the wave id.
async fn seed_wave_with_card(repo: &Arc<SqlxRepo>, bus: &EventBus) -> String {
    let write = calm_server::state::WriteContext::new(
        calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::wave_cove_cache::WaveCoveCache::new(),
    );
    let (cove, _) = write_with_event_typed(
        repo.as_ref(),
        ActorId::User,
        EventScope::System,
        None,
        bus,
        &write,
        move |tx| {
            Box::pin(async move {
                let cove = cove_create_tx(
                    tx,
                    NewCove {
                        name: "c".into(),
                        color: "#000".into(),
                        sort: None,
                    },
                )
                .await?;
                Ok((cove.clone(), Event::CoveUpdated(cove)))
            })
        },
    )
    .await
    .expect("create cove");

    let cove_id = cove.id.clone();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    let (wave, _) = write_with_event_typed(
        repo.as_ref(),
        ActorId::User,
        EventScope::System,
        None,
        bus,
        &write,
        move |tx| {
            Box::pin(async move {
                let wave = wave_create_tx(
                    tx,
                    NewWave {
                        workflow_input: None,
                        cove_id,
                        title: "w".into(),
                        sort: None,
                        cwd: String::new(),
                        workflow_id: None,
                        attach_folder: false,
                        theme: calm_server::routes::theme::RequestTheme::default_dark(),
                    },
                    &wave_cove_cache,
                )
                .await?;
                Ok((
                    wave.clone(),
                    Event::WaveUpdated(calm_server::event::WaveUpdatedPayload::new(wave, None)),
                ))
            })
        },
    )
    .await
    .expect("create wave");

    let wave_id = wave.id.clone();
    let card_role_cache = calm_server::card_role_cache::CardRoleCache::new();
    write_with_event_typed(
        repo.as_ref(),
        ActorId::User,
        EventScope::System,
        None,
        bus,
        &write,
        move |tx| {
            Box::pin(async move {
                let card = card_create_tx(
                    tx,
                    NewCard {
                        wave_id,
                        kind: "terminal".into(),
                        sort: None,
                        payload: serde_json::json!({}),
                    },
                    &card_role_cache,
                )
                .await?;
                Ok((card.clone(), Event::CardAdded(card)))
            })
        },
    )
    .await
    .expect("create card");

    wave.id.as_str().to_string()
}

// ---------------------------------------------------------------------------
// Must-have regression (design §7): overlay.set history whose latest write
// per quad is older than the horizon survives pruning; the layout fold is
// byte-identical pre/post; structural events are untouched.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn prune_preserves_layout_fold_and_structural_events() {
    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory repo"),
    );
    let bus = EventBus::new();
    let wave_id = seed_wave_with_card(&repo, &bus).await;
    let pool = repo.pool();

    // Layout quad: superseded old write, then the LATEST write per quad
    // — itself OLDER than the 30-day horizon. The carve-out must keep it.
    let p1 = serde_json::json!({"card-a": {"x": 0, "y": 0, "w": 6, "h": 12}});
    let p2 = serde_json::json!({
        "card-a": {"x": 0, "y": 0, "w": 6, "h": 12},
        "card-b": {"x": 6, "y": 0, "w": 6, "h": 12}
    });
    let layout_old = insert_event(
        pool,
        "overlay.set",
        &layout_overlay_payload(&wave_id, p1),
        old(90),
    )
    .await;
    // Structural event interleaved between the overlay writes, older than
    // the horizon — must survive (not allowlisted).
    let structural_old = insert_event(
        pool,
        "cove.updated",
        r##"{"id":"c-old","name":"n","color":"#000","sort":0,"created_at":0,"updated_at":0}"##,
        old(60),
    )
    .await;
    let layout_latest = insert_event(
        pool,
        "overlay.set",
        &layout_overlay_payload(&wave_id, p2.clone()),
        old(45),
    )
    .await;

    // Card-status quad: old superseded duplicate + newer-than-horizon
    // latest. The old duplicate goes; the newer one stays.
    let status_old = insert_event(
        pool,
        "overlay.set",
        &status_overlay_payload("card-a", "running"),
        old(50),
    )
    .await;
    let status_new = insert_event(
        pool,
        "overlay.set",
        &status_overlay_payload("card-a", "done"),
        old(2),
    )
    .await;

    // Transient kinds past the horizon: pruned.
    let hook_old = insert_event(
        pool,
        "claude.hook",
        r#"{"card_id":"card-a","kind":"stop","payload":{}}"#,
        old(40),
    )
    .await;
    let codex_hook_old = insert_event(
        pool,
        "codex.hook",
        r#"{"card_id":"card-a","kind":"stop","payload":{}}"#,
        old(40),
    )
    .await;
    let phase_old = insert_event(pool, "harness.phase.changed", "{}", old(40)).await;
    let item_old = insert_event(pool, "harness.item.added", "{}", old(40)).await;
    // Transient kind inside the horizon: kept.
    let hook_new = insert_event(
        pool,
        "claude.hook",
        r#"{"card_id":"card-a","kind":"stop","payload":{}}"#,
        old(1),
    )
    .await;
    // Tombstone for an unrelated quad, older than the horizon: never pruned.
    let tombstone_old = insert_event(
        pool,
        "overlay.deleted",
        r#"{"plugin_id":"p1","entity_kind":"card","entity_id":"card-z","kind":"status"}"#,
        old(70),
    )
    .await;

    let structural_predicate = format!(
        "kind NOT IN ({})",
        EVENTS_PRUNE_KINDS
            .iter()
            .map(|k| format!("'{k}'"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let fold_before = derive_layout_positions(&repo, &wave_id)
        .await
        .expect("fold before prune");
    assert_eq!(
        fold_before.as_ref(),
        p2.as_object(),
        "pre-prune fold must resolve to the latest layout write"
    );
    let structural_before = count_where(pool, &structural_predicate).await;

    let pruned = prune_events_once(pool, &EventsRetentionPolicy::default())
        .await
        .expect("prune pass");

    let fold_after = derive_layout_positions(&repo, &wave_id)
        .await
        .expect("fold after prune");
    assert_eq!(
        serde_json::to_string(&fold_before).unwrap(),
        serde_json::to_string(&fold_after).unwrap(),
        "layout fold must be byte-identical across pruning"
    );
    let structural_after = count_where(pool, &structural_predicate).await;
    assert_eq!(
        structural_before, structural_after,
        "structural event count must be unchanged"
    );

    // layout_old, status_old, hook_old, codex_hook_old, phase_old,
    // item_old pruned.
    assert_eq!(pruned, 6);
    for (id, expect) in [
        (layout_old, false),
        (structural_old, true),
        (layout_latest, true),
        (status_old, false),
        (status_new, true),
        (hook_old, false),
        (codex_hook_old, false),
        (phase_old, false),
        (item_old, false),
        (hook_new, true),
        (tombstone_old, true),
    ] {
        assert_eq!(
            event_exists(pool, id).await,
            expect,
            "unexpected survival state for event {id}"
        );
    }

    // The durable retention watermark advanced to the highest pruned id,
    // so the WS replay guard can detect the interior holes this pass
    // punched (`MIN(id)` cannot — the structural head survives).
    assert_eq!(
        RepoEventWrite::events_prune_watermark(repo.as_ref())
            .await
            .expect("watermark"),
        item_old,
        "watermark = MAX(pruned id)"
    );
}

// ---------------------------------------------------------------------------
// Seeding invariant scan (design §5 / review fix 2): no test suite outside
// this file seeds an allowlisted kind into `events` with a literal INSERT.
// Literal INSERTs are exactly the seeding style that carries a non-`now_ms`
// `at` (e.g. `at = 0` in sync_engine.rs); the production write path always
// stamps `now_ms()`, which an age-horizon pruner can never touch. Best
// effort by construction: kinds bound as parameters are invisible to the
// scan, but parameterized seeding in-tree goes through the repo write path.
// ---------------------------------------------------------------------------
#[test]
fn no_other_suite_seeds_allowlisted_kinds_with_literal_inserts() {
    let tests_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut offenders = Vec::new();
    let mut stack = vec![tests_dir];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read tests dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs")
                || path.file_name().and_then(|n| n.to_str()) == Some("events_pruner.rs")
            {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("read test source");
            let mut from = 0;
            while let Some(pos) = source[from..].find("INSERT INTO events") {
                let start = from + pos;
                let window = &source[start..(start + 600).min(source.len())];
                for kind in EVENTS_PRUNE_KINDS {
                    if window.contains(&format!("'{kind}'")) {
                        offenders.push(format!("{}: literal seed of `{kind}`", path.display()));
                    }
                }
                from = start + "INSERT INTO events".len();
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "allowlisted (prunable) kinds must never be seeded with literal `INSERT INTO events` \
         (they usually carry a fake old `at`, which the retention pruner would delete); seed \
         through the repo write path (stamps now_ms) or extend the pruner's own tests instead:\n{}",
        offenders.join("\n")
    );
}
