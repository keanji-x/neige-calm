//! Conditional re-review schema. Ordinary A2 remains exactly its original two fields.
use super::*;
use crate::operation::Tx;
use calm_types::task_execution::CandidateReviewResult;
use serde::Deserialize;
use serde_json::Value;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RepairReviewResult {
    passed: bool,
    blocking_findings: Vec<String>,
    finding_responses: Vec<FindingResponse>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FindingResponse {
    finding_index: usize,
    status: Resolution,
    evidence: String,
}
#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Resolution {
    Resolved,
    Unresolved,
}

pub(crate) async fn parse_tx(
    tx: &mut Tx<'_>,
    task: &Task,
    value: Value,
) -> Result<CandidateReviewResult> {
    super::repair::validate_contract_tx(tx, task).await?;
    parse_history_tx(tx, task, value).await
}
/// Historical full report facts survive withdrawal; current authority is checked separately.
pub(crate) async fn parse_history_tx(
    tx: &mut Tx<'_>,
    task: &Task,
    value: Value,
) -> Result<CandidateReviewResult> {
    let Some(receipt) = super::repair::for_task_tx(tx, task).await? else {
        let report: CandidateReviewResult = serde_json::from_value(value)
            .map_err(|_| conflict("candidate review requires passed and blocking_findings"))?;
        report.validate().map_err(conflict)?;
        return Ok(report);
    };
    if task.key != receipt.reviewer.key {
        return Err(conflict("repair report is not its designated reviewer"));
    }
    let full: RepairReviewResult = serde_json::from_value(value).map_err(|_| {
        conflict("repair review requires passed, blocking_findings and finding_responses")
    })?;
    let count = receipt.review.report.blocking_findings.len();
    let mut seen = std::collections::BTreeSet::new();
    if full.finding_responses.len() != count
        || full.finding_responses.iter().any(|response| {
            response.finding_index >= count
                || !seen.insert(response.finding_index)
                || response.evidence.trim().is_empty()
                || response.evidence.len() > 4096
                || (full.passed && response.status != Resolution::Resolved)
        })
    {
        return Err(conflict(
            "repair review must answer every original finding exactly once with nonempty evidence; pass requires all resolved",
        ));
    }
    let base = CandidateReviewResult {
        passed: full.passed,
        blocking_findings: full.blocking_findings,
    };
    base.validate().map_err(conflict)?;
    Ok(base)
}
