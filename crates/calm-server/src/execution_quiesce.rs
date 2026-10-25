//! Deletion callers own Stop and Retire; occupancy predicates never drive lifecycle.
pub mod owned;
use crate::{
    db::{RouteRepo, write_in_tx_typed},
    error::{CalmError, Result},
    execution_boot::ExecutionEngine,
    operation::terminal_disposal::{Scope, unsettled},
};
use std::sync::{Arc, OnceLock};
use workspace_runtime::execution::{DriveTicket, ExecutionId};

/// Shared by callbacks and operation contexts assembled before execution recovery.
#[derive(Clone, Default)]
pub struct ExecutionLifecycle(Arc<OnceLock<Arc<ExecutionEngine>>>);
impl std::fmt::Debug for ExecutionLifecycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionLifecycle")
            .field("bound", &self.0.get().is_some())
            .finish()
    }
}

impl ExecutionLifecycle {
    pub fn bind(&self, engine: Arc<ExecutionEngine>) {
        assert!(self.0.set(engine).is_ok(), "execution engine already bound");
    }
    pub fn engine(&self) -> Option<&ExecutionEngine> {
        self.0.get().map(Arc::as_ref)
    }
    pub fn drive(&self, ids: impl IntoIterator<Item = ExecutionId>) {
        drive(self.engine(), ids);
    }
}

pub(crate) fn drive(engine: Option<&ExecutionEngine>, ids: impl IntoIterator<Item = ExecutionId>) {
    if let Some(engine) = engine {
        for id in ids {
            engine.drive(DriveTicket::committed(id));
        }
    }
}

pub(crate) async fn quiesce(
    repo: &dyn RouteRepo,
    engine: Option<&ExecutionEngine>,
    scope: Scope,
) -> Result<()> {
    let ids = write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            let ids = unsettled(tx, &scope).await?;
            for id in &ids {
                calm_truth::db::sqlite::workspace_execution::request(tx, id.as_str(), "stop")
                    .await?;
            }
            Ok(ids)
        })
    })
    .await?;
    if ids.is_empty() {
        return Ok(());
    }
    let conflict = || {
        CalmError::Conflict(
            "workspace executions did not settle; rows and workspace retained".into(),
        )
    };
    let engine = engine.ok_or_else(conflict)?;
    drive(Some(engine), ids.iter().cloned());
    // One deadline for the entire scope, not one deadline per execution.
    tokio::time::timeout(workspace_runtime::execution::QUIESCE_DEADLINE, async {
        for id in &ids {
            engine.wait_settled(id).await.map_err(|_| conflict())?;
        }
        Ok(())
    })
    .await
    .map_err(|_| conflict())?
}

/// The route holds the operation and track fences until its final deletion transaction.
/// Terminal aliases retain their existing quiesce/renderer teardown path.
pub(crate) async fn dispose_card(
    repo: &dyn RouteRepo,
    runtime: &crate::operation::OperationRuntime,
    card: &str,
) -> Result<()> {
    let card = card.to_owned();
    let ids = write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            use calm_truth::db::sqlite::workspace_execution as sql;
            let terminal = crate::db::sqlite::terminal_get_by_card_tx(tx, &card).await?;
            let alias = terminal.map(|t| sql::terminal_alias(&t.id));
            let mut ids = Vec::new();
            for execution in sql::card_executions(tx, &card).await? {
                let metadata: workspace_runtime::execution::ExecutionMetadata =
                    serde_json::from_str(&sql::intent_tx(tx, execution.id.as_str()).await?)
                        .map_err(|e| {
                            CalmError::Conflict(format!(
                                "unreadable card execution; resources retained: {e}"
                            ))
                        })?;
                if alias.as_deref() != Some(metadata.alias.0.as_str()) {
                    ids.push(execution.id);
                }
            }
            Ok(ids)
        })
    })
    .await?;
    if ids.is_empty() {
        return Ok(());
    }
    let engine = runtime.execution().engine().ok_or_else(|| {
        CalmError::Conflict("owned execution disposal requires Engine; resources retained".into())
    })?;
    owned::dispose(engine.store(), runtime, &ids).await
}
