//! One original registered launch in the existing Operation checkpoint. No
//! parallel execution phase, action ledger, source frame or seal journal.

use crate::operation::execution_request::{self, OriginalExecutionRequest};
use crate::{
    db::sqlite::begin_immediate_tx,
    error::{CalmError, Result},
    operation::{Operation, Tx, TxOutput, admission::require_live_claim_tx},
    state::RegisteredExecutorConfig,
    workspace_service::execution_input::OpenedExecutionInput,
};
use calm_session::control::execution::*;
mod recovery;
pub(crate) use recovery::{
    OriginalLaunch, OriginalRegisteredLaunch, lookup, lookup_tx, restore, stop_original,
};

enum ExecutionInputRetention {
    Held(OpenedExecutionInput),
    OriginalControlOnly,
}

#[derive(Clone, Copy)]
enum RenderHistory {
    FreshInitialGeometry,
    UnprovenReattachment,
}

/// Original process control is independent from permission to start a new
/// process. The initial launch also retains its actual transferred objects.
pub(crate) struct RegisteredExecution {
    _retention: ExecutionInputRetention,
    control: crate::terminal_executor_client::ExecutionControlHandle,
    repo: std::sync::Arc<dyn crate::db::RouteRepo>,
    operation_id: String,
    original_payload_hash: String,
    render_history: RenderHistory,
}

impl RegisteredExecution {
    fn new(
        source: OpenedExecutionInput,
        control: crate::terminal_executor_client::ExecutionControlHandle,
        repo: std::sync::Arc<dyn crate::db::RouteRepo>,
        op: &Operation,
        render_history: RenderHistory,
    ) -> Self {
        Self {
            _retention: ExecutionInputRetention::Held(source),
            control,
            repo,
            operation_id: op.id.clone(),
            original_payload_hash: op.payload_hash.clone(),
            render_history,
        }
    }

    pub(crate) fn observation_replay_proven(&self) -> bool {
        matches!(self.render_history, RenderHistory::FreshInitialGeometry)
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
                let original = execution_request::load_tx(tx, &operation)
                    .await?
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
    let existing = execution_request::load(ctx.repo.as_ref(), op).await?;
    let render_history = if existing.is_none() {
        RenderHistory::FreshInitialGeometry
    } else {
        RenderHistory::UnprovenReattachment
    };
    let (original, source) = match existing {
        Some(original) => {
            match recovery::read_original(&mut client, &original.request).await? {
                ExecutionReply::Receipt {
                    state: ExecutionState::Unknown { .. },
                    ..
                } => {
                    return Err(CalmError::Conflict(
                        "original Terminal execution is Unknown; retained for recovery".into(),
                    ));
                }
                ExecutionReply::Receipt {
                    receipt: Some(receipt),
                    ..
                } => {
                    return Ok(std::sync::Arc::new(RegisteredExecution {
                        _retention: ExecutionInputRetention::OriginalControlOnly,
                        control: crate::terminal_executor_client::ExecutionControlHandle::new(
                            config.clone(),
                            original.request,
                            receipt,
                        ),
                        repo: ctx.repo.clone(),
                        operation_id: op.id.clone(),
                        original_payload_hash: op.payload_hash.clone(),
                        render_history: RenderHistory::UnprovenReattachment,
                    }));
                }
                // A refusal is not NoChild. Only the exact original Prepare
                // below can decide whether this accepted intent may continue.
                _ => {}
            }
            admit_new_crossing(op, grant, &pool).await?;
            let held = crate::workspace_service::execution_sources::restore_registered_source(
                &pool,
                op,
                &grant.input,
                &original.source_anchor,
                &original.registration,
                &mut client,
            )
            .await?;
            (original, held)
        }
        None => {
            let prepared = crate::workspace_service::execution_sources::prepare_registered_source(
                &pool,
                op,
                &grant.input,
                &grant.authority,
                &mut client,
                || admit_new_crossing(op, grant, &pool),
            )
            .await?;
            let source = prepared.held;
            let scope = prepared.scope;
            let transfer = prepared.registration;
            #[cfg(feature = "fixtures")]
            crate::test_seams::crash_point("terminal-source-attached-before-request");
            let incarnation = transfer.incarnation.clone();
            let original = OriginalExecutionRequest {
                source_anchor: prepared.anchor,
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
            execution_request::reserve_tx(&mut tx, op, &original).await?;
            tx.commit().await?;
            #[cfg(feature = "fixtures")]
            crate::test_seams::crash_point("terminal-request-reserved-before-prepare");
            (original, source)
        }
    };
    // Per-operation effects are strictly after the original write-once intent.
    // A service crash can replay this exact transfer from the retained anchor.
    admit_new_crossing(op, grant, &pool).await?;
    client
        .attach(original.registration.clone(), &source.objects)
        .await?;
    #[cfg(feature = "fixtures")]
    crate::test_seams::crash_point("terminal-operation-attached-before-prepare");
    // Every new effect uses the same kernel claim and current owning authority;
    // original settled receipt reads will use the executor's exact ticket.
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
            render_history,
        ),
    ))
}
