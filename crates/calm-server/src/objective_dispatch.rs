//! Objective Notify executor. The durable wake uses the same event log and
//! Dispatcher replay as compact settlement; event append and settlement are atomic.
use calm_truth::objective_host::{self, ExecutorSettlement};
use objective::{Intent, State, engine::Notice};

use crate::{
    db::{
        Repo,
        sqlite::{append_decision_event_in_tx, track_get_tx},
        write_in_tx_typed,
    },
    error::{CalmError, Result},
    event::{BroadcastEnvelope, Event, EventBus, EventScope, SYNC_EVENT_VERSION},
    ids::ActorId,
};

fn disposition(notice: &Notice, state: &State) -> Result<std::result::Result<String, String>> {
    let (key, template) = match notice {
        Notice::BudgetExhausted { objective, .. } => (
            objective,
            include_str!("../prompts/objective_budget_exhausted.md"),
        ),
        Notice::ObjectiveDone { objective } => {
            (objective, include_str!("../prompts/objective_done.md"))
        }
        Notice::DefinitionRejected { .. } => {
            return Ok(Err("DefinitionRejected has no consumer yet".into()));
        }
        Notice::ApprovalRequired { .. } => {
            return Ok(Err(
                "ApprovalRequired is requested explicitly by the Planner via neige_objective_request_approval"
                    .into(),
            ));
        }
        Notice::ApprovalInvalidated { .. } => {
            return Ok(Err("ApprovalInvalidated has no consumer yet".into()));
        }
        Notice::EvaluationReady { .. } => {
            return Ok(Err("EvaluationReady has no consumer yet".into()));
        }
        Notice::SettleRefused { .. } => return Ok(Err("SettleRefused has no consumer yet".into())),
        Notice::PlateauReached { .. } => {
            return Ok(Err("PlateauReached has no consumer yet".into()));
        }
        Notice::EventIgnored { .. } => {
            return Ok(Err("EventIgnored does not require Planner attention".into()));
        }
    };
    let objective = state
        .objectives
        .get(key)
        .ok_or_else(|| CalmError::Conflict(format!("notify objective {key} is missing")))?;
    let used = objective.used(&state.usage);
    let limit = if let Notice::BudgetExhausted { limit, .. } = notice {
        if !objective.budget_notified.contains(limit) {
            return Ok(Err(format!(
                "BudgetExhausted {limit:?} is no longer exhausted in engine state"
            )));
        }
        match limit {
            objective::definition::BudgetLimit::Tokens => "tokens",
            objective::definition::BudgetLimit::Turns => "回合",
            objective::definition::BudgetLimit::WallSecs => "秒",
        }
    } else {
        ""
    };
    let budget = &objective.definition.budget;
    Ok(Ok(template
        .trim_end()
        .replace("{limit}", limit)
        .replace("{budget_tokens}", &budget.tokens.to_string())
        .replace("{budget_turns}", &budget.turns.to_string())
        .replace("{budget_secs}", &budget.wall_secs.to_string())
        .replace("{used_tokens}", &used.context_tokens.to_string())
        .replace("{used_turns}", &used.turns.to_string())
        .replace("{used_secs}", &used.elapsed_secs.to_string())
        .replace("{key}", key.as_str())
        .replace("{title}", &objective.definition.title)))
}

/// Called only after the projector's existing indexed capability check. Its
/// serial boot sweep and reconciliation loop own retry; there is no new poller.
pub(crate) async fn dispatch_track(repo: &dyn Repo, bus: &EventBus, track: &str) -> Result<()> {
    let db = repo
        .sqlite_db()
        .ok_or_else(|| CalmError::Internal("objective dispatch requires SQLite".into()))?;
    let pending: Vec<_> = objective_host::unsettled_intents(&db)
        .await?
        .into_iter()
        .filter(|row| {
            row.id.track_id.as_str() == track && matches!(row.intent, Intent::Notify { .. })
        })
        .collect();
    if pending.is_empty() {
        return Ok(());
    }
    for row in pending {
        let envelope = write_in_tx_typed(repo, move |tx| Box::pin(async move {
            // Another invocation may have consumed this boot view. Recheck under
            // writer admission instead of weakening the lifecycle's strict transitions.
            let status: Option<String> = sqlx::query_scalar("SELECT status FROM objective_intents WHERE track_id=? AND event_seq=? AND ordinal=?")
                .bind(row.id.track_id.as_str()).bind(row.id.seq.0 as i64).bind(i64::from(row.id.ordinal))
                .fetch_optional(&mut **tx).await?;
            if status.as_deref().is_none_or(|status| status == "settled") { return Ok(None); }
            if status.as_deref() == Some("pending") {
                objective_host::intent_mark_dispatched_tx(tx, &row.id).await?;
            }
            let Intent::Notify { notice } = &row.intent else { unreachable!() };
            let (_, state) = objective_host::load_state_tx(tx, &row.id.track_id).await?;
            let (envelope, settlement) = match disposition(notice, &state)? {
                Ok(text) => {
                    let track = track_get_tx(tx, &row.id.track_id).await?;
                    let event = Event::TrackWakeRequested {
                        track_id: track.id.clone(), source: "objective".into(), key: row.id.to_string(), text,
                    };
                    let scope = EventScope::Track { track: track.id, area: track.area_id };
                    let id = append_decision_event_in_tx(tx, &ActorId::Kernel, &scope, None, &event).await?;
                    (Some(BroadcastEnvelope {id, event_version: SYNC_EVENT_VERSION, actor: ActorId::Kernel, scope, event}), ExecutorSettlement::Delivered)
                }
                Err(reason) => (None, ExecutorSettlement::Superseded {reason}),
            };
            objective_host::intent_settle_tx(tx, &row.id, settlement).await?;
            Ok(envelope)
        })).await?;
        if let Some(envelope) = envelope {
            bus.emit_envelope(envelope);
        }
    }
    Ok(())
}
