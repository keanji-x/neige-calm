//! Private, immutable single-round provenance and exact derived contracts.
use super::{
    candidate_input::{Binding, BindingPurpose},
    candidate_review::{self, ReviewEvidence},
    *,
};
use crate::{db::sqlite::task_get_tx, operation::Tx};
use calm_types::task_execution::{CandidateRepairReference, IsolatedWorkspace};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RepairArgs {
    pub producer: String,
    pub reason: String,
}
impl RepairArgs {
    pub(crate) fn validate(&self) -> Result<()> {
        if !calm_types::report_blocks::tasks::key_is_valid(&self.producer)
            || self.reason.trim().is_empty()
            || self.reason.len() > 4096
        {
            return Err(CalmError::BadRequest(
                "repair requires producer and nonempty reason (at most 4096 bytes)".into(),
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Derived {
    pub key: String,
    pub block_id: String,
    pub payload: Value,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Receipt {
    pub id: String,
    pub track_id: String,
    pub report_card_id: String,
    pub args: RepairArgs,
    pub input: Binding,
    pub review: ReviewEvidence,
    pub source_payload: Value,
    pub reviewer_payload: Value,
    pub repair: Derived,
    pub reviewer: Derived,
    pub created_at_ms: i64,
}
impl Receipt {
    pub(crate) fn public(&self) -> Value {
        json!({"id":self.id,"producer":self.args.producer,"reason":self.args.reason,
            "repair_key":self.repair.key,"review_key":self.reviewer.key,
            "report_card_id":self.report_card_id,"repair_block_id":self.repair.block_id,
            "review_block_id":self.reviewer.block_id,"created_at_ms":self.created_at_ms,
            "source_attempt_id":self.input.candidate.source.task_id,
            "publication_operation_id":self.input.candidate.publication_operation_id,
            "snapshot":self.input.candidate.snapshot,"verification_operation_id":self.input.verification_operation_id,
            "review_attempt_id":self.review.review_attempt_id,"report_event_id":self.review.report_event_id,
            "blocking_findings":self.review.report.blocking_findings})
    }
}
pub(crate) fn reference(task: &Task) -> Result<Option<CandidateRepairReference>> {
    let context: Value = serde_json::from_str(&task.context_json)?;
    Ok(IsolatedCodexSelection::from_context(&context)
        .map_err(conflict)?
        .and_then(|s| s.repair))
}
pub(crate) async fn lookup_tx(
    tx: &mut Tx<'_>,
    track: &str,
    producer: &str,
) -> Result<Option<Receipt>> {
    let raw: Option<String> = sqlx::query_scalar(
        "SELECT receipt_json FROM task_candidate_repairs WHERE track_id=?1 AND producer_key=?2",
    )
    .bind(track)
    .bind(producer)
    .fetch_optional(&mut **tx)
    .await?;
    raw.map(|raw| serde_json::from_str(&raw).map_err(Into::into))
        .transpose()
}
pub(crate) async fn for_task_tx(tx: &mut Tx<'_>, task: &Task) -> Result<Option<Receipt>> {
    let raw: Option<String> = sqlx::query_scalar("SELECT receipt_json FROM task_candidate_repairs WHERE track_id=?1 AND (repair_key=?2 OR review_key=?2)")
        .bind(&task.track_id).bind(&task.key).fetch_optional(&mut **tx).await?;
    let receipt: Option<Receipt> = raw.map(|raw| serde_json::from_str(&raw)).transpose()?;
    if reference(task)?.map(|r| r.receipt_id) != receipt.as_ref().map(|r| r.id.clone()) {
        return Err(conflict(
            "repair reference has no matching kernel receipt for this task",
        ));
    }
    Ok(receipt)
}
/// Called after evidence_tx validates both original claim freezes in this same transaction.
/// Today's changed goal/acceptance must never become C1's contract.
async fn original_payload_tx(tx: &mut Tx<'_>, task: &Task) -> Result<Value> {
    let (_, blocks) = crate::track_report::report_blocks_snapshot_tx(tx, &task.track_id).await?;
    let mut found = blocks.iter().filter(|b| b.payload["key"] == task.key);
    let block = found
        .next()
        .ok_or_else(|| conflict("original repair contract missing"))?;
    if found.next().is_some() {
        return Err(conflict("original repair contract is ambiguous"));
    }
    Ok(block.payload.clone())
}
pub(crate) async fn prepare_tx(
    tx: &mut Tx<'_>,
    track: &str,
    card: &str,
    args: &RepairArgs,
) -> Result<Receipt> {
    args.validate()?;
    let allocation = crate::db::sqlite::task_attempt_current_tx(tx, track, &args.producer)
        .await?
        .ok_or_else(|| conflict("repair producer missing"))?;
    let source = task_get_tx(tx, &allocation.attempt_id)
        .await?
        .ok_or_else(|| conflict("repair source missing"))?;
    let context: Value = serde_json::from_str(&source.context_json)?;
    let selected = IsolatedCodexSelection::from_context(&context)
        .map_err(conflict)?
        .ok_or_else(|| conflict("repair source is not isolated"))?;
    if selected.workspace != IsolatedWorkspace::Empty
        || selected.repair.is_some()
        || for_task_tx(tx, &source).await?.is_some()
    {
        return Err(conflict(
            "repair only supports an original empty candidate producer; recursive repair is unsupported",
        ));
    }
    let Some(FileDelivery::CandidateProducer { slot, policy, .. }) = selection(&source)? else {
        return Err(conflict("repair requires a candidate producer"));
    };
    let reviewer_key = policy
        .reviewer()
        .ok_or_else(|| conflict("repair requires designated review"))?;
    let mut input = candidate_review::select_input_tx(tx, track, &source.key, &slot).await?;
    let review =
        candidate_review::evidence_tx(tx, &input.candidate, &input.verification_operation_id)
            .await?;
    if review.report.passed || review.report.blocking_findings.is_empty() {
        return Err(conflict(
            "repair requires a settled rejected review with findings",
        ));
    }
    let reviewer = task_get_tx(tx, &review.review_attempt_id)
        .await?
        .ok_or_else(|| conflict("original reviewer missing"))?;
    if reviewer.key != reviewer_key || reference(&reviewer)?.is_some() {
        return Err(conflict("repair original reviewer mismatch"));
    }
    let source_payload = original_payload_tx(tx, &source).await?;
    let reviewer_payload = original_payload_tx(tx, &reviewer).await?;
    let id = uuid::Uuid::new_v4().to_string();
    let repair_key = format!("repair-{}", uuid::Uuid::new_v4().simple());
    let review_key = format!("review-{}", uuid::Uuid::new_v4().simple());
    let derive = |original: &Value, key: &str| -> Value {
        let mut payload = original.clone();
        payload["key"] = json!(key);
        payload["declared_by"] =
            json!(calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR);
        payload["ready"] = json!(true);
        // Admission release is a User decision for each new declaration.
        payload.as_object_mut().unwrap().remove("released_by_user");
        payload["context"]["neige_execution"]["workspace"] = json!("file-input");
        payload["context"]["neige_execution"]["repair"] = json!({"receipt_id":id});
        payload
    };
    let mut repair_payload = derive(&source_payload, &repair_key);
    repair_payload["context"]["neige_execution"]["file_delivery"]["policy"]["reviewer"] =
        json!(review_key);
    let mut review_payload = derive(&reviewer_payload, &review_key);
    review_payload["context"]["neige_execution"]["file_delivery"]["producer"] = json!(repair_key);
    input.purpose = BindingPurpose::CandidateRepairInput;
    Ok(Receipt {
        id,
        track_id: track.into(),
        report_card_id: card.into(),
        args: args.clone(),
        input,
        review,
        source_payload,
        reviewer_payload,
        repair: Derived {
            key: repair_key,
            block_id: uuid::Uuid::new_v4().to_string(),
            payload: repair_payload,
        },
        reviewer: Derived {
            key: review_key,
            block_id: uuid::Uuid::new_v4().to_string(),
            payload: review_payload,
        },
        created_at_ms: crate::model::now_ms(),
    })
}
/// Source/reviewer stay current and frozen for the whole linked round.
pub(crate) async fn validate_lineage_tx(tx: &mut Tx<'_>, receipt: &Receipt) -> Result<()> {
    // This one evidence read validates current C1, its exact machine evidence,
    // both original freezes, designated R1 report identity and successful stop.
    let review = candidate_review::evidence_tx(
        tx,
        &receipt.input.candidate,
        &receipt.input.verification_operation_id,
    )
    .await?;
    let source = task_get_tx(tx, &receipt.input.candidate.source.task_id)
        .await?
        .ok_or_else(|| conflict("original source missing"))?;
    if reference(&source)?.is_some() {
        return Err(conflict("recursive repair lineage"));
    }
    if original_payload_contract_tx(tx, &source).await? != contract(&receipt.source_payload) {
        return Err(conflict("original repair source contract changed"));
    }
    if review != receipt.review || review.report.passed {
        return Err(conflict("original repair review evidence changed"));
    }
    let reviewer = task_get_tx(tx, &review.review_attempt_id)
        .await?
        .ok_or_else(|| conflict("original reviewer missing"))?;
    if original_payload_contract_tx(tx, &reviewer).await? != contract(&receipt.reviewer_payload) {
        return Err(conflict("original repair reviewer contract changed"));
    }
    Ok(())
}
fn contract(payload: &Value) -> String {
    calm_types::task_recovery::task_root_hash_preimage(payload)
}
async fn original_payload_contract_tx(tx: &mut Tx<'_>, task: &Task) -> Result<String> {
    Ok(contract(&original_payload_tx(tx, task).await?))
}
pub(crate) async fn validate_contract_tx(tx: &mut Tx<'_>, task: &Task) -> Result<Option<Receipt>> {
    let Some(receipt) = for_task_tx(tx, task).await? else {
        return Ok(None);
    };
    let derived = if task.key == receipt.repair.key {
        &receipt.repair
    } else {
        &receipt.reviewer
    };
    let (_, blocks) = crate::track_report::report_blocks_snapshot_tx(tx, &task.track_id).await?;
    let mut found = blocks.iter().filter(|b| b.payload["key"] == task.key);
    let block = found
        .next()
        .ok_or_else(|| conflict("derived repair declaration missing"))?;
    if found.next().is_some()
        || block.id != derived.block_id
        || contract(&block.payload) != contract(&derived.payload)
        || block.payload["declared_by"] != derived.payload["declared_by"]
        || block.payload["spawn"] != derived.payload["spawn"]
        || task.declared_by != calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR
        || task.spawn != calm_types::task_recovery::TASK_IN_TRACK_ROUTE
        || task.acceptance_criteria.as_deref() != derived.payload["acceptance"].as_str()
        || task.goal != derived.payload["goal"].as_str().unwrap_or_default()
        || serde_json::from_str::<Value>(&task.context_json)? != derived.payload["context"]
    {
        return Err(conflict(
            "derived repair task does not match its complete receipt contract",
        ));
    }
    Ok(Some(receipt))
}
/// Check lineage once at the input/publication boundary, rather than recursively
/// repeating the complete R1 evidence read for each nested frozen-contract check.
pub(crate) async fn validate_task_tx(tx: &mut Tx<'_>, task: &Task) -> Result<Option<Receipt>> {
    let receipt = validate_contract_tx(tx, task).await?;
    if let Some(receipt) = &receipt {
        Box::pin(validate_lineage_tx(tx, receipt)).await?;
    }
    Ok(receipt)
}
pub(crate) async fn insert_tx(tx: &mut Tx<'_>, receipt: &Receipt) -> Result<()> {
    sqlx::query("INSERT INTO task_candidate_repairs(id,track_id,producer_key,repair_key,review_key,receipt_json) VALUES(?1,?2,?3,?4,?5,?6)")
        .bind(&receipt.id).bind(&receipt.track_id).bind(&receipt.args.producer).bind(&receipt.repair.key)
        .bind(&receipt.reviewer.key).bind(serde_json::to_string(receipt)?).execute(&mut **tx).await?;
    Ok(())
}
