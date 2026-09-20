//! Migration 0014 backfill smoke tests. The SQL is replayed manually because sqlx runs each migration
//! once per DB, so it cannot be re-run against tracks minted after `SqlxRepo::open()`.

#![cfg(unix)]

use std::sync::Arc;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::model::{NewArea, NewTrack};
use calm_server::track_report::TrackReportPayload;
use serde_json::Value;
use sqlx::SqlitePool;

/// The verbatim SQL from `migrations/0014_wave_report_card.sql`, replayed against rows minted after the initial migration sweep.
const MIGRATION_0014_SQL: &str =
    include_str!("../../../calm-truth/migrations/0014_wave_report_card.sql");

/// Apply the migration's statements one at a time (sqlx `query()` accepts one statement per call), after
/// stripping comments so the `;` split cannot land inside a `-- ...` line.
async fn replay_migration(pool: &SqlitePool) {
    let stripped: String = MIGRATION_0014_SQL
        .lines()
        .map(|l| {
            // The migration's strings contain no `--`, so a naive `find("--")` is safe.
            match l.find("--") {
                Some(idx) => &l[..idx],
                None => l,
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    // 0014 is applied history and still spells `cards.wave_id` and the `'wave-report'` card kind, which no
    // longer exist; map them onto the current schema, and require each replacement to fire so a stale mapping panics.
    let stripped = {
        let mut sql = stripped;
        for (old, new) in [
            ("wave_id", "track_id"),
            ("'wave-report'", "'track-report'"),
            ("waves", "tracks"),
        ] {
            assert!(
                sql.contains(old),
                "migration 0014 no longer contains `{old}`; this HEAD-schema \
                 mapping is stale — re-derive it from migrations 0080/0081"
            );
            sql = sql.replace(old, new);
        }
        sql
    };
    for raw in stripped.split(';') {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        sqlx::query(trimmed)
            .execute(pool)
            .await
            .unwrap_or_else(|e| panic!("replay failed on stmt:\n{trimmed}\nerror: {e}"));
    }
}

async fn fresh_repo() -> (Arc<dyn Repo>, SqlitePool) {
    let url = "sqlite::memory:";
    let repo = SqlxRepo::open(url).await.expect("open");
    let pool = repo.pool().clone();
    (Arc::new(repo), pool)
}

#[tokio::test]
async fn fresh_db_migration_is_no_op_when_no_tracks() {
    let (repo, _pool) = fresh_repo().await;
    let tracks = repo
        .tracks_by_area("nonexistent")
        .await
        .expect("tracks_by_area works post-migration");
    assert!(tracks.is_empty(), "no tracks means no report cards");
}

#[tokio::test]
async fn backfill_mints_report_card_per_track() {
    let (repo, pool) = fresh_repo().await;
    // Mint directly via the repo (bypassing the HTTP route, so no report card is auto-minted): the pre-0014 shape.
    let area = repo
        .area_create(NewArea {
            name: "c".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "legacy".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let cards = repo.cards_by_track(track.id.as_str()).await.unwrap();
    assert_eq!(cards.len(), 0);

    replay_migration(&pool).await;

    let cards = repo.cards_by_track(track.id.as_str()).await.unwrap();
    assert_eq!(cards.len(), 1, "exactly one report card backfilled");
    let report = &cards[0];
    assert_eq!(report.kind, "track-report");
    assert!(!report.deletable, "kernel-owned: deletable=false");
    assert!(report.sort < 0.0, "sort < 0, got {}", report.sort);
    // The body is the literal English seed the migration writes, intentionally diverged from `TrackReportPayload::initial()`.
    let payload: TrackReportPayload = serde_json::from_value(report.payload.clone())
        .expect("payload is a valid TrackReportPayload");
    // The migration stays frozen at the v1 shape; v1 rows are upgraded at their next persist.
    assert_eq!(payload.schema_version, 1);
    assert_eq!(payload.summary, "");
    assert_eq!(
        payload.body, "# Goal\n\n_The spec agent will fill this in._\n",
        "migration 0014 backfills the English seed verbatim; the Rust-side initial() \
         seed was changed to Chinese in 5f3278e6 but the SQL migration stayed frozen"
    );
}

#[tokio::test]
async fn backfill_skips_tracks_that_already_have_a_report_card() {
    let (repo, pool) = fresh_repo().await;
    let area = repo
        .area_create(NewArea {
            name: "c".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "already migrated".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    replay_migration(&pool).await;
    let after_first = repo.cards_by_track(track.id.as_str()).await.unwrap();
    assert_eq!(after_first.len(), 1);
    let first_report_id = after_first[0].id.clone();

    replay_migration(&pool).await;
    let after_second = repo.cards_by_track(track.id.as_str()).await.unwrap();
    assert_eq!(after_second.len(), 1, "no duplicate mint");
    assert_eq!(
        after_second[0].id, first_report_id,
        "same report card row — re-run was a no-op"
    );
}

#[tokio::test]
async fn backfill_seeds_layout_overlay_when_absent() {
    let (repo, pool) = fresh_repo().await;
    let area = repo
        .area_create(NewArea {
            name: "c".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "no-layout-yet".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    replay_migration(&pool).await;

    // This track has no planner card, so the planner position is absent from the seed.
    let overlays = repo.overlays_for("view", track.id.as_str()).await.unwrap();
    let layout = overlays
        .iter()
        .find(|o| o.kind == "layout")
        .expect("layout overlay seeded");
    let positions = layout
        .payload
        .get("positions")
        .and_then(Value::as_object)
        .expect("payload.positions is an object");

    let cards = repo.cards_by_track(track.id.as_str()).await.unwrap();
    let report_id = cards[0].id.as_str();
    let pos = positions
        .get(report_id)
        .and_then(Value::as_object)
        .expect("report card has a position entry");
    assert_eq!(pos.get("x").and_then(Value::as_i64), Some(6));
    assert_eq!(pos.get("y").and_then(Value::as_i64), Some(0));
    assert_eq!(pos.get("w").and_then(Value::as_i64), Some(6));
    assert_eq!(pos.get("h").and_then(Value::as_i64), Some(12));
}

#[tokio::test]
async fn backfill_patches_existing_layout_overlay() {
    let (repo, pool) = fresh_repo().await;
    let area = repo
        .area_create(NewArea {
            name: "c".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "already-has-layout".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    repo.overlay_upsert(calm_server::model::NewOverlay {
        plugin_id: "kernel".into(),
        entity_kind: "view".into(),
        entity_id: track.id.as_str().to_string(),
        kind: "layout".into(),
        payload: serde_json::json!({
            "schemaVersion": 1,
            "positions": {
                "existing-card-id": { "x": 0, "y": 4, "w": 6, "h": 3 }
            }
        }),
    })
    .await
    .unwrap();

    replay_migration(&pool).await;

    let overlays = repo.overlays_for("view", track.id.as_str()).await.unwrap();
    let layout = overlays
        .iter()
        .find(|o| o.kind == "layout")
        .expect("layout overlay present");
    let positions = layout
        .payload
        .get("positions")
        .and_then(Value::as_object)
        .expect("payload.positions is an object");
    assert!(
        positions.contains_key("existing-card-id"),
        "pre-existing position survives the patch: {positions:?}"
    );
    let cards = repo.cards_by_track(track.id.as_str()).await.unwrap();
    let report_id = cards[0].id.as_str();
    assert!(
        positions.contains_key(report_id),
        "report card position added: {positions:?}"
    );

    let layout_id_before = layout.id.clone();
    replay_migration(&pool).await;
    let overlays_after = repo.overlays_for("view", track.id.as_str()).await.unwrap();
    let layout_after = overlays_after.iter().find(|o| o.kind == "layout").unwrap();
    assert_eq!(
        layout_after.id, layout_id_before,
        "same overlay row — second pass is a no-op"
    );
}
