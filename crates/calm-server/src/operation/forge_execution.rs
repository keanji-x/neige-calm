//! Forge's sole Pipe intent constructor and durable execution ownership transitions.
pub(crate) use super::owned_execution::{Decision, Release, StopReason, engine, refresh_claim};
use super::{Operation, RecoveryMode, SpawnCtx, Tx, forge_action_adapter as forge};
use crate::{
    error::{CalmError, Result},
    model::now_ms,
};
use calm_truth::db::sqlite::gate_execution as sql;
use environment::WorkspaceEnvironment;
use serde::{Deserialize, Serialize};
use workspace_runtime::execution::*;
mod environment;
#[cfg(test)]
mod protocol_tests;

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
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Record {
    protocol: Protocol,
    pub execution_id: ExecutionId,
    pub release: Release,
    pub decision: Decision,
    pub verdict: Verdict,
    pub probes: Probes,
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum Protocol {
    Legacy,
    Workspace(WorkspaceEnvironment),
}
impl Protocol {
    fn version(&self) -> u8 {
        match self {
            Self::Legacy => 1,
            Self::Workspace(_) => 2,
        }
    }
    fn environment(&self) -> Option<&WorkspaceEnvironment> {
        match self {
            Self::Legacy => None,
            Self::Workspace(environment) => Some(environment),
        }
    }
}

/// Only the released v1 wire shape may omit environment metadata. This DTO is
/// decoded and validated before a domain Record can be constructed.
#[derive(Serialize, Deserialize)]
struct RecordWire {
    version: u8,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_environment"
    )]
    environment: Option<WorkspaceEnvironment>,
    execution_id: ExecutionId,
    release: Release,
    decision: Decision,
    verdict: Verdict,
    probes: Probes,
}
fn present_environment<'de, D: serde::Deserializer<'de>>(
    input: D,
) -> std::result::Result<Option<WorkspaceEnvironment>, D::Error> {
    WorkspaceEnvironment::deserialize(input).map(Some)
}
impl Serialize for Record {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        RecordWire {
            version: self.protocol.version(),
            environment: self.protocol.environment().cloned(),
            execution_id: self.execution_id.clone(),
            release: self.release.clone(),
            decision: self.decision.clone(),
            verdict: self.verdict.clone(),
            probes: self.probes.clone(),
        }
        .serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for Record {
    fn deserialize<D: serde::Deserializer<'de>>(input: D) -> std::result::Result<Self, D::Error> {
        let wire = RecordWire::deserialize(input)?;
        let protocol = match (wire.version, wire.environment) {
            (1, None) => Protocol::Legacy,
            (2, Some(environment)) if environment.validate().is_ok() => {
                Protocol::Workspace(environment)
            }
            _ => return Err(serde::de::Error::custom("invalid forge execution protocol")),
        };
        Ok(Self {
            protocol,
            execution_id: wire.execution_id,
            release: wire.release,
            decision: wire.decision,
            verdict: wire.verdict,
            probes: wire.probes,
        })
    }
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
    Ok(serde_json::from_value(value.clone())?)
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
    previous: Option<&Protocol>,
) -> Result<(ExecutionIntent, Protocol)> {
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
    // Preserve the execution owner's cwd error before project resolution can
    // touch the same missing directory through its environment source.
    let root = tokio::fs::canonicalize(&frozen.cwd_lease)
        .await
        .map_err(|error| {
            conflict(format!(
                "forge cwd_lease {}: {error}",
                frozen.cwd_lease.display()
            ))
        })?;
    let mut environment = forge::forge_environment(ctx.repo.as_ref()).await?;
    environment.push(("NEIGE_FORGE_RESULT_PATH".into(), path.display().to_string()));
    let (environment, wrapper, protocol) = match previous {
        Some(Protocol::Legacy) => (
            environment,
            forge::render_forge_wrapper(argv)?,
            Protocol::Legacy,
        ),
        _ => {
            let (public, read_prefix, contract) =
                if let Some(expected) = previous.and_then(Protocol::environment) {
                    // Probes inherit the action's frozen names. Resolve only at
                    // release, where a refusal has an admitted execution to stop.
                    let release = crate::provider_execution::launch::Release::split(
                        environment
                            .into_iter()
                            .map(|(key, value)| (key, value.into()))
                            .collect(),
                        None,
                        "forge workspace",
                    )?;
                    (release.public, expected.read_prefix()?, expected.clone())
                } else {
                    let (contract, release) =
                        WorkspaceEnvironment::prepare(ctx, frozen, &id, environment, None, || {
                            require_action_admission(ctx, op)
                        })
                        .await??;
                    (release.public, release.read_prefix, contract)
                };
            let environment = public
                .into_iter()
                .map(|(key, value)| {
                    value.into_string().map(|value| (key, value)).map_err(|_| {
                        CalmError::BadRequest("forge bootstrap environment requires UTF-8".into())
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            (
                environment,
                forge::render_forge_wrapper_with_release(argv, &read_prefix)?,
                Protocol::Workspace(contract),
            )
        }
    };
    let intent = ExecutionIntent {
        metadata: ExecutionMetadata {
            consumer: CONSUMER.into(),
            id,
            alias: ProcessAlias(format!("forge:{}{}", op.id, kind.suffix())),
            owner: OperationRef(op.id.clone()),
            scope: workspace_runtime::execution::ExecutionOwnership::Track(TrackId(
                frozen.track_id.clone(),
            )),
            source: PrivateSource {
                root,
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
                args: vec!["-c".into(), wrapper],
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
    };
    Ok((intent, protocol))
}
pub(crate) async fn admit_action(
    ctx: &SpawnCtx,
    op: &Operation,
    frozen: &forge::FrozenForge,
) -> Result<()> {
    let engine = engine(ctx)?;
    let (intent, protocol) = intent(ctx, op, frozen, Kind::Action, None).await?;
    let record = Record {
        protocol,
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
    let write_db = (pool).clone();
    write_db
        .writer()
        .with_writer(|write_session| {
            Box::pin(async move {
                let mut tx = write_session.begin_immediate().await?;
                super::repo_sqlite::require_live_claim_tx(&mut tx, op).await?;
                // Claim plus create-if-absent serialize cleanup with a newer claimant's admission.
                sql::insert(&mut tx, &op.id, KEY, &encoded(&record)?).await?;
                forge::remove_stale_result_files(&frozen.result_path).await?;
                super::repo_sqlite::require_live_claim_tx(&mut tx, op).await?;
                let ticket = engine.admit(&mut tx, intent).await.map_err(conflict)?;
                tx.commit().await?;
                engine.drive(ticket.committed());
                Ok(())
            })
        })
        .await
        .map_err(calm_truth::TruthError::from)?
}

async fn require_action_admission(ctx: &SpawnCtx, op: &Operation) -> Result<()> {
    let db = ctx.operation_repo.sqlite_db().clone();
    db.writer()
        .with_writer(|session| {
            Box::pin(async move {
                let mut tx = session.begin_immediate().await?;
                super::repo_sqlite::require_live_claim_tx(&mut tx, op).await?;
                tx.rollback().await?;
                Ok(())
            })
        })
        .await
        .map_err(calm_truth::TruthError::from)?
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
        Err(error @ (CalmError::WorkspaceBusy(_) | CalmError::OperationBlocked(_))) => {
            return Err(error);
        }
        result => result,
    };
    // Collect environment/cwd I/O outside the writer, but defer its error until
    // the live-claim transaction has distinguished a deleted track.
    let intent = intent(ctx, op, frozen, kind, Some(&old.protocol)).await;
    let write_db = (pool).clone();
    write_db
        .writer()
        .with_writer(|write_session| {
            Box::pin(async move {
                let mut tx = write_session.begin_immediate().await?;
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
                let (intent, _) = intent?;
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
                sql::compare_exchange(&mut tx, &op.id, KEY, &encoded(&old)?, &encoded(&new)?)
                    .await?;
                tx.commit().await?;
                engine.drive(ticket.committed());
                Ok(ProbeAdmission::Admitted)
            })
        })
        .await
        .map_err(calm_truth::TruthError::from)?
}
async fn transition(
    ctx: &SpawnCtx,
    op: &Operation,
    old: &Record,
    new: &Record,
    stop: Option<&ExecutionId>,
) -> Result<()> {
    let pool = ctx.operation_repo.sqlite_db();
    let write_db = (pool).clone();
    write_db
        .writer()
        .with_writer(|write_session| {
            Box::pin(async move {
                let mut tx = write_session.begin_immediate().await?;
                require_transition_owner_tx(&mut tx, op, old, new).await?;
                sql::compare_exchange(&mut tx, &op.id, KEY, &encoded(old)?, &encoded(new)?).await?;
                if let Some(id) = stop {
                    calm_truth::db::sqlite::workspace_execution::request(
                        &mut tx,
                        id.as_str(),
                        "stop",
                    )
                    .await?;
                }
                tx.commit().await?;
                ctx.execution.drive(new.ids());
                Ok(())
            })
        })
        .await
        .map_err(calm_truth::TruthError::from)?
}

async fn require_transition_owner_tx(
    tx: &mut super::Tx<'_>,
    op: &Operation,
    old: &Record,
    new: &Record,
) -> Result<()> {
    super::repo_sqlite::require_live_claim_tx(tx, op).await?;
    // This provision fence belongs only to the action's Held -> Releasing edge.
    if old.release == Release::Held && matches!(new.release, Release::Releasing { .. }) {
        let frozen = forge::FrozenForge::from_output(super::required_output(op)?)?;
        forge::require_provision_target_active_tx(
            tx,
            &frozen.cwd_lease,
            frozen.event_spec.as_ref(),
            &frozen.context,
        )
        .await?;
    }
    sql::require_record(tx, &op.id, KEY, &encoded(old)?).await?;
    Ok(())
}

async fn require_environment_release(
    ctx: &SpawnCtx,
    op: &Operation,
    old: &Record,
    kind: Kind,
) -> Result<()> {
    let mut probe = old.execution(kind);
    if probe.release != Release::Held || probe.decision != Decision::Observing {
        return Err(conflict("forge execution is no longer held for release"));
    }
    probe.release = Release::Releasing {
        authorized_at_ms: now_ms(),
    };
    let mut releasing = old.clone();
    releasing.set(kind, probe);
    let db = ctx.operation_repo.sqlite_db().clone();
    db.writer()
        .with_writer(|session| {
            Box::pin(async move {
                let mut tx = session.begin_immediate().await?;
                require_transition_owner_tx(&mut tx, op, old, &releasing).await?;
                tx.rollback().await?;
                Ok(())
            })
        })
        .await
        .map_err(calm_truth::TruthError::from)?
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

fn release_deadline(probe: &Probe) -> Option<i64> {
    match probe.release {
        Release::Releasing { authorized_at_ms } => {
            Some(authorized_at_ms.saturating_add(RELEASE_TIMEOUT.as_millis() as i64))
        }
        _ => None,
    }
}

fn probe_deadline(kind: Kind, probe: &Probe) -> Option<i64> {
    match (kind, &probe.release) {
        (
            Kind::Verdict | Kind::Output,
            Release::Releasing { authorized_at_ms } | Release::Released { authorized_at_ms },
        ) => Some(authorized_at_ms.saturating_add(PROBE_TIMEOUT_MS)),
        _ => None,
    }
}

/// Timers the live observer can act on; the action deadline belongs to the sweep.
pub(crate) fn observer_deadline(kind: Kind, probe: &Probe) -> Option<i64> {
    if probe.decision != Decision::Observing {
        return None;
    }
    release_deadline(probe)
        .into_iter()
        .chain(probe_deadline(kind, probe))
        .min()
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
    if release_deadline(probe).is_some_and(|at| settled || now >= at) {
        return Some(StopReason::ReleaseRefused);
    }
    let expired = match kind {
        Kind::Action => {
            matches!(mode, RecoveryMode::PastDeadline)
                && op.parked_deadline_ms.is_some_and(|deadline| now > deadline)
        }
        _ => probe_deadline(kind, probe).is_some_and(|at| now >= at),
    };
    (!settled && expired).then_some(StopReason::Timeout)
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
    let payload = match release_payload(ctx, op, &old, kind, &probe.execution_id).await {
        Ok(Ok(payload)) => payload,
        Ok(Err(_)) => return stop(ctx, op, kind, StopReason::ReleaseRefused).await,
        Err(error) => return release_owner_error(ctx, op, kind, error).await,
    };
    let authorized_at_ms = now_ms();
    probe.release = Release::Releasing { authorized_at_ms };
    let mut releasing = old.clone();
    releasing.set(kind, probe.clone());
    if let Err(error) = transition(ctx, op, &old, &releasing, None).await {
        return release_owner_error(ctx, op, kind, error).await;
    }
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

async fn release_owner_error(
    ctx: &SpawnCtx,
    op: &Operation,
    kind: Kind,
    error: CalmError,
) -> Result<()> {
    match error {
        CalmError::BadRequest(_) => stop(ctx, op, kind, StopReason::ReleaseRefused).await,
        error => Err(error),
    }
}

async fn release_payload(
    ctx: &SpawnCtx,
    op: &Operation,
    record: &Record,
    kind: Kind,
    execution: &ExecutionId,
) -> Result<Result<Vec<u8>>> {
    let Some(expected) = record.protocol.environment() else {
        // A v1 Held wrapper still expects exactly its released six-key payload.
        return Ok(forge::credentials::render_payload(|key| {
            std::env::var_os(key)
        }));
    };
    let (frozen, admitted) = match async {
        let frozen = forge::FrozenForge::from_output(super::required_output(op)?)?;
        let row = calm_truth::db::sqlite::workspace_execution::load(
            ctx.operation_repo.sqlite_db().read_pool(),
            execution.as_str(),
        )
        .await?
        .ok_or_else(|| conflict("forge execution missing"))?;
        let admitted: ExecutionMetadata = serde_json::from_str(&row.intent_json)?;
        Ok((frozen, admitted))
    }
    .await
    {
        Ok(prepared) => prepared,
        Err(error) => return environment::payload_result(Err(error)),
    };
    let prepared = WorkspaceEnvironment::prepare(
        ctx,
        &frozen,
        execution,
        admitted.program.environment,
        Some(expected),
        || require_environment_release(ctx, op, record, kind),
    )
    .await?;
    Ok(prepared.map(|(_, release)| release.payload))
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
