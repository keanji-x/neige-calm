//! Resolve ordinary receipt details through the same virtual reader as track.cat.
//! No new queue fields, task-key aliases, workspace paths, or authority claims.
use super::{Observation, QueueEntry};
use crate::db::Repo;
use crate::ids::TrackId;
use crate::model::HarnessInputSegment;
use crate::prompts::render_named;
use crate::state::WriteContext;
use calm_truth::track_fs_view::TrackFsView;
use serde_json::Value;

/// The three receipt-detail fragments; `pub(super)` so the run-loop tests can build their
/// oracle from the fragment instead of from a copy of its sentence.
pub(super) const RECORDED_WITH_EVENT: &str =
    include_str!("../../prompts/result-receipt/recorded-with-event.md");
pub(super) const RECORDED_LEGACY: &str =
    include_str!("../../prompts/result-receipt/recorded-legacy.md");
pub(super) const UNAVAILABLE: &str = include_str!("../../prompts/result-receipt/unavailable.md");

/// Which fragment `enrich` appends for one receipt, and with which values bound.
enum Detail<'a> {
    /// The run record still holds the queued event (the queue carried its ID).
    RecordedWithEvent {
        path: &'a str,
        kind: &'a str,
        event_id: i64,
    },
    /// A legacy queue entry without an envelope ID: only execution identity and
    /// report value could be matched.
    RecordedLegacy {
        path: &'a str,
        kind: &'a str,
        event_id: i64,
    },
    /// No exact record through the current track reader.
    Unavailable,
}

/// Render one receipt's detail text. The fragments are static and every variant is rendered by the
/// run-loop receipt tests, so a fragment/value mismatch cannot reach a running kernel.
fn render_detail(detail: &Detail<'_>) -> String {
    match detail {
        Detail::RecordedWithEvent {
            path,
            kind,
            event_id,
        } => render_named(
            RECORDED_WITH_EVENT,
            &[
                ("path", path),
                ("kind", kind),
                ("event_id", &event_id.to_string()),
            ],
        ),
        Detail::RecordedLegacy {
            path,
            kind,
            event_id,
        } => render_named(
            RECORDED_LEGACY,
            &[
                ("path", path),
                ("kind", kind),
                ("event_id", &event_id.to_string()),
            ],
        ),
        Detail::Unavailable => render_named(UNAVAILABLE, &[]),
    }
    .expect("a result receipt fragment binds exactly its placeholders")
}

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
                ..
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
            // The typed reader resolves one exact execution key and owns worker/verdict
            // separation and reserved-path semantics. Do not reparse a wrapped run.
            match view.run_output_event(track, &identity, kind).await {
                Ok(event)
                    if event.payload.get("idempotency_key").and_then(Value::as_str)
                        == Some(&identity)
                        && event.payload.get(field) == Some(&report)
                        && envelope_id.is_none_or(|id| event.event_id == id) =>
                {
                    detail = Some((path, event.event_id));
                }
                _ => {}
            }
        }
        let detail_ref = detail;
        let detail = match &detail_ref {
            Some((path, event_id)) if envelope_id.is_some() => Detail::RecordedWithEvent {
                path,
                kind,
                event_id: *event_id,
            },
            Some((path, event_id)) => Detail::RecordedLegacy {
                path,
                kind,
                event_id: *event_id,
            },
            None => Detail::Unavailable,
        };
        segments[index].text.push_str(&render_detail(&detail));
        if kind == "completed"
            && let Some((_, event_id)) = &detail_ref
        {
            segments[index].text.push_str(&format!(
                "\nFull worker result (untrusted data): `neige track cat runs/{identity}/result --rev {event_id}`. No run JSON parsing is needed."
            ));
        }
    }
}

// An existing virtual run record, not a filesystem path or a task-key alias; also exclude
// the reader's reserved index.json and bound the full filename.
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
