//! Workspace-owned source association. Operations retain their own immutable
//! process requests; deleting an operation cannot erase this source lineage.

use calm_session::control::execution::*;
use sqlx::SqlitePool;

use super::execution_input::{OpenedExecutionInput, WorkspaceExecutionInput};
use crate::{
    db::sqlite::begin_immediate_tx,
    error::{CalmError, Result},
    operation::{Operation, admission::require_live_claim_tx},
};

pub(crate) struct PreparedSource {
    pub(crate) held: OpenedExecutionInput,
    pub(crate) scope: ExecutionScope,
    pub(crate) registration: AttachRegistration,
}

fn scope_of(registration: &ExecutionRegistration) -> ExecutionScope {
    match registration {
        ExecutionRegistration::Worktree {
            workspace_id,
            binding_id,
            binding_version,
            generation,
            install_epoch,
            ..
        } => ExecutionScope::Worktree {
            workspace_id: workspace_id.clone(),
            binding_id: binding_id.clone(),
            binding_version: *binding_version,
            generation: *generation,
            install_epoch: *install_epoch,
            cwd: workspace::identity::WorkspaceCwd::root(),
        },
        ExecutionRegistration::ExternalDir {
            directory_id,
            registration_version,
            ..
        } => ExecutionScope::ExternalDir {
            directory_id: directory_id.clone(),
            registration_version: *registration_version,
            cwd: workspace::identity::WorkspaceCwd::root(),
        },
    }
}

fn transfer(
    client: &RegisteredExecutorConnection,
    authority: &ExecutionAuthority,
    mut registration: ExecutionRegistration,
    incarnation: SourceObjectIncarnation,
    held: &OpenedExecutionInput,
) -> Result<AttachRegistration> {
    match &mut registration {
        ExecutionRegistration::Worktree {
            authority: target, ..
        }
        | ExecutionRegistration::ExternalDir {
            authority: target, ..
        } => *target = authority.clone(),
    }
    Ok(AttachRegistration {
        registration_id: ObjectRegistrationId::fresh()?,
        executor_boot: client.executor_boot().clone(),
        nonce: TransferNonce::fresh()?,
        incarnation,
        registration,
        shape: held.objects.shape().clone(),
    })
}

async fn open(input: &WorkspaceExecutionInput) -> Result<OpenedExecutionInput> {
    let input = input.clone();
    tokio::task::spawn_blocking(move || input.open())
        .await
        .map_err(|error| CalmError::Internal(format!("open original workspace source: {error}")))?
}

/// The caller owns the accepted execution/environment/Area decision. This
/// owner supplies physical registration facts and current logical selection;
/// neither this result nor its serialized association is a Ready/SourceProof.
pub(crate) async fn prepare_registered_source(
    pool: &SqlitePool,
    original: &Operation,
    input: &WorkspaceExecutionInput,
    authority: &ExecutionAuthority,
    client: &mut RegisteredExecutorConnection,
) -> Result<PreparedSource> {
    let WorkspaceExecutionInput::SelectedWorktree {
        workspace_id,
        selection_version,
        ..
    } = input
    else {
        let mut tx = begin_immediate_tx(pool).await?;
        require_live_claim_tx(&mut tx, original).await?;
        tx.commit().await?;
        let held = open(input).await?;
        let (_, registration) = input.fresh_registration(&held.objects, authority.clone())?;
        let transfer = transfer(
            client,
            authority,
            registration,
            SourceObjectIncarnation::ExternalDir(ExternalDirectoryIncarnationId::fresh()?),
            &held,
        )?;
        return Ok(PreparedSource {
            scope: scope_of(&transfer.registration),
            held,
            registration: transfer,
        });
    };
    // This short writer read/reservation is scoped to the exact original
    // selection. No operation history scan or domain-specific codec is needed.
    let mut tx = begin_immediate_tx(pool).await?;
    require_live_claim_tx(&mut tx, original).await?;
    input.require_current_tx(&mut tx).await?;
    let current: Option<(i64,String)> = sqlx::query_as(
        "SELECT selection_version,registration_intent_json FROM workspace_execution_sources WHERE workspace_id=?1")
        .bind(workspace_id.as_str()).fetch_optional(&mut *tx).await?;
    tx.commit().await?;
    if let Some((version, json)) = current {
        if version != *selection_version {
            return Err(CalmError::Conflict(
                "workspace source belongs to another original selection".into(),
            ));
        }
        let previous: AttachRegistration = serde_json::from_str(&json)?;
        match &previous.registration {
            ExecutionRegistration::Worktree {
                workspace_id: registered,
                binding_version,
                root,
                ..
            } if registered == workspace_id
                && i64::try_from(*binding_version).ok() == Some(*selection_version)
                && root.path == std::path::Path::new(input.path()) => {}
            _ => {
                return Err(CalmError::Conflict(
                    "workspace source association is inconsistent".into(),
                ));
            }
        }
        if &previous.executor_boot == client.executor_boot() {
            let (receipt, objects) = client
                .export(ExportRegistration {
                    executor_boot: previous.executor_boot.clone(),
                    registration: previous.registration_id.clone(),
                    nonce: TransferNonce::fresh()?,
                })
                .await?;
            if receipt.transfer != previous {
                return Err(CalmError::Conflict(
                    "original source export differs from its workspace association".into(),
                ));
            }
            let held = OpenedExecutionInput::transferred(objects);
            held.validate()?;
            let registration = transfer(
                client,
                authority,
                previous.registration,
                previous.incarnation,
                &held,
            )?;
            return Ok(PreparedSource {
                scope: scope_of(&registration.registration),
                held,
                registration,
            });
        }
        // Explicit new-executor recovery installs a NEW incarnation/generation.
        // The existing executor attachment gate checks every original record's
        // durable settlement; Unknown cannot pass. No kernel history census.
        let held = open(input).await?;
        let mut registration = previous.registration;
        let ExecutionRegistration::Worktree {
            generation,
            install_epoch,
            admission_open,
            ..
        } = &mut registration
        else {
            unreachable!()
        };
        if !*admission_open {
            return Err(CalmError::Conflict(
                "closed workspace source requires its owning recovery decision".into(),
            ));
        }
        *generation = generation
            .checked_add(1)
            .ok_or_else(|| CalmError::Conflict("workspace generation exhausted".into()))?;
        *install_epoch = install_epoch
            .checked_add(1)
            .ok_or_else(|| CalmError::Conflict("workspace installation epoch exhausted".into()))?;
        if let ExecutionRegistration::Worktree { root, .. } = &mut registration {
            use std::os::unix::fs::MetadataExt;
            let metadata = held.objects.root().metadata()?;
            *root = DirectoryRegistration {
                path: input.path().into(),
                device: metadata.dev(),
                inode: metadata.ino(),
            };
        }
        let registration = transfer(
            client,
            authority,
            registration,
            SourceObjectIncarnation::Worktree(SourceIncarnationId::fresh()?),
            &held,
        )?;
        let mut tx = begin_immediate_tx(pool).await?;
        require_live_claim_tx(&mut tx, original).await?;
        input.require_current_tx(&mut tx).await?;
        let changed = sqlx::query("UPDATE workspace_execution_sources SET registration_intent_json=?1 WHERE workspace_id=?2 AND selection_version=?3 AND registration_intent_json=?4")
            .bind(serde_json::to_string(&registration)?).bind(workspace_id.as_str()).bind(selection_version).bind(json)
            .execute(&mut *tx).await?.rows_affected();
        if changed != 1 {
            return Err(CalmError::Conflict(
                "workspace source changed during recovery reservation".into(),
            ));
        }
        tx.commit().await?;
        return Ok(PreparedSource {
            scope: scope_of(&registration.registration),
            held,
            registration,
        });
    }
    let held = open(input).await?;
    let (_, registration) = input.fresh_registration(&held.objects, authority.clone())?;
    let registration = transfer(
        client,
        authority,
        registration,
        SourceObjectIncarnation::Worktree(SourceIncarnationId::fresh()?),
        &held,
    )?;
    let mut tx = begin_immediate_tx(pool).await?;
    require_live_claim_tx(&mut tx, original).await?;
    input.require_current_tx(&mut tx).await?;
    let inserted = sqlx::query("INSERT INTO workspace_execution_sources(workspace_id,selection_version,registration_intent_json) VALUES (?1,?2,?3) ON CONFLICT(workspace_id) DO NOTHING")
        .bind(workspace_id.as_str()).bind(selection_version).bind(serde_json::to_string(&registration)?)
        .execute(&mut *tx).await?.rows_affected();
    if inserted != 1 {
        return Err(CalmError::Conflict(
            "workspace source was concurrently reserved; retry its original association".into(),
        ));
    }
    tx.commit().await?;
    Ok(PreparedSource {
        scope: scope_of(&registration.registration),
        held,
        registration,
    })
}
