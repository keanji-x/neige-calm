//! Forge's sole Pipe intent constructor and durable execution ownership transitions.
pub(crate) use super::owned_execution::{Decision, Release, StopReason, engine, refresh_claim};
use super::{Operation, RecoveryMode, SpawnCtx, Tx, forge_action_adapter as forge};
use crate::{
    error::{CalmError, Result},
    model::now_ms,
};
use calm_truth::db::sqlite::gate_execution as sql;
use serde::{Deserialize, Serialize};
use workspace_runtime::execution::*;

pub(crate) const KEY: &str = "forge_execution";
pub(crate) const CONSUMER: &str = "forge";
pub(crate) const RELEASE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
pub(crate) const PROBE_TIMEOUT_MS: i64 = 60_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Probe {
    pub execution_id: ExecutionId,
    pub release: Release,
    pub decision: Decision,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Verdict {
    Unprobed,
    Landed,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Probes {
    #[serde(deserialize_with = "required_probe")]
    pub verdict: Option<Probe>,
    #[serde(deserialize_with = "required_probe")]
    pub output: Option<Probe>,
}
fn required_probe<'de, D: serde::Deserializer<'de>>(
    input: D,
) -> std::result::Result<Option<Probe>, D::Error> {
    Option::deserialize(input)
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Record {
    version: u8,
    pub execution_id: ExecutionId,
    pub release: Release,
    pub decision: Decision,
    pub verdict: Verdict,
    pub probes: Probes,
}
#[derive(Clone, Copy, Debug)]
pub(crate) enum Kind {
    Action,
    Verdict,
    Output,
}
impl Kind {
    fn suffix(self) -> &'static str {
        match self {
            Self::Action => "",
            Self::Verdict => ":verdict",
            Self::Output => ":output",
        }
    }
}
impl Record {
    pub fn ids(&self) -> Vec<ExecutionId> {
        std::iter::once(self.execution_id.clone())
            .chain(
                self.probes
                    .verdict
                    .iter()
                    .chain(self.probes.output.iter())
                    .map(|p| p.execution_id.clone()),
            )
            .collect()
    }
    pub fn execution(&self, kind: Kind) -> Probe {
        match kind {
            Kind::Action => Probe {
                execution_id: self.execution_id.clone(),
                release: self.release.clone(),
                decision: self.decision.clone(),
            },
            Kind::Verdict => self.probes.verdict.clone().expect("admitted verdict probe"),
            Kind::Output => self.probes.output.clone().expect("admitted output probe"),
        }
    }
    fn set(&mut self, kind: Kind, probe: Probe) {
        match kind {
            Kind::Action => {
                self.release = probe.release;
                self.decision = probe.decision;
            }
            Kind::Verdict => self.probes.verdict = Some(probe),
            Kind::Output => self.probes.output = Some(probe),
        }
    }
}
fn conflict(value: impl std::fmt::Debug) -> CalmError {
    CalmError::Conflict(format!("{value:?}"))
}
// Canonical nested object ordering also survives generic TxOutput round trips.
fn encoded(record: &Record) -> Result<String> {
    Ok(serde_json::to_string(&serde_json::to_value(record)?)?)
}
pub(crate) fn record(op: &Operation) -> Result<Record> {
    let value = op
        .tx_output
        .as_ref()
        .and_then(|o| o.data.get(KEY))
        .ok_or_else(|| conflict("forge execution record missing"))?;
    let record: Record = serde_json::from_value(value.clone())?;
    if record.version != 1 {
        return Err(conflict("unsupported forge execution record"));
    }
    Ok(record)
}
pub(crate) fn probe_path(frozen: &forge::FrozenForge, id: &ExecutionId) -> std::path::PathBuf {
    let mut path = frozen.result_path.as_os_str().to_os_string();
    path.push(format!(".probe.{}", id.as_str()));
    path.into()
}

/// All three consumers differ only in argv, alias and execution-scoped result files.
async fn intent(
    ctx: &SpawnCtx,
    op: &Operation,
    frozen: &forge::FrozenForge,
    kind: Kind,
) -> Result<ExecutionIntent> {
    let id = ExecutionId::fresh()?;
    let (argv, path) = match kind {
        Kind::Action => (&frozen.argv, frozen.result_path.clone()),
        Kind::Verdict => (
            &frozen
                .probe
                .as_ref()
                .ok_or_else(|| conflict("missing probe"))?
                .probe_argv,
            probe_path(frozen, &id),
        ),
        Kind::Output => (
            frozen
                .probe
                .as_ref()
                .and_then(|p| p.output_probe_argv.as_ref())
                .ok_or_else(|| conflict("missing output probe"))?,
            probe_path(frozen, &id),
        ),
    };
    let mut environment = forge::forge_environment(ctx.repo.as_ref()).await?;
    environment.push(("NEIGE_FORGE_RESULT_PATH".into(), path.display().to_string()));
    Ok(ExecutionIntent {
        metadata: ExecutionMetadata {
            consumer: CONSUMER.into(),
            id,
            alias: ProcessAlias(format!("forge:{}{}", op.id, kind.suffix())),
            owner: OperationRef(op.id.clone()),
            track: TrackId(frozen.track_id.clone()),
            source: PrivateSource {
                root: tokio::fs::canonicalize(&frozen.cwd_lease)
                    .await
                    .map_err(|error| {
                        conflict(format!(
                            "forge cwd_lease {}: {error}",
                            frozen.cwd_lease.display()
                        ))
                    })?,
                cwd: workspace::identity::WorkspaceCwd::root(),
                registration: ObjectRegistrationId::fresh()?,
                nonce: TransferNonce::fresh()?,
                directory: ExternalDirectoryId::fresh()?,
                incarnation: ExternalDirectoryIncarnationId::fresh()?,
                authority: ExecutionAuthority {
                    grant_id: ExecutionGrantId::fresh()?,
                    authorization_binding: AuthorizationBindingId::fresh()?,
                    authorization_version: 1,
                },
            },
            program: ExecutionSpecification {
                ambient_environment: calm_session::control::execution::AmbientEnvironment::None,
                program: "/bin/sh".into(),
                args: vec!["-c".into(), forge::render_forge_wrapper(argv)?],
                environment,
                io_mode: calm_session::control::IoMode::Pipe,
                replay_bytes: 8192,
            },
            specification: SpecificationId::fresh()?,
            ticket_id: LaunchTicketId::fresh()?,
        },
        secrets: ExecutionSecrets {
            ticket: CapabilityToken::fresh()?,
            tokens: ExecutionControlTokens::fresh()?,
        },
    })
}
pub(crate) async fn admit_action(
    ctx: &SpawnCtx,
    op: &Operation,
    frozen: &forge::FrozenForge,
) -> Result<()> {
    let engine = engine(ctx)?;
    let intent = intent(ctx, op, frozen, Kind::Action).await?;
    let record = Record {
        version: 1,
        execution_id: intent.metadata.id.clone(),
        release: Release::Held,
        decision: Decision::Observing,
        verdict: Verdict::Unprobed,
        probes: Probes {
            verdict: None,
            output: None,
        },
    };
    let pool = ctx.operation_repo.sqlite_db();
    let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
    super::repo_sqlite::require_live_claim_tx(&mut tx, op).await?;
    // Claim plus create-if-absent serialize cleanup with a newer claimant's admission.
    sql::insert(&mut tx, &op.id, KEY, &encoded(&record)?).await?;
    forge::remove_stale_result_files(&frozen.result_path).await?;
    super::repo_sqlite::require_live_claim_tx(&mut tx, op).await?;
    let ticket = engine.admit(&mut tx, intent).await.map_err(conflict)?;
    tx.commit().await?;
    engine.drive(ticket.committed());
    Ok(())
}

#[derive(Debug)]
pub(crate) enum ProbeAdmission {
    Admitted,
    TrackGone,
}

/// Lifecycle fences precede the writer; claim, track existence and null-slot CAS
/// are checked together. Every guard survives through the admission commit.
pub(crate) async fn admit_probe(
    ctx: &SpawnCtx,
    op: &Operation,
    frozen: &forge::FrozenForge,
    kind: Kind,
) -> Result<ProbeAdmission> {
    let old = record(op)?;
    if !matches!(op.phase, super::Phase::Parked) {
        return Err(conflict("probe requires parked operation"));
    }
    let absent = match kind {
        Kind::Verdict => old.probes.verdict.is_none(),
        Kind::Output => old.probes.output.is_none(),
        Kind::Action => false,
    };
    if !absent {
        return Err(conflict("probe already admitted"));
    }
    let admission = ctx.location_admission.as_ref().ok_or_else(|| {
        CalmError::Internal("forge probe location admission is not configured".into())
    })?;
    let _track = crate::per_card_lock::try_lock_key(&admission.track_locks, &frozen.track_id)
        .ok_or_else(|| CalmError::WorkspaceBusy(frozen.track_id.clone()))?;
    // Resolve aliases before owner lookup. Missing tails remain resolvable after
    // deletion, so the writer can report TrackGone even when cwd no longer exists.
    let pool = ctx.operation_repo.sqlite_db();
    let location = match async {
        let root = workspace::worktree::physical_path(&frozen.cwd_lease).map_err(conflict)?;
        let mut conn = pool.read_pool().acquire().await?;
        let location = admission
            .try_fence(&mut conn, &root, Some(&frozen.track_id))
            .await?;
        Ok((root, location))
    }
    .await
    {
        Err(error @ CalmError::WorkspaceBusy(_)) => return Err(error),
        result => result,
    };
    // Collect environment/cwd I/O outside the writer, but defer its error until
    // the live-claim transaction has distinguished a deleted track.
    let intent = intent(ctx, op, frozen, kind).await;
    let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
    super::repo_sqlite::require_live_claim_tx(&mut tx, op).await?;
    if calm_truth::db::sqlite::track_find_tx(&mut tx, &frozen.track_id)
        .await?
        .is_none()
    {
        return Ok(ProbeAdmission::TrackGone);
    }
    // A deleted track also wins over path errors such as a now-dangling cwd alias.
    let (root, location) = location?;
    let engine = engine(ctx)?;
    let intent = intent?;
    // A cwd alias may have been repointed during intent construction. Never
    // admit a root different from the one whose owners we fenced.
    if intent.metadata.source.root != root {
        return Err(CalmError::WorkspaceBusy(
            frozen.cwd_lease.display().to_string(),
        ));
    }
    location.recheck(&mut tx).await?;
    let mut new = old.clone();
    new.set(
        kind,
        Probe {
            execution_id: intent.metadata.id.clone(),
            release: Release::Held,
            decision: Decision::Observing,
        },
    );
    if matches!(kind, Kind::Output) {
        new.verdict = Verdict::Landed;
    }
    let ticket = engine.admit(&mut tx, intent).await.map_err(conflict)?;
    sql::compare_exchange(&mut tx, &op.id, KEY, &encoded(&old)?, &encoded(&new)?).await?;
    tx.commit().await?;
    engine.drive(ticket.committed());
    Ok(ProbeAdmission::Admitted)
}
async fn transition(
    ctx: &SpawnCtx,
    op: &Operation,
    old: &Record,
    new: &Record,
    stop: Option<&ExecutionId>,
) -> Result<()> {
    let pool = ctx.operation_repo.sqlite_db();
    let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
    super::repo_sqlite::require_live_claim_tx(&mut tx, op).await?;
    sql::compare_exchange(&mut tx, &op.id, KEY, &encoded(old)?, &encoded(new)?).await?;
    if let Some(id) = stop {
        calm_truth::db::sqlite::workspace_execution::request(&mut tx, id.as_str(), "stop").await?;
    }
    tx.commit().await?;
    ctx.execution.drive(new.ids());
    Ok(())
}
pub(crate) async fn stop(
    ctx: &SpawnCtx,
    op: &Operation,
    kind: Kind,
    reason: StopReason,
) -> Result<()> {
    let old = record(op)?;
    let mut probe = old.execution(kind);
    if probe.decision != Decision::Observing {
        ctx.execution.drive(old.ids());
        return Ok(());
    }
    probe.decision = Decision::Stopping { reason };
    let id = probe.execution_id.clone();
    let mut new = old.clone();
    new.set(kind, probe);
    transition(ctx, op, &old, &new, Some(&id)).await
}

/// Only past-deadline recovery owns the action timer; probe and release timers are shared.
pub(crate) fn stop_reason(
    op: &Operation,
    kind: Kind,
    probe: &Probe,
    settled: bool,
    mode: RecoveryMode,
) -> Option<StopReason> {
    if probe.decision != Decision::Observing {
        return None;
    }
    let now = now_ms();
    if let Release::Releasing { authorized_at_ms } = probe.release
        && (settled || now >= authorized_at_ms.saturating_add(RELEASE_TIMEOUT.as_millis() as i64))
    {
        return Some(StopReason::ReleaseRefused);
    }
    let expired = match kind {
        Kind::Action => {
            matches!(mode, RecoveryMode::PastDeadline)
                && op.parked_deadline_ms.is_some_and(|deadline| now > deadline)
        }
        _ => {
            matches!(probe.release,Release::Releasing {authorized_at_ms}|Release::Released {authorized_at_ms} if now>=authorized_at_ms.saturating_add(PROBE_TIMEOUT_MS))
        }
    };
    if !settled && expired {
        return Some(StopReason::Timeout);
    }
    None
}

/// Only a post-park observer sends bytes. Unavailable before authorization leaves Held.
pub(crate) async fn release(ctx: &SpawnCtx, op: &Operation, kind: Kind) -> Result<()> {
    let old = record(op)?;
    let mut probe = old.execution(kind);
    if probe.release != Release::Held || probe.decision != Decision::Observing {
        return Ok(());
    }
    let settled = super::owned_execution::settlement(ctx, &probe.execution_id)
        .await?
        .is_some();
    if let Some(reason) = stop_reason(op, kind, &probe, settled, RecoveryMode::PreDeadlineProbe) {
        return stop(ctx, op, kind, reason).await;
    }
    if settled {
        return Ok(());
    }
    engine(ctx)?;
    let payload = match forge::credentials::render_payload(|key| std::env::var_os(key)) {
        Ok(payload) => payload,
        Err(_) => return stop(ctx, op, kind, StopReason::ReleaseRefused).await,
    };
    let authorized_at_ms = now_ms();
    probe.release = Release::Releasing { authorized_at_ms };
    let mut releasing = old.clone();
    releasing.set(kind, probe.clone());
    transition(ctx, op, &old, &releasing, None).await?;
    match tokio::time::timeout(
        RELEASE_TIMEOUT,
        super::owned_execution::send_release(ctx, &probe.execution_id, &payload),
    )
    .await
    {
        Ok(Ok(())) => {
            #[cfg(feature = "fixtures")]
            crate::test_seams::pause_point("forge-before-release-confirmed", &op.id).await;
            probe.release = Release::Released { authorized_at_ms };
            let mut released = releasing.clone();
            released.set(kind, probe);
            transition(ctx, op, &releasing, &released, None).await
        }
        _ => {
            let latest = refresh_claim(ctx, op).await?;
            stop(ctx, &latest, kind, StopReason::ReleaseRefused).await
        }
    }
}

pub(crate) fn eligible(probe: &Probe, settlement: &Settlement) -> bool {
    probe.decision == Decision::Observing
        && matches!(probe.release, Release::Released { .. })
        && matches!(
            settlement,
            Settlement::Journal(
                ExecutionState::Drained { .. } | ExecutionState::OriginalGroupEmpty { .. }
            )
        )
}
pub(crate) async fn retire(tx: &mut Tx<'_>, op: &Operation) -> Result<()> {
    let record = record(op)?;
    sql::require_record(tx, &op.id, KEY, &encoded(&record)?).await?;
    for id in record.ids() {
        if sql::settlement(tx, id.as_str()).await?.is_none() {
            return Err(conflict("unsettled forge execution"));
        }
        calm_truth::db::sqlite::workspace_execution::request(tx, id.as_str(), "retire").await?;
    }
    Ok(())
}
pub struct Projection;
#[async_trait::async_trait]
impl crate::execution_store::ExecutionProjection for Projection {
    async fn spawned(
        &self,
        _tx: &mut Tx<'_>,
        _alias: &str,
        _now: i64,
    ) -> std::result::Result<(), sqlx::Error> {
        Ok(())
    }
    async fn settled(
        &self,
        _tx: &mut Tx<'_>,
        _alias: &str,
        _settlement: &Settlement,
        _now: i64,
    ) -> std::result::Result<(), sqlx::Error> {
        Ok(())
    }
}
