//! Retained candidate identity, distinct from machine qualification.
use super::*;
use crate::{db::write_in_tx_typed, operation::*};
use calm_task_artifacts::{FileArtifactPath, FileSetCaptureRequest, SlotBinding, SnapshotId};
use calm_types::task_execution::CandidateMachinePolicy;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Candidate {
    pub publication_operation_id: String,
    pub source: PublicationPayload,
    pub contract: FileDelivery,
    pub snapshot: SnapshotId,
    pub store_root: PathBuf,
}
impl Candidate {
    pub fn policy(&self) -> Result<&CandidateMachinePolicy> {
        match &self.contract {
            FileDelivery::CandidateProducer { policy, .. } => Ok(policy),
            _ => Err(conflict("candidate producer contract missing")),
        }
    }
    pub fn slot(&self) -> Result<&str> {
        match &self.contract {
            FileDelivery::CandidateProducer { slot, .. } => Ok(slot),
            _ => Err(conflict("candidate producer contract missing")),
        }
    }
    pub fn slots(&self) -> Result<Vec<SlotBinding>> {
        Ok(vec![SlotBinding {
            snapshot: self.snapshot.clone(),
            output: self.slot()?.into(),
            into: "source".into(),
        }])
    }
    pub fn prepare(&self, destination: &Path) -> Result<()> {
        let store = store(&self.store_root)?;
        match store.materialize(&self.slots()?, destination) {
            Ok(_) => Ok(()),
            Err(calm_task_artifacts::Error::DestinationExists(_)) => self.verify(destination),
            Err(error) => Err(artifact_error(error)),
        }
    }
    pub fn verify(&self, destination: &Path) -> Result<()> {
        store(&self.store_root)?
            .verify_materialized(&self.slots()?, destination)
            .map_err(artifact_error)?;
        Ok(())
    }
}
pub(crate) async fn load_tx(tx: &mut Tx<'_>, id: &str) -> Result<Candidate> {
    let raw: String =
        sqlx::query_scalar("SELECT candidate_json FROM task_file_candidates WHERE operation_id=?1")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| conflict("candidate missing"))?;
    Ok(serde_json::from_str(&raw)?)
}
pub(crate) async fn authorize_tx(tx: &mut Tx<'_>, candidate: &Candidate) -> Result<()> {
    let (task, _) = source_tx(tx, &candidate.source).await?;
    if selection(&task)?.as_ref() != Some(&candidate.contract)
        || load_tx(tx, &candidate.publication_operation_id).await? != *candidate
    {
        return Err(conflict("candidate authority changed"));
    }
    let succeeded: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM operations WHERE id=?1 AND kind='task-file-publication' AND phase='succeeded')")
        .bind(&candidate.publication_operation_id).fetch_one(&mut **tx).await?;
    if !succeeded {
        return Err(conflict("candidate publication has not succeeded"));
    }
    candidate.policy()?.validate().map_err(conflict)
}
pub(crate) async fn capture(
    repo: &dyn crate::db::RouteRepo,
    op: &Operation,
    task: Task,
    source: crate::isolated_codex::files::FileSnapshot,
) -> Result<()> {
    let contract = selection(&task)?.ok_or_else(|| conflict("candidate contract missing"))?;
    let FileDelivery::CandidateProducer { slot, paths, .. } = &contract else {
        return Err(conflict("not a candidate producer"));
    };
    let root = source.store_root()?;
    let paths = paths
        .iter()
        .map(|p| FileArtifactPath::new(p, &limits()).map_err(artifact_error))
        .collect::<Result<Vec<_>>>()?;
    let slot = slot.clone();
    let owned = op.clone();
    let payload: PublicationPayload = serde_json::from_value(op.payload.clone())?;
    let candidate = tokio::task::spawn_blocking(move || {
        let store = store(&root)?;
        let mut source = Some(source);
        let mut directory = None;
        let captured = store
            .capture_files(
                FileSetCaptureRequest {
                    key: &owned.id,
                    boundary_id: &payload.source_operation_id,
                    output: &slot,
                    paths: &paths,
                },
                |path| {
                    if directory.is_none() {
                        directory =
                            Some(source.take().expect("opened once").open().map_err(|_| {
                                calm_task_artifacts::Error::Invalid(
                                    "source ownership unavailable".into(),
                                )
                            })?);
                    }
                    Ok(crate::routes::fs::open_workspace_regular_file_fd(
                        directory.as_ref().expect("opened"),
                        Path::new(path.as_str()),
                        crate::routes::fs::WorkspaceSymlinks::Refused,
                        true,
                    )?)
                },
            )
            .map_err(artifact_error)?;
        Ok::<_, CalmError>(Candidate {
            publication_operation_id: owned.id,
            source: payload,
            contract,
            snapshot: captured.snapshot,
            store_root: root,
        })
    })
    .await
    .map_err(|_| conflict("candidate capture interrupted"))??;
    let op = op.clone();
    write_in_tx_typed(repo, move |tx| Box::pin(async move {
        super::adapter::require_owner_tx(tx, &op).await?;
        let (task, _) = super::publication::authorize_tx(tx, &op).await?;
        if selection(&task)?.as_ref() != Some(&candidate.contract) { return Err(conflict("candidate contract changed during capture")); }
        let raw = serde_json::to_string(&candidate)?;
        let existing: Option<String> = sqlx::query_scalar("SELECT candidate_json FROM task_file_candidates WHERE operation_id=?1").bind(&op.id).fetch_optional(&mut **tx).await?;
        if let Some(existing) = existing {
            if existing != raw { return Err(conflict("candidate replay identity changed")); }
        } else {
            sqlx::query("INSERT INTO task_file_candidates(operation_id,track_id,producer_attempt_id,slot,candidate_json) VALUES(?1,?2,?3,?4,?5)")
                .bind(&op.id).bind(&task.track_id).bind(&task.id).bind(candidate.slot()?).bind(raw).execute(&mut **tx).await?;
        }
        Ok(())
    })).await
}
