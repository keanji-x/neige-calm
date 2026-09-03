use crate::db::rows::TRACK_SELECT_COLUMNS;
use crate::error::{CalmError, Result};
use crate::event::Event;
use crate::ids::{CardId, TrackId};
use crate::model::{Card, CardRole, Track, now_ms};
use crate::session_projection_lookup;
use crate::session_projection_row::{
    projectable_runtimes_for_cards_from_rows, projectable_runtimes_for_cards_query,
};
use crate::track_fs_view::{self, HookEventProjection, RunProjection};
use crate::track_report::TrackReportPayload;
use serde::Serialize;
use serde_json::Value;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use std::collections::BTreeMap;

use super::runs::{EventRow, project_runs_tx, track_event_from_row};
use super::store::{CommitTreeMeta, commit_tree_at_tx, put_rendered_entry, store_tree};
use super::types::{BlobContent, CardProjection, CardVisibility};
use super::{MANIFEST_SCHEMA_VERSION, ManifestEntry, TreeSnapshot};

pub async fn backfill_existing_tracks(pool: &SqlitePool) -> Result<usize> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let res = backfill_existing_tracks_tx(&mut tx).await;
    match res {
        Ok(count) => {
            tx.commit().await?;
            Ok(count)
        }
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e)
        }
    }
}

async fn backfill_existing_tracks_tx(tx: &mut Transaction<'_, Sqlite>) -> Result<usize> {
    let tracks: Vec<Track> = sqlx::query_as::<_, crate::db::rows::TrackRow>(&format!(
        "SELECT {TRACK_SELECT_COLUMNS} FROM tracks \
         WHERE id NOT IN (SELECT track_id FROM track_vcs_refs) \
         ORDER BY created_at ASC, id ASC"
    ))
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(Track::from)
    .collect();

    let mut count = 0usize;
    for track in tracks {
        let latest = latest_track_event_tx(tx, &track.id).await?;
        let created_at = latest.map(|(_, at)| at).unwrap_or(track.updated_at);
        let event_id = latest.map(|(id, _)| id);
        let tree = snapshot_tree_at_tx(
            tx,
            &track.id,
            MANIFEST_SCHEMA_VERSION,
            created_at,
            &Some(track.clone()),
            &CardVisibility::AllRows,
        )
        .await?;
        commit_tree_at_tx(
            tx,
            &track.id,
            &tree,
            CommitTreeMeta {
                parent_hash: None,
                author: None,
                event_id,
                message: "backfill root",
                manifest_schema_version: MANIFEST_SCHEMA_VERSION,
                created_at,
            },
        )
        .await?;
        count += 1;
    }

    Ok(count)
}

pub async fn snapshot_tree(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &TrackId,
    schema_version: i64,
) -> Result<TreeSnapshot> {
    snapshot_tree_at_tx(
        tx,
        track_id,
        schema_version,
        now_ms(),
        &None,
        &CardVisibility::announced_only(),
    )
    .await
}

pub(super) async fn snapshot_tree_at_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &TrackId,
    schema_version: i64,
    object_created_at: i64,
    prefetched_track: &Option<Track>,
    card_visibility: &CardVisibility,
) -> Result<TreeSnapshot> {
    let track = match prefetched_track {
        Some(track) => track.clone(),
        None => load_track_tx(tx, track_id).await?,
    };
    let cards = cards_for_track_tx(tx, track_id, card_visibility).await?;
    let mut runtime_projected_cards = cards.clone();
    project_runtime_into_cards_tx(tx, &mut runtime_projected_cards).await?;
    let runs = project_runs_tx(tx, track_id, &cards).await?;
    let mut entries = BTreeMap::new();

    put_rendered_entry(
        tx,
        &mut entries,
        "index.md",
        index_markdown(&track, cards.len()),
        object_created_at,
    )
    .await?;
    put_rendered_entry(
        tx,
        &mut entries,
        "track.json",
        track_json(&track)?,
        object_created_at,
    )
    .await?;
    if let Some(report) = report_markdown(&cards)? {
        put_rendered_entry(tx, &mut entries, "report.md", report, object_created_at).await?;
    }
    put_rendered_entry(
        tx,
        &mut entries,
        "cards/index.json",
        cards_index_json(&cards)?,
        object_created_at,
    )
    .await?;

    for (card, runtime_projected_card) in cards.iter().zip(runtime_projected_cards.iter()) {
        let card_id = card.card.id.as_str();
        put_rendered_entry(
            tx,
            &mut entries,
            format!("cards/{card_id}/.meta.json"),
            card_meta_json(card)?,
            object_created_at,
        )
        .await?;
        put_rendered_entry(
            tx,
            &mut entries,
            format!("cards/{card_id}/.payload.json"),
            card_payload_json(card)?,
            object_created_at,
        )
        .await?;
        put_rendered_entry(
            tx,
            &mut entries,
            format!("cards/{card_id}/runtime.json"),
            card_runtime_json(&runtime_projected_card.card)?,
            object_created_at,
        )
        .await?;
        let hook_events = hook_events_for_card_tx(tx, track_id, &card.card.id).await?;
        put_rendered_entry(
            tx,
            &mut entries,
            format!("cards/{card_id}/events.json"),
            hook_events_json(&hook_events)?,
            object_created_at,
        )
        .await?;
        put_rendered_entry(
            tx,
            &mut entries,
            format!("cards/{card_id}/conversation.md"),
            content_markdown(conversation_markdown(&card.card.id, &hook_events)),
            object_created_at,
        )
        .await?;
    }

    insert_run_entries(tx, &mut entries, &runs, object_created_at).await?;
    store_tree(tx, schema_version, entries, object_created_at).await
}

async fn insert_run_entries(
    tx: &mut Transaction<'_, Sqlite>,
    entries: &mut BTreeMap<String, ManifestEntry>,
    runs: &[RunProjection],
    created_at: i64,
) -> Result<()> {
    put_rendered_entry(
        tx,
        entries,
        "runs/index.json",
        runs_index_json(runs)?,
        created_at,
    )
    .await?;
    for run in runs {
        if track_fs_view::is_reserved_run_key(&run.idempotency_key) {
            // `track_file` errors for reserved run keys because a live read can
            // fail without side effects. Track VCS is in an event write tx, so
            // it skips the pathological key instead of rolling back unrelated
            // events; byte parity with `track_file` is intentionally waived for
            // that invariant violation.
            tracing::error!(
                target: "track_vcs",
                idempotency_key = %run.idempotency_key,
                "runs projection: skipping idempotency_key that collides with reserved path"
            );
            continue;
        }
        put_rendered_entry(
            tx,
            entries,
            format!("runs/{}.json", run.idempotency_key),
            run_json(run)?,
            created_at,
        )
        .await?;
        put_rendered_entry(
            tx,
            entries,
            format!("runs/{}.md", run.idempotency_key),
            content_markdown(run_markdown(run)),
            created_at,
        )
        .await?;
    }
    Ok(())
}

async fn latest_track_event_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &TrackId,
) -> Result<Option<(i64, i64)>> {
    let row: Option<(i64, i64)> =
        sqlx::query_as("SELECT id, at FROM events WHERE scope_track = ?1 ORDER BY id DESC LIMIT 1")
            .bind(track_id.as_str())
            .fetch_optional(&mut **tx)
            .await?;
    Ok(row)
}

async fn load_track_tx(tx: &mut Transaction<'_, Sqlite>, track_id: &TrackId) -> Result<Track> {
    load_track_optional_tx(tx, track_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {}", track_id.as_str())))
}

pub(super) async fn load_track_optional_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &TrackId,
) -> Result<Option<Track>> {
    let row = sqlx::query_as::<_, crate::db::rows::TrackRow>(&format!(
        "SELECT {TRACK_SELECT_COLUMNS} FROM tracks WHERE id = ?1"
    ))
    .bind(track_id.as_str())
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(Track::from))
}

pub(super) async fn cards_for_track_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &TrackId,
    visibility: &CardVisibility,
) -> Result<Vec<CardProjection>> {
    // Keep this ORDER BY aligned with SqlxRepo::cards_by_track in db/sqlite.rs;
    // tests pin the sort ASC, id ASC tie-break for duplicate worker run keys.
    let rows = sqlx::query(
        r#"SELECT id, track_id, kind, sort, payload, title, role, deletable, created_at, updated_at,
                  EXISTS (
                    SELECT 1
                    FROM events
                    WHERE events.scope_track = cards.track_id
                      AND events.kind = 'card.added'
                      AND json_extract(events.payload, '$.id') = cards.id
                  ) AS vcs_announced
           FROM cards
           WHERE track_id = ?1
           ORDER BY sort ASC, id ASC"#,
    )
    .bind(track_id.as_str())
    .fetch_all(&mut **tx)
    .await?;
    let mut out = Vec::new();
    for row in rows {
        let id: String = row.try_get("id")?;
        let announced: i64 = row.try_get("vcs_announced")?;
        if visibility.includes(&id, announced != 0) {
            out.push(card_projection_from_row(row)?);
        }
    }
    Ok(out)
}

pub(super) async fn card_in_track_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &TrackId,
    card_id: &str,
    visibility: &CardVisibility,
) -> Result<Option<CardProjection>> {
    let row = sqlx::query(
        r#"SELECT id, track_id, kind, sort, payload, title, role, deletable, created_at, updated_at,
                  EXISTS (
                    SELECT 1
                    FROM events
                    WHERE events.scope_track = cards.track_id
                      AND events.kind = 'card.added'
                      AND json_extract(events.payload, '$.id') = cards.id
                  ) AS vcs_announced
           FROM cards
           WHERE id = ?1 AND track_id = ?2"#,
    )
    .bind(card_id)
    .bind(track_id.as_str())
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let id: String = row.try_get("id")?;
    let announced: i64 = row.try_get("vcs_announced")?;
    if visibility.includes(&id, announced != 0) {
        card_projection_from_row(row).map(Some)
    } else {
        Ok(None)
    }
}

pub(super) fn card_projection_from_row(row: SqliteRow) -> Result<CardProjection> {
    let payload_text: String = row.try_get("payload")?;
    let payload = serde_json::from_str(&payload_text)?;
    let deletable: i64 = row.try_get("deletable")?;
    Ok(CardProjection {
        card: Card {
            id: CardId::from(row.try_get::<String, _>("id")?),
            track_id: TrackId::from(row.try_get::<String, _>("track_id")?),
            kind: row.try_get("kind")?,
            sort: row.try_get("sort")?,
            payload,
            title: row.try_get("title")?,
            runtime: None,
            deletable: deletable != 0,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        },
        role: row.try_get("role")?,
    })
}

pub(super) async fn hook_events_for_card_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &TrackId,
    card_id: &CardId,
) -> Result<Vec<HookEventProjection>> {
    let rows: Vec<EventRow> = sqlx::query_as(
        r#"SELECT id, kind, payload, actor, at,
                  scope_kind, scope_area, scope_track, scope_card
           FROM (
               SELECT id, kind, payload, actor, at,
                      scope_kind, scope_area, scope_track, scope_card
               FROM events
               WHERE scope_track = ?1
                 AND scope_card = ?2
                 AND kind IN ('codex.hook', 'claude.hook')
               ORDER BY id DESC
               LIMIT ?3
           )
           ORDER BY id ASC"#,
    )
    .bind(track_id.as_str())
    .bind(card_id.as_str())
    .bind(track_fs_view::HOOK_EVENT_TRANSCRIPT_CAP as i64)
    .fetch_all(&mut **tx)
    .await?;

    rows.into_iter()
        .map(track_event_from_row)
        .map(|row| {
            row.and_then(|row| match row.event {
                Event::CodexHook { kind, payload, .. } => Ok(HookEventProjection {
                    event_id: row.id,
                    at: row.at,
                    kind: "codex.hook",
                    hook_kind: kind,
                    payload,
                }),
                Event::ClaudeHook { kind, payload, .. } => Ok(HookEventProjection {
                    event_id: row.id,
                    at: row.at,
                    kind: "claude.hook",
                    hook_kind: kind,
                    payload,
                }),
                _ => Err(CalmError::Internal(
                    "track-vcs: non-hook event returned from hook query".into(),
                )),
            })
        })
        .collect()
}

pub(super) async fn project_runtime_into_cards_tx(
    tx: &mut Transaction<'_, Sqlite>,
    cards: &mut [CardProjection],
) -> Result<()> {
    if cards.is_empty() {
        return Ok(());
    }
    let card_ids = cards
        .iter()
        .map(|card| card.card.id.to_string())
        .collect::<Vec<_>>();
    let mut query = projectable_runtimes_for_cards_query(&card_ids);
    let rows = query.build().fetch_all(&mut **tx).await?;
    let runtimes = projectable_runtimes_for_cards_from_rows(rows)?;
    for card in cards {
        if let Some(runtime) = runtimes.get(card.card.id.as_str()) {
            session_projection_lookup::project_runtime_fields(&mut card.card, runtime);
        }
    }
    Ok(())
}

pub(super) fn index_markdown(track: &Track, card_count: usize) -> BlobContent {
    content_markdown(track_fs_view::index_markdown(track, card_count))
}

pub(super) fn track_json(track: &Track) -> Result<BlobContent> {
    content_json(track)
}

pub(super) fn report_markdown(cards: &[CardProjection]) -> Result<Option<BlobContent>> {
    let Some(report_card) = cards.iter().find(|card| card.card.kind == "track-report") else {
        return Ok(None);
    };
    let payload = serde_json::from_value::<TrackReportPayload>(report_card.card.payload.clone())
        .map_err(|e| {
            CalmError::Internal(format!(
                "track_report: malformed payload on card {}: {e}",
                report_card.card.id.as_str()
            ))
        })?;
    Ok(Some(content_markdown(payload.body)))
}

pub(super) fn cards_index_json(cards: &[CardProjection]) -> Result<BlobContent> {
    let mut values = Vec::with_capacity(cards.len());
    for card in cards {
        values.push(card_meta_value(card)?);
    }
    content_json(&values)
}

pub(super) fn card_meta_json(card: &CardProjection) -> Result<BlobContent> {
    content_json(&card_meta_value(card)?)
}

fn card_meta_value(card: &CardProjection) -> Result<crate::track_fs_dto::TrackFsCardMeta> {
    // Hard-erroring on an unknown role is intentional and unreachable in practice:
    // migration 0037_drop_plain_role.sql backfilled 'plain'→'worker' and added
    // insert/update triggers restricting cards.role to worker|planner|reportcard, so
    // any parse failure here is DB corruption worth failing loudly on.
    let role = serde_json::from_value::<CardRole>(Value::String(card.role.clone()))?;
    Ok(track_fs_view::card_meta_value(&card.card, role))
}

pub(super) fn card_payload_json(card: &CardProjection) -> Result<BlobContent> {
    content_json(&card.card.payload)
}

pub(super) fn card_runtime_json(card: &Card) -> Result<BlobContent> {
    match &card.runtime {
        Some(runtime) => content_json(runtime),
        None => content_json(&Value::Null),
    }
}

pub(super) fn hook_events_json(events: &[HookEventProjection]) -> Result<BlobContent> {
    content_json(&track_fs_view::hook_events_json(events))
}

pub(super) fn conversation_markdown(card_id: &CardId, events: &[HookEventProjection]) -> String {
    track_fs_view::conversation_markdown(card_id, events)
}

pub(super) fn runs_index_json(runs: &[RunProjection]) -> Result<BlobContent> {
    let values = runs
        .iter()
        .map(track_fs_view::run_index_entry)
        .collect::<Vec<_>>();
    content_json(&values)
}

pub(super) fn run_json(run: &RunProjection) -> Result<BlobContent> {
    content_json(&track_fs_view::run_json(run))
}

pub(super) fn run_markdown(run: &RunProjection) -> String {
    track_fs_view::run_markdown(run)
}

pub(super) fn content_markdown(content: String) -> BlobContent {
    blob_from_fs_content(track_fs_view::content_markdown(content))
}

pub(super) fn content_json<T: Serialize>(value: &T) -> Result<BlobContent> {
    Ok(blob_from_fs_content(track_fs_view::content_json(value)?))
}

fn blob_from_fs_content(content: track_fs_view::TrackFsContent) -> BlobContent {
    BlobContent {
        bytes: content.content.into_bytes(),
        content_type: content.content_type,
    }
}
