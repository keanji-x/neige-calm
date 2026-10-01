//! Native workspace facts come from provider history, never a missing local turn cache.
use super::*;
use crate::codex_appserver::{ThreadStatus, TurnStatus};
use crate::operation::workspace_lease::execution_guard::NativeProvider;
use serde_json::Value;
use std::path::Path;

#[derive(serde::Deserialize)]
pub(crate) struct NativeThreadRead {
    pub thread: NativeThread,
}
#[derive(serde::Deserialize)]
pub(crate) struct NativeThread {
    pub id: String,
    pub cwd: String,
    pub status: ThreadStatus,
    pub turns: Vec<NativeTurn>,
}
#[derive(serde::Deserialize)]
pub(crate) struct NativeTurn {
    pub id: String,
    pub status: TurnStatus,
    pub items: Vec<Value>,
}
impl NativeThread {
    pub(super) fn stopped(&self) -> bool {
        matches!(self.status, ThreadStatus::Idle | ThreadStatus::SystemError)
            && self.turns.iter().all(|turn| {
                matches!(
                    turn.status,
                    TurnStatus::Completed | TurnStatus::Failed | TurnStatus::Interrupted
                )
            })
    }
    pub(super) fn turn_for_nonce(&self, nonce: &str) -> Result<Option<&NativeTurn>> {
        let mut matches = self.turns.iter().filter(|turn| {
            turn.items.iter().any(|item| {
                item.get("type").and_then(Value::as_str) == Some("userMessage")
                    && item.get("clientId").and_then(Value::as_str) == Some(nonce)
            })
        });
        let found = matches.next();
        if matches.next().is_some() {
            return Err(CalmError::Conflict(
                "native request identity appears in multiple provider turns".into(),
            ));
        }
        Ok(found)
    }
}

impl SharedCodexAppServer {
    pub(super) async fn bind_resumed_workspace(
        &self,
        thread: &str,
        card: &str,
        response: &Value,
    ) -> Result<()> {
        let id = response.get("id").and_then(Value::as_str).ok_or_else(|| {
            CalmError::CodexAppServer("resumed thread identity is missing".into())
        })?;
        if id != thread {
            return Err(CalmError::Conflict(
                "resumed provider thread identity differs".into(),
            ));
        }
        let cwd = response.get("cwd").and_then(Value::as_str).ok_or_else(|| {
            CalmError::CodexAppServer("resumed provider thread cwd is missing".into())
        })?;
        if !Path::new(cwd).is_absolute() {
            return Err(CalmError::Conflict(
                "resumed provider cwd is not absolute".into(),
            ));
        }
        if let Some(pool) = self.repo.sqlite_pool() {
            let scope = serde_json::from_value::<NativeThread>(response.clone());
            let observed = scope.as_ref().ok().and_then(|scope| {
                scope
                    .turns
                    .iter()
                    .rev()
                    .find(|turn| matches!(turn.status, TurnStatus::InProgress))
                    .map(|turn| turn.id.clone())
            });
            let stopped = match &scope {
                Ok(scope) if scope.stopped() => self
                    .connected_client()
                    .await?
                    .background_terminals_stopped(thread)
                    .await
                    .unwrap_or(false),
                _ => false,
            };
            let read =
                crate::operation::workspace_lease::task_guard::is_read_card(&pool, card).await?;
            let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
            crate::operation::workspace_lease::execution_guard::adopt_native_scope_tx(
                &mut tx,
                card,
                thread,
                NativeProvider::Codex,
                cwd,
                if read {
                    calm_types::workspace_access::WorkspaceAccess::ReadOnly
                } else {
                    calm_types::workspace_access::WorkspaceAccess::ReadWrite
                },
                observed.as_deref(),
                stopped,
            )
            .await?;
            tx.commit().await?;
        }
        Ok(())
    }

    pub(super) async fn retain_unresolved_resumed_scope(
        &self,
        thread: &str,
        card: &str,
        response: &Value,
    ) -> Result<()> {
        let Some(pool) = self.repo.sqlite_pool() else {
            return Ok(());
        };
        let actual = response
            .get("cwd")
            .and_then(Value::as_str)
            .filter(|cwd| Path::new(cwd).is_absolute())
            .filter(|_| response.get("id").and_then(Value::as_str) == Some(thread))
            .and_then(|cwd| std::fs::canonicalize(cwd).ok())
            .and_then(|cwd| cwd.to_str().map(str::to_owned));
        if let Some(cwd) = actual {
            let mut conn = pool.acquire().await?;
            crate::operation::workspace_lease::execution_guard::persist_execution_scope(
                &mut conn,
                NativeProvider::Codex,
                card,
                thread,
                &cwd,
                calm_types::workspace_access::WorkspaceScopePhase::Recovering,
            )
            .await?;
        } else {
            sqlx::query("DELETE FROM workspace_execution_bindings WHERE provider='codex' AND holder_id=?1 AND card_id=?2")
                .bind(thread).bind(card).execute(&pool).await?;
        }
        Ok(())
    }

    /// Managed cancellation owns the durable request and positive release transaction.
    pub async fn cancel_native_workspace_guard(&self, lease: &str) -> Result<bool> {
        let pool = self
            .repo
            .sqlite_pool()
            .ok_or_else(|| CalmError::Conflict("execution storage unavailable".into()))?;
        super::super::super::ExecutionManager::new(pool)
            .cancel(&super::execution_backend::CodexBackend(self), lease)
            .await
    }

    /// Recovery is a manager transition; the native backend supplies only evidence.
    pub async fn reconcile_native_workspace_guard(&self, lease: &str) -> Result<bool> {
        let pool = self
            .repo
            .sqlite_pool()
            .ok_or_else(|| CalmError::Conflict("execution storage unavailable".into()))?;
        super::super::super::ExecutionManager::new(pool)
            .recover(&super::execution_backend::CodexBackend(self), lease)
            .await
    }
}
