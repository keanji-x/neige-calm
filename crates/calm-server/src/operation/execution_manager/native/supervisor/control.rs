//! Existing-turn control consumes a capability minted from the durable generation.
use super::{InputItem, TurnId, execution_backend::CodexBackend};
use crate::error::{CalmError, Result};
use crate::operation::execution_manager::{ExecutionManager, Record, storage};

pub(in crate::operation::execution_manager) struct TurnControlPermit {
    record: Record,
    turn: String,
}
impl TurnControlPermit {
    pub(in crate::operation::execution_manager) fn thread(&self) -> &str {
        &self.record.holder
    }
    pub(in crate::operation::execution_manager) fn turn(&self) -> &str {
        &self.turn
    }
}
impl ExecutionManager {
    pub(in crate::operation::execution_manager) async fn steer_native(
        &self,
        backend: &CodexBackend,
        thread: &str,
        turn: &str,
        items: Vec<InputItem>,
        client_id: Option<&str>,
    ) -> Result<TurnId> {
        let record = storage::running_native_turn(&self.pool, thread, turn)
            .await?
            .ok_or_else(|| {
                CalmError::CodexRefused(
                    "turn/steer failed: no active turn to steer (code -32600)".into(),
                )
            })?;
        let turn = record.observed.clone().ok_or_else(|| {
            CalmError::Conflict("native control lacks an observed generation".into())
        })?;
        backend
            .steer(TurnControlPermit { record, turn }, items, client_id)
            .await
    }
}

impl ExecutionManager {
    pub(in crate::operation::execution_manager) async fn interrupt_native(
        &self,
        backend: &CodexBackend,
        thread: &str,
        turn: &str,
    ) -> Result<()> {
        if storage::native_turn_stopped(&self.pool, thread, turn).await? {
            return Ok(());
        }
        let executions = storage::native_executions(&self.pool, thread).await?;
        let [execution] = executions.as_slice() else {
            return Err(CalmError::Conflict(
                "native interrupt requires one managed generation".into(),
            ));
        };
        let record = storage::load(&self.pool, execution)
            .await?
            .ok_or_else(|| CalmError::Conflict("native interrupt generation changed".into()))?;
        match record.observed.as_deref() {
            Some(observed) if observed != turn => {
                return Err(CalmError::CodexRefused(
                    "turn/interrupt failed: expected turn differs from managed generation".into(),
                ));
            }
            None => {
                use crate::operation::execution_manager::backend::Backend;
                let observation = backend.recover(&record).await?;
                if observation.execution != record.id
                    || observation.identity.as_deref() != Some(turn)
                {
                    return Err(CalmError::Conflict(
                        "native interrupt identity remains unconfirmed".into(),
                    ));
                }
                storage::observe(&self.pool, &record, turn).await?;
            }
            _ => {}
        }
        self.cancel(backend, execution).await?;
        Ok(())
    }

    pub(in crate::operation::execution_manager) async fn cancel_native_thread(
        &self,
        backend: &CodexBackend,
        thread: &str,
    ) -> Result<()> {
        let executions = storage::native_executions(&self.pool, thread).await?;
        if executions.is_empty()
            && storage::native_thread_requires_recovery(&self.pool, thread).await?
        {
            return Err(CalmError::Conflict(
                "persisted native scope requires provider recovery before cancellation".into(),
            ));
        }
        for execution in executions {
            self.cancel(backend, &execution).await?;
        }
        Ok(())
    }
}
