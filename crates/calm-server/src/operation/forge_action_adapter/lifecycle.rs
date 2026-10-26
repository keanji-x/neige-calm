//! One forge-owned stepper for observers, sweeps and boot; files are sampled only after settlement.
use super::*;
use crate::operation::{ParkedRecovery, RecoveryMode, owned_execution, owned_parked};
use execution::{Decision, Kind, Release, StopReason, Verdict};
use workspace_runtime::execution::{ExecutionReply, ExecutionState, Settlement};

pub(super) fn legacy_error(op: &Operation) -> CalmError {
    CalmError::Conflict(format!(
        "drain under the old kernel or manually stop pgid {}",
        op.spawn_artifacts.as_ref().map_or(0, |a| a.pgid)
    ))
}
fn failed(reason: impl Into<String>, class: &str) -> ParkedRecovery {
    ParkedRecovery::Complete(Box::new((
        ParkedOutcome::Failed {
            last_error: reason.into(),
            last_error_class: Some(class.into()),
        },
        None,
    )))
}
async fn completed(frozen: &FrozenForge, code: i32, stdout: &str) -> Result<ParkedRecovery> {
    #[cfg(feature = "fixtures")]
    crate::test_seams::pause_point("forge-before-completion", &frozen.idem_key).await;
    if let Err(error) = persist_forge_artifact_if_needed(frozen, code, stdout).await {
        return Ok(failed(
            format!("gate-infra: forge artifact write failed: {error}"),
            "gate-infra",
        ));
    }
    Ok(ParkedRecovery::Complete(Box::new(forge_result_outcome(
        frozen, code, stdout,
    ))))
}

pub(super) async fn recover(
    op: &Operation,
    mode: RecoveryMode,
    ctx: &SpawnCtx,
) -> Result<ParkedRecovery> {
    if op
        .tx_output
        .as_ref()
        .is_none_or(|o| o.data.get(execution::KEY).is_none())
    {
        return Err(legacy_error(op));
    }
    let record = execution::record(op)?;
    let frozen = FrozenForge::from_output(
        op.tx_output
            .as_ref()
            .ok_or_else(|| CalmError::Internal("forge missing frozen output".into()))?,
    )?;
    ctx.execution.drive(record.ids());
    if matches!(mode, RecoveryMode::Boot) {
        let mut latest = op.clone();
        for kind in [Kind::Action, Kind::Verdict, Kind::Output] {
            if matches!(kind, Kind::Verdict) && record.probes.verdict.is_none()
                || matches!(kind, Kind::Output) && record.probes.output.is_none()
            {
                continue;
            }
            let probe = record.execution(kind);
            let reason = if probe.decision != Decision::Observing {
                None
            } else {
                match (&kind, &probe.release) {
                    (Kind::Action, Release::Held) => Some(StopReason::KernelRestarted),
                    (_, Release::Releasing { .. }) => Some(StopReason::ReleaseRefused),
                    _ => None,
                }
            };
            if let Some(reason) = reason {
                execution::stop(ctx, &latest, kind, reason).await?;
                latest = execution::refresh_claim(ctx, &latest).await?;
            }
        }
        return Ok(ParkedRecovery::LeaveParked);
    }
    let mut settlements = Vec::new();
    let mut waiting = false;
    for kind in [Kind::Action, Kind::Verdict, Kind::Output] {
        if matches!(kind, Kind::Verdict) && record.probes.verdict.is_none()
            || matches!(kind, Kind::Output) && record.probes.output.is_none()
        {
            continue;
        }
        let probe = record.execution(kind);
        let settled = owned_execution::settlement(ctx, &probe.execution_id).await?;
        if let Some(reason) = execution::stop_reason(op, kind, &probe, settled.is_some(), mode) {
            execution::stop(ctx, op, kind, reason).await?;
            return Ok(ParkedRecovery::LeaveParked);
        }
        if settled.is_none() {
            waiting = true;
            if probe.decision == Decision::Observing && probe.release == Release::Held {
                execution::engine(ctx)?;
            }
            owned_execution::drain_exited(ctx, op, &probe.execution_id).await?;
        }
        settlements.push((kind, settled));
    }
    if waiting {
        return Ok(ParkedRecovery::LeaveParked);
    }
    // Restart evidence supplies no successful action or probe result, even when
    // an exit artifact was written before the executor lost its native handles.
    if settlements.iter().any(|(_, settlement)| {
        matches!(
            settlement,
            Some(Settlement::Journal(
                ExecutionState::InterruptedQuiescent { .. }
            ))
        )
    }) {
        return Ok(failed(
            "forge execution interrupted; gate-infra",
            "gate-infra",
        ));
    }
    if record.release == Release::Held
        || matches!(
            record.decision,
            Decision::Stopping {
                reason: StopReason::KernelRestarted | StopReason::ReleaseRefused
            }
        )
    {
        return Ok(failed(
            "gate-infra: forge release was not confirmed",
            "gate-infra",
        ));
    }
    // Probe progress outranks the original action evidence. Landed is never recomputed.
    if record.probes.verdict.is_some() || record.verdict == Verdict::Landed {
        return probe_step(ctx, op, &frozen, &record, &settlements).await;
    }
    if let Ok(result) = read_result_file(&frozen.result_path).await {
        let (outcome, _) = forge_result_outcome(&frozen, result.exit_code, &result.stdout);
        if matches!(outcome, ParkedOutcome::Succeeded { .. }) {
            return completed(&frozen, result.exit_code, &result.stdout).await;
        }
        if frozen.probe.is_none() {
            if let ParkedOutcome::Failed {
                last_error,
                last_error_class,
            } = &outcome
                && last_error_class.as_deref() == Some("gate-infra")
            {
                return Ok(failed(
                    format!(
                        "gate-infra: extraction failed: {last_error}; no probe to resolve outcome"
                    ),
                    "gate-infra",
                ));
            }
            return completed(&frozen, result.exit_code, &result.stdout).await;
        }
    }
    if frozen.probe.is_none() {
        return Ok(
            if matches!(mode, RecoveryMode::PastDeadline)
                || matches!(
                    record.decision,
                    Decision::Stopping {
                        reason: StopReason::Timeout
                    }
                )
            {
                failed("action-timeout", "action-timeout")
            } else {
                failed(
                    "forge action process dead with no probe; gate-infra",
                    "gate-infra",
                )
            },
        );
    }
    admit_probe(ctx, op, &frozen, Kind::Verdict).await
}
/// Busy leaves the durable record and deadline untouched; the next claimed
/// observer/sweep step retries. A deleted track is a terminal infrastructure failure.
async fn admit_probe(
    ctx: &SpawnCtx,
    op: &Operation,
    frozen: &FrozenForge,
    kind: Kind,
) -> Result<ParkedRecovery> {
    match execution::admit_probe(ctx, op, frozen, kind).await {
        Ok(execution::ProbeAdmission::Admitted) | Err(CalmError::WorkspaceBusy(_)) => {
            Ok(ParkedRecovery::LeaveParked)
        }
        Ok(execution::ProbeAdmission::TrackGone) => Ok(failed(
            "gate-infra: forge probe track was deleted",
            "gate-infra",
        )),
        Err(error) => Err(error),
    }
}

async fn probe_step(
    ctx: &SpawnCtx,
    op: &Operation,
    frozen: &FrozenForge,
    record: &execution::Record,
    settlements: &[(Kind, Option<Settlement>)],
) -> Result<ParkedRecovery> {
    if record.verdict != Verdict::Landed {
        let probe = record
            .probes
            .verdict
            .as_ref()
            .expect("verdict probe admitted");
        let settlement = settlements
            .iter()
            .find(|(kind, _)| matches!(kind, Kind::Verdict))
            .and_then(|(_, s)| s.as_ref())
            .expect("settled verdict probe");
        if !execution::eligible(probe, settlement) {
            return Ok(failed(
                "forge action probe failed; gate-infra",
                "gate-infra",
            ));
        }
        let code = read_result_file(&execution::probe_path(frozen, &probe.execution_id))
            .await
            .ok()
            .map(|r| r.exit_code);
        match verdict_from_exit_code(code) {
            ProbeVerdict::NotLanded => {
                let reason = match read_result_file(&frozen.result_path).await {
                    Ok(result) if result.exit_code != 0 => format!(
                        "forge action exited with code {}; probe reports not landed",
                        result.exit_code
                    ),
                    _ => "forge action process dead and probe reports not landed".into(),
                };
                return Ok(failed(reason, "action-not-landed"));
            }
            ProbeVerdict::Unknown => {
                return Ok(failed(
                    "forge action probe verdict unknown; gate-infra",
                    "gate-infra",
                ));
            }
            ProbeVerdict::Landed => {}
        }
    }
    if !forge_event_spec_needs_json(frozen.event_spec.as_ref()) {
        return completed(frozen, 0, "").await;
    }
    if frozen
        .probe
        .as_ref()
        .and_then(|p| p.output_probe_argv.as_ref())
        .is_none()
    {
        return Ok(failed(
            "forge action output probe missing for JSON re-extraction; gate-infra",
            "gate-infra",
        ));
    }
    let Some(probe) = &record.probes.output else {
        return admit_probe(ctx, op, frozen, Kind::Output).await;
    };
    let settlement = settlements
        .iter()
        .find(|(kind, _)| matches!(kind, Kind::Output))
        .and_then(|(_, s)| s.as_ref())
        .expect("settled output probe");
    if execution::eligible(probe, settlement)
        && let Ok(result) =
            read_result_file(&execution::probe_path(frozen, &probe.execution_id)).await
        && result.exit_code == 0
    {
        return completed(frozen, 0, &result.stdout).await;
    }
    Ok(failed(
        "forge action output probe failed; gate-infra",
        "gate-infra",
    ))
}

async fn observer_step(ctx: &SpawnCtx, op: &Operation) -> Result<()> {
    let record = execution::record(op)?;
    for kind in [Kind::Action, Kind::Verdict, Kind::Output] {
        if matches!(kind, Kind::Verdict) && record.probes.verdict.is_none()
            || matches!(kind, Kind::Output) && record.probes.output.is_none()
        {
            continue;
        }
        let probe = record.execution(kind);
        if probe.decision == Decision::Observing && probe.release == Release::Held {
            #[cfg(feature = "fixtures")]
            if matches!(kind, Kind::Verdict) {
                crate::test_seams::crash_point("forge-probe-held");
            }
            if let Err(error) = execution::release(ctx, op, kind).await {
                let latest = execution::refresh_claim(ctx, op).await?;
                owned_parked::reconcile(
                    &ForgeActionAdapter,
                    &latest,
                    RecoveryMode::PreDeadlineProbe,
                    ctx,
                )
                .await?;
                return Err(error);
            }
            #[cfg(feature = "fixtures")]
            if matches!(kind, Kind::Verdict) {
                crate::test_seams::crash_point("forge-probe-released");
            }
            break;
        }
        if matches!(probe.release, Release::Released { .. })
            && let Some(engine) = ctx.execution.engine()
        {
            // Unknown is not evidence of a live session; never close it.
            // Once authorization is durable, a failed transport cannot authorize a replay.
            if matches!(
                engine.output(&probe.execution_id, None).await,
                Ok(ExecutionReply::Observation {
                    state: ExecutionState::Spawned | ExecutionState::Exited { .. },
                    ..
                })
            ) {
                // Released is already confirmed. A lost observation or idempotent close
                // response changes neither the decision nor Desired; retry next step.
                let _ = engine.close_stdin(&probe.execution_id).await;
            }
        }
    }
    let latest = execution::refresh_claim(ctx, op).await?;
    owned_parked::reconcile(
        &ForgeActionAdapter,
        &latest,
        RecoveryMode::PreDeadlineProbe,
        ctx,
    )
    .await
}
pub(super) async fn observe(
    ctx: SpawnCtx,
    id: String,
    mut registration: super::observers::Registration,
) {
    #[cfg(feature = "fixtures")]
    let _live = super::observers::Live::new(&id);
    loop {
        observe_inner(&ctx, &id).await;
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point("forge-observer-exit", &id).await;
        if !registration.restart_or_exit() {
            return;
        }
    }
}

async fn observe_inner(ctx: &SpawnCtx, id: &str) {
    #[cfg(feature = "fixtures")]
    if let Ok(Some(op)) = ctx.operation_repo.get_operation(id).await
        && let Some(output) = &op.tx_output
        && let Ok(frozen) = FrozenForge::from_output(output)
    {
        crate::test_seams::crash_point(&format!(
            "forge-pre-go-token:{}",
            frozen
                .event_spec
                .as_ref()
                .map_or("none", |s| s.event_kind.as_str())
        ));
    }
    let mut retry_delay = std::time::Duration::from_millis(50);
    loop {
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point("forge-observer-before-read", id).await;
        match ctx.operation_repo.get_operation(id).await {
            Ok(Some(current)) => {
                if current.phase.tag() != PhaseTag::Parked {
                    if let Ok(record) = execution::record(&current) {
                        ctx.execution.drive(record.ids());
                    }
                    return;
                }
                if current
                    .parked_deadline_ms
                    .is_some_and(|deadline| crate::model::now_ms() >= deadline)
                    && let Ok(record) = execution::record(&current)
                    && record.decision == Decision::Observing
                {
                    // Only the sweep owns the action deadline. Do not compete for
                    // its claim once due. A settled action has no remaining timer
                    // and may still need its probes driven to completion.
                    match owned_execution::settlement(ctx, &record.execution_id).await {
                        Ok(Some(_)) => {}
                        _ => return,
                    }
                }
            }
            Ok(None) => return,
            Err(error) => {
                repository_backoff(id, &error, &mut retry_delay).await;
                continue;
            }
        }
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point("forge-observer-before-claim", id).await;
        let op = match ctx.operation_repo.claim_parked(id).await {
            Ok(op) => op,
            Err(error) => {
                repository_backoff(id, &error, &mut retry_delay).await;
                continue;
            }
        };
        retry_delay = std::time::Duration::from_millis(50);
        if let Some(op) = op {
            let result = observer_step(ctx, &op).await;
            #[cfg(feature = "fixtures")]
            crate::test_seams::pause_point("forge-observer-after-step", id).await;
            if let Err(error) = result {
                // Preserve the error and lease backoff through the same owned recovery path.
                tracing::warn!(op_id=%id,%error,"forge execution retained");
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

async fn repository_backoff(id: &str, error: &CalmError, delay: &mut std::time::Duration) {
    tracing::warn!(op_id=%id, %error, "forge observer repository access will retry");
    tokio::time::sleep(*delay).await;
    *delay = (*delay * 2).min(std::time::Duration::from_secs(2));
}
