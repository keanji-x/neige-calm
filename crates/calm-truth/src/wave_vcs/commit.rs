use crate::error::{CalmError, Result};
use crate::event::Event;
use crate::ids::{ActorId, WaveId};
use crate::model::now_ms;
use sqlx::{Sqlite, Transaction};

use super::delta::{PathDelta, add_card_event_paths, apply_delta_tx, paths_changed_by_event};
use super::snapshot::{cards_for_wave_tx, snapshot_tree_at_tx};
use super::store::{CommitTreeMeta, commit_tree_at_tx, head_in_tx, store_tree, tree_at_in_tx};
use super::types::{CardVisibility, has_legacy_card_lens_paths};
use super::{CommitHash, TreeSnapshot};

pub async fn commit_tree(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    parent_hash: Option<&str>,
    tree: &TreeSnapshot,
    event_id: Option<i64>,
    message: &str,
    manifest_schema_version: i64,
) -> Result<CommitHash> {
    commit_tree_at_tx(
        tx,
        wave_id,
        tree,
        CommitTreeMeta {
            parent_hash,
            author: None,
            event_id,
            message,
            manifest_schema_version,
            created_at: now_ms(),
        },
    )
    .await
}

pub async fn commit_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    actor: &ActorId,
    event_id: i64,
    event: &Event,
    manifest_schema_version: i64,
) -> Result<Option<CommitHash>> {
    commit_events_in_tx(
        tx,
        wave_id,
        actor,
        event_id,
        std::slice::from_ref(event),
        manifest_schema_version,
    )
    .await
}

pub async fn commit_events_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    actor: &ActorId,
    event_id: i64,
    events: &[Event],
    manifest_schema_version: i64,
) -> Result<Option<CommitHash>> {
    commit_events_with_author_in_tx(
        tx,
        wave_id,
        Some(actor),
        event_id,
        events,
        manifest_schema_version,
    )
    .await
}

pub async fn commit_events_with_author_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    author: Option<&ActorId>,
    event_id: i64,
    events: &[Event],
    manifest_schema_version: i64,
) -> Result<Option<CommitHash>> {
    if events
        .iter()
        .any(|event| matches!(event, Event::WaveDeleted { .. }))
    {
        return Ok(None);
    }
    if events.iter().all(|event| {
        matches!(
            event,
            Event::WorkspaceLeased { .. }
                | Event::WorkspaceReleased { .. }
                | Event::ForgePrMerged { .. }
                | Event::ReviewRound { .. }
                | Event::RatifyRequested { .. }
                | Event::RatifyResolved { .. }
                | Event::ForgeScanCompleted { .. }
                | Event::ForgePrOpened { .. }
                | Event::ForgePrDiffRead { .. }
                | Event::ForgePrChecks { .. }
                | Event::ForgeIssueRead { .. }
                | Event::ForgeIssueClosed { .. }
                | Event::WorktreeProvisioned { .. }
                | Event::WorktreeCommitted { .. }
                | Event::WorktreeRemoved { .. }
        )
    }) {
        return Ok(None);
    }

    let now = now_ms();
    let mut delta = PathDelta::default();
    for event in events {
        delta.merge(paths_changed_by_event(event, wave_id));
    }
    let author = author.map(ToString::to_string);
    commit_delta_in_tx(
        tx,
        wave_id,
        delta,
        CommitTreeMeta {
            parent_hash: None,
            author: author.as_deref(),
            event_id: Some(event_id),
            message: events.last().map(Event::kind_tag).unwrap_or("event"),
            manifest_schema_version,
            created_at: now,
        },
    )
    .await
    .map(Some)
}

pub async fn snapshot_transcripts_for_cards_in_wave(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    event_id: Option<i64>,
    manifest_schema_version: i64,
) -> Result<CommitHash> {
    let card_visibility = match head_in_tx(tx, wave_id).await? {
        Some(parent) => {
            let parent_manifest = tree_at_in_tx(tx, &parent).await?.ok_or_else(|| {
                CalmError::Internal(format!("wave-vcs: missing tree for {parent}"))
            })?;
            CardVisibility::from_manifest(&parent_manifest)
        }
        None => CardVisibility::announced_only(),
    };
    let cards = cards_for_wave_tx(tx, wave_id, &card_visibility).await?;
    let mut delta = PathDelta::default();
    for card in cards {
        add_card_event_paths(&mut delta, &card.card.id);
    }

    let author = ActorId::Kernel.to_string();
    commit_delta_in_tx(
        tx,
        wave_id,
        delta,
        CommitTreeMeta {
            parent_hash: None,
            author: Some(author.as_str()),
            event_id,
            message: "transcript refresh",
            manifest_schema_version,
            created_at: now_ms(),
        },
    )
    .await
}

async fn commit_delta_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &WaveId,
    delta: PathDelta,
    meta: CommitTreeMeta<'_>,
) -> Result<CommitHash> {
    let parent_hash = head_in_tx(tx, wave_id).await?;
    let tree = if let Some(parent) = parent_hash.as_deref() {
        let mut parent_manifest = tree_at_in_tx(tx, parent)
            .await?
            .ok_or_else(|| CalmError::Internal(format!("wave-vcs: missing tree for {parent}")))?;
        let card_visibility = CardVisibility::from_manifest(&parent_manifest);
        if delta.full_snapshot || has_legacy_card_lens_paths(&parent_manifest) {
            snapshot_tree_at_tx(
                tx,
                wave_id,
                meta.manifest_schema_version,
                meta.created_at,
                &None,
                &card_visibility,
            )
            .await?
        } else {
            apply_delta_tx(
                tx,
                wave_id,
                &mut parent_manifest,
                delta,
                &card_visibility,
                meta.created_at,
            )
            .await?;
            store_tree(
                tx,
                meta.manifest_schema_version,
                parent_manifest.entries,
                meta.created_at,
            )
            .await?
        }
    } else {
        snapshot_tree_at_tx(
            tx,
            wave_id,
            meta.manifest_schema_version,
            meta.created_at,
            &None,
            &CardVisibility::announced_only(),
        )
        .await?
    };

    commit_tree_at_tx(
        tx,
        wave_id,
        &tree,
        CommitTreeMeta {
            parent_hash: parent_hash.as_deref(),
            ..meta
        },
    )
    .await
}
