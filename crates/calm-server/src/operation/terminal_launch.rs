//! One-use launch ownership in the existing prepared Operation, shared by UI
//! attachment and task startup. Missing historical state is never permission.
use super::task_launch::TaskLaunch;
use crate::db::{RouteRepo, write_in_tx_typed};
use crate::error::{CalmError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum RequestState {
    NotRequested {
        version: u8,
    },
    Requested {
        version: u8,
        terminal_id: String,
        supervisor_sock: PathBuf,
    },
}
impl RequestState {
    pub(crate) fn read(output: &Value) -> Result<Option<Self>> {
        let Some(value) = output.get("terminal_launch") else {
            return Ok(None);
        };
        let state: Self = serde_json::from_value(value.clone())?;
        let version = match &state {
            Self::NotRequested { version } | Self::Requested { version, .. } => *version,
        };
        if version != 1 {
            return Err(CalmError::Conflict(
                "unknown terminal launch record version; reconcile owned operation".into(),
            ));
        }
        Ok(Some(state))
    }
}
pub(crate) fn fresh_state() -> Value {
    json!({"version":1,"state":"not_requested"})
}

/// Only callers transitioning from a recorded pre-spawn phase may initialize
/// a legacy output. Missing state at SpawnStarted remains permanently unknown.
pub(crate) fn initialize_prestart(kind: &str, output: &mut super::TxOutput) -> Result<()> {
    if matches!(kind, "codex-worker" | "claude-worker" | "terminal-worker") {
        let data = output
            .data
            .as_object_mut()
            .ok_or_else(|| CalmError::Conflict("worker preparation data is missing".into()))?;
        data.entry("terminal_launch").or_insert_with(fresh_state);
    }
    Ok(())
}

pub(crate) enum TerminalStart {
    Unbound,
    Fresh(Box<TaskLaunch>),
    AttachOnly(PathBuf),
}

pub(crate) async fn resolve(
    repo: &dyn RouteRepo,
    terminal_id: &str,
    supervisor_sock: &Path,
    launch: Option<TaskLaunch>,
) -> Result<TerminalStart> {
    let terminal_id = terminal_id.to_owned();
    let sock = if supervisor_sock.is_absolute() {
        supervisor_sock.to_path_buf()
    } else {
        std::env::current_dir()?.join(supervisor_sock)
    };
    write_in_tx_typed(repo, move |tx| Box::pin(async move {
        let card: String = sqlx::query_scalar("SELECT card_id FROM terminals WHERE id=?1")
            .bind(&terminal_id).fetch_optional(&mut **tx).await?
            .ok_or_else(|| CalmError::NotFound(format!("terminal {terminal_id}")))?;
        let rows: Vec<(String,String,Option<String>,String)> = sqlx::query_as(
            "SELECT id,phase,lease_owner,tx_output_json FROM operations WHERE target_type='card' AND target_id=?1 AND kind IN ('codex-worker','claude-worker','terminal-worker') LIMIT 2"
        ).bind(&card).fetch_all(&mut **tx).await?;
        if rows.len() > 1 { return Err(CalmError::Conflict("terminal has conflicting worker operation ownership".into())); }
        let Some((op_id, phase, owner, output)) = rows.into_iter().next() else {
            if launch.is_some() { return Err(CalmError::Conflict("task launch operation does not own this terminal".into())); }
            let task_owned: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM tasks WHERE worker_card_id=?1)")
                .bind(&card).fetch_one(&mut **tx).await?;
            return Ok(if task_owned { TerminalStart::AttachOnly(sock) } else { TerminalStart::Unbound });
        };
        let output: Value = serde_json::from_str(&output)?;
        match RequestState::read(&output["data"])? {
            Some(RequestState::Requested {terminal_id: recorded, supervisor_sock,..}) => {
                if recorded != terminal_id { return Err(CalmError::Conflict("terminal launch identity disagrees with prepared operation".into())); }
                Ok(TerminalStart::AttachOnly(supervisor_sock))
            }
            Some(RequestState::NotRequested {..}) => {
                let Some(launch) = launch else { return Ok(TerminalStart::AttachOnly(sock)); };
                if launch.operation().id != op_id || phase != "spawn_started" || owner.is_none() || owner != launch.operation().lease_owner {
                    return Err(CalmError::Conflict("terminal launch operation lease or phase changed".into()));
                }
                if output["data"]["terminal_id"].as_str() != Some(terminal_id.as_str()) {
                    return Err(CalmError::Conflict("prepared terminal launch target changed".into()));
                }
                let request = serde_json::to_string(&RequestState::Requested {version:1, terminal_id, supervisor_sock:sock})?;
                let changed = sqlx::query("UPDATE operations SET tx_output_json=json_set(tx_output_json,'$.data.terminal_launch',json(?1)) WHERE id=?2 AND lease_owner=?3 AND phase='spawn_started' AND json_extract(tx_output_json,'$.data.terminal_launch.state')='not_requested'")
                    .bind(request).bind(&op_id).bind(owner).execute(&mut **tx).await?.rows_affected();
                if changed != 1 { return Err(CalmError::Conflict("terminal launch request was already consumed".into())); }
                Ok(TerminalStart::Fresh(Box::new(launch)))
            }
            // Old prepared operations may already have sent an unacknowledged
            // EnsureProc. UI and boot can attach, never create a replacement.
            None => Ok(TerminalStart::AttachOnly(sock)),
        }
    })).await
}

pub(crate) async fn reset_unissued(repo: &dyn RouteRepo, launch: &TaskLaunch) -> Result<()> {
    let op = launch.operation().clone();
    write_in_tx_typed(repo, move |tx| Box::pin(async move {
        sqlx::query("UPDATE operations SET tx_output_json=json_set(tx_output_json,'$.data.terminal_launch',json(?1)) WHERE id=?2 AND lease_owner=?3 AND phase='spawn_started' AND json_extract(tx_output_json,'$.data.terminal_launch.state')='requested'")
            .bind(fresh_state().to_string()).bind(op.id).bind(op.lease_owner)
            .execute(&mut **tx).await?;
        Ok(())
    })).await
}
