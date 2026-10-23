//! Kernel-owned fact executors. The support set lives here, shared by admission and execution.
use super::*;
use objective::check::{CheckConfig, Fact, FactOutput, FactParams, SampleOutput};
use objective::engine::Evaluation;
use objective::standard::standard_hash;
use sqlx::{Row, Sqlite, Transaction};

pub(crate) fn supports(config: &CheckConfig) -> bool {
    match config {
        CheckConfig::Fact(params) => match params.fact {
            Fact::TaskAccepted | Fact::AskAnswered => true,
            Fact::PrMerged => false,
        },
        CheckConfig::Command(_) | CheckConfig::Metric(_) | CheckConfig::Evaluator(_) => false,
    }
}

/// Caller owns the one writer transaction, including claim, measurement and report.
pub(super) async fn run_tx(
    tx: &mut Transaction<'_, Sqlite>,
    row: &objective_host::StoredIntent,
    pending: bool,
) -> Result<()> {
    let Intent::RunCheck {
        objective: key,
        evaluation,
        check,
        revision,
        sample,
    } = &row.intent
    else {
        unreachable!()
    };
    let (_, state) = objective_host::load_state_tx(tx, &row.id.track_id).await?;
    let current = state.objectives.get(key);
    let valid = current.is_some_and(|o| {
        matches!(&o.evaluation, Some(Evaluation::Running { id, revision: r, .. }) if id == evaluation && r == revision)
        && matches!(&row.effect, objective_host::StoredEffect::RunCheck { standard_hash: hash } if *hash == standard_hash(&o.definition))
        && o.status == objective::engine::Status::Open
    });
    if !valid {
        objective_host::intent_settle_tx(
            tx,
            &row.id,
            ExecutorSettlement::Superseded {
                reason: "evaluation is no longer current".into(),
            },
        )
        .await?;
        return Ok(());
    }
    let config = &current
        .unwrap()
        .definition
        .check(check)
        .ok_or_else(|| CalmError::Conflict("objective check is missing".into()))?
        .config;
    if !supports(config) {
        return Err(CalmError::Conflict(
            "objective check has no executor".into(),
        ));
    }
    let CheckConfig::Fact(params) = config else {
        unreachable!()
    };
    if pending {
        objective_host::intent_mark_dispatched_tx(tx, &row.id).await?;
    }
    let holds = measure_tx(tx, &row.id.track_id, params).await?;
    objective_host::react_tx(
        tx,
        &objective_host::ObjectiveInput {
            track_id: row.id.track_id.clone(),
            source: objective_host::InputSource::IntentJob {
                intent: row.id.clone(),
            },
            event: objective::Event::CheckFinished {
                objective: key.clone(),
                evaluation: *evaluation,
                check: check.clone(),
                revision: revision.clone(),
                sample: *sample,
                output: SampleOutput::Fact(FactOutput { holds }),
            },
        },
        crate::model::now_ms(),
    )
    .await?;
    Ok(())
}

async fn measure_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &crate::ids::TrackId,
    params: &FactParams,
) -> Result<bool> {
    match params.fact {
        Fact::TaskAccepted => {
            let rows = sqlx::query(
                "SELECT kind,payload,actor,event_version,scope_kind,scope_area,scope_track,scope_card \
                FROM events WHERE scope_track=? \
                AND kind IN ('task.completed','task.failed') \
                AND json_extract(payload,'$.idempotency_key')=? ORDER BY id DESC",
            )
            .bind(track.as_str())
            .bind(&params.subject)
            .fetch_all(&mut **tx)
            .await?;
            for row in rows {
                let scope = EventScope::from_row(
                    row.try_get("scope_kind")?,
                    row.try_get("scope_area")?,
                    row.try_get("scope_track")?,
                    row.try_get("scope_card")?,
                );
                let actor: ActorId = serde_json::from_str(row.try_get("actor")?)?;
                if !calm_truth::track_fs_view::is_planner_verdict_event(&scope, &actor) {
                    continue;
                }
                let event = Event::from_stored_kind_and_payload(
                    row.try_get("kind")?,
                    serde_json::from_str(row.try_get("payload")?)?,
                    row.try_get("event_version")?,
                )
                .map_err(|e| CalmError::Internal(e.to_string()))?;
                return Ok(
                    matches!(event, Event::TaskCompleted { result, .. } if result.get("status").and_then(serde_json::Value::as_str) == Some("accepted")),
                );
            }
            Ok(false)
        }
        Fact::AskAnswered => {
            let Ok(id) = params.subject.parse::<i64>() else {
                return Ok(false);
            };
            Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM events WHERE scope_track=? AND kind='ask.answered' AND json_extract(payload,'$.ask_id')=?)")
                .bind(track.as_str()).bind(id).fetch_one(&mut **tx).await?)
        }
        Fact::PrMerged => Err(CalmError::Conflict("PrMerged has no executor".into())),
    }
}
