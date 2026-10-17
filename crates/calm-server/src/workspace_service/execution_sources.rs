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
    pub(crate) anchor: AttachRegistration,
}

struct SourceAnchor {
    held: OpenedExecutionInput,
    registration: AttachRegistration,
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
pub(crate) async fn prepare_registered_source<F, Fut>(
    pool: &SqlitePool,
    original: &Operation,
    input: &WorkspaceExecutionInput,
    authority: &ExecutionAuthority,
    client: &mut RegisteredExecutorConnection,
    mut authorize: F,
) -> Result<PreparedSource>
where
    F: FnMut() -> Fut + Send,
    Fut: std::future::Future<Output = Result<()>> + Send,
{
    let anchor = reserve_source_anchor(pool, original, input, authority, client).await?;
    // Only this workspace-owned anchor may precede an operation's immutable
    // request. Its exact intent is already in the workspace association.
    authorize().await?;
    client
        .attach(anchor.registration.clone(), &anchor.held.objects)
        .await?;
    let registration = transfer(
        client,
        authority,
        anchor.registration.registration.clone(),
        anchor.registration.incarnation.clone(),
        &anchor.held,
    )?;
    Ok(PreparedSource {
        scope: scope_of(&registration.registration),
        held: anchor.held,
        registration,
        anchor: anchor.registration,
    })
}

async fn reserve_source_anchor(
    pool: &SqlitePool,
    original: &Operation,
    input: &WorkspaceExecutionInput,
    authority: &ExecutionAuthority,
    client: &mut RegisteredExecutorConnection,
) -> Result<SourceAnchor> {
    let WorkspaceExecutionInput::SelectedWorktree {
        workspace_id,
        selection_version,
        ..
    } = input
    else {
        return Err(CalmError::Conflict(
            "registered source requires a selected owned workspace".into(),
        ));
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
            let exported = client
                .export(ExportRegistration {
                    executor_boot: previous.executor_boot.clone(),
                    registration: previous.registration_id.clone(),
                    nonce: TransferNonce::fresh()?,
                })
                .await?;
            match exported {
                RegistrationExport::Held { receipt, objects } => {
                    if receipt.transfer != previous {
                        return Err(CalmError::Conflict(
                            "original source export differs from its workspace association".into(),
                        ));
                    }
                    let held = OpenedExecutionInput::transferred(objects);
                    held.validate()?;
                    return Ok(SourceAnchor {
                        held,
                        registration: previous,
                    });
                }
                RegistrationExport::Unwitnessed => {}
            }
        }
        // A new executor or its exact correlated Unwitnessed fact permits only
        // a new incarnation/generation candidate. It is not proof that old
        // executions are absent; actual Attach still owns that decision.
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
        #[cfg(feature = "fixtures")]
        crate::test_seams::crash_point("workspace-source-intent-committed");
        return Ok(SourceAnchor { held, registration });
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
    #[cfg(feature = "fixtures")]
    crate::test_seams::crash_point("workspace-source-intent-committed");
    Ok(SourceAnchor { held, registration })
}

/// Continue only an operation's already-recorded transfer, using its recorded
/// workspace anchor's actual FDs. Neither a missing lease nor a new boot opens
/// a path under the old immutable identity.
pub(crate) async fn restore_registered_source(
    pool: &SqlitePool,
    original: &Operation,
    input: &WorkspaceExecutionInput,
    anchor: &AttachRegistration,
    registration: &AttachRegistration,
    client: &mut RegisteredExecutorConnection,
) -> Result<OpenedExecutionInput> {
    let WorkspaceExecutionInput::SelectedWorktree {
        workspace_id,
        selection_version,
        ..
    } = input
    else {
        return Err(CalmError::Conflict(
            "original registered input is not selected".into(),
        ));
    };
    let mut tx = begin_immediate_tx(pool).await?;
    require_live_claim_tx(&mut tx, original).await?;
    input.require_current_tx(&mut tx).await?;
    let json: String = sqlx::query_scalar(
        "SELECT registration_intent_json FROM workspace_execution_sources WHERE workspace_id=?1 AND selection_version=?2")
        .bind(workspace_id.as_str()).bind(selection_version).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    if serde_json::from_str::<AttachRegistration>(&json)? != *anchor
        || client.executor_boot() != &anchor.executor_boot
        || registration.executor_boot != anchor.executor_boot
        || registration.incarnation != anchor.incarnation
        || registration.shape != anchor.shape
    {
        return Err(CalmError::Conflict(
            "original workspace anchor changed or is Unwitnessed".into(),
        ));
    }
    let mut source = registration.registration.clone();
    match (&mut source, &anchor.registration) {
        (
            ExecutionRegistration::Worktree { authority, .. },
            ExecutionRegistration::Worktree {
                authority: original,
                ..
            },
        ) => *authority = original.clone(),
        _ => {
            return Err(CalmError::Conflict(
                "original source is not an owned worktree".into(),
            ));
        }
    }
    if source != anchor.registration {
        return Err(CalmError::Conflict(
            "original operation transfer differs from its anchor".into(),
        ));
    }
    let exported = client
        .export(ExportRegistration {
            executor_boot: anchor.executor_boot.clone(),
            registration: anchor.registration_id.clone(),
            nonce: TransferNonce::fresh()?,
        })
        .await?;
    match exported {
        RegistrationExport::Held { receipt, objects } if receipt.transfer == *anchor => {
            let held = OpenedExecutionInput::transferred(objects);
            held.validate()?;
            Ok(held)
        }
        _ => Err(CalmError::Conflict(
            "original workspace anchor has no actual retained objects".into(),
        )),
    }
}

/// Release the current selection's shared source only after its owning end
/// flow has quiesced every execution, while the operation and track fences are
/// still held. Keep the original intent: a rolled-back delete must recover by
/// advancing its generation, never by recreating generation one.
pub(crate) async fn release_track_source(
    pool: &SqlitePool,
    track_id: &str,
    transport: &crate::state::TerminalExecutionTransport,
) -> Result<()> {
    let source: Option<(String, i64, i64, String)> = sqlx::query_as(
        "SELECT b.workspace_id,b.selection_version,s.selection_version,s.registration_intent_json \
         FROM track_workspace_bindings b JOIN workspace_execution_sources s \
         ON s.workspace_id=b.workspace_id WHERE b.track_id=?1",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await?;
    let Some((workspace_id, selection_version, source_version, json)) = source else {
        // An unconverted selection has no executor-owned shared anchor.
        return Ok(());
    };
    let original: AttachRegistration = serde_json::from_str(&json)?;
    if source_version != selection_version
        || !matches!(
            &original.registration,
            ExecutionRegistration::Worktree {
                workspace_id: registered,
                binding_version,
                ..
            } if registered.as_str() == workspace_id.as_str()
                && u64::try_from(selection_version).ok() == Some(*binding_version)
        )
    {
        return Err(CalmError::Conflict(
            "workspace source does not match its owning selection".into(),
        ));
    }
    let crate::state::TerminalExecutionTransport::Registered(config) = transport else {
        return Err(CalmError::Conflict(
            "workspace source requires its configured registered executor for release".into(),
        ));
    };
    let expected = ObjectRegistrationReceipt {
        contract: EXECUTION_ENVELOPE_VERSION,
        id: original.registration_id.clone(),
        transfer: original,
    };
    let mut client = config.connect().await?;
    // The constructed request is not a receipt of retirement. Only the native
    // exact ACK ends ownership. Existing lifecycle rights must not depend on
    // reopening or validating the source's current named path.
    let registration = expected.id.clone();
    let failure = match client
        .request(ExecutionRequest::RetireRegistration(expected))
        .await
    {
        Ok(ExecutionReply::RegistrationRetired {
            registration: retired,
        }) if retired == registration => return Ok(()),
        Ok(ExecutionReply::Refused { reason }) => {
            CalmError::Conflict(format!("workspace source release was refused: {reason}"))
        }
        Ok(_) => CalmError::Conflict("workspace source release acknowledgement mismatched".into()),
        Err(error) => error.into(),
    };
    // A lost ACK or a different boot is not absence. Only QueryV1's exact
    // current-boot/id/nonce Unwitnessed fact can prove this holder is absent;
    // every other outcome preserves the original retirement failure.
    let observation: std::io::Result<RegistrationExport> = async {
        let mut query = config.connect().await?;
        let executor_boot = query.executor_boot().clone();
        query
            .export(ExportRegistration {
                executor_boot,
                registration,
                nonce: TransferNonce::fresh()?,
            })
            .await
    }
    .await;
    match observation {
        Ok(RegistrationExport::Unwitnessed) => Ok(()),
        _ => Err(failure),
    }
}
