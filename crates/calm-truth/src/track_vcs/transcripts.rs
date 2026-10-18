//! Turn-boundary hook preparation is external; only exact witnesses and publication take the writer.
use super::commit::commit_delta_in_tx;
use super::delta::{PathDelta, add_card_event_paths};
use super::snapshot::{
    cards_for_track, cards_for_track_tx, content_markdown, conversation_markdown,
    hook_events_for_card_tx, hook_events_from_rows, hook_events_json, hook_ids_for_card_tx,
    hook_rows_for_card,
};
use super::store::{
    CommitTreeMeta, PreparedBlob, head, head_in_tx, put_rendered_entry, tree_at, tree_at_in_tx,
};
use super::types::CardVisibility;
use super::{CommitHash, ManifestEntry};
use crate::db::sqlite::begin_immediate_tx;
use crate::error::{CalmError, Result};
use crate::ids::{ActorId, CardId, TrackId};
use crate::model::now_ms;
use sqlx::{Sqlite, SqlitePool, Transaction};
use std::collections::{BTreeMap, HashMap, HashSet};

const PREPARATION_ATTEMPTS: usize = 3;

pub(super) struct PreparedTranscript {
    ids: Vec<i64>,
    events: PreparedBlob,
    conversation: PreparedBlob,
}
type PreparedHooks = HashMap<CardId, PreparedTranscript>;

/// Ordinary event commits read live hooks. The refresh must supply every prepared transcript,
/// including no-parent and legacy full snapshots; a missing input never falls back to live work.
#[derive(Clone, Copy)]
pub(super) enum HookInputs<'a> {
    Live,
    Prepared(&'a PreparedHooks),
}
impl HookInputs<'_> {
    pub(super) async fn put_prepared_path(
        self,
        tx: &mut Transaction<'_, Sqlite>,
        entries: &mut BTreeMap<String, ManifestEntry>,
        path: &str,
        created_at: i64,
    ) -> Result<bool> {
        let Self::Prepared(prepared) = self else {
            return Ok(false);
        };
        let parts: Vec<_> = path.split('/').collect();
        let ["cards", card_id, name @ ("events.json" | "conversation.md")] = parts.as_slice()
        else {
            return Ok(false);
        };
        let input = prepared.get(&CardId::from(*card_id)).ok_or_else(|| {
            CalmError::Internal(format!(
                "track-vcs: missing prepared transcript for {card_id}"
            ))
        })?;
        let blob = if *name == "events.json" {
            &input.events
        } else {
            &input.conversation
        };
        blob.put_entry(tx, entries, path.to_string(), created_at)
            .await?;
        Ok(true)
    }

    pub(super) async fn put_card(
        self,
        tx: &mut Transaction<'_, Sqlite>,
        entries: &mut BTreeMap<String, ManifestEntry>,
        track_id: &TrackId,
        card_id: &CardId,
        created_at: i64,
    ) -> Result<()> {
        match self {
            Self::Live => {
                let events = hook_events_for_card_tx(tx, track_id, card_id).await?;
                put_rendered_entry(
                    tx,
                    entries,
                    format!("cards/{card_id}/events.json"),
                    hook_events_json(&events)?,
                    created_at,
                )
                .await?;
                put_rendered_entry(
                    tx,
                    entries,
                    format!("cards/{card_id}/conversation.md"),
                    content_markdown(conversation_markdown(card_id, &events)),
                    created_at,
                )
                .await?;
            }
            Self::Prepared(_) => {
                for name in ["events.json", "conversation.md"] {
                    self.put_prepared_path(
                        tx,
                        entries,
                        &format!("cards/{card_id}/{name}"),
                        created_at,
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }
}

async fn prepare(pool: &SqlitePool, track_id: &TrackId) -> Result<PreparedHooks> {
    let visibility = match head(pool, track_id).await? {
        Some(parent) => {
            let manifest = tree_at(pool, &parent).await?.ok_or_else(|| {
                CalmError::Internal(format!("track-vcs: missing tree for {parent}"))
            })?;
            CardVisibility::from_manifest(&manifest)
        }
        None => CardVisibility::announced_only(),
    };
    let cards = cards_for_track(pool, track_id, &visibility).await?;
    let mut prepared = HashMap::new();
    for card in cards {
        // IDs and raw payloads come from one SELECT, so their witness cannot describe another slice.
        let rows = hook_rows_for_card(pool, track_id, &card.card.id).await?;
        let ids = rows.iter().map(|row| row.0).collect();
        let events = hook_events_from_rows(rows).await?;
        let input = PreparedTranscript {
            ids,
            events: PreparedBlob::new(hook_events_json(&events)?),
            conversation: PreparedBlob::new(content_markdown(conversation_markdown(
                &card.card.id,
                &events,
            ))),
        };
        prepared.insert(card.card.id, input);
    }
    Ok(prepared)
}

async fn inputs_are_current(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &TrackId,
    prepared: &PreparedHooks,
) -> Result<bool> {
    // No parent or non-hook projection was staged. Use CURRENT visibility and decode current
    // cards eagerly as the original entrance did; advancing HEAD alone must not starve refresh.
    let visibility = match head_in_tx(tx, track_id).await? {
        Some(parent) => {
            let manifest = tree_at_in_tx(tx, &parent).await?.ok_or_else(|| {
                CalmError::Internal(format!("track-vcs: missing tree for {parent}"))
            })?;
            CardVisibility::from_manifest(&manifest)
        }
        None => CardVisibility::announced_only(),
    };
    let cards = cards_for_track_tx(tx, track_id, &visibility).await?;
    let current: HashSet<_> = cards.iter().map(|card| &card.card.id).collect();
    if current != prepared.keys().collect() {
        return Ok(false);
    }
    for card in cards {
        if hook_ids_for_card_tx(tx, track_id, &card.card.id).await? != prepared[&card.card.id].ids {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Refresh both transcript formats without holding SQLite's writer during hook reads, decoding,
/// rendering or hashing. Exact current selected IDs fence optimistic reads before any inserts.
/// Production event bodies are immutable; replay's destructive reset runs without background tasks.
/// Sustained hook/visibility churn exhausts a finite retry budget and leaves HEAD unchanged.
pub async fn snapshot_transcripts_for_cards_in_track(
    pool: &SqlitePool,
    track_id: &TrackId,
    event_id: Option<i64>,
    manifest_schema_version: i64,
) -> Result<CommitHash> {
    for _ in 0..PREPARATION_ATTEMPTS {
        let prepared = prepare(pool, track_id).await?;
        #[cfg(test)]
        super::transcript_writer_tests::at_phase(
            super::transcript_writer_tests::Phase::BeforeWrite,
        )
        .await;
        let mut tx = begin_immediate_tx(pool).await?;
        if !inputs_are_current(&mut tx, track_id, &prepared).await? {
            tx.rollback().await?;
            continue;
        }
        let mut delta = PathDelta::default();
        for card_id in prepared.keys() {
            add_card_event_paths(&mut delta, card_id);
        }
        let author = ActorId::Kernel.to_string();
        let commit = commit_delta_in_tx(
            &mut tx,
            track_id,
            delta,
            CommitTreeMeta {
                parent_hash: None,
                author: Some(author.as_str()),
                event_id,
                message: "transcript refresh",
                manifest_schema_version,
                created_at: now_ms(),
            },
            HookInputs::Prepared(&prepared),
        )
        .await?;
        tx.commit().await?;
        return Ok(commit);
    }
    Err(CalmError::Internal(
        "track-vcs: transcript inputs changed throughout preparation retries".into(),
    ))
}

#[cfg(test)]
#[path = "transcripts/race_tests.rs"]
mod race_tests;
#[cfg(test)]
#[path = "transcripts/semantics_tests.rs"]
mod semantics_tests;
