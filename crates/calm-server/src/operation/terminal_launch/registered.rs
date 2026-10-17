//! One original registered launch in the existing Operation checkpoint. No
//! parallel execution phase, action ledger, source frame or seal journal.

use crate::{
    db::sqlite::begin_immediate_tx,
    error::{CalmError, Result},
    operation::{Operation, Tx, TxOutput, admission::require_live_claim_tx},
    state::RegisteredExecutorConfig,
    workspace_service::execution_input::{OpenedExecutionInput, WorkspaceExecutionInput},
};
use calm_session::control::execution::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
mod recovery;
pub(crate) use recovery::{
    OriginalLaunch, OriginalRegisteredLaunch, lookup, lookup_tx, restore, stop_original,
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegisteredRequest {
    pub(crate) registration: AttachRegistration,
    pub(crate) request: PrepareExecution,
}

impl RegisteredRequest {
    pub(crate) fn read(output: &Value) -> Result<Option<Self>> {
        output
            .get("registered_execution")
            .map(|value| serde_json::from_value(value.clone()).map_err(Into::into))
            .transpose()
    }
}

enum ExecutionInputRetention {
    Held(OpenedExecutionInput),
    OriginalControlOnly,
}

/// Original process control is independent from permission to start a new
/// process. The initial launch also retains its actual transferred objects.
pub(crate) struct RegisteredExecution {
    _retention: ExecutionInputRetention,
    control: crate::terminal_executor_client::ExecutionControlHandle,
    repo: std::sync::Arc<dyn crate::db::RouteRepo>,
    operation_id: String,
    original_payload_hash: String,
}

impl RegisteredExecution {
    fn new(
        source: OpenedExecutionInput,
        control: crate::terminal_executor_client::ExecutionControlHandle,
        repo: std::sync::Arc<dyn crate::db::RouteRepo>,
        op: &Operation,
    ) -> Self {
        Self {
            _retention: ExecutionInputRetention::Held(source),
            control,
            repo,
            operation_id: op.id.clone(),
            original_payload_hash: op.payload_hash.clone(),
        }
    }

    pub(crate) async fn control(&self, action: ProcessAction) -> anyhow::Result<ExecutionReply> {
        if matches!(
            action,
            ProcessAction::WriteStdin { .. } | ProcessAction::Resize { .. }
        ) {
            let id = self.operation_id.clone();
            let hash = self.original_payload_hash.clone();
            let authority = self.control.identity().authority.clone();
            crate::db::write_in_tx_typed(self.repo.as_ref(), move |tx| {
                Box::pin(async move {
                    let row =
                        sqlx::query("SELECT * FROM operations WHERE id=?1 AND payload_hash=?2")
                            .bind(id)
                            .bind(hash)
                            .fetch_one(&mut **tx)
                            .await?;
                    let op = super::super::repo_sqlite::operation_from_row(&row)?;
                    let grant =
                        super::super::terminal_adapter::admission::TerminalExecutionGrant::load_tx(
                            tx, &op,
                        )
                        .await?
                        .ok_or_else(|| {
                            CalmError::Conflict("original Terminal grant is missing".into())
                        })?;
                    if grant.authority != authority {
                        return Err(CalmError::Conflict(
                            "original Terminal control authority changed".into(),
                        ));
                    }
                    grant.require_current_tx(tx).await
                })
            })
            .await?;
            if let ExecutionInputRetention::Held(source) = &self._retention {
                source.validate()?;
            }
        }
        Ok(self.control.control(action).await?)
    }

    pub(crate) async fn revoke(&self) -> std::io::Result<ExecutionReply> {
        self.control.revoke().await
    }

    pub(crate) async fn stop(&self) -> Result<()> {
        let id = self.operation_id.clone();
        let hash = self.original_payload_hash.clone();
        let identity = self.control.identity().clone();
        let original = crate::db::write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move {
                let row = sqlx::query("SELECT * FROM operations WHERE id=?1 AND payload_hash=?2")
                    .bind(id)
                    .bind(hash)
                    .fetch_one(&mut **tx)
                    .await?;
                let operation = super::super::repo_sqlite::operation_from_row(&row)?;
                let original = RegisteredRequest::read(
                    &operation
                        .tx_output
                        .as_ref()
                        .ok_or_else(|| {
                            CalmError::Conflict("original launch output missing".into())
                        })?
                        .data,
                )?
                .ok_or_else(|| CalmError::Conflict("original launch request missing".into()))?;
                if original.request.identity != identity {
                    return Err(CalmError::Conflict(
                        "original terminal control identity changed".into(),
                    ));
                }
                Ok(recovery::OriginalRegisteredLaunch {
                    operation,
                    original,
                })
            })
        })
        .await?;
        stop_original(self.control.config(), &original).await
    }
}

/// Persist the actual immutable request before any registration/prepare/fork
/// message. The owning output slot is written once under the live claim.
async fn reserve_tx(tx: &mut Tx<'_>, op: &Operation, request: &RegisteredRequest) -> Result<()> {
    require_live_claim_tx(tx, op).await?;
    let expected = op
        .tx_output
        .as_ref()
        .ok_or_else(|| CalmError::Conflict("registered launch has no prepared Operation".into()))?;
    if RegisteredRequest::read(&expected.data)?.is_some() {
        return Err(CalmError::Conflict(
            "registered launch already has its original request".into(),
        ));
    }
    let changed = sqlx::query("UPDATE operations SET tx_output_json=json_set(tx_output_json,'$.data.registered_execution',json(?1)) \
        WHERE id=?2 AND tx_output_json=?3 AND json_type(tx_output_json,'$.data.registered_execution') IS NULL")
        .bind(serde_json::to_string(request)?).bind(&op.id).bind(serde_json::to_string(expected)?)
        .execute(&mut **tx).await?.rows_affected();
    if changed != 1 {
        return Err(CalmError::Conflict(
            "original launch checkpoint changed".into(),
        ));
    }
    Ok(())
}

async fn new_source(
    op: &Operation,
    grant: &super::super::terminal_adapter::admission::TerminalExecutionGrant,
    pool: &sqlx::SqlitePool,
    client: &mut RegisteredExecutorConnection,
) -> Result<(
    OpenedExecutionInput,
    ExecutionScope,
    ExecutionRegistration,
    SourceObjectIncarnation,
)> {
    let previous = if let WorkspaceExecutionInput::SelectedWorktree { workspace_id, .. } =
        &grant.input
    {
        let mut tx = begin_immediate_tx(pool).await?;
        let row = sqlx::query("SELECT * FROM operations WHERE id<>?1 AND \
            json_extract(tx_output_json,'$.data.registered_execution.request.identity.scope.Worktree.workspace_id')=?2 \
            ORDER BY created_at_ms,id LIMIT 1")
            .bind(&op.id).bind(workspace_id.as_str()).fetch_optional(&mut *tx).await?;
        let previous = if let Some(row) = row {
            let previous = super::super::repo_sqlite::operation_from_row(&row)?;
            let original = RegisteredRequest::read(
                &previous
                    .tx_output
                    .as_ref()
                    .ok_or_else(|| {
                        CalmError::Conflict("previous source has no original launch".into())
                    })?
                    .data,
            )?
            .ok_or_else(|| CalmError::Conflict("previous source checkpoint is missing".into()))?;
            let accepted =
                super::super::terminal_adapter::admission::TerminalExecutionGrant::load_tx(
                    &mut tx, &previous,
                )
                .await?
                .ok_or_else(|| {
                    CalmError::Conflict("previous source has no original accepted input".into())
                })?;
            if accepted.input != grant.input {
                return Err(CalmError::Conflict(
                    "previous source belongs to another original selection".into(),
                ));
            }
            Some(original)
        } else {
            None
        };
        tx.commit().await?;
        previous
    } else {
        None
    };
    if let Some(previous) = previous {
        // Only actual SCM_RIGHTS export supplies continuity after the kernel
        // holder disappeared. Neither path nor saved stat facts reopen it.
        let (receipt, objects) = client
            .export(ExportRegistration {
                executor_boot: previous.registration.executor_boot.clone(),
                registration: previous.registration.registration_id.clone(),
                nonce: TransferNonce::fresh()?,
            })
            .await?;
        if receipt.transfer != previous.registration {
            return Err(CalmError::Conflict(
                "previous live source export disagrees with original receipt".into(),
            ));
        }
        objects.qualify_local().map_err(|error| {
            CalmError::Conflict(format!("retained worktree qualification: {error:?}"))
        })?;
        let mut registration = previous.registration.registration;
        match &mut registration {
            ExecutionRegistration::Worktree { authority, .. } => {
                *authority = grant.authority.clone()
            }
            _ => {
                return Err(CalmError::Conflict(
                    "selected worktree has an external source checkpoint".into(),
                ));
            }
        }
        return Ok((
            OpenedExecutionInput::transferred(objects),
            previous.request.identity.scope,
            registration,
            previous.registration.incarnation,
        ));
    }
    let input = grant.input.clone();
    let source = tokio::task::spawn_blocking(move || input.open())
        .await
        .map_err(|error| {
            CalmError::Internal(format!("open declared execution input: {error}"))
        })??;
    let (scope, registration) = grant
        .input
        .fresh_registration(&source.objects, grant.authority.clone())?;
    let incarnation = match &grant.input {
        WorkspaceExecutionInput::SelectedWorktree { .. } => {
            SourceObjectIncarnation::Worktree(SourceIncarnationId::fresh()?)
        }
        WorkspaceExecutionInput::ExternalDirectory { .. } => {
            SourceObjectIncarnation::ExternalDir(ExternalDirectoryIncarnationId::fresh()?)
        }
    };
    Ok((source, scope, registration, incarnation))
}

fn consume(request: &PrepareExecution) -> ConsumeExecution {
    ConsumeExecution {
        execution_id: request.identity.execution_id.clone(),
        ticket_id: request.identity.ticket_id.clone(),
        ticket: request.ticket.clone(),
    }
}

async fn admit_new_crossing(
    op: &Operation,
    grant: &super::super::terminal_adapter::admission::TerminalExecutionGrant,
    pool: &sqlx::SqlitePool,
) -> Result<()> {
    let mut tx = begin_immediate_tx(pool).await?;
    require_live_claim_tx(&mut tx, op).await?;
    grant.require_current_tx(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn launch(
    op: &Operation,
    output: &TxOutput,
    ctx: &crate::operation::SpawnCtx,
    grant: &super::super::terminal_adapter::admission::TerminalExecutionGrant,
    config: &RegisteredExecutorConfig,
    specification: ExecutionSpecification,
) -> Result<std::sync::Arc<crate::operation::terminal_launch::registered::RegisteredExecution>> {
    let mut client = config.connect().await?;
    let pool = ctx.operation_repo.sqlite_pool();
    let existing = RegisteredRequest::read(&output.data)?;
    let (original, source) = match existing {
        Some(original) => {
            match client.request(ExecutionRequest::Query(consume(&original.request))).await? {
                ExecutionReply::Receipt {receipt: Some(receipt), ..} if receipt.identity == original.request.identity => {
                    return Ok(std::sync::Arc::new(RegisteredExecution {
                        _retention: ExecutionInputRetention::OriginalControlOnly,
                        control: crate::terminal_executor_client::ExecutionControlHandle::new(config.clone(), original.request, receipt),
                        repo: ctx.repo.clone(), operation_id: op.id.clone(), original_payload_hash: op.payload_hash.clone(),
                    }));
                }
                ExecutionReply::Receipt {state: ExecutionState::Prepared | ExecutionState::Forking, receipt: None} => {},
                _ => return Err(CalmError::Conflict("original execution is unresolved or has no child permission; reconcile original ticket".into())),
            }
            let mut tx = begin_immediate_tx(&pool).await?;
            require_live_claim_tx(&mut tx, op).await?;
            grant.require_current_tx(&mut tx).await?;
            tx.commit().await?;
            // Exact original journal lookup must happen before any path open.
            // Export obtains actual retained objects, never an inode resurrection.
            if client.executor_boot() != &original.request.identity.executor_boot {
                return Err(CalmError::Conflict(
                    "WorkspaceRecoveryRequired: executor restarted without original live source"
                        .into(),
                ));
            }
            let (receipt, held) = client
                .export(ExportRegistration {
                    executor_boot: client.executor_boot().clone(),
                    registration: original.registration.registration_id.clone(),
                    nonce: TransferNonce::fresh()?,
                })
                .await?;
            if receipt.transfer != original.registration {
                return Err(CalmError::Conflict(
                    "exported source differs from original launch".into(),
                ));
            }
            (original, OpenedExecutionInput::transferred(held))
        }
        None => {
            let (source, scope, registration, incarnation) =
                new_source(op, grant, &pool, &mut client).await?;
            let transfer = AttachRegistration {
                registration_id: ObjectRegistrationId::fresh()?,
                executor_boot: client.executor_boot().clone(),
                nonce: TransferNonce::fresh()?,
                incarnation: incarnation.clone(),
                registration,
                shape: source.objects.shape().clone(),
            };
            let original = RegisteredRequest {
                request: PrepareExecution {
                    identity: ExecutionIdentity {
                        execution_id: ExecutionId::fresh()?,
                        executor_boot: client.executor_boot().clone(),
                        ticket_id: LaunchTicketId::fresh()?,
                        source_registration: transfer.registration_id.clone(),
                        source_incarnation: incarnation,
                        authority: grant.authority.clone(),
                        scope,
                        operation_id: op.id.clone(),
                        specification_id: SpecificationId::fresh()?,
                        process_alias: output.output_string("terminal_id", "terminal")?,
                    },
                    ticket: CapabilityToken::fresh()?,
                    control_tokens: ExecutionControlTokens::fresh()?,
                    handoff: None,
                    specification,
                },
                registration: transfer,
            };
            let mut tx = begin_immediate_tx(&pool).await?;
            grant.require_current_tx(&mut tx).await?;
            reserve_tx(&mut tx, op, &original).await?;
            tx.commit().await?;
            (original, source)
        }
    };
    // Every new effect uses the same kernel claim and current owning authority;
    // original settled receipt reads will use the executor's exact ticket.
    admit_new_crossing(op, grant, &pool).await?;
    client
        .attach(original.registration.clone(), &source.objects)
        .await?;
    admit_new_crossing(op, grant, &pool).await?;
    let prepared = client
        .request(ExecutionRequest::Prepare(original.request.clone()))
        .await?;
    let reply = match prepared {
        ExecutionReply::Prepared => {
            admit_new_crossing(op, grant, &pool).await?;
            client
                .request(ExecutionRequest::Launch(consume(&original.request)))
                .await?
        }
        ExecutionReply::Receipt { .. } => prepared,
        ExecutionReply::Refused { reason } => return Err(CalmError::Conflict(reason)),
        _ => {
            return Err(CalmError::Conflict(
                "unexpected executor prepare response".into(),
            ));
        }
    };
    let receipt = match reply {
        ExecutionReply::Receipt {
            receipt: Some(receipt),
            ..
        } if receipt.identity == original.request.identity => receipt,
        ExecutionReply::Receipt { state, .. } => {
            return Err(CalmError::Conflict(format!(
                "execution did not produce an original process receipt: {state:?}"
            )));
        }
        ExecutionReply::Refused { reason } => return Err(CalmError::Conflict(reason)),
        _ => {
            return Err(CalmError::Conflict(
                "unexpected executor launch response".into(),
            ));
        }
    };
    Ok(std::sync::Arc::new(
        crate::operation::terminal_launch::registered::RegisteredExecution::new(
            source,
            crate::terminal_executor_client::ExecutionControlHandle::new(
                config.clone(),
                original.request,
                receipt,
            ),
            ctx.repo.clone(),
            op,
        ),
    ))
}
