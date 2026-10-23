//! Canonical evaluator declarations use the Planner's existing Report write funnel.
use super::*;
use crate::mcp_server::registry::{AppContext, ToolCallIdentity};
use crate::track_report::{ReportDocOp, ReportEditTarget};
use calm_truth::objective_host::{LogSeq, check_jobs};
use calm_types::report_blocks::{parse_fence, render_fence, tasks::task_block_is_live};
use objective::check::CheckConfig;
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};
use std::sync::Arc;

// Like worker_report::REPEATED: abort the event-writer transaction on a proven
// no-op, because its contract requires a nonempty event batch. Never reaches MCP.
const ALREADY_DECLARED: &str = "objective evaluator declaration already present";

pub(crate) async fn bind_request_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &crate::ids::TrackId,
    seq: LogSeq,
) -> Result<Vec<Value>> {
    let (state, rows) = check_jobs::request_jobs_tx(tx, track, seq).await?;
    let mut payloads = vec![];
    for row in rows {
        let Intent::RunCheck {
            objective,
            check,
            revision,
            sample,
            ..
        } = &row.intent
        else {
            unreachable!()
        };
        let o = &state.objectives[objective];
        let CheckConfig::Evaluator(params) = &o
            .definition
            .check(check)
            .ok_or_else(|| CalmError::Internal("request check missing".into()))?
            .config
        else {
            continue;
        };
        let key = format!("objcheck-{}-{}", row.id.seq.0, row.id.ordinal);
        let baseline = o
            .ledger
            .baseline
            .as_ref()
            .map(|b| b.revision.as_str())
            .unwrap_or("absent");
        // Replace in one pass: rubric text is data, never a second template pass.
        let goal = include_str!("../../prompts/objective_evaluator.md")
            .split_inclusive('\n')
            .map(|line| match line.trim_end() {
                "{rubric}" => format!(
                    "{}\n",
                    serde_json::to_string(&params.rubric).expect("rubric serializes")
                ),
                "Candidate revision: {candidate}" => format!("Candidate revision: {revision}\n"),
                "Baseline revision: {baseline}" => format!("Baseline revision: {baseline}\n"),
                "Sample: {sample}" => format!("Sample: {sample}\n"),
                _ => line.to_owned(),
            })
            .collect::<String>();
        let payload = json!({"key":key,"kind":"codex","access":"read_only","result_contract":"evaluation-v1","ready":true,"declared_by":calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR,"goal":goal});
        calm_types::report_blocks::validate_payload("task", &payload)
            .map_err(CalmError::BadRequest)?;
        if let Some(hash) = check_jobs::bound_hash_tx(tx, track, &key).await? {
            if hash != check_jobs::payload_hash(&payload) {
                return Err(CalmError::Conflict(
                    "evaluator declaration differs from binding".into(),
                ));
            }
        } else {
            check_jobs::bind_tx(tx, &row.id, &key, &payload).await?;
        }
        payloads.push(payload);
    }
    Ok(payloads)
}

/// Recognize only canonical retries after loading the Report under writer admission.
/// Immutability and withdrawal rules belong to check_jobs::guard_report_tx.
/// A plain upsert still has its usual anchor/id contract; only a bound canonical
/// create can prove a no-op, by key rather than by a fabricated block id.
pub(crate) async fn guard_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &crate::ids::TrackId,
    blocks: &[calm_types::track_report::ReportBlock],
    op: &ReportDocOp,
) -> Result<()> {
    let ReportDocOp::UpsertBlock {
        id: None,
        kind,
        content,
        ..
    } = op
    else {
        return Ok(());
    };
    if kind != "task" || !objective_host::capability::enabled_tx(tx, track).await? {
        return Ok(());
    }
    let fence =
        parse_fence(content).ok_or_else(|| CalmError::BadRequest("invalid task fence".into()))?;
    let Some(key) = fence.payload.get("key").and_then(Value::as_str) else {
        return Ok(());
    };
    let Some(hash) = check_jobs::bound_hash_tx(tx, track, key).await? else {
        return Ok(());
    };
    let same_key: Vec<_> = blocks
        .iter()
        .filter(|b| b.kind == "task" && b.payload.get("key").and_then(Value::as_str) == Some(key))
        .collect();
    if same_key.len() == 1
        && task_block_is_live(same_key[0])
        && check_jobs::payload_hash(&same_key[0].payload) == hash
        && check_jobs::payload_hash(&fence.payload) == hash
    {
        return Err(CalmError::IdempotencyCollision(ALREADY_DECLARED.into()));
    }
    Ok(())
}

pub(crate) async fn declare(
    ctx: &AppContext,
    identity: &ToolCallIdentity,
    track: &crate::ids::TrackId,
    payloads: Vec<Value>,
) -> Result<()> {
    for payload in payloads {
        let (track_row, card, current) =
            crate::track_report::resolve_report_for_track(ctx.repo.as_ref(), track.as_str())
                .await?;
        let rev = current.doc_rev;
        let target = ReportEditTarget::for_resolved_parts(track_row, card, current)?;
        let result = crate::track_report::write::agent_report_op(
            ctx.repo.as_ref(),
            &ctx.events,
            &ctx.write,
            ActorId::AiPlanner(identity.card_id.clone().into()),
            crate::event::EditAuthor::Planner,
            target,
            ReportDocOp::UpsertBlock {
                id: None,
                kind: "task".into(),
                content: render_fence("task", &payload),
                if_rev: None,
                if_doc_rev: Some(rev),
                position: None,
            },
            None,
            None,
            Arc::new(
                crate::decision_sink::CardDecisionSinkRecorderAdmissionGate::for_identity(
                    identity,
                    track.clone(),
                ),
            ),
        )
        .await;
        match result {
            Ok(_) => {}
            Err(CalmError::IdempotencyCollision(reason)) if reason == ALREADY_DECLARED => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// The existing projector reconciliation owns recovery. Wake evidence is permanent:
/// track.wake_requested is outside events_prune's exact-kind retention allowlist.
pub(super) async fn missing_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &str,
) -> Result<Vec<BroadcastEnvelope>> {
    let track_id = crate::ids::TrackId::from(track);
    let bindings: Vec<(i64,i64,String,String)> = sqlx::query_as("SELECT j.event_seq,j.ordinal,j.task_key,i.intent_json FROM objective_check_jobs j JOIN objective_intents i USING(track_id,event_seq,ordinal) WHERE j.track_id=? AND i.status='dispatched' ORDER BY j.event_seq,j.ordinal")
        .bind(track).fetch_all(&mut **tx).await?;
    if bindings.is_empty() {
        return Ok(vec![]);
    }
    let blocks = check_jobs::report_blocks_tx(tx, &track_id).await?;
    let mut envelopes = vec![];
    for (seq, ordinal, task_key, intent_json) in bindings {
        if blocks.iter().any(|b| {
            task_block_is_live(b)
                && b.payload.get("key").and_then(Value::as_str) == Some(task_key.as_str())
        }) {
            continue;
        }
        let key = format!("objective-check-job:{seq}:{ordinal}");
        let seen:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM events WHERE scope_track=? AND kind='track.wake_requested' AND json_extract(payload,'$.source')='objective-check-job' AND json_extract(payload,'$.key')=?)")
            .bind(track).bind(&key).fetch_one(&mut **tx).await?;
        if seen {
            continue;
        }
        let Intent::RunCheck {
            objective,
            revision,
            ..
        } = serde_json::from_str(&intent_json)?
        else {
            return Err(CalmError::Internal(
                "bound evaluator intent is not RunCheck".into(),
            ));
        };
        let request = json!({"key":objective,"revision":revision}).to_string();
        let track = track_get_tx(tx, &track_id).await?;
        let event = Event::TrackWakeRequested {
            track_id: track_id.clone(),
            source: "objective-check-job".into(),
            key,
            text: include_str!("../../prompts/objective_evaluator_missing.md")
                .trim_end()
                .replace("{task_key}", &task_key)
                .replace("{request}", &request),
        };
        let scope = EventScope::Track {
            track: track_id.clone(),
            area: track.area_id,
        };
        let id = append_decision_event_in_tx(tx, &ActorId::Kernel, &scope, None, &event).await?;
        envelopes.push(BroadcastEnvelope {
            id,
            event_version: SYNC_EVENT_VERSION,
            actor: ActorId::Kernel,
            scope,
            event,
        });
    }
    Ok(envelopes)
}
