use crate::error::{CalmError, Result};
use crate::event::Event;
use crate::ids::{ActorId, TrackId};
use crate::model::now_ms;
use sqlx::{Sqlite, Transaction};

use super::delta::{PathDelta, apply_delta_tx, paths_changed_by_event};
use super::snapshot::project_manifest_at_tx;
use super::store::{CommitTreeMeta, commit_tree_at_tx, head_in_tx, store_tree, tree_at_in_tx};
use super::transcripts::HookInputs;
use super::types::{CardVisibility, has_legacy_card_lens_paths};
use super::{CommitHash, TreeSnapshot};

/// Event projection publishes content; explicit refreshes also mark a history boundary.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CommitIntent {
    Projection,
    Checkpoint,
}

pub async fn commit_tree(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &TrackId,
    parent_hash: Option<&str>,
    tree: &TreeSnapshot,
    event_id: Option<i64>,
    message: &str,
    manifest_schema_version: i64,
) -> Result<CommitHash> {
    commit_tree_at_tx(
        tx,
        track_id,
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
    track_id: &TrackId,
    actor: &ActorId,
    event_id: i64,
    event: &Event,
    manifest_schema_version: i64,
) -> Result<Option<CommitHash>> {
    commit_events_in_tx(
        tx,
        track_id,
        actor,
        event_id,
        std::slice::from_ref(event),
        manifest_schema_version,
    )
    .await
}

pub async fn commit_events_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &TrackId,
    actor: &ActorId,
    event_id: i64,
    events: &[Event],
    manifest_schema_version: i64,
) -> Result<Option<CommitHash>> {
    commit_events_with_author_in_tx(
        tx,
        track_id,
        Some(actor),
        event_id,
        events,
        manifest_schema_version,
    )
    .await
}

/// Return the resulting content identity for projected events, reusing HEAD on equal content.
/// `None` means the batch has no track-file projection (or deletes the track). Event receipts
/// remain separate; a reused HEAD keeps its original event/author and ref update metadata.
pub async fn commit_events_with_author_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &TrackId,
    author: Option<&ActorId>,
    event_id: i64,
    events: &[Event],
    manifest_schema_version: i64,
) -> Result<Option<CommitHash>> {
    if events
        .iter()
        .any(|event| matches!(event, Event::TrackDeleted { .. }))
    {
        return Ok(None);
    }
    let now = now_ms();
    let mut delta = PathDelta::default();
    for event in events {
        delta.merge(paths_changed_by_event(event, track_id));
    }
    // The projection owns which events change track files. Event receipts stay
    // durable even when there is no file delta to snapshot or commit.
    if delta.is_empty() {
        return Ok(None);
    }
    let author = author.map(ToString::to_string);
    commit_delta_in_tx(
        tx,
        track_id,
        delta,
        CommitTreeMeta {
            parent_hash: None,
            author: author.as_deref(),
            event_id: Some(event_id),
            message: events.last().map(Event::kind_tag).unwrap_or("event"),
            manifest_schema_version,
            created_at: now,
        },
        HookInputs::Live,
        CommitIntent::Projection,
    )
    .await
    .map(Some)
}

pub(super) async fn commit_delta_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &TrackId,
    delta: PathDelta,
    meta: CommitTreeMeta<'_>,
    hooks: HookInputs<'_>,
    intent: CommitIntent,
) -> Result<CommitHash> {
    let parent_hash = head_in_tx(tx, track_id).await?;
    let parent_manifest =
        match parent_hash.as_deref() {
            Some(parent) => Some(tree_at_in_tx(tx, parent).await?.ok_or_else(|| {
                CalmError::Internal(format!("track-vcs: missing tree for {parent}"))
            })?),
            None => None,
        };
    let manifest = if let Some(parent_manifest) = &parent_manifest {
        let card_visibility = CardVisibility::from_manifest(parent_manifest);
        if delta.full_snapshot || has_legacy_card_lens_paths(parent_manifest) {
            project_manifest_at_tx(
                tx,
                track_id,
                meta.manifest_schema_version,
                meta.created_at,
                &None,
                &card_visibility,
                hooks,
            )
            .await?
        } else {
            let mut projected = parent_manifest.clone();
            apply_delta_tx(
                tx,
                track_id,
                &mut projected,
                delta,
                &card_visibility,
                meta.created_at,
                hooks,
            )
            .await?;
            projected.schema_version = meta.manifest_schema_version;
            projected
        }
    } else {
        project_manifest_at_tx(
            tx,
            track_id,
            meta.manifest_schema_version,
            meta.created_at,
            &None,
            &CardVisibility::announced_only(),
            hooks,
        )
        .await?
    };

    // Compare the complete projection, never notification cursors or event identities.
    // Rendering above still repairs missing blobs before an existing identity is reused.
    if intent == CommitIntent::Projection && parent_manifest.as_ref() == Some(&manifest) {
        return Ok(parent_hash.expect("an equal parent manifest requires a HEAD"));
    }
    let tree = store_tree(
        tx,
        manifest.schema_version,
        manifest.entries,
        meta.created_at,
    )
    .await?;
    commit_tree_at_tx(
        tx,
        track_id,
        &tree,
        CommitTreeMeta {
            parent_hash: parent_hash.as_deref(),
            ..meta
        },
    )
    .await
}
