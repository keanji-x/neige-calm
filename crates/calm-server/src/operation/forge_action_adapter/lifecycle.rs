//! Durable execution references protect action and recovery probes independently.
use super::super::workspace_lease::execution_guard::{
    acquire_execution_child_tx, record_execution_artifacts, release_stopped_execution,
};
use super::*;
use calm_worker_runtime::execution_process::{self, ProcessArtifacts};

pub(super) fn marker(holder: &str) -> String {
    format!("forge:{holder}")
}
fn process_artifacts(a: &SpawnArtifacts) -> ProcessArtifacts {
    ProcessArtifacts {
        pid: a.pid,
        pgid: a.pgid,
        start_time: a.start_time,
        boot_id: a.boot_id.clone(),
    }
}
pub(super) async fn stop(artifacts: Option<&SpawnArtifacts>, holder: &str) -> Result<()> {
    // Pre-guard parked executions did not carry the marker. Preserve their strict group proof;
    // absent authentication cannot justify signaling a recycled group or ignoring live members.
    if let Some(artifacts) = artifacts {
        if artifacts
            .extra
            .get("execution_marker")
            .and_then(Value::as_str)
            != Some(marker(holder).as_str())
        {
            super::super::gate_process::kill(artifacts);
            return super::super::gate_process::wait_group_stopped(artifacts).await;
        }
        super::super::gate_process::kill(artifacts);
    }
    let marker = marker(holder);
    let artifacts = artifacts.map(process_artifacts);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let done = match &artifacts {
                Some(a) => execution_process::stop_pass(a, &marker),
                None => execution_process::stop_marker_pass(&marker),
            }
            .map_err(|e| CalmError::Conflict(format!("forge execution stop uncertain: {e}")))?;
            if done {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|_| CalmError::Conflict("forge execution stop remains unresolved".into()))?
}

/// An outcome is not evidence: every terminal path scans the durable execution identity first.
pub(super) async fn confirm_stopped(pool: &sqlx::SqlitePool, holder: &str) -> Result<()> {
    let row: Option<Option<String>> = sqlx::query_scalar(
        "SELECT execution_artifacts_json FROM workspace_leases WHERE holder_kind='forge' AND \
        holder_id=?1 AND state IN ('held','releasing') ORDER BY created_at_ms DESC LIMIT 1",
    )
    .bind(holder)
    .fetch_optional(pool)
    .await?;
    let Some(raw) = row else { return Ok(()) };
    let artifacts: Option<SpawnArtifacts> = raw.map(|v| serde_json::from_str(&v)).transpose()?;
    stop(artifacts.as_ref(), holder).await?;
    Ok(())
}

pub(super) async fn finish(pool: &sqlx::SqlitePool, holder: &str) -> Result<()> {
    confirm_stopped(pool, holder).await?;
    release_stopped_execution(pool, "forge", holder).await
}

pub(super) async fn probe(
    pool: &sqlx::SqlitePool,
    op: &str,
    owner: &str,
    frozen: &FrozenForge,
    argv: &[String],
    repo: &dyn RouteRepo,
) -> Result<(Option<i32>, String)> {
    if argv.is_empty() {
        return Err(CalmError::Internal(
            "forge-action probe argv must not be empty".into(),
        ));
    }
    let holder = crate::model::new_id();
    let mut tx = begin_immediate_tx(pool).await?;
    acquire_execution_child_tx(
        &mut tx,
        &frozen.track_id,
        &frozen.card_id,
        &holder,
        "forge",
        &frozen.cwd_lease,
        op,
        owner,
    )
    .await?;
    tx.commit().await?;
    #[cfg(test)]
    pause_issuance(op, false).await;
    let result = async {
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args([
            "-c",
            "IFS= read -r go && [ \"$go\" = go ] || exit 75; exec \"$@\"",
            "forge-probe",
        ])
        .args(argv)
        .stdin(Stdio::piped())
        .current_dir(&frozen.cwd_lease)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
        apply_forge_subprocess_env(&mut cmd, repo).await;
        cmd.env(execution_process::MARKER_KEY, marker(&holder));
        // SAFETY: setsid is async-signal-safe and runs before executing the trusted provider.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn()?;
        let identity = execution_process::capture(
            child
                .id()
                .ok_or_else(|| CalmError::Internal("forge probe has no pid".into()))?
                as i32,
        )
        .map_err(|e| CalmError::Conflict(format!("forge probe identity: {e}")))?;
        let artifacts = SpawnArtifacts {
            pid: identity.pid,
            pgid: identity.pgid,
            start_time: identity.start_time,
            boot_id: identity.boot_id,
            log_path: None,
            extra: json!({"parent_holder":op,"execution_marker":marker(&holder)}),
        };
        #[cfg(test)]
        pause_issuance(op, true).await;
        record_execution_artifacts(pool, "forge", &holder, &artifacts).await?;
        // A delayed issuer may spawn its waiting wrapper after recovery released its reference;
        // recording requires held ownership, and an owner fence precedes the only go-token.
        let mut tx = begin_immediate_tx(pool).await?;
        super::super::owned_parked::require_owner_tx(&mut tx, op, owner).await?;
        tx.commit().await?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| CalmError::Internal("forge probe handshake missing".into()))?;
        tokio::time::timeout(RELEASE_TIMEOUT, stdin.write_all(b"go\n"))
            .await
            .map_err(|_| CalmError::Conflict("forge probe handshake timed out".into()))??;
        drop(stdin);

        let output = tokio::time::timeout(PROBE_TIMEOUT, child.wait_with_output())
            .await
            .map_err(|_| CalmError::Internal("forge-action probe timed out".into()))??;
        Ok((
            output.status.code(),
            String::from_utf8_lossy(&output.stdout).to_string(),
        ))
    }
    .await;
    // Even spawn/artifact/output failure requires physical stop before freeing this child reference.
    finish(pool, &holder).await?;
    result
}

pub(super) async fn finish_children(pool: &sqlx::SqlitePool, parent: &str) -> Result<()> {
    let holders: Vec<String> = sqlx::query_scalar(
        "SELECT holder_id FROM workspace_leases WHERE holder_kind='forge' AND state IN \
        ('held','releasing') AND execution_parent_holder_id=?1",
    )
    .bind(parent)
    .fetch_all(pool)
    .await?;
    for holder in holders {
        finish(pool, &holder).await?;
    }
    Ok(())
}

/// Existing periodic settlement also repairs references after a crash between stop and release.
pub(crate) async fn reconcile(pool: &sqlx::SqlitePool) -> Result<()> {
    let holders:Vec<String>=sqlx::query_scalar(
        "SELECT l.holder_id FROM workspace_leases l LEFT JOIN operations o ON \
        o.id=COALESCE(l.execution_parent_holder_id,l.holder_id) \
         WHERE l.holder_kind='forge' AND l.state IN ('held','releasing') AND \
         (o.phase IN ('succeeded','failed','stuck') OR (o.id IS NULL AND l.execution_artifacts_json IS NOT NULL)) ORDER BY l.updated_at_ms LIMIT 16"
    ).fetch_all(pool).await?;
    let mut first_error = None;
    for holder in holders {
        sqlx::query("UPDATE workspace_leases SET updated_at_ms=?2 WHERE holder_kind='forge' AND holder_id=?1 AND \
        state IN ('held','releasing')").bind(&holder).bind(crate::model::now_ms()).execute(pool).await?;
        if let Err(error) = finish(pool, &holder).await {
            tracing::warn!(holder_id=%holder,error=%error,"forge execution reference remains held; settlement will retry");
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// A physical exit is only a wakeup; existing parked claims own all recovery and completion.
pub(super) async fn reconcile_if_claimed(ctx: &SpawnCtx, op: &str, observation: ForgeObservation) {
    match ctx.operation_repo.claim_parked(op).await {
        Ok(Some(claimed)) => {
            if let Err(error) = super::super::owned_parked::reconcile(
                &ForgeActionAdapter { observation },
                &claimed,
                RecoveryMode::PreDeadlineProbe,
                ctx,
            )
            .await
            {
                tracing::warn!(op_id=op,error=%error,"forge observer recovery remains unresolved");
            }
        }
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(op_id=op,error=%error,"forge observer claim failed; sweep will retry")
        }
    }
}

pub(super) fn observe_recovered_exit(ctx: SpawnCtx, op: String, artifacts: SpawnArtifacts) {
    tokio::spawn(async move {
        while verify_owned_pid(artifacts.pid, artifacts.start_time, &artifacts.boot_id) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        // Boot recovery's short claim may still be completing as this process exits.
        loop {
            let Ok(Some(current)) = ctx.operation_repo.get_operation(&op).await else {
                return;
            };
            if !matches!(current.phase, super::super::Phase::Parked) {
                return;
            }
            if current.lease_owner.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        reconcile_if_claimed(
            &ctx,
            &op,
            ForgeObservation::Exit("forge action process dead"),
        )
        .await;
    });
}

#[cfg(test)]
#[derive(Clone)]
pub(super) struct ProbeIssuancePause {
    pub op: String,
    pub prepared: std::sync::Arc<tokio::sync::Notify>,
    pub resume_prepare: std::sync::Arc<tokio::sync::Notify>,
    pub spawned: std::sync::Arc<tokio::sync::Notify>,
    pub resume_spawn: std::sync::Arc<tokio::sync::Notify>,
}
#[cfg(test)]
pub(super) static PROBE_ISSUANCE_PAUSE: std::sync::Mutex<Option<ProbeIssuancePause>> =
    std::sync::Mutex::new(None);
#[cfg(test)]
async fn pause_issuance(op: &str, spawned: bool) {
    let pause = PROBE_ISSUANCE_PAUSE
        .lock()
        .unwrap()
        .as_ref()
        .filter(|pause| pause.op == op)
        .cloned();
    if let Some(pause) = pause {
        let (ready, resume) = if spawned {
            (pause.spawned, pause.resume_spawn)
        } else {
            (pause.prepared, pause.resume_prepare)
        };
        ready.notify_one();
        resume.notified().await;
    }
}
