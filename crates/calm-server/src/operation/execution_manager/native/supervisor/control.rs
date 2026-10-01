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
