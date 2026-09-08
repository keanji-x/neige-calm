//! Post-publication machine checks. No task status or Worker-start authority.
use super::{
    candidate::{self, Candidate},
    *,
};
use crate::{
    db::{RouteRepo, write_in_tx_typed},
    operation::gate_process,
    operation::task_verify_adapter::{GateStep, GateVerdict},
    operation::*,
    proc_identity::*,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::io::AsyncWriteExt;
pub(crate) const KIND: &str = "candidate-verify";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Payload {
    pub publication_operation_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Frozen {
    pub candidate: Candidate,
    pub policy: calm_types::task_execution::CandidateMachinePolicy,
    pub workspace: PathBuf,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProcessIdentity {
    pid: i32,
    pgid: i32,
    start_time: u64,
    boot_id: String,
}
impl From<&SpawnArtifacts> for ProcessIdentity {
    fn from(value: &SpawnArtifacts) -> Self {
        Self {
            pid: value.pid,
            pgid: value.pgid,
            start_time: value.start_time,
            boot_id: value.boot_id.clone(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Evidence {
    pub verification_operation_id: String,
    pub candidate: Candidate,
    pub policy: calm_types::task_execution::CandidateMachinePolicy,
    pub verdict: GateVerdict,
    pub process: ProcessIdentity,
}
impl Frozen {
    fn from_output(output: &TxOutput) -> Result<Self> {
        Ok(serde_json::from_value(output.data.clone())?)
    }
    fn input(&self) -> PathBuf {
        self.workspace.join("input")
    }
    fn log(&self) -> PathBuf {
        self.workspace.join("gate.log")
    }
    fn exit(&self) -> PathBuf {
        self.workspace.join("gate.exit")
    }
    fn evidence(&self, id: &str, artifacts: &SpawnArtifacts, verdict: GateVerdict) -> Evidence {
        Evidence {
            verification_operation_id: id.into(),
            candidate: self.candidate.clone(),
            policy: self.policy.clone(),
            verdict,
            process: artifacts.into(),
        }
    }
}
pub(crate) async fn active_tx(tx: &mut Tx<'_>, track: &str) -> Result<i64> {
    Ok(sqlx::query_scalar("SELECT count(*) FROM task_candidate_verification_allocations a LEFT JOIN operations o ON o.operation_key=a.operation_key AND o.kind='candidate-verify' WHERE a.track_id=?1 AND (o.id IS NULL OR o.phase NOT IN ('succeeded','failed'))")
        .bind(track).fetch_one(&mut **tx).await?)
}
pub(crate) async fn authorize_tx(tx: &mut Tx<'_>, op: &Operation, frozen: &Frozen) -> Result<()> {
    let payload: Payload = serde_json::from_value(op.payload.clone())?;
    if op.kind != KIND
        || payload.publication_operation_id != frozen.candidate.publication_operation_id
        || op.idempotency_key.as_deref()
            != Some(&format!("candidate:{}", payload.publication_operation_id))
        || &frozen.policy != frozen.candidate.policy()?
    {
        return Err(conflict(
            "candidate verification identity or policy changed",
        ));
    }
    let expected_workspace = frozen
        .candidate
        .store_root
        .parent()
        .ok_or_else(|| conflict("candidate store parent missing"))?
        .join("candidate-checks")
        .join(&op.id);
    if frozen.workspace != expected_workspace {
        return Err(conflict(
            "candidate verification workspace identity changed",
        ));
    }
    candidate::authorize_tx(tx, &frozen.candidate).await?;
    let reserved: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM task_candidate_verification_allocations WHERE publication_operation_id=?1 AND track_id=?2 AND operation_key=?3)")
        .bind(&payload.publication_operation_id).bind(&frozen.candidate.source.track_id).bind(&op.operation_key).fetch_one(&mut **tx).await?;
    if !reserved {
        return Err(conflict(
            "candidate verification capacity allocation missing",
        ));
    }
    Ok(())
}
async fn owned_tx(tx: &mut Tx<'_>, op: &Operation, frozen: &Frozen) -> Result<()> {
    authorize_tx(tx, op, frozen).await?;
    let owner = op
        .lease_owner
        .as_deref()
        .ok_or_else(|| conflict("candidate verification lease missing"))?;
    let stored: Option<String> = sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE id=?1 AND kind='candidate-verify' AND phase='spawn_started' AND lease_owner=?2")
        .bind(&op.id).bind(owner).fetch_optional(&mut **tx).await?;
    let stored: TxOutput = serde_json::from_str(
        &stored.ok_or_else(|| conflict("candidate verification lease changed"))?,
    )?;
    if Frozen::from_output(&stored)? != *frozen {
        return Err(conflict("candidate verification frozen input changed"));
    }
    Ok(())
}
pub(crate) async fn qualified_tx(
    tx: &mut Tx<'_>,
    id: &str,
    candidate: &Candidate,
) -> Result<Evidence> {
    candidate::authorize_tx(tx, candidate).await?;
    let allocated: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM task_candidate_verification_allocations a JOIN operations o ON o.operation_key=a.operation_key WHERE o.id=?1 AND a.publication_operation_id=?2 AND a.track_id=?3)")
        .bind(id).bind(&candidate.publication_operation_id).bind(&candidate.source.track_id).fetch_one(&mut **tx).await?;
    if !allocated {
        return Err(conflict(
            "candidate verification allocation identity changed",
        ));
    }
    let row: Option<(String, String, String, String)> = sqlx::query_as("SELECT payload_json,tx_output_json,idempotency_key,spawn_artifacts_json FROM operations WHERE id=?1 AND kind='candidate-verify' AND phase='succeeded'")
        .bind(id).fetch_optional(&mut **tx).await?;
    let (payload, output, key, artifacts) =
        row.ok_or_else(|| conflict("waiting for successful candidate verification"))?;
    let payload: Payload = serde_json::from_str(&payload)?;
    let output: TxOutput = serde_json::from_str(&output)?;
    let frozen = Frozen::from_output(&output)?;
    let evidence: Evidence = serde_json::from_value(output.result)?;
    let artifacts: SpawnArtifacts = serde_json::from_str(&artifacts)?;
    if payload.publication_operation_id != candidate.publication_operation_id
        || key != format!("candidate:{}", candidate.publication_operation_id)
        || frozen.candidate != *candidate
        || frozen.policy != *candidate.policy()?
        || evidence.verification_operation_id != id
        || evidence.candidate != *candidate
        || evidence.process != ProcessIdentity::from(&artifacts)
        || evidence.policy != frozen.policy
        || !evidence.verdict.passed
        || evidence.verdict.exit_code != Some(0)
    {
        return Err(conflict(
            "candidate verification evidence does not qualify this input",
        ));
    }
    Ok(evidence)
}
pub(crate) struct CandidateVerifyAdapter {
    repo: Arc<dyn RouteRepo>,
}
impl CandidateVerifyAdapter {
    pub fn new(repo: Arc<dyn RouteRepo>) -> Self {
        Self { repo }
    }
}
async fn complete(
    ctx: &SpawnCtx,
    op: &Operation,
    frozen: &Frozen,
    artifacts: &SpawnArtifacts,
    verdict: GateVerdict,
) -> Result<()> {
    if !gate_process::group_stopped(artifacts)? {
        return Err(conflict(
            "candidate process-group cleanup remains unresolved",
        ));
    }
    let Some(owned) = ctx.operation_repo.claim_parked(&op.id).await? else {
        let current = ctx.operation_repo.get_operation(&op.id).await?;
        return if current.is_some_and(|current| matches!(current.phase, Phase::Parked)) {
            Err(conflict("candidate completion awaits parked lease"))
        } else {
            Ok(())
        };
    };
    let owner = owned
        .lease_owner
        .as_deref()
        .ok_or_else(|| conflict("candidate completion lease missing"))?;
    let pool = ctx.operation_repo.sqlite_pool();
    let result = async {
        let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
        let recorded: Option<String> = sqlx::query_scalar(
            "SELECT spawn_artifacts_json FROM operations WHERE id=?1 AND phase='parked' AND lease_owner=?2",
        ).bind(&op.id).bind(owner).fetch_optional(&mut *tx).await?;
        let recorded: SpawnArtifacts = serde_json::from_str(&recorded.ok_or_else(|| conflict("candidate completion lost parked lease"))?)?;
        if ProcessIdentity::from(&recorded) != ProcessIdentity::from(artifacts) {
            return Ok(());
        }
        // A previous observer cannot settle a re-driven execution, or race a
        // replacement lease. The Child stays unreaped until this write commits.
        let outcome = ParkedOutcome::Succeeded {
            result: serde_json::to_value(frozen.evidence(&op.id, artifacts, verdict))?,
        };
        let completion = complete_parked_tx(&mut tx, &op.id, &outcome).await?;
        tx.commit().await?;
        if let ParkedCompletion::Completed(result) = completion {
            ctx.completion.complete(result);
        }
        Ok(())
    }.await;
    sqlx::query("UPDATE operations SET lease_owner=NULL,lease_until_ms=NULL WHERE id=?1 AND lease_owner=?2 AND phase='parked'")
        .bind(&op.id).bind(owner).execute(&pool).await?;
    result
}

#[async_trait]
impl ProviderAdapter for CandidateVerifyAdapter {
    fn kind(&self) -> &'static str {
        KIND
    }
    fn phases(&self) -> &'static [PhaseTag] {
        &[
            PhaseTag::Pending,
            PhaseTag::TxCommitted,
            PhaseTag::SpawnStarted,
            PhaseTag::Parked,
            PhaseTag::Succeeded,
        ]
    }
    fn owns_parked_resource(&self) -> bool {
        true
    }
    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: Payload = serde_json::from_value(input.clone())?;
        if payload.publication_operation_id.is_empty() {
            return Err(conflict("candidate publication identity missing"));
        }
        Ok(())
    }
    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        op: &Operation,
    ) -> Result<TxOutput> {
        self.validate(input).await?;
        let payload: Payload = serde_json::from_value(input.clone())?;
        let candidate = candidate::load_tx(tx, &payload.publication_operation_id).await?;
        let root = candidate
            .store_root
            .parent()
            .ok_or_else(|| conflict("candidate storage parent missing"))?
            .join("candidate-checks");
        let frozen = Frozen {
            policy: candidate.policy()?.clone(),
            candidate,
            workspace: root.join(&op.id),
        };
        authorize_tx(tx, op, &frozen).await?;
        let mut output = TxOutput::new(
            "track",
            Some(frozen.candidate.source.track_id.clone()),
            json!({}),
        );
        output.data = serde_json::to_value(frozen)?;
        Ok(output)
    }
    async fn app_server_interact(
        &self,
        _: &mut TxOutput,
        _: &Operation,
        _: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        Ok(AppServerInteractOutcome::NotApplicable)
    }
    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        let frozen = Frozen::from_output(output)?;
        if let Some(artifacts) = &op.spawn_artifacts {
            stop_recorded(artifacts).await?;
        }
        let owned = op.clone();
        let f = frozen.clone();
        write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move { owned_tx(tx, &owned, &f).await })
        })
        .await?;
        let root = frozen
            .workspace
            .parent()
            .ok_or_else(|| conflict("verification workspace parent missing"))?;
        crate::isolated_codex::workspace::prepare_root(root)?;
        crate::isolated_codex::workspace::prepare(root, &op.id)?;
        // Only a recorded, stopped prior execution permits replacing its dirty copy.
        // A pre-release conflict without process evidence must fail closed.
        if let Some(prior) = &op.spawn_artifacts {
            let previous = frozen
                .workspace
                .join(format!("input-before-{}-{}", prior.pid, prior.start_time));
            if frozen.input().try_exists()? && !previous.try_exists()? {
                std::fs::rename(frozen.input(), previous)?;
                std::fs::File::open(&frozen.workspace)?.sync_all()?;
            }
        }
        frozen.candidate.prepare(&frozen.input())?;
        for path in [frozen.exit(), frozen.workspace.join("gate.exit.tmp")] {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        let steps = frozen
            .policy
            .steps()
            .iter()
            .map(|step| GateStep {
                name: step.name.clone(),
                cmd: step.cmd.clone(),
            })
            .collect::<Vec<_>>();
        let mut child = gate_process::spawn_held(
            self.repo.as_ref(),
            &frozen.input().join("source"),
            &steps,
            &frozen.workspace.join("gate.sh"),
            &frozen.log(),
            &frozen.exit(),
        )
        .await?;
        let pid = child
            .id()
            .ok_or_else(|| conflict("candidate wrapper PID missing"))? as i32;
        let release = async {
            let artifacts = SpawnArtifacts {
                pid,
                pgid: pid,
                start_time: read_proc_start_time(pid)
                    .ok_or_else(|| conflict("candidate PID identity missing"))?,
                boot_id: read_boot_id()
                    .ok_or_else(|| conflict("candidate boot identity missing"))?,
                log_path: Some(frozen.log().display().to_string()),
                extra: json!({"exit_path":frozen.exit(),"script_path":frozen.workspace.join("gate.sh")}),
            };
            ctx.record_spawn_artifacts(op, &artifacts).await?;
            #[cfg(any(test, feature = "fixtures"))]
            super::test_hooks::before_release(
                &frozen.candidate.publication_operation_id,
                frozen.workspace.clone(),
            )
            .await;
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| conflict("candidate release pipe missing"))?;
            let f = frozen.clone();
            let owned = op.clone();
            write_in_tx_typed(self.repo.as_ref(), move |tx| Box::pin(async move {
                owned_tx(tx, &owned, &f).await?;
                crate::isolated_codex::workspace::open_retained(f.workspace.parent().ok_or_else(|| conflict("verification parent missing"))?, &owned.id, &f.workspace)?;
                f.candidate.verify(&f.input())?;
                sqlx::query("UPDATE operations SET tx_output_json=json_set(tx_output_json,'$.result.release',json(?1)),lease_until_ms=?2 WHERE id=?3 AND lease_owner=?4 AND phase='spawn_started'")
                    .bind(json!({"candidate":f.candidate.snapshot,"policy":f.policy}).to_string()).bind(crate::model::now_ms()+60_000).bind(&owned.id).bind(owned.lease_owner.as_deref()).execute(&mut **tx).await?;
                stdin.write_all(b"go\n").await?;
                Ok(())
            })).await?;
            Ok::<_, CalmError>(artifacts)
        };
        let artifacts = match tokio::time::timeout(Duration::from_secs(60), release).await {
            Ok(Ok(artifacts)) => artifacts,
            other => {
                signal_process_group(pid, libc::SIGKILL);
                let _ = child.wait().await;
                return Err(match other {
                    Ok(Err(e)) => e,
                    _ => conflict("candidate release timed out"),
                });
            }
        };
        let timeout = i64::from(frozen.policy.timeout_secs());
        let op = op.clone();
        let ctx = ctx.clone();
        let observer = Box::pin(async move {
            let observation =
                gate_process::observe_verdict(child, artifacts.clone(), frozen.log(), 1, timeout)
                    .await;
            #[cfg(any(test, feature = "fixtures"))]
            super::test_hooks::before_completion(
                &frozen.candidate.publication_operation_id,
                frozen.workspace.clone(),
            )
            .await;
            // Keep the leader unreaped until its real wait verdict commits. A
            // concurrent sweep must not replace it with exit-file inference.
            loop {
                match complete(&ctx, &op, &frozen, &artifacts, observation.verdict.clone()).await {
                    Ok(()) => break,
                    Err(error) => {
                        tracing::error!(%error, "candidate completion retains owned wait status for retry");
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }
            }
            observation.reap().await;
        });
        Ok(SpawnOutcome::Parked {
            deadline_ms: crate::model::now_ms() + (timeout + 120) * 1000,
            observer,
        })
    }
    async fn owned_parked_recovery_eligible(&self, op: &Operation) -> bool {
        let eligible = !retains_quiescent_leader(op);
        #[cfg(any(test, feature = "fixtures"))]
        super::test_hooks::after_recovery_hint(&op.id, eligible).await;
        eligible
    }
    async fn recover_owned_parked(
        &self,
        op: &Operation,
        mode: RecoveryMode,
        _: &SpawnCtx,
    ) -> Result<ParkedRecovery> {
        let frozen = Frozen::from_output(
            op.tx_output
                .as_ref()
                .ok_or_else(|| conflict("candidate frozen input missing"))?,
        )?;
        let artifacts = op
            .spawn_artifacts
            .as_ref()
            .ok_or_else(|| conflict("candidate process identity missing"))?;
        let alive = verify_owned_pid(artifacts.pid, artifacts.start_time, &artifacts.boot_id);
        let verdict = if alive {
            if !matches!(mode, RecoveryMode::PastDeadline) {
                // The existing periodic owned-resource sweep observes exit;
                // boot must not spawn an unfenced duplicate completion observer.
                return Ok(ParkedRecovery::LeaveParked);
            }
            let stat = std::fs::read_to_string(format!("/proc/{}/stat", artifacts.pid))?;
            let leader = parse_proc_stat_fields(&stat)
                .ok_or_else(|| conflict("candidate deadline process state unavailable"))?;
            if leader.start_time != artifacts.start_time || leader.pgrp != artifacts.pgid {
                return Err(conflict("candidate deadline process identity changed"));
            }
            // Identity survives exit while the live observer retains its Child.
            // Cleanup is still required, but an exited leader cannot acquire a
            // new timeout verdict while its actual wait result awaits the lease.
            let exited = matches!(leader.state, 'Z' | 'X');
            stop_recorded(artifacts).await?;
            if exited {
                return Ok(ParkedRecovery::LeaveParked);
            }
            gate_process::timeout_verdict(&frozen.log(), 1, i64::from(frozen.policy.timeout_secs()))
        } else {
            if !gate_process::group_stopped(artifacts)? {
                return Ok(ParkedRecovery::LeaveParked);
            }
            match gate_process::read_exit_file(&frozen.exit()) {
                Ok(Some(code)) => gate_process::verdict_from_exit_code(code, &frozen.log(), 1),
                _ => gate_process::infra_verdict(
                    "candidate execution interrupted without exit evidence",
                    &frozen.log(),
                    1,
                ),
            }
        };
        Ok(ParkedRecovery::Complete(ParkedOutcome::Succeeded {
            result: serde_json::to_value(frozen.evidence(&op.id, artifacts, verdict))?,
        }))
    }
    async fn complete_owned_parked_tx(
        &self,
        tx: &mut Tx<'_>,
        op: &Operation,
    ) -> Result<Vec<crate::event::BroadcastEnvelope>> {
        let expected = op
            .spawn_artifacts
            .as_ref()
            .ok_or_else(|| conflict("candidate process identity missing"))?;
        let raw: String =
            sqlx::query_scalar("SELECT spawn_artifacts_json FROM operations WHERE id=?1")
                .bind(&op.id)
                .fetch_one(&mut **tx)
                .await?;
        let recorded: SpawnArtifacts = serde_json::from_str(&raw)?;
        if ProcessIdentity::from(&recorded) != ProcessIdentity::from(expected) {
            return Err(conflict(
                "candidate owned completion process identity changed",
            ));
        }
        Ok(Vec::new())
    }
    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        _: &TxOutput,
        _: &Operation,
    ) -> Result<CompensationStateVersioned> {
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.into(),
            steps: vec![CompensationStep {
                op: "kill_candidate_group".into(),
                args: json!({}),
                completed: false,
                attempts: 0,
                last_error: None,
            }],
        })
    }
    async fn compensate_step(
        &self,
        step: &CompensationStep,
        _: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<()> {
        if step.op != "kill_candidate_group" {
            return Err(conflict("unknown candidate compensation"));
        }
        let current = ctx
            .operation_repo
            .get_operation(&op.id)
            .await?
            .ok_or_else(|| conflict("candidate cleanup operation missing"))?;
        if let Some(artifacts) = &current.spawn_artifacts {
            stop_recorded(artifacts).await?;
        }
        Ok(())
    }
}

async fn stop_recorded(artifacts: &SpawnArtifacts) -> Result<()> {
    gate_process::kill(artifacts);
    gate_process::wait_group_stopped(artifacts).await
}

/// Deferral requires positive proof; uncertainty leaves normal claimed recovery
/// responsible for validation and cleanup. This observation grants no authority.
fn retains_quiescent_leader(op: &Operation) -> bool {
    let Some(artifacts) = &op.spawn_artifacts else {
        return false;
    };
    if !verify_owned_pid(artifacts.pid, artifacts.start_time, &artifacts.boot_id) {
        return false;
    }
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", artifacts.pid)) else {
        return false;
    };
    let Some(leader) = parse_proc_stat_fields(&stat) else {
        return false;
    };
    leader.start_time == artifacts.start_time
        && leader.pgrp == artifacts.pgid
        && matches!(leader.state, 'Z' | 'X')
        && matches!(gate_process::group_stopped(artifacts), Ok(true))
}
