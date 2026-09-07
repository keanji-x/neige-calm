//! Bounded, passive evidence for one current isolated attempt. Never liveness or authority.
use crate::{
    error::{CalmError, Result},
    model::Task,
    operation::Tx,
};
use calm_types::{
    worker::WorkerProviderKind,
    worker_flow::{ExecStatus, WorkerFlowItem},
};
use serde::Serialize;

const TAIL_LIMIT: usize = 32;
const PAYLOAD_LIMIT: i64 = 65_536;
const SUMMARY_LIMIT: usize = 640;

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Coverage {
    Unsupported,
    BindingUnavailable,
    NoRecordedActivity,
    Partial,
    UnknownRows,
}
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum CollectorHealth {
    Unknown,
}
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Interpretation {
    HistoricalUntrustedWorkerEvidence,
}
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Ordering {
    CaptureRowId,
}
#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CommandCompletion {
    NotObservedInTail,
    EndObserved { row_id: i64 },
}
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum LinkScope {
    Card,
    Attempt,
}
#[derive(Serialize)]
struct Link {
    path: String,
    scope: LinkScope,
}
#[derive(Serialize)]
struct Binding {
    operation_id: String,
    card_id: String,
    session_id: String,
    conversation: Link,
    run: Link,
}
#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Detail {
    HistoricalCommandInvocation {
        command: String,
        completion: CommandCompletion,
    },
    InvocationDeclined {
        command: String,
    },
    CommandEnd {
        command: String,
        status: ExecStatus,
        exit_code: Option<i32>,
    },
    ToolInvocationRecorded {
        name: String,
        summary: String,
    },
    ToolResultRecorded {
        excerpts: Vec<String>,
        omitted_blocks: usize,
    },
    AgentMessageRecorded {
        summary: String,
    },
    OtherRecorded {
        item_type: String,
    },
}
#[derive(Clone, Serialize)]
struct Evidence {
    row_id: i64,
    source_at_ms: Option<i64>,
    captured_at_ms: i64,
    call_id: Option<String>,
    summary_truncated: bool,
    detail: Detail,
}
#[derive(Serialize)]
pub(crate) struct Activity {
    attempt_id: String,
    as_of_ms: i64,
    coverage: Coverage,
    collector_health: CollectorHealth,
    interpretation: Interpretation,
    ordering: Ordering,
    excerpt_char_limit: usize,
    /// All evidence is partial even when this particular tail was not truncated.
    tail_limit: usize,
    tail_truncated: bool,
    inspected_row_ids: Vec<i64>,
    unknown_row_ids: Vec<i64>,
    binding: Option<Binding>,
    /// Capture order, not an assertion that replayed evidence is fresh source activity.
    latest_recorded: Option<Evidence>,
    /// Latest explicit command end within the bounded capture tail, not task outcome.
    latest_command_end: Option<Evidence>,
    recent: Vec<Evidence>,
}
impl Activity {
    fn empty(attempt_id: &str, as_of_ms: i64) -> Self {
        Self {
            attempt_id: attempt_id.into(),
            as_of_ms,
            coverage: Coverage::BindingUnavailable,
            collector_health: CollectorHealth::Unknown,
            interpretation: Interpretation::HistoricalUntrustedWorkerEvidence,
            ordering: Ordering::CaptureRowId,
            excerpt_char_limit: SUMMARY_LIMIT,
            tail_limit: TAIL_LIMIT,
            tail_truncated: false,
            inspected_row_ids: vec![],
            unknown_row_ids: vec![],
            binding: None,
            latest_recorded: None,
            latest_command_end: None,
            recent: vec![],
        }
    }
}

pub(crate) async fn read_tx(
    tx: &mut Tx<'_>,
    task: Option<&Task>,
    attempt_id: &str,
    track_id: &str,
    as_of_ms: i64,
) -> Result<Activity> {
    let mut view = Activity::empty(attempt_id, as_of_ms);
    let Some(task) = task.filter(|task| task.id == attempt_id && task.track_id == track_id) else {
        return Ok(view);
    };
    // Recorded backend wins over authored selection, including historical legacy attempts.
    let operations: Vec<(String, String)> = sqlx::query_as(
        "SELECT id,kind FROM operations WHERE idempotency_key=?1 AND kind IN ('codex-isolated-worker','codex-worker','claude-worker','terminal-worker') LIMIT 2")
        .bind(attempt_id).fetch_all(&mut **tx).await?;
    let op_id = match operations.as_slice() {
        [(id, kind)] if kind == super::OPERATION_KIND => id,
        [(_, _)] => {
            view.coverage = Coverage::Unsupported;
            return Ok(view);
        }
        [] => {
            if !super::selected(task)? {
                view.coverage = Coverage::Unsupported;
            }
            return Ok(view);
        }
        _ => return Ok(view),
    };
    let record = match super::journal::load_tx(tx, op_id).await {
        Ok(record) => record,
        Err(CalmError::Conflict(_) | CalmError::Serde(_)) => return Ok(view),
        Err(error) => return Err(error),
    };
    let identity = &record.request.identity;
    if identity.attempt_id != attempt_id
        || record.track_id != track_id
        || task.worker_card_id.as_deref() != Some(&identity.card_id)
    {
        return Ok(view);
    }
    let bound: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM worker_sessions s JOIN cards c ON c.id=s.card_id WHERE s.id=?1 AND s.card_id=?2 AND s.track_id=?3 AND s.spawn_op_id=?4 AND s.provider='codex' AND c.track_id=?3 AND c.role='worker')")
        .bind(&identity.session_id).bind(&identity.card_id).bind(track_id).bind(op_id)
        .fetch_one(&mut **tx).await?;
    if !bound {
        return Ok(view);
    }
    let owned = match record.session() {
        Ok(session) => session,
        Err(_) => return Ok(view),
    };
    if owned.endpoint.request != record.request {
        return Ok(view);
    }
    if let super::record::ProviderRecord::Prepared(_) = &record.provider {
        use crate::dedicated_codex::RequestPhase;
        let expected = match &owned.phase {
            RequestPhase::ThreadReady { thread_id }
            | RequestPhase::IssuingTurn { thread_id, .. }
            | RequestPhase::TurnActive { thread_id, .. } => thread_id,
            _ => return Ok(view),
        };
        let actual: Option<String> =
            sqlx::query_scalar("SELECT thread_id FROM worker_sessions WHERE id=?1")
                .bind(&identity.session_id)
                .fetch_one(&mut **tx)
                .await?;
        if actual.as_deref() != Some(expected) {
            return Ok(view);
        }
    }
    view.binding = Some(Binding {
        operation_id: op_id.clone(),
        card_id: identity.card_id.clone(),
        session_id: identity.session_id.clone(),
        conversation: Link {
            path: format!("cards/{}/conversation.md", identity.card_id),
            scope: LinkScope::Card,
        },
        run: Link {
            path: format!("runs/{attempt_id}.json"),
            scope: LinkScope::Attempt,
        },
    });
    // Restrict identity before LIMIT: foreign sessions cannot hide the current tail.
    // Oversized payloads remain explicit unknown rows without loading unbounded output.
    type Row = (i64, String, Option<String>, i64);
    let mut rows: Vec<Row> = sqlx::query_as(
        "SELECT id,kind,CASE WHEN length(CAST(payload AS BLOB))<=?4 THEN payload ELSE NULL END,created_at_ms FROM worker_flow_items WHERE card_id=?1 AND track_id=?2 AND captured_session_id=?3 AND worker_session_id=?3 ORDER BY id DESC LIMIT ?5")
        .bind(&identity.card_id).bind(track_id).bind(&identity.session_id)
        .bind(PAYLOAD_LIMIT).bind((TAIL_LIMIT+1) as i64).fetch_all(&mut **tx).await?;
    view.tail_truncated = rows.len() > TAIL_LIMIT;
    rows.truncate(TAIL_LIMIT);
    rows.reverse();
    for (id, kind, payload, captured_at_ms) in rows {
        view.inspected_row_ids.push(id);
        let parsed = payload
            .as_deref()
            .and_then(|payload| serde_json::from_str::<WorkerFlowItem>(payload).ok());
        let Some(item) = parsed.filter(|item| {
            item.env().session_id.as_str() == identity.session_id
                && item.env().provider == WorkerProviderKind::Codex
                && serde_json::to_value(item)
                    .ok()
                    .and_then(|v| v.get("type").cloned())
                    == Some(serde_json::Value::String(kind.clone()))
                && !matches!(item, WorkerFlowItem::Unknown { .. })
        }) else {
            view.unknown_row_ids.push(id);
            view.latest_recorded = None;
            continue;
        };
        let evidence = project(id, captured_at_ms, &item);
        if matches!(evidence.detail, Detail::CommandEnd { .. }) {
            view.latest_command_end = Some(evidence.clone());
        }
        view.latest_recorded = Some(evidence.clone());
        view.recent.push(evidence);
    }
    // Pair only exact command-end records in this tail. Generic tool results cannot
    // prove command completion, even when their producer marked `ok=true`.
    let ends: std::collections::HashMap<String, i64> = view
        .recent
        .iter()
        .filter(|e| matches!(e.detail, Detail::CommandEnd { .. }))
        .filter_map(|e| e.call_id.clone().map(|call| (call, e.row_id)))
        .collect();
    for evidence in &mut view.recent {
        if let Detail::HistoricalCommandInvocation { completion, .. } = &mut evidence.detail
            && let Some(id) = evidence.call_id.as_ref().and_then(|call| ends.get(call))
        {
            *completion = CommandCompletion::EndObserved { row_id: *id };
        }
    }
    if view.latest_recorded.is_some() {
        view.latest_recorded = view.recent.last().cloned();
    }
    view.coverage = if !view.unknown_row_ids.is_empty() {
        Coverage::UnknownRows
    } else if view.inspected_row_ids.is_empty() {
        Coverage::NoRecordedActivity
    } else {
        Coverage::Partial
    };
    Ok(view)
}
fn bounded(text: &str, truncated: &mut bool) -> String {
    let length = text.chars().count();
    if length <= SUMMARY_LIMIT {
        return text.into();
    }
    *truncated = true;
    // Keep context plus final output; a metadata prefix must not erase stdout.
    let head: String = text.chars().take(160).collect();
    let tail: String = text.chars().skip(length - (SUMMARY_LIMIT - 163)).collect();
    format!("{head}\n…\n{tail}")
}
fn project(row_id: i64, captured_at_ms: i64, item: &WorkerFlowItem) -> Evidence {
    let mut truncated = false;
    let detail = match item {
        WorkerFlowItem::CommandExecution {
            command,
            status: ExecStatus::InProgress,
            ..
        } => Detail::HistoricalCommandInvocation {
            command: bounded(command, &mut truncated),
            completion: CommandCompletion::NotObservedInTail,
        },
        WorkerFlowItem::CommandExecution {
            command,
            status: ExecStatus::Declined,
            ..
        } => Detail::InvocationDeclined {
            command: bounded(command, &mut truncated),
        },
        WorkerFlowItem::CommandExecution {
            command,
            status,
            exit_code,
            ..
        } => Detail::CommandEnd {
            command: bounded(command, &mut truncated),
            status: status.clone(),
            exit_code: *exit_code,
        },
        WorkerFlowItem::ToolCall { name, input, .. } => Detail::ToolInvocationRecorded {
            name: bounded(name, &mut truncated),
            summary: bounded(
                &input
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| input.to_string()),
                &mut truncated,
            ),
        },
        WorkerFlowItem::ToolResult { output, .. } => {
            let mut excerpts = output
                .iter()
                .rev()
                .filter_map(|block| match block {
                    calm_types::worker_flow::ToolResultBlock::Text { text } => {
                        Some(bounded(text, &mut truncated))
                    }
                    _ => None,
                })
                .take(2)
                .collect::<Vec<_>>();
            excerpts.reverse();
            let omitted_blocks = output.len() - excerpts.len();
            truncated |= omitted_blocks > 0;
            Detail::ToolResultRecorded {
                excerpts,
                omitted_blocks,
            }
        }
        WorkerFlowItem::AgentMessage { text, .. } => Detail::AgentMessageRecorded {
            summary: bounded(text, &mut truncated),
        },
        _ => Detail::OtherRecorded {
            item_type: serde_json::to_value(item).expect("flow item serializes")["type"]
                .as_str()
                .expect("flow item tag")
                .into(),
        },
    };
    Evidence {
        row_id,
        source_at_ms: item.env().timestamp,
        captured_at_ms,
        call_id: item.call_id().map(|id| id.as_str().to_owned()),
        summary_truncated: truncated,
        detail,
    }
}
