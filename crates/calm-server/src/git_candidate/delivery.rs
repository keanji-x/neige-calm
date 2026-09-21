//! `task_git_deliveries`: one row per delivery attempt of one worker attempt.
//!
//! The row is written in the report transaction (`ordinal = 1`) before the forge Operation is
//! submitted, so a crash between the two leaves a durable record the scheduler re-submits with
//! the same `operation_key`. Settlement writes the six settlement columns once, in one UPDATE
//! guarded by `WHERE settlement IS NULL`; the migration's trigger is the backstop.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use calm_types::forge_git::{
    GIT_DELIVERY_OUTPUT_PROBE_SCRIPT, GIT_DELIVERY_PROBE_SCRIPT, GIT_DELIVERY_SCRIPT,
    GIT_LEASE_PROVENANCE_SCRIPT,
};
use calm_types::git_candidate::{DeliveryFailureCode, DeliveryWakeReason};
use sqlx::Row;

use super::candidate::CandidateRow;
use crate::db::write_in_tx_typed;
use crate::error::{CalmError, Result};
use crate::event::{FieldSource, ForgeEventSpec};
use crate::mcp_server::registry::AppContext;
use crate::mcp_server::tools::emit::{GIT_FORGE_PLUGIN_ID, worker_delivery_payload};
use crate::mcp_server::transport::{
    ForgeActionSubmission, PluginForgePayload, submit_forge_action_with_key,
};
use crate::model::new_id;
use crate::operation::forge_action_adapter::{FORGE_ACTION_KIND, ForgeActionResultFile, ProbeSpec};
use crate::operation::workspace_lease::facts::{
    LeaseStates, latest_workspace_lease_for_card_tx, workspace_lease_by_id_tx,
};
use crate::operation::workspace_lease::{
    DeliveryPolicy, WorkspaceLease, workspace_slice_branch_for,
};
use crate::operation::{OperationRuntime, Tx};

/// Fixed sentences for a failed delivery, keyed by the script's exit code or the kernel's
/// classification (`prompts/delivery/git-delivery-failures.md`). Rust only maps a code to a key.
const FAILURE_SENTENCES: &str = include_str!("../../prompts/delivery/git-delivery-failures.md");

/// Evidence copied from `<result_path>.stdout` into `failure_reason` is cut to this many lines...
pub(crate) const FAILURE_EVIDENCE_MAX_LINES: usize = 8;
/// ...of at most this many bytes each. A production-shaped provenance observation line
/// (`provenance realpath=<…/.claude/worktrees/<t>/<c>> common_dir=<…> registered=0`) measures
/// 250–320 bytes; the cap must keep its `common_dir=` / `registered=` facts, which are what the
/// 10 / 12 sentences point the reader at.
pub(crate) const FAILURE_EVIDENCE_MAX_LINE_BYTES: usize = 1024;

/// The `last_error_class` values of a forge action that ran (or was proven not to have landed):
/// with no result file these are the only classes that mean "the script did not pin a ref".
const COMMIT_FAILED_CLASSES: &[&str] = &["action-failed", "action-not-landed"];

/// The delivery script's exit codes that are provenance mismatches (D2 code table).
const PROVENANCE_MISMATCH_EXIT_CODES: &[i32] = &[10, 11, 12, 15];

/// The six settlement columns of one row, decoded as the two shapes the CHECK admits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DeliverySettled {
    Candidate {
        settled_event_id: i64,
        wake_reason: DeliveryWakeReason,
    },
    Failed {
        settled_event_id: i64,
        code: DeliveryFailureCode,
        reason: String,
        retry_allowed: bool,
        wake_reason: DeliveryWakeReason,
    },
}

/// One `task_git_deliveries` row, every column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeliveryRow {
    pub delivery_id: String,
    pub track_id: String,
    pub producer_attempt_id: String,
    pub card_id: String,
    pub lease_id: String,
    pub ordinal: i64,
    pub operation_key: String,
    pub forge_idempotency_key: String,
    pub predecessor_delivery_id: Option<String>,
    pub request_idempotency_key: Option<String>,
    pub reason: Option<String>,
    pub created_at_ms: i64,
    /// `None` while unsettled.
    pub settlement: Option<DeliverySettled>,
}

/// One unsettled delivery row and whether a forge Operation carries its `operation_key`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UnsettledDelivery {
    pub row: DeliveryRow,
    /// The `operations.id` of the forge action submitted under the row's key; `None` when the
    /// process died between the report transaction and the submission.
    pub operation_id: Option<String>,
}

const DELIVERY_COLUMNS: &str = "d.delivery_id, d.track_id, d.producer_attempt_id, d.card_id, \
     d.lease_id, d.ordinal, d.operation_key, d.forge_idempotency_key, d.predecessor_delivery_id, \
     d.request_idempotency_key, d.reason, d.created_at_ms, d.settlement, d.settled_event_id, \
     d.failure_code, d.failure_reason, d.retry_allowed, d.wake_reason";

/// The forge `idem_key` of one delivery; the full idempotency key is
/// `<plugin>:<track>:<card>:<this>` (`submit_forge_action_with_key`).
pub(crate) fn delivery_idem_key(delivery_id: &str) -> String {
    format!("git.commit:d:{delivery_id}")
}

/// `refs/neige/candidates/<track>/<card>/<delivery_id>` — the ref the delivery script pins.
pub(crate) fn candidate_ref_name(track_id: &str, card_id: &str, delivery_id: &str) -> String {
    format!("refs/neige/candidates/{track_id}/{card_id}/{delivery_id}")
}

/// The commit message the delivery script uses when it has to commit; human-readable only.
pub(crate) fn delivery_message(track_id: &str, card_id: &str, delivery_id: &str) -> String {
    format!("neige: worker {card_id} @ track {track_id} (delivery {delivery_id})")
}

/// The argv of one delivery: the provenance function and the delivery script joined into one
/// `sh -c` text, followed by the six positional parameters the script reads.
pub(crate) fn delivery_argv(
    message: &str,
    branch: &str,
    ref_name: &str,
    base_sha: &str,
    canonical_path: &str,
    git_common_dir: &str,
) -> Vec<String> {
    vec![
        "sh".into(),
        "-c".into(),
        format!("{GIT_LEASE_PROVENANCE_SCRIPT}\n{GIT_DELIVERY_SCRIPT}"),
        "sh".into(),
        message.into(),
        branch.into(),
        ref_name.into(),
        base_sha.into(),
        canonical_path.into(),
        git_common_dir.into(),
    ]
}

/// The four-field extraction table of a kernel delivery's `worktree.committed`
/// (`branch`, `commit_sha`, `delivery_id`, `base_is_ancestor` from the script's JSON line).
pub(super) fn worktree_committed_delivery_fields() -> ForgeEventSpec {
    let json_field = |path: &str| FieldSource::JsonField { path: path.into() };
    ForgeEventSpec {
        event_kind: "worktree.committed".into(),
        fields: [
            ("branch", "/branch"),
            ("commit_sha", "/commit"),
            ("delivery_id", "/delivery_id"),
            ("base_is_ancestor", "/base_is_ancestor"),
        ]
        .into_iter()
        .map(|(field, path)| (field.to_string(), json_field(path)))
        .collect(),
    }
}

/// The forge payload that runs one delivery. `Err` when the lease is not a kernel-delivery
/// lease with a recorded base — unreachable by construction (the row is only inserted for one).
pub(crate) fn forge_payload_for(
    delivery: &DeliveryRow,
    lease: &WorkspaceLease,
) -> Result<PluginForgePayload> {
    if lease.delivery_policy != Some(DeliveryPolicy::Kernel) {
        return Err(CalmError::Internal(format!(
            "delivery {} on lease {} whose delivery_policy is not kernel",
            delivery.delivery_id, lease.lease_id
        )));
    }
    let Some(base) = lease.base.as_ref() else {
        return Err(CalmError::Internal(format!(
            "delivery {} on lease {} without a recorded base",
            delivery.delivery_id, lease.lease_id
        )));
    };
    let canonical_path = utf8(&base.canonical_path, "canonical_path")?;
    let git_common_dir = utf8(&base.git_common_dir, "git_common_dir")?;
    let branch = workspace_slice_branch_for(&delivery.track_id, &delivery.card_id)?;
    let ref_name = candidate_ref_name(&delivery.track_id, &delivery.card_id, &delivery.delivery_id);
    let message = delivery_message(&delivery.track_id, &delivery.card_id, &delivery.delivery_id);
    Ok(worker_delivery_payload(
        delivery_idem_key(&delivery.delivery_id),
        delivery_argv(
            &message,
            &branch,
            &ref_name,
            &base.base_sha,
            canonical_path,
            git_common_dir,
        ),
        worktree_committed_delivery_fields(),
        ProbeSpec {
            probe_argv: vec![
                "sh".into(),
                "-c".into(),
                GIT_DELIVERY_PROBE_SCRIPT.into(),
                "sh".into(),
                ref_name.clone(),
            ],
            output_probe_argv: Some(vec![
                "sh".into(),
                "-c".into(),
                GIT_DELIVERY_OUTPUT_PROBE_SCRIPT.into(),
                "sh".into(),
                branch,
                ref_name,
                base.base_sha.clone(),
            ]),
        },
    ))
}

fn utf8<'a>(path: &'a std::path::Path, what: &str) -> Result<&'a str> {
    path.to_str().ok_or_else(|| {
        CalmError::Internal(format!(
            "workspace lease {what} {} is not UTF-8",
            path.display()
        ))
    })
}

/// Insert the first delivery row of one attempt (`ordinal = 1`, fresh `delivery_id` and
/// `operation_key`). The lease must be a kernel-delivery lease: the row exists only for one.
pub(crate) async fn insert_initial_delivery_tx(
    tx: &mut Tx<'_>,
    track_id: &str,
    card_id: &str,
    producer_attempt_id: &str,
    lease: &WorkspaceLease,
    now_ms: i64,
) -> Result<DeliveryRow> {
    if lease.delivery_policy != Some(DeliveryPolicy::Kernel) {
        return Err(CalmError::Internal(format!(
            "attempt {producer_attempt_id}: lease {} delivery_policy is not kernel",
            lease.lease_id
        )));
    }
    let delivery_id = new_id();
    let row = DeliveryRow {
        delivery_id: delivery_id.clone(),
        track_id: track_id.to_string(),
        producer_attempt_id: producer_attempt_id.to_string(),
        card_id: card_id.to_string(),
        lease_id: lease.lease_id.clone(),
        ordinal: 1,
        operation_key: new_id(),
        forge_idempotency_key: format!(
            "{GIT_FORGE_PLUGIN_ID}:{track_id}:{card_id}:{}",
            delivery_idem_key(&delivery_id)
        ),
        predecessor_delivery_id: None,
        request_idempotency_key: None,
        reason: None,
        created_at_ms: now_ms,
        settlement: None,
    };
    sqlx::query(
        "INSERT INTO task_git_deliveries (delivery_id, track_id, producer_attempt_id, card_id, \
         lease_id, ordinal, operation_key, forge_idempotency_key, predecessor_delivery_id, \
         request_idempotency_key, reason, created_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL, NULL, ?9)",
    )
    .bind(&row.delivery_id)
    .bind(&row.track_id)
    .bind(&row.producer_attempt_id)
    .bind(&row.card_id)
    .bind(&row.lease_id)
    .bind(row.ordinal)
    .bind(&row.operation_key)
    .bind(&row.forge_idempotency_key)
    .bind(row.created_at_ms)
    .execute(&mut **tx)
    .await?;
    Ok(row)
}

/// Insert the retry row of one attempt (`calm.task.delivery{action:"retry"}`, slice 3): the
/// predecessor's `ordinal + 1`, `predecessor_delivery_id` naming it, the request key and reason
/// the Planner sent (the replay key). Fresh `delivery_id` and `operation_key`; same lease and
/// card as the predecessor. The caller has admitted the retry (predecessor failed, retryable,
/// workspace present) in the same transaction.
pub(crate) async fn insert_retry_delivery_tx(
    tx: &mut Tx<'_>,
    predecessor: &DeliveryRow,
    request_idempotency_key: &str,
    reason: Option<&str>,
    now_ms: i64,
) -> Result<DeliveryRow> {
    let delivery_id = new_id();
    let row = DeliveryRow {
        delivery_id: delivery_id.clone(),
        track_id: predecessor.track_id.clone(),
        producer_attempt_id: predecessor.producer_attempt_id.clone(),
        card_id: predecessor.card_id.clone(),
        lease_id: predecessor.lease_id.clone(),
        ordinal: predecessor.ordinal + 1,
        operation_key: new_id(),
        forge_idempotency_key: format!(
            "{GIT_FORGE_PLUGIN_ID}:{}:{}:{}",
            predecessor.track_id,
            predecessor.card_id,
            delivery_idem_key(&delivery_id)
        ),
        predecessor_delivery_id: Some(predecessor.delivery_id.clone()),
        request_idempotency_key: Some(request_idempotency_key.to_string()),
        reason: reason.map(str::to_string),
        created_at_ms: now_ms,
        settlement: None,
    };
    sqlx::query(
        "INSERT INTO task_git_deliveries (delivery_id, track_id, producer_attempt_id, card_id, \
         lease_id, ordinal, operation_key, forge_idempotency_key, predecessor_delivery_id, \
         request_idempotency_key, reason, created_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
    )
    .bind(&row.delivery_id)
    .bind(&row.track_id)
    .bind(&row.producer_attempt_id)
    .bind(&row.card_id)
    .bind(&row.lease_id)
    .bind(row.ordinal)
    .bind(&row.operation_key)
    .bind(&row.forge_idempotency_key)
    .bind(&row.predecessor_delivery_id)
    .bind(&row.request_idempotency_key)
    .bind(&row.reason)
    .bind(row.created_at_ms)
    .execute(&mut **tx)
    .await?;
    Ok(row)
}

/// The report transaction's hook (D2 "persistent hand-off"): a successful report of an attached
/// attempt whose card holds an active kernel-delivery lease inserts the first delivery row in the
/// same transaction as the task flip. Isolated attempts and legacy leases (`delivery_policy`
/// NULL — a slice 1 lease, a plain directory op) insert nothing and keep today's path. `None`
/// when no row was inserted.
pub(crate) async fn insert_initial_delivery_if_kernel_tx(
    tx: &mut Tx<'_>,
    track_id: &str,
    card_id: &str,
    producer_attempt_id: &str,
    now_ms: i64,
) -> Result<Option<DeliveryRow>> {
    if crate::isolated_codex::lookup::is_isolated_task_tx(tx, producer_attempt_id).await? {
        return Ok(None);
    }
    let Some(lease) = latest_workspace_lease_for_card_tx(tx, card_id, LeaseStates::Active).await?
    else {
        return Ok(None);
    };
    if lease.delivery_policy != Some(DeliveryPolicy::Kernel) {
        return Ok(None);
    }
    insert_initial_delivery_tx(tx, track_id, card_id, producer_attempt_id, &lease, now_ms)
        .await
        .map(Some)
}

/// Whether `attempt_id` has a delivery row — the pairing rule of `is_deferred_self_report`
/// (the row is written in the same transaction as `task.completed` and is never deleted below
/// a Track, so live push and replay read the same answer).
pub(crate) async fn attempt_has_delivery(
    pool: &sqlx::SqlitePool,
    attempt_id: &str,
) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM task_git_deliveries WHERE producer_attempt_id = ?1)",
    )
    .bind(attempt_id)
    .fetch_one(pool)
    .await?)
}

/// Submit one delivery's forge action under the row's persisted `operation_key` (the one
/// submission function, D2): the report handler and the scheduler's re-submission both come
/// here, so the semantic hash and the key are equal and the runtime dedups the second call.
pub(crate) async fn submit_delivery(
    runtime: &Arc<OperationRuntime>,
    gate_logs_dir: &Path,
    delivery: &DeliveryRow,
    lease: &WorkspaceLease,
) -> Result<ForgeActionSubmission> {
    let payload = forge_payload_for(delivery, lease)?;
    submit_forge_action_with_key(
        runtime,
        gate_logs_dir,
        GIT_FORGE_PLUGIN_ID,
        delivery.track_id.clone(),
        delivery.card_id.clone(),
        PathBuf::from(&lease.path),
        payload,
        delivery.operation_key.clone(),
    )
    .await
    .map_err(|error| CalmError::Internal(format!("delivery {}: {error}", delivery.delivery_id)))?
    .map_err(|error| CalmError::Internal(format!("delivery {}: {error}", delivery.delivery_id)))
}

/// After the report transaction: read the attempt's delivery row back and submit it. `Ok(false)`
/// when the attempt has no row (a failed first report, or no kernel-delivery lease) — the caller
/// runs the legacy auto-commit; `Ok(true)` when the row was submitted.
pub(crate) async fn submit_reported_delivery(
    ctx: &Arc<AppContext>,
    attempt_id: &str,
) -> std::result::Result<bool, String> {
    let attempt_id = attempt_id.to_string();
    let found = write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let Some(delivery) = delivery_latest_for_attempt_tx(tx, &attempt_id).await? else {
                return Ok(None);
            };
            let lease = lease_for_delivery_tx(tx, &delivery).await?;
            Ok(Some((delivery, lease)))
        })
    })
    .await
    .map_err(|error| error.to_string())?;
    let Some((delivery, lease)) = found else {
        return Ok(false);
    };
    let Some(runtime) = ctx.operation_runtime.get().cloned() else {
        return Err("operation runtime not bound".into());
    };
    submit_delivery(&runtime, &ctx.gate_logs_dir, &delivery, &lease)
        .await
        .map_err(|error| error.to_string())?;
    Ok(true)
}

/// The lease row a delivery names, in whatever state it is now (the worker has usually released
/// it by the time the delivery settles). A delivery without its lease row is an internal error:
/// the row references it and nothing deletes leases below a Track.
pub(crate) async fn lease_for_delivery_tx(
    tx: &mut Tx<'_>,
    delivery: &DeliveryRow,
) -> Result<WorkspaceLease> {
    workspace_lease_by_id_tx(tx, &delivery.lease_id)
        .await?
        .ok_or_else(|| {
            CalmError::Internal(format!(
                "delivery {} names lease {} which has no row",
                delivery.delivery_id, delivery.lease_id
            ))
        })
}

/// The highest-`ordinal` delivery row of one attempt.
pub(crate) async fn delivery_latest_for_attempt_tx(
    tx: &mut Tx<'_>,
    producer_attempt_id: &str,
) -> Result<Option<DeliveryRow>> {
    let sql = format!(
        "SELECT {DELIVERY_COLUMNS} FROM task_git_deliveries d \
         WHERE d.producer_attempt_id = ?1 ORDER BY d.ordinal DESC LIMIT 1"
    );
    let row = sqlx::query(&sql)
        .bind(producer_attempt_id)
        .fetch_optional(&mut **tx)
        .await?;
    row.map(row_to_delivery).transpose()
}

/// The retry row one request key already produced for this attempt in this Track (the replay
/// key of `calm.task.delivery{retry}`, Track-scoped: another Track's Planner quoting this
/// attempt id and key finds nothing and falls through to admission); `ordinal = 1` rows carry
/// no key and are never returned.
pub(crate) async fn delivery_by_request_key_tx(
    tx: &mut Tx<'_>,
    track_id: &str,
    producer_attempt_id: &str,
    request_idempotency_key: &str,
) -> Result<Option<DeliveryRow>> {
    let sql = format!(
        "SELECT {DELIVERY_COLUMNS} FROM task_git_deliveries d \
         WHERE d.track_id = ?1 AND d.producer_attempt_id = ?2 AND d.request_idempotency_key = ?3"
    );
    let row = sqlx::query(&sql)
        .bind(track_id)
        .bind(producer_attempt_id)
        .bind(request_idempotency_key)
        .fetch_optional(&mut **tx)
        .await?;
    row.map(row_to_delivery).transpose()
}

pub(crate) async fn delivery_by_id_tx(
    tx: &mut Tx<'_>,
    delivery_id: &str,
) -> Result<Option<DeliveryRow>> {
    let sql =
        format!("SELECT {DELIVERY_COLUMNS} FROM task_git_deliveries d WHERE d.delivery_id = ?1");
    let row = sqlx::query(&sql)
        .bind(delivery_id)
        .fetch_optional(&mut **tx)
        .await?;
    row.map(row_to_delivery).transpose()
}

/// Every unsettled delivery row of one Track with the id of the forge Operation submitted under
/// its `operation_key`, if any (`LEFT JOIN operations`, the isolated allocation replay's shape).
pub(crate) async fn unsettled_deliveries_for_track_tx(
    tx: &mut Tx<'_>,
    track_id: &str,
) -> Result<Vec<UnsettledDelivery>> {
    let sql = format!(
        "SELECT {DELIVERY_COLUMNS}, o.id AS operation_id FROM task_git_deliveries d \
         LEFT JOIN operations o ON o.operation_key = d.operation_key AND o.kind = ?2 \
         WHERE d.track_id = ?1 AND d.settlement IS NULL \
         ORDER BY d.created_at_ms ASC, d.delivery_id ASC"
    );
    let rows = sqlx::query(&sql)
        .bind(track_id)
        .bind(FORGE_ACTION_KIND)
        .fetch_all(&mut **tx)
        .await?;
    rows.into_iter()
        .map(|row| {
            let operation_id: Option<String> = row.try_get("operation_id")?;
            Ok(UnsettledDelivery {
                row: row_to_delivery(row)?,
                operation_id,
            })
        })
        .collect()
}

/// Settle one delivery as a candidate: the settlement UPDATE (guarded by
/// `WHERE settlement IS NULL`) and, when it took, the candidate row. Returns the rows the UPDATE
/// affected: `0` means the row was already settled and nothing was written.
pub(crate) async fn settle_candidate_tx(
    tx: &mut Tx<'_>,
    delivery_id: &str,
    event_id: i64,
    wake_reason: DeliveryWakeReason,
    candidate: &CandidateRow,
) -> Result<u64> {
    if candidate.candidate_id != delivery_id {
        return Err(CalmError::Internal(format!(
            "candidate {} does not name delivery {delivery_id}",
            candidate.candidate_id
        )));
    }
    let affected = sqlx::query(
        "UPDATE task_git_deliveries SET settlement = 'candidate', settled_event_id = ?2, \
         failure_code = NULL, failure_reason = NULL, retry_allowed = NULL, wake_reason = ?3 \
         WHERE delivery_id = ?1 AND settlement IS NULL",
    )
    .bind(delivery_id)
    .bind(event_id)
    .bind(wake_reason_column(wake_reason))
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if affected == 0 {
        return Ok(0);
    }
    super::candidate::insert_candidate_tx(tx, candidate).await?;
    Ok(affected)
}

/// Settle one delivery as failed: the six columns in one UPDATE guarded by
/// `WHERE settlement IS NULL`, `wake_reason = 'failed'`. Returns the rows affected (`0` = already
/// settled, nothing written).
pub(crate) async fn settle_failed_tx(
    tx: &mut Tx<'_>,
    delivery_id: &str,
    event_id: i64,
    code: DeliveryFailureCode,
    reason: &str,
    retry_allowed: bool,
) -> Result<u64> {
    Ok(sqlx::query(
        "UPDATE task_git_deliveries SET settlement = 'failed', settled_event_id = ?2, \
         failure_code = ?3, failure_reason = ?4, retry_allowed = ?5, wake_reason = ?6 \
         WHERE delivery_id = ?1 AND settlement IS NULL",
    )
    .bind(delivery_id)
    .bind(event_id)
    .bind(code.wire_str())
    .bind(reason)
    .bind(retry_allowed as i64)
    .bind(wake_reason_column(DeliveryWakeReason::Failed))
    .execute(&mut **tx)
    .await?
    .rows_affected())
}

fn wake_reason_column(wake_reason: DeliveryWakeReason) -> &'static str {
    match wake_reason {
        DeliveryWakeReason::Failed => "failed",
        DeliveryWakeReason::UngatedCandidate => "ungated_candidate",
        DeliveryWakeReason::GateAlreadyTerminal => "gate_already_terminal",
        DeliveryWakeReason::DeferredToGate => "deferred_to_gate",
    }
}

fn wake_reason_from_column(value: &str) -> Result<DeliveryWakeReason> {
    match value {
        "failed" => Ok(DeliveryWakeReason::Failed),
        "ungated_candidate" => Ok(DeliveryWakeReason::UngatedCandidate),
        "gate_already_terminal" => Ok(DeliveryWakeReason::GateAlreadyTerminal),
        "deferred_to_gate" => Ok(DeliveryWakeReason::DeferredToGate),
        other => Err(CalmError::Internal(format!(
            "task_git_deliveries.wake_reason {other:?} is not a wake reason"
        ))),
    }
}

fn failure_code_from_column(value: &str) -> Result<DeliveryFailureCode> {
    match value {
        "workspace_missing" => Ok(DeliveryFailureCode::WorkspaceMissing),
        "provenance_mismatch" => Ok(DeliveryFailureCode::ProvenanceMismatch),
        "commit_failed" => Ok(DeliveryFailureCode::CommitFailed),
        "unresolved" => Ok(DeliveryFailureCode::Unresolved),
        other => Err(CalmError::Internal(format!(
            "task_git_deliveries.failure_code {other:?} is not a failure code"
        ))),
    }
}

fn row_to_delivery(row: sqlx::sqlite::SqliteRow) -> Result<DeliveryRow> {
    let delivery_id: String = row.try_get("delivery_id")?;
    let settlement: Option<String> = row.try_get("settlement")?;
    let settled_event_id: Option<i64> = row.try_get("settled_event_id")?;
    let failure_code: Option<String> = row.try_get("failure_code")?;
    let failure_reason: Option<String> = row.try_get("failure_reason")?;
    let retry_allowed: Option<i64> = row.try_get("retry_allowed")?;
    let wake_reason: Option<String> = row.try_get("wake_reason")?;
    let settlement = match (
        settlement.as_deref(),
        settled_event_id,
        failure_code,
        failure_reason,
        retry_allowed,
        wake_reason,
    ) {
        (None, None, None, None, None, None) => None,
        (Some("candidate"), Some(settled_event_id), None, None, None, Some(wake_reason)) => {
            Some(DeliverySettled::Candidate {
                settled_event_id,
                wake_reason: wake_reason_from_column(&wake_reason)?,
            })
        }
        (
            Some("failed"),
            Some(settled_event_id),
            Some(code),
            Some(reason),
            Some(retry_allowed),
            Some(wake_reason),
        ) => Some(DeliverySettled::Failed {
            settled_event_id,
            code: failure_code_from_column(&code)?,
            reason,
            retry_allowed: retry_allowed != 0,
            wake_reason: wake_reason_from_column(&wake_reason)?,
        }),
        _ => {
            return Err(CalmError::Internal(format!(
                "task_git_deliveries {delivery_id} settlement columns are not one of the CHECK shapes"
            )));
        }
    };
    Ok(DeliveryRow {
        delivery_id,
        track_id: row.try_get("track_id")?,
        producer_attempt_id: row.try_get("producer_attempt_id")?,
        card_id: row.try_get("card_id")?,
        lease_id: row.try_get("lease_id")?,
        ordinal: row.try_get("ordinal")?,
        operation_key: row.try_get("operation_key")?,
        forge_idempotency_key: row.try_get("forge_idempotency_key")?,
        predecessor_delivery_id: row.try_get("predecessor_delivery_id")?,
        request_idempotency_key: row.try_get("request_idempotency_key")?,
        reason: row.try_get("reason")?,
        created_at_ms: row.try_get("created_at_ms")?,
        settlement,
    })
}

/// Map a terminal forge action to `(failure_code, failure_reason, retry_allowed)` (D2 code table).
/// Order is the table's: a missing workspace first (the directory's absence also makes the spawn
/// fail without a class, which would otherwise read as `unresolved`); then the script's own exit
/// code from the result file; then the class of an action that left no file; everything else is
/// `unresolved` — a class this table does not name is never read as something it is not.
pub(crate) fn classify_failure(
    result: Option<&ForgeActionResultFile>,
    last_error_class: Option<&str>,
    workspace_present: bool,
) -> (DeliveryFailureCode, String, bool) {
    if !workspace_present {
        return (
            DeliveryFailureCode::WorkspaceMissing,
            failure_sentence("workspace_missing", None),
            false,
        );
    }
    match result {
        Some(file) if PROVENANCE_MISMATCH_EXIT_CODES.contains(&file.exit_code) => {
            let mut reason = failure_sentence(&file.exit_code.to_string(), None);
            if file.exit_code != 11 {
                for line in evidence_lines(&file.stdout) {
                    reason.push('\n');
                    reason.push_str(&line);
                }
            }
            (DeliveryFailureCode::ProvenanceMismatch, reason, true)
        }
        Some(file) if file.exit_code == 13 || file.exit_code == 14 => (
            DeliveryFailureCode::CommitFailed,
            failure_sentence(&file.exit_code.to_string(), None),
            true,
        ),
        Some(file) if file.exit_code != 0 => (
            DeliveryFailureCode::CommitFailed,
            failure_sentence("git", Some(file.exit_code)),
            true,
        ),
        None if last_error_class.is_some_and(|class| COMMIT_FAILED_CLASSES.contains(&class)) => (
            DeliveryFailureCode::CommitFailed,
            failure_sentence("no_result", None),
            true,
        ),
        _ => (
            DeliveryFailureCode::Unresolved,
            failure_sentence("unresolved", None),
            true,
        ),
    }
}

/// The default arm of the code table with one detail line: the kernel could not prove what the
/// delivery did (a result the ref does not confirm, a result event it cannot read).
pub(crate) fn unresolved_failure(detail: &str) -> (DeliveryFailureCode, String, bool) {
    (
        DeliveryFailureCode::Unresolved,
        format!("{}\n{detail}", failure_sentence("unresolved", None)),
        true,
    )
}

/// The stdout lines the script printed before exiting, cut to
/// [`FAILURE_EVIDENCE_MAX_LINES`] × [`FAILURE_EVIDENCE_MAX_LINE_BYTES`] (at a char boundary).
fn evidence_lines(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(FAILURE_EVIDENCE_MAX_LINES)
        .map(|line| {
            let mut end = line.len().min(FAILURE_EVIDENCE_MAX_LINE_BYTES);
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            line[..end].to_string()
        })
        .collect()
}

static FAILURE_TABLE: LazyLock<Vec<(&'static str, &'static str)>> = LazyLock::new(|| {
    FAILURE_SENTENCES
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .filter_map(|line| line.split_once('\t'))
        .collect()
});

/// The fixed sentence for `key`; `code` fills the `{code}` placeholder of the `git` line. A key
/// the table does not carry (a defect in this binary; `classify_failure_maps_every_code` pins
/// every key the mapping uses) falls back to the key itself rather than to nothing.
pub(super) fn failure_sentence(key: &str, code: Option<i32>) -> String {
    let sentence = FAILURE_TABLE
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, sentence)| *sentence)
        .unwrap_or(key);
    match code {
        Some(code) => sentence.replace("{code}", &code.to_string()),
        None => sentence.to_string(),
    }
}
