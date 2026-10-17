//! One-shot ordinary worker checkout observation. Only named filesystem-mutation exceptions
//! retain their original transaction boundary; normal observation errors never choose them.
use super::*;
use crate::db::sqlite::{
    CheckoutOccupancy, WorkspaceObservation, begin_immediate_tx, checkout_occupancy, task_get_tx,
    track_workspace_read_tx, workspace_observation_snapshot,
};
use crate::model::{Task, TrackWorkspace};
use crate::operation::{
    Operation, PreparedTx, PreparedTxResult, SpawnCtx, preparation_claim::PreparationClaim,
};
use async_trait::async_trait;
use sqlx::Row;

#[async_trait]
pub(crate) trait WorkerAdmission: Send + Sync + Clone + 'static {
    async fn publish_worker(
        &self,
        tx: &mut Tx<'_>,
        op: &Operation,
        plan: WorkerLeasePlan,
    ) -> Result<TxOutput>;
}

#[derive(PartialEq)]
struct LeaseFact {
    lease: super::super::WorkspaceLease,
    owner: String,
    phase: Option<PhaseTag>,
}
#[derive(PartialEq)]
struct Facts {
    task: Task,
    declared: (Option<String>, Option<String>),
    workspace: TrackWorkspace,
    observation: WorkspaceObservation,
    occupancy: CheckoutOccupancy,
    leases: Vec<LeaseFact>,
}

// These are a deliberate owner-level exception, not a generic error fallback.
enum OwnedMutation {
    ManagedRepair,
    UpstreamCatchUp,
}
struct Ordinary {
    plan: WorkerLeasePlan,
    identity: Option<workspace::directory::DirectoryIdentity>,
    managed: Option<workspace::materialize::ManagedWorkspaceObservation>,
}
enum Evidence {
    Ordinary(Result<Box<Ordinary>>),
    OwnedMutation(OwnedMutation),
}
struct PreparedWorker<A> {
    adapter: A,
    claim: PreparationClaim,
    facts: Facts,
    evidence: Evidence,
    root: PathBuf,
}

async fn read_facts(tx: &mut Tx<'_>, track_id: &str, attempt: &str) -> Result<Facts> {
    crate::operation::refuse_if_context_stale(tx, Some(attempt)).await?;
    let task = task_get_tx(tx, attempt)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("task {attempt}")))?;
    if task.track_id != track_id {
        return Err(CalmError::Conflict(
            "worker task belongs to a different track".into(),
        ));
    }
    let declared = sqlx::query_as("SELECT head, base FROM tasks WHERE id=?1")
        .bind(attempt)
        .fetch_one(&mut **tx)
        .await?;
    let workspace = track_workspace_read_tx(tx, track_id).await?;
    let observation = workspace_observation_snapshot(tx, track_id).await?;
    let occupancy = checkout_occupancy(tx, track_id, attempt).await?;
    if !occupancy.admits(task.access) {
        return Err(CalmError::Conflict(
            "track checkout is in use by another attempt".into(),
        ));
    }
    let sql = format!(
        include_str!("preparation_leases.sql"),
        columns = WORKSPACE_LEASE_COLUMNS
    );
    let rows = sqlx::query(&sql)
        .bind(track_id)
        .fetch_all(&mut **tx)
        .await?;
    let leases = rows
        .into_iter()
        .map(|row| {
            let phase: Option<String> = row.try_get("owner_phase")?;
            Ok(LeaseFact {
                owner: row.try_get("lease_owner")?,
                phase: phase
                    .map(|phase| PhaseTag::from_db_str(&phase))
                    .transpose()?,
                lease: row_to_workspace_lease(row)?,
            })
        })
        .collect::<Result<_>>()?;
    Ok(Facts {
        task,
        declared,
        workspace,
        observation,
        occupancy,
        leases,
    })
}

pub(crate) async fn collect<A: WorkerAdmission>(
    adapter: A,
    op: &Operation,
    ctx: &SpawnCtx,
    track_id: &str,
    attempt: &str,
    root: &Path,
) -> Result<Option<Box<dyn PreparedTx>>> {
    let pool = ctx.operation_repo.sqlite_pool();
    let mut tx = begin_immediate_tx(&pool).await?;
    if !crate::operation::repo_sqlite::has_live_claim_tx(&mut tx, op).await? {
        tx.rollback().await?;
        return Ok(None); // Repository's original-claim guard rejects before direct prepare.
    }
    let admitted = read_facts(&mut tx, track_id, attempt).await;
    tx.rollback().await?;
    admitted?;
    observe_worker_checkout(ctx, track_id).await?;
    let mut tx = begin_immediate_tx(&pool).await?;
    let facts = read_facts(&mut tx, track_id, attempt).await;
    tx.rollback().await?;
    let facts = facts?;
    let evidence = observe(&facts).await;
    Ok(Some(Box::new(PreparedWorker {
        adapter,
        claim: PreparationClaim::new(op)?,
        facts,
        evidence,
        root: root.to_owned(),
    })))
}

async fn observe(facts: &Facts) -> Evidence {
    if facts.task.start == TaskStart::Upstream {
        return Evidence::OwnedMutation(OwnedMutation::UpstreamCatchUp);
    }
    let Some(path) = worker_checkout(
        facts.workspace.kind,
        &facts.workspace.path,
        facts.workspace.worktree.as_deref(),
    ) else {
        return Evidence::Ordinary(Err(track_without_worktree()));
    };
    let path = path.to_path_buf();
    let managed = if facts.workspace.kind == TrackWorkspaceKind::Managed {
        let selected = path.clone();
        let track = facts.task.track_id.clone();
        let observed = tokio::task::spawn_blocking(move || {
            workspace::materialize::observe_materialized(&selected, &track)
        })
        .await;
        match observed {
            Ok(Ok(Some(managed))) => Some(managed),
            Ok(Ok(None)) => return Evidence::OwnedMutation(OwnedMutation::ManagedRepair),
            Ok(Err(error)) => return Evidence::Ordinary(Err(error.into())),
            Err(error) => {
                return Evidence::Ordinary(Err(CalmError::Internal(format!(
                    "managed workspace observation: {error}"
                ))));
            }
        }
    } else {
        None
    };
    Evidence::Ordinary(observe_ordinary(facts, path, managed).await)
}
async fn observe_ordinary(
    facts: &Facts,
    path: PathBuf,
    managed: Option<workspace::materialize::ManagedWorkspaceObservation>,
) -> Result<Box<Ordinary>> {
    ensure_clean_tree(&path).await?;
    let branch = worker_branch(&facts.task.track_id, facts.workspace.worktree.is_some())?;
    let access = facts.task.access;
    let (head, base) = facts.declared.clone();
    let worktree = facts.workspace.worktree.clone();
    let track = facts.task.track_id.clone();
    let managed_kind = facts.workspace.kind == TrackWorkspaceKind::Managed;
    let (plan, identity) = tokio::task::spawn_blocking(move || {
        let identity = if managed_kind {
            None
        } else {
            Some(
                workspace::directory::DirectoryIdentity::open(&path, true)?
                    .verify_track_worktree_owner(&track)?,
            )
        };
        let plan = observe_plan(path, branch, access, head, base, worktree, &track, None)?;
        Ok::<_, CalmError>((plan, identity))
    })
    .await
    .map_err(|error| CalmError::Internal(format!("worker checkout observation: {error}")))??;
    Ok(Box::new(Ordinary {
        plan,
        identity,
        managed,
    }))
}

#[async_trait]
impl<A: WorkerAdmission> PreparedTx for PreparedWorker<A> {
    async fn apply(self: Box<Self>, tx: &mut Tx<'_>, op: &Operation) -> Result<PreparedTxResult> {
        self.claim.require_operation(op)?;
        if task_get_tx(tx, &self.facts.task.id).await?.as_ref() != Some(&self.facts.task) {
            return Ok(PreparedTxResult::Stale);
        }
        let current = read_facts(tx, &self.facts.task.track_id, &self.facts.task.id).await?;
        if current != self.facts {
            return Ok(PreparedTxResult::Stale);
        }
        let ordinary = matches!(&self.evidence, Evidence::Ordinary(_));
        let mut plan = match self.evidence {
            Evidence::Ordinary(observed) => {
                let observed = *observed?;
                if let Some(identity) = observed.identity {
                    if !current.observation.matches_identity(&identity) || !identity.is_current()? {
                        return Ok(PreparedTxResult::Stale);
                    }
                    let named = match std::fs::canonicalize(&observed.plan.path) {
                        Ok(named) => named,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            return Ok(PreparedTxResult::Stale);
                        }
                        Err(error) => {
                            return Err(CalmError::Internal(format!(
                                "canonicalize observed checkout: {error}"
                            )));
                        }
                    };
                    if named != identity.root() {
                        return Ok(PreparedTxResult::Stale);
                    }
                }
                if let Some(managed) = observed.managed {
                    if !current.observation.matches_identity(managed.identity()) {
                        return Ok(PreparedTxResult::Stale);
                    }
                    if !workspace::materialize::consume_materialized(
                        managed,
                        &self.root,
                        &observed.plan.path,
                        &current.task.track_id,
                    )? {
                        return Ok(PreparedTxResult::Stale);
                    }
                }
                observed.plan
            }
            // Deliberate low-frequency exceptions: preserve original writeful ownership and
            // recovery rather than widen an external reset/initialization authorization window.
            Evidence::OwnedMutation(
                OwnedMutation::ManagedRepair | OwnedMutation::UpstreamCatchUp,
            ) => {
                prepare_worker_lease_tx(tx, &current.task.track_id, &current.task.id, &self.root)
                    .await?
            }
        };
        if ordinary {
            plan.superseded = supersede_stuck_leases_tx(tx, &plan.path).await?;
        }
        Ok(PreparedTxResult::Ready(
            self.adapter.publish_worker(tx, op, plan).await?,
        ))
    }
}
