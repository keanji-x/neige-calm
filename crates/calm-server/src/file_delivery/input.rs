use super::{publication::Receipt, *};
use crate::{
    db::sqlite::{task_attempt_get_tx, task_get_tx},
    db::{RouteRepo, write_in_tx_typed},
    operation::{Operation, Tx},
};
use calm_task_artifacts::SlotBinding;
use calm_types::task_recovery::TaskAttemptOrigin;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    receipt: Receipt,
    purpose: calm_types::task_execution::JsonInputPurpose,
    directory: String,
}
impl Binding {
    fn slots(&self) -> Result<Vec<SlotBinding>> {
        let FileDelivery::Producer { slot, .. } = &self.receipt.contract else {
            return Err(conflict("bound source is not a producer"));
        };
        Ok(vec![SlotBinding {
            snapshot: self.receipt.snapshot.clone(),
            output: slot.clone(),
            into: "source".into(),
        }])
    }
    fn public_path(&self) -> Result<String> {
        let FileDelivery::Producer { path, .. } = &self.receipt.contract else {
            return Err(conflict("bound source is not a producer"));
        };
        Ok(format!("{}/source/{path}", self.directory))
    }
}
async fn load_tx(
    tx: &mut Tx<'_>,
    task_id: &str,
) -> Result<Option<(Binding, String, Option<String>)>> {
    let row: Option<(String, String, Option<String>)> = sqlx::query_as("SELECT binding_json,state,prepared_operation_id FROM task_file_input_bindings WHERE attempt_id=?1")
        .bind(task_id).fetch_optional(&mut **tx).await?;
    row.map(|(json, state, op)| Ok((serde_json::from_str(&json)?, state, op)))
        .transpose()
}
async fn validate_binding_tx(tx: &mut Tx<'_>, task: &Task, binding: &Binding) -> Result<()> {
    let Some(FileDelivery::Consumer {
        producer,
        slot,
        purpose,
    }) = selection(task)?
    else {
        return Err(conflict("file input consumer contract missing"));
    };
    let (source, _) = source_tx(tx, &binding.receipt.source).await?;
    if source.track_id != task.track_id
        || source.key != producer
        || binding.purpose != purpose
        || binding.directory != "inputs"
        || selection(&source)?.as_ref() != Some(&binding.receipt.contract)
        || !matches!(&binding.receipt.contract, FileDelivery::Producer { slot: output, .. } if output == &slot)
    {
        return Err(conflict("frozen file input authority changed"));
    }
    let json: Option<String> = sqlx::query_scalar("SELECT p.receipt_json FROM task_file_publications p JOIN operations o ON o.id=p.operation_id WHERE p.operation_id=?1 AND o.kind='task-file-publication' AND o.phase='succeeded'")
        .bind(&binding.receipt.publication_operation_id).fetch_optional(&mut **tx).await?;
    if json
        .as_deref()
        .map(serde_json::from_str::<Receipt>)
        .transpose()?
        .as_ref()
        != Some(&binding.receipt)
    {
        return Err(conflict(
            "file input has no matching successful publication",
        ));
    }
    Ok(())
}
/// Runs in the existing claim transaction. Recovery only inherits its predecessor.
pub(crate) async fn bind_claim_tx(tx: &mut Tx<'_>, task: &Task) -> Result<()> {
    if matches!(
        selection(task)?,
        Some(FileDelivery::CandidateConsumer { .. })
    ) {
        return super::candidate_input::bind_claim_tx(tx, task).await;
    }
    let Some(FileDelivery::Consumer {
        producer,
        slot,
        purpose,
    }) = selection(task)?
    else {
        return Ok(());
    };
    if let Some((binding, _, _)) = load_tx(tx, &task.id).await? {
        return validate_binding_tx(tx, task, &binding).await;
    }
    let allocation = task_attempt_get_tx(tx, &task.id)
        .await?
        .ok_or_else(|| conflict("consumer allocation missing"))?;
    let binding = if let TaskAttemptOrigin::Recovery {
        previous_attempt_id,
        ..
    } = allocation.origin
    {
        load_tx(tx, &previous_attempt_id)
            .await?
            .ok_or_else(|| conflict("recovery predecessor file input binding missing"))?
            .0
    } else {
        let json: Option<String> = sqlx::query_scalar("SELECT p.receipt_json FROM task_file_publications p JOIN current_tasks t ON t.id=p.producer_attempt_id JOIN operations o ON o.id=p.operation_id WHERE t.track_id=?1 AND t.key=?2 AND p.slot=?3 AND o.kind='task-file-publication' AND o.phase='succeeded'")
            .bind(&task.track_id).bind(&producer).bind(&slot).fetch_optional(&mut **tx).await?;
        let receipt = serde_json::from_str(
            &json.ok_or_else(|| conflict("waiting for verified file publication"))?,
        )?;
        Binding {
            receipt,
            purpose,
            directory: "inputs".into(),
        }
    };
    validate_binding_tx(tx, task, &binding).await?;
    sqlx::query("INSERT INTO task_file_input_bindings(attempt_id,track_id,publication_operation_id,binding_json,state) VALUES(?1,?2,?3,?4,'bound')")
        .bind(&task.id).bind(&task.track_id).bind(&binding.receipt.publication_operation_id).bind(serde_json::to_string(&binding)?)
        .execute(&mut **tx).await?;
    Ok(())
}
pub(crate) async fn prompt_tx(tx: &mut Tx<'_>, task: &Task) -> Result<String> {
    match selection(task)? {
        Some(FileDelivery::CandidateProducer { .. } | FileDelivery::CandidateConsumer { .. }) => {
            super::candidate_input::prompt(tx, task).await
        }
        Some(FileDelivery::Consumer { producer, .. }) => {
            let (binding, _, _) = load_tx(tx, &task.id)
                .await?
                .ok_or_else(|| conflict("claimed file input binding missing"))?;
            validate_binding_tx(tx, task, &binding).await?;
            Ok(format!(
                "Read the kernel-provided JSON input from producer `{producer}` at `/workspace/{}`. Its JSON syntax was verified; business acceptance remains your task.",
                binding.public_path()?
            ))
        }
        Some(FileDelivery::Producer { slot, path, .. }) => Ok(format!(
            "Write the declared JSON document output `{slot}` at `/workspace/{path}` before reporting completion. The kernel will seal and check JSON syntax after this execution stops."
        )),
        None => Ok("This task starts in a new empty workspace.".into()),
    }
}
async fn authorized_tx(
    tx: &mut Tx<'_>,
    op: &Operation,
) -> Result<Option<(Binding, String, Option<String>)>> {
    let task_id = op
        .idempotency_key
        .as_deref()
        .ok_or_else(|| conflict("input operation task identity missing"))?;
    let task = task_get_tx(tx, task_id)
        .await?
        .ok_or_else(|| conflict("input consumer missing"))?;
    if !matches!(selection(&task)?, Some(FileDelivery::Consumer { .. })) {
        return Ok(None);
    }
    crate::isolated_codex::admission::validate_start_tx(tx, op).await?;
    let binding = load_tx(tx, task_id)
        .await?
        .ok_or_else(|| conflict("claimed file input binding missing"))?;
    validate_binding_tx(tx, &task, &binding.0).await?;
    Ok(Some(binding))
}
fn check_files(binding: Binding, workspace: PathBuf, create: bool) -> Result<()> {
    let store = store(&binding.receipt.store_root)?;
    let slots = binding.slots()?;
    let destination = workspace.join(&binding.directory);
    if create {
        match store.materialize(&slots, &destination) {
            Ok(_) => {}
            Err(calm_task_artifacts::Error::DestinationExists(_)) => {
                store
                    .verify_materialized(&slots, &destination)
                    .map_err(artifact_error)?;
            }
            Err(error) => return Err(artifact_error(error)),
        }
    } else {
        store
            .verify_materialized(&slots, &destination)
            .map_err(artifact_error)?;
    }
    Ok(())
}
pub(crate) async fn prepare_input(
    repo: &dyn RouteRepo,
    op: &Operation,
    workspace: &Path,
) -> Result<()> {
    let owned = op.clone();
    let candidate = write_in_tx_typed(repo, move |tx| {
        Box::pin(async move { is_candidate_tx(tx, &owned).await })
    })
    .await?;
    if candidate {
        return super::candidate_input::prepare(repo, op, workspace).await;
    }
    let owned = op.clone();
    let binding = write_in_tx_typed(repo, move |tx| {
        Box::pin(async move { authorized_tx(tx, &owned).await })
    })
    .await?;
    let Some((binding, state, prepared)) = binding else {
        return Ok(());
    };
    if state == "prepared" && prepared.as_deref() != Some(&op.id) {
        return Err(conflict("input preparation operation changed"));
    }
    let path = workspace.to_owned();
    tokio::task::spawn_blocking(move || check_files(binding, path, true))
        .await
        .map_err(|_| conflict("input preparation interrupted"))??;
    let owned = op.clone();
    write_in_tx_typed(repo, move |tx| Box::pin(async move {
        crate::isolated_codex::journal::require_owner_tx(tx, &owned).await?;
        let Some((_, state, _)) = authorized_tx(tx, &owned).await? else { return Err(conflict("input contract missing")) };
        if state == "bound" {
            sqlx::query("UPDATE task_file_input_bindings SET state='prepared',prepared_operation_id=?1 WHERE attempt_id=?2 AND state='bound'")
                .bind(&owned.id).bind(owned.idempotency_key.as_deref()).execute(&mut **tx).await?;
        }
        Ok(())
    })).await
}
/// Final byte check occurs under TaskLaunch's authority transaction before first turn.
pub(crate) async fn verify_input(tx: &mut Tx<'_>, op: &Operation, workspace: &Path) -> Result<()> {
    if is_candidate_tx(tx, op).await? {
        return super::candidate_input::verify(tx, op, workspace).await;
    }
    let Some((binding, state, prepared)) = authorized_tx(tx, op).await? else {
        return Ok(());
    };
    if state != "prepared" || prepared.as_deref() != Some(&op.id) {
        return Err(conflict(
            "file input has not been prepared by this operation",
        ));
    }
    let path = workspace.to_owned();
    tokio::task::spawn_blocking(move || check_files(binding, path, false))
        .await
        .map_err(|_| conflict("input verification interrupted"))?
}

async fn is_candidate_tx(tx: &mut Tx<'_>, op: &Operation) -> Result<bool> {
    let task = task_get_tx(
        tx,
        op.idempotency_key
            .as_deref()
            .ok_or_else(|| conflict("input task missing"))?,
    )
    .await?
    .ok_or_else(|| conflict("input task missing"))?;
    Ok(matches!(
        selection(&task)?,
        Some(FileDelivery::CandidateConsumer { .. })
    ))
}

/// Recovery admission must not promise a continuation whose original input is missing.
pub(crate) async fn require_recovery_input_tx(tx: &mut Tx<'_>, task: &Task) -> Result<()> {
    if matches!(
        selection(task)?,
        Some(FileDelivery::CandidateConsumer { .. })
    ) {
        return super::candidate_input::require_recovery(tx, task).await;
    }
    if matches!(selection(task)?, Some(FileDelivery::Consumer { .. })) {
        let (binding, _, _) = load_tx(tx, &task.id)
            .await?
            .ok_or_else(|| conflict("recovery predecessor file input binding missing"))?;
        validate_binding_tx(tx, task, &binding).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use calm_task_artifacts::{Entry, FileArtifactPath, FileCaptureRequest};
    use std::os::unix::fs::OpenOptionsExt;

    #[test]
    fn file_delivery_uncertain_materialization_reconciles_exact_bytes_and_never_overwrites() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let store = store(&root).unwrap();
        let original = temp.path().join("source");
        std::fs::write(&original, b"42").unwrap();
        let path = FileArtifactPath::new("result.json", &limits()).unwrap();
        let capture = store
            .capture_file(
                FileCaptureRequest {
                    key: "publication",
                    boundary_id: "source-op",
                    output: "result",
                    path: &path,
                },
                || {
                    Ok(std::fs::OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_NONBLOCK)
                        .open(&original)?)
                },
            )
            .unwrap();
        let manifest = store.open_snapshot(&capture.snapshot).unwrap();
        let Entry::File { digest, .. } = &manifest.manifest().entries[0] else {
            panic!("one ordinary file")
        };
        let binding = Binding {
            receipt: Receipt {
                publication_operation_id: "publication".into(),
                source: PublicationPayload {
                    task_id: "a".into(),
                    track_id: "track".into(),
                    source_operation_id: "source-op".into(),
                },
                contract: FileDelivery::Producer {
                    slot: "result".into(),
                    path: "result.json".into(),
                    policy: calm_types::task_execution::JsonDocumentPolicy::V1,
                },
                snapshot: capture.snapshot,
                file_digest: digest.clone(),
                store_root: root,
            },
            purpose: calm_types::task_execution::JsonInputPurpose::JsonInput,
            directory: "inputs".into(),
        };
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        // Represents a rename that became durable before the binding's prepared stamp.
        store
            .materialize(&binding.slots().unwrap(), &workspace.join("inputs"))
            .unwrap();
        std::fs::remove_file(original).unwrap();
        check_files(binding.clone(), workspace.clone(), true).unwrap();
        check_files(binding.clone(), workspace.clone(), false).unwrap();
        let target = workspace.join("inputs/source/result.json");
        std::fs::write(&target, b"99").unwrap();
        assert!(check_files(binding.clone(), workspace.clone(), true).is_err());
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"99",
            "conflicting prepared input must not be overwritten"
        );
        assert!(check_files(binding, workspace, false).is_err());
    }
}
