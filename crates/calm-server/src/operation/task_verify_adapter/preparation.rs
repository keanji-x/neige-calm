use super::*;
use crate::model::{Task, TrackWorkspace};
use crate::operation::workspace_lease::WorkspaceLease;
use crate::operation::{PreparedTx, PreparedTxResult, Tx};

#[derive(PartialEq)]
enum CwdSource {
    Declared(String),
    Lease(WorkspaceLease),
    Terminal {
        id: String,
        key: String,
        payload_hash: String,
        cwd: String,
    },
    Track(TrackWorkspace),
}
impl CwdSource {
    fn cwd(&self) -> &str {
        match self {
            Self::Declared(cwd) | Self::Terminal { cwd, .. } => cwd,
            Self::Lease(lease) => &lease.path,
            Self::Track(workspace) => workspace.agent_cwd(),
        }
    }
}
#[derive(PartialEq)]
struct Facts {
    task: Task,
    area_id: String,
    cwd_source: CwdSource,
    cwd: String,
    identity: target::VerifyIdentity,
    reuse: reuse::Facts,
}

struct PreparedVerify {
    // The concrete value never leaves the task-verify owner or enters persisted tx_output.
    claim: crate::operation::preparation_claim::PreparationClaim,
    facts: Facts,
    gate: GateSpec,
    sampled: Option<std::result::Result<target::Sampled, target::SampleFailure>>,
    refs: Option<String>,
    logs: PathBuf,
}

pub(super) async fn collect(
    adapter: &TaskVerifyAdapter,
    input: &Value,
    op: &Operation,
    ctx: &SpawnCtx,
) -> Result<Option<Box<dyn PreparedTx>>> {
    let pool = ctx.operation_repo.sqlite_db();
    // Keep the repository's IMMEDIATE-only transaction discipline: a short SQL-only snapshot
    // avoids shared-cache deferred-reader deadlocks, and ends before any Git subprocess.
    let mut tx = begin_immediate_tx(&pool).await?;
    // Reuse the kernel's exact original-input, phase, owner and expiry predicate before I/O.
    let live = super::super::repo_sqlite::has_live_claim_tx(&mut tx, op).await?;
    if !live {
        tx.rollback().await?;
        // Let the repository report ClaimLost, without observing or modifying external state.
        return Ok(Some(Box::new(LostClaim)));
    }
    let facts = read_facts(&mut tx, input, op).await;
    tx.rollback().await?;
    let (facts, gate) = facts?;
    let deadline = tokio::time::Instant::now() + SAMPLE_TIMEOUT;
    let sampled = match &facts.identity {
        target::VerifyIdentity::Bound {
            lease,
            candidate: Some(candidate),
            ..
        } if gate.cwd.is_none() => {
            let expected = target::Expected::for_candidate(lease, candidate)?;
            Some(
                target::sample_by(Path::new(&facts.cwd), &expected, deadline, SAMPLE_TIMEOUT).await,
            )
        }
        _ => None,
    };
    // A missing or failed digest preserves reuse's existing fall-through-to-running behavior.
    // It is collected before taking a writer, under the same bound as the target sample.
    let attempt = serde_json::from_value::<TaskVerifyOperationPayload>(input.clone())?.attempt;
    let refs = if reuse::needs_refs(
        &facts.reuse,
        &facts.task.id,
        attempt,
        &facts.identity,
        sampled.as_ref(),
        &adapter.gate_logs_dir,
    )? {
        super::super::task_gate_run::refs_digest(Path::new(&facts.cwd), deadline).await
    } else {
        None
    };
    Ok(Some(Box::new(PreparedVerify {
        claim: crate::operation::preparation_claim::PreparationClaim::new(op)?,
        facts,
        gate,
        sampled,
        refs,
        logs: adapter.gate_logs_dir.clone(),
    })))
}

struct LostClaim;
#[async_trait]
impl PreparedTx for LostClaim {
    async fn apply(self: Box<Self>, _tx: &mut Tx<'_>, _op: &Operation) -> Result<PreparedTxResult> {
        // The generic repository normally rejects the lost claim before this can be consumed.
        Ok(PreparedTxResult::Stale)
    }
}

#[async_trait]
impl PreparedTx for PreparedVerify {
    async fn apply(self: Box<Self>, tx: &mut Tx<'_>, op: &Operation) -> Result<PreparedTxResult> {
        self.claim.require_operation(op)?;
        // Compare the raw task first, before new admission errors can obscure stale evidence.
        if task_get_tx(tx, &self.facts.task.id).await?.as_ref() != Some(&self.facts.task) {
            return Ok(PreparedTxResult::Stale);
        }
        let (current, _) = read_facts(tx, &op.payload, op).await?;
        if current != self.facts {
            return Ok(PreparedTxResult::Stale);
        }
        let Facts {
            task,
            area_id,
            cwd,
            identity,
            ..
        } = current;
        let gate = self.gate;
        let attempt: i64 =
            serde_json::from_value::<TaskVerifyOperationPayload>(op.payload.clone())?.attempt;
        // Guarded attempt bump: exactly one op prepares attempt N.
        let rows = task_gate_attempt_bump_tx(tx, &task.id, attempt, now_ms()).await?;
        if rows == 0 {
            return Err(CalmError::Conflict(format!(
                "task {} gate attempt {attempt} lost the bump (current attempt {} / status {:?})",
                task.id, task.gate_attempt, task.status
            )));
        }

        // The external observation is bound to revalidated rows. Attempt, target and verdict
        // still commit atomically. Small final log operations retain attempt ownership here.
        let log_path = self.logs.join(format!("{}-g{attempt}.log", task.id));
        let reuse = reuse::Gate {
            task_id: &task.id,
            attempt,
            log_path: &log_path,
            gate_logs_dir: &self.logs,
        };
        reuse::unlink_leftover_log(&reuse).await?;
        let mut prepared = target::prepare_target(
            &task,
            &gate,
            &cwd,
            attempt,
            &log_path,
            identity,
            self.sampled,
        )
        .await?;
        reuse::reuse_passing_run_tx(tx, &reuse, &mut prepared, self.refs.as_deref()).await?;
        let frozen = FrozenVerify {
            task_id: task.id.clone(),
            track_id: task.track_id.clone(),
            area_id,
            key: task.key.clone(),
            attempt,
            cwd,
            gate,
            target: prepared.target,
        };
        let mut output = TxOutput::new("task", Some(task.id.clone()), json!({}));
        output.data = serde_json::to_value(&frozen)?;
        if let Some(verdict) = &prepared.verdict {
            output
                .post_commit_events
                .extend(apply_gate_result_in_tx(tx, &frozen.result_ctx(), verdict).await?);
            output.result = serde_json::to_value(verdict)?;
        }
        if let Some(card_id) = task.worker_card_id.as_deref() {
            output.post_commit_events.extend(
                display::record_gate_cwd_tx(
                    tx,
                    card_id,
                    &frozen.track_id,
                    &frozen.area_id,
                    &frozen.cwd,
                )
                .await?,
            );
        }
        Ok(PreparedTxResult::Ready(output))
    }
}

async fn read_facts(tx: &mut Tx<'_>, input: &Value, op: &Operation) -> Result<(Facts, GateSpec)> {
    let payload: TaskVerifyOperationPayload = serde_json::from_value(input.clone())?;
    crate::operation::refuse_if_context_stale(tx, Some(&payload.task_id)).await?;
    let key = op
        .idempotency_key
        .as_deref()
        .and_then(parse_attempt_key)
        .ok_or_else(|| {
            CalmError::BadRequest(format!(
                "task-verify idempotency key must be \"{{task_id}}#g{{N}}\", got {:?}",
                op.idempotency_key
            ))
        })?;
    if key != (payload.task_id.as_str(), payload.attempt) {
        return Err(CalmError::BadRequest(
            "task-verify idempotency key does not match payload".into(),
        ));
    }
    let attempt = payload.attempt;
    super::execution::require_prior_settled(tx, &payload.task_id, attempt).await?;

    let task = task_get_tx(tx, &payload.task_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("task {}", payload.task_id)))?;
    if task.status != TaskStatus::Verifying {
        return Err(CalmError::Conflict(format!(
            "task {} is not verifying (status {:?}); gate attempt {attempt} is moot",
            task.id, task.status
        )));
    }
    let gate_json = task
        .gate_json
        .as_deref()
        .ok_or_else(|| CalmError::Conflict(format!("task {} declares no gate", task.id)))?;
    let gate = GateSpec::parse(&task.id, gate_json)?;

    // Successful workers release their lease before the gate starts, retaining files.
    let area_id: Option<String> = sqlx::query_scalar("SELECT area_id FROM tracks WHERE id = ?1")
        .bind(&task.track_id)
        .fetch_optional(&mut **tx)
        .await?;
    let area_id =
        area_id.ok_or_else(|| CalmError::Conflict(format!("track {} is gone", task.track_id)))?;
    let cwd_source = if let Some(cwd) = gate.cwd.as_ref().filter(|c| !c.trim().is_empty()) {
        CwdSource::Declared(cwd.clone())
    } else if let Some(card_id) = task.worker_card_id.as_deref() {
        // A missing lease is an infrastructure defect, never a reason to inspect the Track.
        let worker_lease: Option<String> = sqlx::query_scalar(
            "SELECT lease_id FROM workspace_leases WHERE card_id = ?1 AND track_id = ?2 \
                 ORDER BY created_at_ms DESC, lease_id DESC LIMIT 1",
        )
        .bind(card_id)
        .bind(&task.track_id)
        .fetch_optional(&mut **tx)
        .await?;
        if let Some(lease_id) = worker_lease {
            CwdSource::Lease(
                crate::operation::workspace_lease::facts::workspace_lease_by_id_tx(tx, &lease_id)
                    .await?
                    .ok_or_else(|| CalmError::Internal("selected gate lease vanished".into()))?,
            )
        } else if task.kind == crate::model::TaskKind::Terminal {
            // Terminal tasks do not acquire a worktree lease; use the immutable spawn operation, not the mutable card payload.
            let row: Option<(String, String, String, Option<String>)> = sqlx::query_as(
                    "SELECT id, operation_key, payload_hash, json_extract(tx_output_json, '$.data.cwd') FROM operations \
                     WHERE kind = 'terminal-worker' AND idempotency_key = ?1 \
                     AND target_type = 'card' AND target_id = ?2",
                )
                .bind(&task.id)
                .bind(card_id)
                .fetch_optional(&mut **tx)
                .await?;
            let (id, key, payload_hash, cwd) = row.ok_or_else(|| {
                CalmError::Conflict(format!("task {}: bound terminal cwd missing", task.id))
            })?;
            CwdSource::Terminal {
                id,
                key,
                payload_hash,
                cwd: cwd.ok_or_else(|| {
                    CalmError::Conflict(format!("task {}: bound terminal cwd missing", task.id))
                })?,
            }
        } else {
            return Err(CalmError::Conflict(format!(
                "task {}: bound worker {card_id} workspace lease missing",
                task.id
            )));
        }
    } else {
        // Legacy/unbound tasks retain their declaration defaults; the track default is where
        // its tasks run, `agent_cwd()` (#2139 R2).
        match task.cwd.clone().filter(|c| !c.trim().is_empty()) {
            Some(cwd) => CwdSource::Declared(cwd),
            None => CwdSource::Track(
                crate::db::sqlite::track_workspace_read_tx(tx, &task.track_id).await?,
            ),
        }
    };
    let cwd = cwd_source.cwd().to_string();
    if cwd.trim().is_empty() {
        return Err(CalmError::BadRequest(format!(
            "task {}: gate cwd is empty",
            task.id
        )));
    }

    let identity = target::verify_target_identity(tx, &task).await?;
    let reuse = reuse::Facts::read(tx, &task.id).await?;
    Ok((
        Facts {
            task,
            area_id,
            cwd_source,
            cwd,
            identity,
            reuse,
        },
        gate,
    ))
}
