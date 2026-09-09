//! Named dispatch provenance and current read state under the report transaction.
use super::{ReportDoc, ReportDocOp};
use crate::error::{CalmError, Result};
use crate::ids::TrackId;
use crate::mcp_server::registry::ToolCallIdentity;
use crate::model::{CardRole, now_ms};
use calm_types::report_blocks;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DispatchArgs {
    pub name: String,
    pub goal: String,
    pub acceptance: String,
    pub executor: Executor,
    pub workspace: Workspace,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Executor {
    Codex,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Workspace {
    Empty,
}

impl DispatchArgs {
    pub(crate) fn normalize(mut self) -> Result<Self> {
        self.name = self.name.trim().to_owned();
        if self.name.is_empty() || self.name.len() > 200 || self.name.chars().any(char::is_control)
        {
            return Err(CalmError::BadRequest("name must be nonempty, at most 200 UTF-8 bytes after trim, and contain no control characters".into()));
        }
        if self.goal.trim().is_empty() || self.acceptance.trim().is_empty() {
            return Err(CalmError::BadRequest(
                "goal and acceptance must be nonempty".into(),
            ));
        }
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
    .bind(&args.name)
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
        .bind(track.as_str()).bind(&args.name).fetch_one(&mut **tx).await?))
}

pub(super) fn prepare(doc: &ReportDoc, args: &DispatchArgs, task_key: &str) -> Result<ReportDocOp> {
    let content = report_blocks::render_data_block(report_blocks::KIND_TASK, &json!({
        "key": task_key, "kind": "codex", "goal": args.goal,
        "acceptance": args.acceptance, "ready": true, "declared_by": "spec",
        "no_gate_reason": "Semantic acceptance is reviewed from the completion report; it is not a machine gate or file candidate qualification.",
        "context": {"neige_execution": {"version": "isolated-codex-v1", "workspace": "empty"}}
    })).map_err(CalmError::BadRequest)?;
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
        .bind(track.as_str()).bind(&args.name).bind(serde_json::to_string(args)?)
        .bind(&receipt.task_key).bind(&receipt.report_card_id).bind(&receipt.block_id)
        .bind(receipt.created_at_ms).execute(&mut **tx).await?;
    Ok(())
}

pub(super) async fn snapshot_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    receipt: &DispatchReceipt,
    task_budget_default: i64,
) -> Result<Value> {
    let (_, blocks) = super::report_blocks_snapshot_tx(tx, track.as_str()).await?;
    let (declarations, local) = report_blocks::tasks::project_task_declarations(&blocks);
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
    Ok(json!({
        "receipt": receipt,
        "current": {
            "as_of_ms": now_ms(),
            "track": {"lifecycle": track_state.lifecycle, "archived_at": track_state.archived_at, "lifecycle_allows_scheduling": crate::scheduler::lifecycle_allows_scheduling(track_state.lifecycle)},
            "blocking_reason": blocking_reason, "declaration_present": declaration_present,
            "declaration_unavailable": !declaration_present, "declaration_withdrawn": declaration_withdrawn, "diagnostics": verdicts,
            "allocation": allocation,
            "task": task.map(|t| json!({"attempt_id": t.id, "status": t.status, "status_detail": t.status_detail, "worker_card_id": t.worker_card_id}))
        }
    }))
}
