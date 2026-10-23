//! CheckFinished and operation completion commit together under the live claim.
use super::*;

pub(super) async fn finish_tx(tx: &mut Tx<'_>, op: &Operation) -> Result<Vec<BroadcastEnvelope>> {
    let frozen = Frozen::from_output(required_output(op)?)?;
    let authorized = objective_host::current_check_tx(tx, &frozen.intent_id).await?;
    if authorized.is_none() {
        // Definition replacement may already have settled the intent.
        let status: String = sqlx::query_scalar(
            "SELECT status FROM objective_intents WHERE track_id=? AND event_seq=? AND ordinal=?",
        )
        .bind(frozen.intent_id.track_id.as_str())
        .bind(frozen.intent_id.seq.0 as i64)
        .bind(i64::from(frozen.intent_id.ordinal))
        .fetch_one(&mut **tx)
        .await?;
        if status != "settled" {
            objective_host::intent_settle_tx(
                tx,
                &frozen.intent_id,
                objective_host::ExecutorSettlement::Superseded {
                    reason: "objective check authorization expired".into(),
                },
            )
            .await?;
        }
        return Ok(vec![]);
    }
    // The owned recovery derived this result exclusively from the executor journal's wait status.
    let raw: String = sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE id=?")
        .bind(&op.id)
        .fetch_one(&mut **tx)
        .await?;
    let output: TxOutput = serde_json::from_str(&raw)?;
    if let CheckRunResult::Measured { exit_code } = serde_json::from_value(output.result)? {
        let Intent::RunCheck {
            objective,
            evaluation,
            check,
            revision,
            sample,
        } = frozen.intent
        else {
            unreachable!()
        };
        objective_host::react_tx(
            tx,
            &objective_host::ObjectiveInput {
                track_id: frozen.intent_id.track_id.clone(),
                source: objective_host::InputSource::IntentJob {
                    intent: frozen.intent_id,
                },
                event: objective::Event::CheckFinished {
                    objective,
                    evaluation,
                    check,
                    revision,
                    sample,
                    output: objective::check::SampleOutput::Command(
                        objective::check::CommandOutput { exit_code },
                    ),
                },
            },
            now_ms(),
        )
        .await?;
    }
    // Infrastructure results remain dispatched; the dispatcher owns bounded retry and wake.
    Ok(vec![])
}
