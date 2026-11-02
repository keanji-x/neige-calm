//! The Planner acknowledges inspection evidence; execution authority stays with cancellation.
use super::*;
use crate::scheduler::followup as state;
use calm_truth::decision_gate::WriteTx;
use serde::{Deserialize, Serialize};

const TOOL: &str = "neige_followup_settle";
const REPLAY: &str = "task-followup: replay";

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Decision {
    Wait,
    Bypass,
    Terminate,
}
impl Decision {
    fn name(self) -> &'static str {
        match self {
            Self::Wait => "wait",
            Self::Bypass => "bypass",
            Self::Terminate => "terminate",
        }
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    attempt_id: String,
    notice_id: String,
    decision: Decision,
    review_after_secs: Option<i64>,
    message: String,
}

pub(super) fn descriptor() -> ToolDescriptor {
    ToolDescriptor {
        name: TOOL.into(),
        description: include_str!("../../../../prompts/tools/neige_followup_settle.md")
            .trim_end()
            .into(),
        input_schema: json!({"type":"object","additionalProperties":false,
            "required":["attempt_id","notice_id","decision","message"],
            "properties":{
                "attempt_id":{"type":"string","minLength":1},
                "notice_id":{"type":"string","minLength":1},
                "decision":{"enum":["wait","bypass","terminate"]},
                "review_after_secs":{"type":"integer","minimum":60,"maximum":86400},
                "message":message_schema()}}),
        annotations: Some(role_gated_write_annotations()),
        roles: &[CardRole::Planner],
        listed_for: &[CardRole::Planner],
    }
}

async fn checked(
    tx: &mut crate::operation::Tx<'_>,
    identity: &ToolCallIdentity,
    track: &str,
    request: &Request,
) -> Result<(Task, state::Notice, bool), CalmError> {
    if identity.role != CardRole::Planner
        || tx.read_card_role(&identity.card_id.clone().into()).await? != Some(CardRole::Planner)
        || !identity.session_is_active(tx, track).await?
    {
        return Err(CalmError::Forbidden(
            "current Planner session required".into(),
        ));
    }
    let task = state::current_tx(tx, track, &request.attempt_id).await?;
    let notice = state::notice_tx(tx, &task.id)
        .await?
        .ok_or_else(|| CalmError::Conflict("no task-stall notice".into()))?;
    if notice.notice_id != request.notice_id {
        return Err(CalmError::Conflict("task-stall notice changed".into()));
    }
    let encoded = serde_json::to_string(request)?;
    if let Some(previous) = &notice.decision_json {
        if previous == &encoded {
            return Ok((task, notice, true));
        }
        return Err(CalmError::Conflict(
            "notice already has a different decision".into(),
        ));
    }
    if !state::watched(&task) || !notice.matches(&task) {
        return Err(CalmError::Conflict(
            "task phase changed; refresh current evidence".into(),
        ));
    }
    Ok((task, notice, false))
}

async fn record(
    tx: &mut crate::operation::Tx<'_>,
    notice: &mut state::Notice,
    request: &Request,
) -> Result<(), CalmError> {
    notice.next_review_at_ms = now_ms().saturating_add(
        request
            .review_after_secs
            .map_or(state::REVIEW_AFTER_MS, |secs| secs * 1000),
    );
    let encoded = serde_json::to_string(request)?;
    sqlx::query("UPDATE task_followups SET next_review_at_ms=?1,decision_json=?2 WHERE task_id=?3 AND notice_id=?4")
        .bind(notice.next_review_at_ms).bind(&encoded).bind(&notice.task_id).bind(&notice.notice_id).execute(&mut **tx).await?;
    notice.decision_json = Some(encoded);
    Ok(())
}

pub(super) async fn decide(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    let message = parse_write_args(&args, TOOL)?;
    let mut request: Request = serde_json::from_value(args)
        .map_err(|e| RpcError::invalid_params(format!("{TOOL}: {e}")))?;
    request.message = message;
    if request.attempt_id.is_empty()
        || request.notice_id.is_empty()
        || match request.decision {
            Decision::Wait => request
                .review_after_secs
                .is_none_or(|secs| !(60..=86400).contains(&secs)),
            _ => request.review_after_secs.is_some(),
        }
    {
        return Err(RpcError::invalid_params(
            "wait requires review_after_secs 60..86400; other decisions forbid it; identifiers must be nonempty",
        ));
    }
    let (_, track) = resolve_track_for_identity(&ctx, &identity).await?;
    let decision = request.decision;
    let result = if decision == Decision::Terminate {
        let req = request.clone();
        let caller = identity.clone();
        let track = track.clone();
        write_with_actor_events_typed(
            ctx.repo.as_ref(),
            None,
            &ctx.events,
            &ctx.write,
            move |tx| {
                Box::pin(async move {
                    let (task, mut notice, replay) =
                        checked(tx, &caller, track.id.as_str(), &req).await?;
                    if replay {
                        return Err(CalmError::Conflict(REPLAY.into()));
                    }
                    let events = cancel_execution_tx(
                        tx,
                        &task,
                        task.status != TaskStatus::Pending,
                        caller.to_actor_id(),
                        EventScope::Track {
                            track: track.id,
                            area: track.area_id,
                        },
                        req.message.clone(),
                    )
                    .await?;
                    record(tx, &mut notice, &req).await?;
                    Ok((notice.response(req.decision.name()), events))
                })
            },
        )
        .await
        .map(|(value, _)| value)
    } else {
        let req = request.clone();
        let caller = identity.clone();
        let track = track.id.clone();
        // Private reminder metadata changes no task lifecycle. The tool receipt and the next
        // kernel observation retain the decision; no fake task/plan event is emitted.
        crate::db::write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
            Box::pin(async move {
                let (_, mut notice, replay) = checked(tx, &caller, track.as_str(), &req).await?;
                if !replay {
                    record(tx, &mut notice, &req).await?;
                }
                Ok(notice.response(req.decision.name()))
            })
        })
        .await
    };
    match result {
        Err(CalmError::Conflict(reason)) if reason == REPLAY => {
            crate::db::write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
                Box::pin(async move {
                    let (_, notice, replay) =
                        checked(tx, &identity, track.id.as_str(), &request).await?;
                    if !replay {
                        return Err(CalmError::Conflict("decision changed".into()));
                    }
                    Ok(notice.response(decision.name()))
                })
            })
            .await
            .map_err(|e| map_plan_error(TOOL, e))
        }
        other => other.map_err(|e| map_plan_error(TOOL, e)),
    }
}
