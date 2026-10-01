//! Native workspace facts come from provider history, never a missing local turn cache.
use super::*;
use crate::codex_appserver::{ThreadStatus, TurnStatus};
use crate::operation::workspace_lease::execution_guard::{NativeProvider, bind_execution};
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
    fn stopped(&self) -> bool {
        matches!(self.status, ThreadStatus::Idle | ThreadStatus::SystemError)
            && self.turns.iter().all(|turn| {
                matches!(
                    turn.status,
                    TurnStatus::Completed | TurnStatus::Failed | TurnStatus::Interrupted
                )
            })
    }
    fn turn_for_nonce(&self, nonce: &str) -> Result<Option<&NativeTurn>> {
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
            bind_execution(&pool, NativeProvider::Codex, card, thread, cwd).await?;
        }
        Ok(())
    }

    /// A requested stop is durable and seals new requests; acknowledgments never release access.
    pub async fn cancel_native_workspace_guard(&self, lease: &str) -> Result<bool> {
        let pool = self
            .repo
            .sqlite_pool()
            .ok_or_else(|| CalmError::Conflict("native guard database unavailable".into()))?;
        let thread: Option<String> = sqlx::query_scalar(
            "SELECT holder_id FROM workspace_leases WHERE lease_id=?1 AND holder_kind='native' AND \
        native_provider='codex' AND state='held'",
        )
        .bind(lease)
        .fetch_optional(&pool)
        .await?;
        let Some(thread) = thread else {
            return Ok(true);
        };
        self.seal_turn_thread_for_deletion(&thread);
        sqlx::query("UPDATE workspace_leases SET holder_phase='stopping' WHERE lease_id=?1 AND state='held'").bind(lease).execute(&pool).await?;
        self.reconcile_native_workspace_guard(lease).await
    }

    /// Unknown issuance is recoverable only when the exact durable nonce appears in full history.
    pub async fn reconcile_native_workspace_guard(&self, lease: &str) -> Result<bool> {
        let pool = self
            .repo
            .sqlite_pool()
            .ok_or_else(|| CalmError::Conflict("native guard database unavailable".into()))?;
        let row:Option<(String,String,Option<String>,String,String)>=sqlx::query_as(
            "SELECT holder_id,holder_phase,native_client_id,lease_owner,path FROM workspace_leases \
             WHERE lease_id=?1 AND holder_kind='native' AND native_provider='codex' AND state='held'"
        ).bind(lease).fetch_optional(&pool).await?;
        let Some((thread, phase, nonce, known_turn, cwd)) = row else {
            return Ok(true);
        };
        let client = self.connected_client().await?;
        let mut facts = client.thread_workspace_history(&thread).await?.thread;
        if facts.id != thread || std::fs::canonicalize(&facts.cwd)? != std::fs::canonicalize(&cwd)?
        {
            return Err(CalmError::Conflict(
                "native provider workspace facts differ from frozen scope".into(),
            ));
        }
        let turn=match nonce.as_deref() {
            Some(nonce)=>facts.turn_for_nonce(nonce)?,
            None if phase=="running"=>facts.turns.iter().find(|turn|turn.id==known_turn),
            None=>None,
        }.ok_or_else(||CalmError::Conflict("native issuance is unknown; guard remains held until provider history confirms its request".into()))?.id.clone();
        sqlx::query("UPDATE workspace_leases SET lease_owner=?2,holder_phase=CASE WHEN holder_phase='stopping' \
        THEN 'stopping' ELSE 'running' END WHERE lease_id=?1 AND state='held'").bind(lease).bind(&turn).execute(&pool).await?;
        if phase == "stopping" {
            self.seal_turn_thread_for_deletion(&thread);
            client.turn_interrupt(&thread, &turn).await?;
            client.clean_background_terminals(&thread).await?;
            facts = client.thread_workspace_history(&thread).await?.thread;
        }
        if facts.id != thread || std::fs::canonicalize(&facts.cwd)? != std::fs::canonicalize(&cwd)?
        {
            return Err(CalmError::Conflict(
                "stopped native provider scope changed".into(),
            ));
        }
        let matched = facts.turns.iter().find(|candidate| candidate.id == turn);
        if !matched.is_some_and(|candidate| {
            matches!(
                candidate.status,
                TurnStatus::Completed | TurnStatus::Interrupted | TurnStatus::Failed
            )
        }) {
            return Ok(false);
        }
        if !facts.stopped() || !client.background_terminals_stopped(&thread).await? {
            return Ok(false);
        }
        let changed=sqlx::query("UPDATE workspace_leases SET \
        state='released',holder_phase='stopped',released_at_ms=?2,updated_at_ms=?2 WHERE lease_id=?1 AND state='held' AND lease_owner=?3")
            .bind(lease).bind(crate::model::now_ms()).bind(&turn).execute(&pool).await?.rows_affected();
        Ok(changed == 1)
    }
}
