//! Named dispatch provenance and current read state under the report transaction.
use super::{ReportDoc, ReportDocOp};
use crate::error::{CalmError, Result};
use crate::ids::TrackId;
use crate::mcp_server::registry::ToolCallIdentity;
use crate::model::{CardRole, now_ms};
use calm_types::{
    report_blocks,
    task_execution::{
        CandidateInputPurpose, FileDelivery, IsolatedCodexSelection, IsolatedCodexVersion,
        IsolatedWorkspace,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};

// The workspace tag preserves the released flat empty contract JSON.
// Required candidate fields belong to their variant, never optional backfills.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "workspace", deny_unknown_fields)]
pub(crate) enum DispatchArgs {
    #[serde(rename = "empty")]
    Empty {
        name: String,
        goal: String,
        acceptance: String,
        executor: Executor,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        plugin_tools: Vec<String>,
    },
    #[serde(rename = "verified-candidate")]
    VerifiedCandidate {
        name: String,
        goal: String,
        acceptance: String,
        executor: Executor,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        plugin_tools: Vec<String>,
        input: CandidateInput,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CandidateInput {
    producer: String,
    slot: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Executor {
    Codex,
}

impl DispatchArgs {
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Empty { name, .. } | Self::VerifiedCandidate { name, .. } => name,
        }
    }
    fn goal(&self) -> &str {
        match self {
            Self::Empty { goal, .. } | Self::VerifiedCandidate { goal, .. } => goal,
        }
    }
    fn acceptance(&self) -> &str {
        match self {
            Self::Empty { acceptance, .. } | Self::VerifiedCandidate { acceptance, .. } => {
                acceptance
            }
        }
    }
    pub(crate) fn plugin_tools(&self) -> &[String] {
        match self {
            Self::Empty { plugin_tools, .. } | Self::VerifiedCandidate { plugin_tools, .. } => {
                plugin_tools
            }
        }
    }
    fn execution(&self) -> IsolatedCodexSelection {
        let (workspace, file_delivery) = match self {
            Self::Empty { .. } => (IsolatedWorkspace::Empty, None),
            Self::VerifiedCandidate { input, .. } => (
                IsolatedWorkspace::FileInput,
                Some(FileDelivery::CandidateConsumer {
                    producer: input.producer.clone(),
                    slot: input.slot.clone(),
                    purpose: CandidateInputPurpose::VerifiedCandidateInput,
                }),
            ),
        };
        IsolatedCodexSelection {
            version: IsolatedCodexVersion::V1,
            workspace,
            file_delivery,
            repair: None,
            plugin_tools: self.plugin_tools().to_vec(),
        }
    }
    pub(crate) fn normalize(mut self) -> Result<Self> {
        let (Self::Empty { name, .. } | Self::VerifiedCandidate { name, .. }) = &mut self;
        *name = name.trim().to_owned();
        if name.is_empty() || name.len() > 200 || name.chars().any(char::is_control) {
            return Err(CalmError::BadRequest("name must be nonempty, at most 200 UTF-8 bytes after trim, and contain no control characters".into()));
        }
        if self.goal().trim().is_empty() || self.acceptance().trim().is_empty() {
            return Err(CalmError::BadRequest(
                "goal and acceptance must be nonempty".into(),
            ));
        }
        let (Self::Empty { plugin_tools, .. } | Self::VerifiedCandidate { plugin_tools, .. }) =
            &mut self;
        plugin_tools.sort();
        self.execution()
            .validate_delivery()
            .map_err(CalmError::BadRequest)?;
        Ok(self)
    }
}

#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub(crate) struct DispatchReceipt {
    pub name: String,
    pub task_key: String,
    pub report_card_id: String,
    pub block_id: String,
    pub created_at_ms: i64,
}

/// Validate the persisted current Planner, including on a read-only replay.
/// MCP's card-bound sentinel does not claim a provider turn identity.
pub(super) async fn authorize_tx(
    tx: &mut Transaction<'_, Sqlite>,
    identity: &ToolCallIdentity,
    track: &TrackId,
    report_card: &str,
) -> Result<()> {
    if identity.role != CardRole::Planner || identity.track_id.as_deref() != Some(track.as_str()) {
        return Err(CalmError::Forbidden(
            "dispatch requires a Planner bound to this Track".into(),
        ));
    }
    let current: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM worker_sessions s \
         JOIN cards c ON c.session_id=s.id AND s.card_id=c.id \
         JOIN tracks t ON t.id=c.track_id \
         JOIN cards r ON r.id=?5 AND r.track_id=t.id AND r.kind='track-report' \
         WHERE s.id=?1 AND c.id=?2 AND c.track_id=?3 AND t.area_id=?4 \
         AND c.role='planner' AND s.track_id=c.track_id \
         AND s.state IN ('starting','running','idle','turn_pending'))",
    )
    .bind(&identity.session_id)
    .bind(&identity.card_id)
    .bind(track.as_str())
    .bind(&identity.area_id)
    .bind(report_card)
    .fetch_one(&mut **tx)
    .await?;
    if !current {
        return Err(CalmError::Forbidden(
            "dispatch requires the current active Planner session on this Track".into(),
        ));
    }
    // Replay emits no events, so check the existing event authority explicitly.
    calm_truth::decision_gate::enforce_role_resolving_session_from_tx(
        tx,
        &identity.to_actor_id(),
        &crate::event::Event::PlanUpdated {
            track_id: track.clone(),
            changed_keys: Vec::new(),
            agent_message: None,
        },
        &crate::event::EventScope::Track {
            track: track.clone(),
            area: crate::ids::AreaId::from(identity.area_id.clone()),
        },
    )
    .await
    .map_err(|error| CalmError::Forbidden(error.to_string()))
}

pub(super) async fn lookup_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    args: &DispatchArgs,
) -> Result<Option<DispatchReceipt>> {
    let saved: Option<String> = sqlx::query_scalar(
        "SELECT contract_json FROM planner_dispatch_receipts WHERE track_id=?1 AND name=?2",
    )
    .bind(track.as_str())
    .bind(args.name())
    .fetch_optional(&mut **tx)
    .await?;
    let Some(saved) = saved else {
        return Ok(None);
    };
    if serde_json::from_str::<DispatchArgs>(&saved)? != *args {
        return Err(CalmError::Conflict(
            "dispatch name already identifies a different contract".into(),
        ));
    }
    Ok(Some(sqlx::query_as(
        "SELECT name,task_key,report_card_id,block_id,created_at_ms FROM planner_dispatch_receipts WHERE track_id=?1 AND name=?2")
        .bind(track.as_str()).bind(args.name()).fetch_one(&mut **tx).await?))
}

fn declaration_payload(args: &DispatchArgs, task_key: &str) -> Value {
    let no_gate_reason = match args {
        DispatchArgs::Empty { .. } => {
            "Semantic acceptance is reviewed from the completion report; it is not a machine gate or file candidate qualification."
        }
        DispatchArgs::VerifiedCandidate { .. } => {
            "Consumer semantic acceptance is reviewed from its completion report; input qualification follows the source candidate policy."
        }
    };
    json!({
        "key": task_key, "kind": "codex", "goal": args.goal(),
        "acceptance": args.acceptance(), "ready": true, "declared_by": report_blocks::tasks::PLANNER_DECLARATION_AUTHOR,
        "no_gate_reason": no_gate_reason,
        "context": {"neige_execution": args.execution()}
    })
}

pub(super) fn prepare(doc: &ReportDoc, args: &DispatchArgs, task_key: &str) -> Result<ReportDocOp> {
    let content = report_blocks::render_data_block(
        report_blocks::KIND_TASK,
        &declaration_payload(args, task_key),
    )
    .map_err(CalmError::BadRequest)?;
    Ok(ReportDocOp::UpsertBlock {
        id: None,
        kind: report_blocks::KIND_TASK.into(),
        content,
        if_rev: None,
        if_doc_rev: Some(
            doc.doc_rev()
                .map_err(|e| CalmError::Internal(e.to_string()))?,
        ),
        position: None,
    })
}

pub(super) async fn insert_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    args: &DispatchArgs,
    receipt: &DispatchReceipt,
) -> Result<()> {
    sqlx::query("INSERT INTO planner_dispatch_receipts(track_id,name,contract_json,task_key,report_card_id,block_id,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7)")
        .bind(track.as_str()).bind(args.name()).bind(serde_json::to_string(args)?)
        .bind(&receipt.task_key).bind(&receipt.report_card_id).bind(&receipt.block_id)
        .bind(receipt.created_at_ms).execute(&mut **tx).await?;
    Ok(())
}

pub(super) async fn snapshot_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    receipt: &DispatchReceipt,
    args: &DispatchArgs,
    task_budget_default: i64,
) -> Result<Value> {
    let configured_default: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key=?1")
            .bind(crate::routes::settings::TASK_BUDGET_DEFAULT_KEY)
            .fetch_optional(&mut **tx)
            .await?;
    let task_budget_default = crate::routes::settings::effective_task_budget_default(
        configured_default.as_deref(),
        task_budget_default,
    );
    let (_, blocks) = super::report_blocks_snapshot_tx(tx, track.as_str()).await?;
    let (declarations, local) = report_blocks::tasks::project_task_declarations(&blocks);
    // Compare only the existing execution-root contract fields. Readiness,
    // User release and other admission controls do not change this contract.
    // This describes the current declaration, never the frozen attempt.
    let current_block = blocks
        .iter()
        .enumerate()
        .find(|(_, block)| block.id == receipt.block_id);
    let contract_status = match current_block {
        Some((index, block))
            if declarations
                .iter()
                .filter(|d| d.key == receipt.task_key)
                .count()
                == 1
                && declarations.iter().any(|d| {
                    d.block_id == receipt.block_id && d.key == receipt.task_key && !d.tombstone
                })
                && local[index].is_empty() =>
        {
            if calm_types::task_recovery::task_root_hash_preimage(&block.payload)
                == calm_types::task_recovery::task_root_hash_preimage(&declaration_payload(
                    args,
                    &receipt.task_key,
                ))
            {
                "matches_dispatch"
            } else {
                "differs_from_dispatch"
            }
        }
        _ => "unavailable",
    };
    let verdicts = crate::db::sqlite::evaluate_schedulability_with_task_budget_default(
        tx,
        track.as_str(),
        &declarations,
        &local,
        task_budget_default,
    )
    .await?;
    let declaration_present = declarations
        .iter()
        .any(|d| d.block_id == receipt.block_id && d.key == receipt.task_key);
    let declaration_withdrawn = declarations
        .iter()
        .any(|d| d.block_id == receipt.block_id && d.key == receipt.task_key && d.tombstone);
    let verdicts: Vec<_> = verdicts
        .into_iter()
        .filter(|v| v.key == receipt.task_key || v.block_id == receipt.block_id)
        .collect();
    let allocation =
        crate::db::sqlite::task_attempt_current_tx(tx, track.as_str(), &receipt.task_key).await?;
    let task = match &allocation {
        Some(a) => crate::db::sqlite::task_get_tx(tx, &a.attempt_id).await?,
        None => None,
    };
    let track_state = crate::track_lifecycle::track_get_tx(tx, track).await?;
    let blocking_reason = match &allocation {
        Some(allocation) => {
            crate::task_recovery::current_blocking_reason_tx(
                tx,
                &track_state,
                allocation,
                task.as_ref(),
                task_budget_default,
            )
            .await?
        }
        None => None,
    };
    let candidate_input = if matches!(args, DispatchArgs::VerifiedCandidate { .. }) {
        Some(match &task {
            Some(task) => compact_candidate_input(&crate::file_delivery::view_tx(tx, task).await?),
            None => {
                json!({"kind":"input-admission", "state":"unavailable", "reason":"No current task allocation; inspect declaration diagnostics"})
            }
        })
    } else {
        None
    };
    let executor_environment = match task.as_ref() {
        Some(task) => crate::task_recovery::executor_statement(task)?.environment,
        None => {
            json!({"executor":"codex", "note":"No current execution allocation; original requested grants are in requested_executor_environment"})
        }
    };
    let mut response = json!({
        "requested_executor_environment": crate::dedicated_codex::executor_environment_with_plugins(args.plugin_tools()),
        "receipt": receipt,
        "current": {
            "as_of_ms": now_ms(), "contract_status": contract_status,
            "track": {"lifecycle": track_state.lifecycle, "archived_at": track_state.archived_at, "lifecycle_allows_scheduling": crate::scheduler::lifecycle_allows_scheduling(track_state.lifecycle)},
            "blocking_reason": blocking_reason, "declaration_present": declaration_present,
            "declaration_unavailable": !declaration_present, "declaration_withdrawn": declaration_withdrawn, "diagnostics": verdicts,
            "allocation": allocation,
            "task": task.map(|t| json!({"attempt_id": t.id, "status": t.status, "status_detail": t.status_detail, "worker_card_id": t.worker_card_id})),
            // Stated up front so a Planner never learns the envelope at failure time.
            "executor_environment": executor_environment
        }
    });
    if let Some(input) = candidate_input {
        response["current"]["candidate_input"] = input;
    }
    Ok(response)
}

/// Project existing evidence only. Never return review history or policy commands,
/// and never treat the requested Dispatch input as the actual execution contract.
fn compact_candidate_input(view: &Value) -> Value {
    fn fields(value: &Value, names: &[&str]) -> Value {
        Value::Object(
            names
                .iter()
                .filter_map(|name| value.get(*name).map(|v| ((*name).into(), v.clone())))
                .collect(),
        )
    }
    if view.is_null() {
        return json!({"kind":"input-admission", "state":"unavailable", "reason":"Current task has no file delivery"});
    }
    let mut result = fields(view, &["state", "failure", "qualified", "qualification"]);
    result["kind"] = json!("input-admission");
    for (source, target, keys) in [
        (
            "contract",
            "contract",
            &["role", "producer", "slot", "purpose"][..],
        ),
        (
            "publication",
            "publication",
            &["operation_id", "state", "failure", "reason"][..],
        ),
        (
            "candidate",
            "candidate",
            &["state", "publication_operation_id", "snapshot"][..],
        ),
        (
            "verification",
            "verification",
            &[
                "operation_id",
                "state",
                "passed",
                "failure",
                "failing_step",
                "exit_code",
                "status_detail",
            ][..],
        ),
        (
            "review",
            "review",
            &[
                "reviewer",
                "review_attempt_id",
                "review_operation_id",
                "state",
                "reason",
            ][..],
        ),
        (
            "input",
            "preparation",
            &[
                "state",
                "publication_operation_id",
                "verification_operation_id",
                "decision_event_id",
            ][..],
        ),
        ("decision", "decision", &["state", "event_id"][..]),
    ] {
        if let Some(value) = view.get(source) {
            result[target] = fields(value, keys);
        }
    }
    // A report may say passed while its operation failed to settle. Preserve
    // both existing states, without copying report findings or repair history.
    if let Some(operation) = view
        .get("review")
        .and_then(|review| review.get("operation"))
    {
        result["review"]["operation"] = fields(operation, &["state", "failure"]);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_diagnostic_retains_failure_details_without_full_evidence() {
        let view = json!({
            "qualified":false,"qualification":{"qualified":false,"reason":"waiting for acceptance"},
            "publication":{"operation_id":"publication","state":"failed","failure":"capture failed"},
            "verification":{"operation_id":"checks","state":"succeeded","passed":false,
                "failing_step":"stdlib-tests","exit_code":7,"status_detail":"check failed",
                "policy":{"steps":[{"cmd":"bulky command"}]},"log_tail":"bulky logs"},
            "review":{"reviewer":"review","review_attempt_id":"review-attempt","review_operation_id":"review-op",
                "state":"passed","operation":{"state":"failed","failure":"settlement failed"},
                "blocking_findings":["bulky findings"],"finding_responses":["bulky history"]},
            "repair":{"history":"bulky lineage"}
        });
        let out = compact_candidate_input(&view);
        assert_eq!(out["publication"], view["publication"]);
        assert_eq!(out["verification"]["failing_step"], "stdlib-tests");
        assert_eq!(out["verification"]["exit_code"], 7);
        assert_eq!(out["verification"]["status_detail"], "check failed");
        assert_eq!(out["review"]["state"], "passed");
        assert_eq!(out["review"]["operation"], view["review"]["operation"]);
        assert_eq!(out["qualification"], view["qualification"]);
        assert!(!out.to_string().contains("bulky"), "{out}");
        assert_eq!(out["kind"], "input-admission");
    }
}
