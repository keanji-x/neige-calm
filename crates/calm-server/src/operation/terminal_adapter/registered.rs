//! Terminal declares its original input; the workspace/launch owner prepares
//! actual objects and the renderer consumes the returned execution handle.

use super::admission::TerminalExecutionGrant;
use crate::{
    db::sqlite::begin_immediate_tx,
    error::{CalmError, Result},
    operation::{
        Operation, SpawnCtx, SpawnHandle, TxOutput, admission::require_live_claim_tx,
        execution_request, terminal_launch::registered,
    },
    state::TerminalExecutionTransport,
};

pub(super) async fn spawn(
    op: &Operation,
    output: &TxOutput,
    ctx: &SpawnCtx,
    grant: &TerminalExecutionGrant,
) -> Result<SpawnHandle> {
    let TerminalExecutionTransport::Registered(config) = &ctx.daemon.execution else {
        return Err(CalmError::Conflict(
            "original Terminal requires its configured registered executor".into(),
        ));
    };
    let original_request = execution_request::load(ctx.repo.as_ref(), op).await?;
    if original_request.is_none() {
        let pool = ctx.operation_repo.sqlite_pool();
        let mut tx = begin_immediate_tx(&pool).await?;
        require_live_claim_tx(&mut tx, op).await?;
        grant.require_current_tx(&mut tx).await?;
        tx.commit().await?;
    }
    let terminal_id = output.output_string("terminal_id", "terminal")?;
    let term = ctx
        .repo
        .terminal_get(&terminal_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("terminal {terminal_id}")))?;
    if output.output_string("card_id", "terminal")? != term.card_id.as_str()
        || output.output_string("cwd", "terminal")? != grant.input.path()
    {
        return Err(CalmError::Conflict(
            "Terminal prepared subject differs from its original acceptance".into(),
        ));
    }
    let specification = match original_request {
        Some(original) => original.request.specification,
        None => crate::routes::terminal::terminal_execution_specification(
            &output.output_string("program", "terminal")?,
            output.data.get("env").ok_or_else(|| {
                CalmError::Conflict("original Terminal environment is missing".into())
            })?,
        )?,
    };
    let renderer_config = crate::routes::terminal::registered_renderer_config(
        &term,
        grant.input.path(),
        config.socket(),
        &specification,
    )?;
    let execution = registered::launch(op, output, ctx, grant, config, specification).await?;
    let entry = ctx
        .terminal_renderer
        .attach_prepared(renderer_config, execution)
        .await
        .map_err(|error| CalmError::Internal(error.to_string()))?;
    Ok(SpawnHandle::Terminal {
        terminal_id,
        renderer_id: entry.terminal_id.clone(),
    })
}
