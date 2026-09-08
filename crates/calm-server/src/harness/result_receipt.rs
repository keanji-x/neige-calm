//! Resolve ordinary receipt details through the same virtual reader as track.cat.
//! No new queue fields, task-key aliases, workspace paths, or authority claims.
use super::{Observation, QueueEntry};
use crate::db::Repo;
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
) {
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
        return;
    }
    let track = match repo.track_get(track_id.as_str()).await {
        Ok(track) => track,
        Err(error) => {
            tracing::warn!(?error, "result receipt detail track unavailable");
            None
        }
    };
    let view = TrackFsView::new(repo, write);
    for (index, envelope_id, identity, kind, field, report) in receipts {
        let mut detail = None;
        if let (Some(track), Some(path)) = (&track, run_detail_path(&identity)) {
            // cat resolves one exact execution key. Avoid a preliminary runs
            // listing; the reader owns projection and reserved-path semantics.
            let run = match view.cat(track, &path).await {
                Ok(content) => match serde_json::from_str::<Value>(&content.content) {
                    Ok(run) => Some(run),
                    Err(error) => {
                        // Valid ingress can exceed the parser's recursion limit
                        // after run wrapping. Optional detail must not requeue
                        // the durable report or other entries in its batch.
                        tracing::warn!(?error, "result receipt detail parse unavailable");
                        None
                    }
                },
                Err(error) => {
                    tracing::warn!(?error, "result receipt detail read unavailable");
                    None
                }
            };
            if let Some(run) = run {
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
        }
        segments[index].text.push_str(&match detail {
            Some((path, event_id)) => format!(
                "\nRecorded execution details: calm.track.cat({}). This virtual JSON record contains the recorded events and any artifact claims; it is not a worker report file or independent verification. Read events.{kind} and require event_id={event_id}; do not substitute another event or attempt.",
                serde_json::json!({"path": path}),
            ),
            None => "\nExact execution details unavailable through the current track reader. No worker report file is asserted to exist; retain the original queued receipt.".into(),
        });
    }
}

// This address is an existing virtual run record, not a filesystem path or a
// task-key alias. The gate-log helper validates a different address shape; here
// also exclude the reader's reserved index.json and bound the full filename.
fn run_detail_path(identity: &str) -> Option<String> {
    if identity.is_empty()
        || identity.len() > 507
        || matches!(identity, "." | ".." | "index")
        || !identity
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
    {
        return None;
    }
    Some(format!("runs/{identity}.json"))
}
