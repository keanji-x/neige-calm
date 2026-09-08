//! Resolve ordinary receipt details through the same virtual reader as track.cat.
//! No new queue fields, task-key aliases, workspace paths, or authority claims.
use super::{Observation, QueueEntry};
use crate::db::Repo;
use crate::error::Result;
use crate::ids::TrackId;
use crate::model::HarnessInputSegment;
use crate::state::WriteContext;
use calm_truth::track_fs_view::TrackFsView;
use serde_json::Value;

pub(super) async fn enrich(
    repo: &dyn Repo,
    write: &WriteContext,
    track_id: &TrackId,
    entries: &[QueueEntry],
    segments: &mut [HarnessInputSegment],
) -> Result<()> {
    let receipts: Vec<_> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| match entry.observation() {
            Observation::TaskCompleted {
                idempotency_key,
                result,
            } => Some((
                index,
                entry.envelope_id(),
                idempotency_key,
                "completed",
                "result",
                result,
            )),
            Observation::TaskFailed {
                idempotency_key,
                error,
            } => Some((
                index,
                entry.envelope_id(),
                idempotency_key,
                "failed",
                "reason",
                Value::String(error),
            )),
            _ => None,
        })
        .collect();
    if receipts.is_empty() {
        return Ok(());
    }
    let track = repo.track_get(track_id.as_str()).await?;
    let view = TrackFsView::new(repo, write);
    let listing = match &track {
        Some(track) => match view.ls(track, Some("runs")).await {
            Ok(listing) => listing,
            Err(error) => {
                // Details are optional: unrelated corrupt card/run projections
                // must not strand the already-persisted result in this batch.
                tracing::warn!(?error, "result receipt detail listing unavailable");
                Vec::new()
            }
        },
        None => Vec::new(),
    };
    for (index, envelope_id, identity, kind, field, report) in receipts {
        // Accept only a single bounded virtual filename from the real listing.
        // Never derive a location from arbitrary task keys or report contents.
        let entry = listing.iter().find(|entry| {
            entry.extra.get("idempotency_key").and_then(Value::as_str) == Some(&identity)
                && entry.name.ends_with(".json")
                && entry.name.len() <= 512
                && entry
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
        });
        let mut detail = None;
        if let (Some(track), Some(entry)) = (&track, entry) {
            let path = format!("runs/{}", entry.name);
            let content = match view.cat(track, &path).await {
                Ok(content) => Some(content),
                Err(error) => {
                    tracing::warn!(?error, "result receipt detail read unavailable");
                    None
                }
            };
            let run: Value = match content {
                Some(content) => serde_json::from_str(&content.content)?,
                None => Value::Null,
            };
            let event = &run["events"][kind];
            // A run projection can advance. Only advertise it when it still
            // contains the queued event, or (legacy queues lack envelope IDs)
            // the exact recorded payload and identity. Never substitute latest.
            if run["idempotency_key"].as_str() == Some(&identity)
                && event["payload"]["idempotency_key"].as_str() == Some(&identity)
                && event["payload"].get(field) == Some(&report)
                && envelope_id.is_none_or(|id| event["event_id"].as_i64() == Some(id))
                && let Some(event_id) = event["event_id"].as_i64()
            {
                detail = Some((path, event_id));
            }
        }
        segments[index].text.push_str(&match detail {
            Some((path, event_id)) => format!(
                "\nRecorded execution details: calm.track.cat({}). This virtual JSON record contains the recorded events and any artifact claims; it is not a worker report file or independent verification. Read events.{kind} and require event_id={event_id}; do not substitute another event or attempt.",
                serde_json::json!({"path": path}),
            ),
            None => "\nExact execution details unavailable through the current track reader. No worker report file is asserted to exist; retain the original queued receipt.".into(),
        });
    }
    Ok(())
}
