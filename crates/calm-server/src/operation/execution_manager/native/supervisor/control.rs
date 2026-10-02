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
        let mut executions = storage::native_executions(&self.pool, thread).await?;
        if executions.is_empty() {
            if self.restore_native_scope(backend, thread, false).await? {
                return Ok(());
            }
            executions = storage::native_executions(&self.pool, thread).await?;
        }
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
        self.cancel_native_thread_inner(backend, thread, false)
            .await
    }

    pub(in crate::operation::execution_manager) async fn quiesce_native_scope(
        &self,
        backend: &CodexBackend,
        thread: &str,
    ) -> Result<()> {
        storage::close_native_scope(&self.pool, thread).await?;
        self.cancel_native_thread_inner(backend, thread, true).await
    }

    async fn cancel_native_thread_inner(
        &self,
        backend: &CodexBackend,
        thread: &str,
        close_scope: bool,
    ) -> Result<()> {
        let mut executions = storage::native_executions(&self.pool, thread).await?;
        if executions.is_empty()
            && storage::native_thread_requires_recovery(&self.pool, thread).await?
        {
            if self
                .restore_native_scope(backend, thread, close_scope)
                .await?
            {
                return Ok(());
            }
            executions = storage::native_executions(&self.pool, thread).await?;
        }
        for execution in executions {
            self.cancel(backend, &execution).await?;
        }
        Ok(())
    }
}

impl ExecutionManager {
    pub(in crate::operation::execution_manager) async fn steer_native_protocol(
        &self,
        backend: &CodexBackend,
        session: &str,
        thread: &str,
        turn: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let record = storage::running_native_turn(&self.pool, thread, turn)
            .await?
            .ok_or_else(|| {
                CalmError::Conflict("native steer requires an active managed generation".into())
            })?;
        let id =
            storage::admit_session_control(&self.pool, session, &record, "turn/steer", &params)
                .await?;
        let reply = backend
            .steer_protocol(
                TurnControlPermit {
                    record,
                    turn: turn.into(),
                },
                params,
            )
            .await?;
        sqlx::query("UPDATE native_session_controls SET reply_json=?2 WHERE id=?1")
            .bind(id)
            .bind(reply.to_string())
            .execute(&self.pool)
            .await?;
        Ok(reply)
    }
}

impl ExecutionManager {
    pub(in crate::operation::execution_manager) async fn reply_native_protocol(
        &self,
        backend: &CodexBackend,
        session: &str,
        thread: &str,
        turn: &str,
        frame: serde_json::Value,
    ) -> Result<()> {
        let record = storage::running_native_turn(&self.pool, thread, turn)
            .await?
            .ok_or_else(|| {
                CalmError::Conflict("server response belongs to a stopped generation".into())
            })?;
        storage::admit_session_control(
            &self.pool,
            session,
            &record,
            "server/userInputReply",
            &frame,
        )
        .await?;
        backend
            .reply_protocol(
                TurnControlPermit {
                    record,
                    turn: turn.into(),
                },
                frame,
            )
            .await
    }
}

impl ExecutionManager {
    async fn restore_native_scope(
        &self,
        backend: &CodexBackend,
        thread: &str,
        close_scope: bool,
    ) -> Result<bool> {
        let facts = backend.discover(thread).await?;
        if facts.thread.id != thread || !std::path::Path::new(&facts.thread.cwd).is_absolute() {
            return Err(CalmError::Conflict(
                "discovered native scope identity differs".into(),
            ));
        }
        let stopped =
            facts.thread.stopped() && facts.background_stopped && facts.descendants_stopped;
        let observed = facts
            .thread
            .turns
            .iter()
            .rev()
            .find(|turn| matches!(turn.status, crate::codex_appserver::TurnStatus::InProgress))
            .or_else(|| stopped.then(|| facts.thread.turns.last()).flatten())
            .map(|turn| turn.id.as_str());
        storage::adopt_discovered_native_scope(
            &self.pool,
            thread,
            &facts.thread.cwd,
            observed,
            stopped,
            close_scope,
        )
        .await?;
        Ok(stopped)
    }
}
