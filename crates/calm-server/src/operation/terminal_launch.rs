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
    HandedOff {
        version: u8,
        terminal_id: String,
        supervisor_sock: PathBuf,
        pid: u32,
    },
    Rejected {
        version: u8,
        terminal_id: String,
        supervisor_sock: PathBuf,
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
            Self::NotRequested { version }
            | Self::Requested { version, .. }
            | Self::Rejected { version, .. }
            | Self::HandedOff { version, .. } => *version,
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

/// The worker operations that own card `?1`'s terminal launch.
const WORKER_OPS_OF_CARD: &str = "FROM operations WHERE target_type='card' AND target_id=?1 \
     AND kind IN ('codex-worker','claude-worker','terminal-worker')";

/// Whether a spawn with no task launch of its own starts `card_id`'s terminal: neither a worker
/// operation nor a task owns it, so [`resolve`] answers `Unbound`; otherwise it only attaches.
pub(crate) async fn card_launch_unbound_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card_id: &str,
) -> Result<bool> {
    let worker_owned: bool =
        sqlx::query_scalar(&format!("SELECT EXISTS(SELECT 1 {WORKER_OPS_OF_CARD})"))
            .bind(card_id)
            .fetch_one(&mut **tx)
            .await?;
    if worker_owned {
        return Ok(false);
    }
    let task_owned: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM tasks WHERE worker_card_id=?1)")
            .bind(card_id)
            .fetch_one(&mut **tx)
            .await?;
    Ok(!task_owned)
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
        let rows: Vec<(String,String,Option<String>,String)> = sqlx::query_as(&format!(
            "SELECT id,phase,lease_owner,tx_output_json {WORKER_OPS_OF_CARD} LIMIT 2"
        )).bind(&card).fetch_all(&mut **tx).await?;
        if rows.len() > 1 { return Err(CalmError::Conflict("terminal has conflicting worker operation ownership".into())); }
        let Some((op_id, phase, owner, output)) = rows.into_iter().next() else {
            if launch.is_some() { return Err(CalmError::Conflict("task launch operation does not own this terminal".into())); }
            return Ok(if card_launch_unbound_tx(tx, &card).await? { TerminalStart::Unbound } else { TerminalStart::AttachOnly(sock) });
        };
        let output: Value = serde_json::from_str(&output)?;
        match RequestState::read(&output["data"])? {
            Some(RequestState::Requested {terminal_id: recorded, supervisor_sock,..} | RequestState::HandedOff {terminal_id: recorded, supervisor_sock,..}) => {
                if recorded != terminal_id { return Err(CalmError::Conflict("terminal launch identity disagrees with prepared operation".into())); }
                Ok(TerminalStart::AttachOnly(supervisor_sock))
            }
            Some(RequestState::Rejected {..}) => Err(CalmError::Conflict("terminal launch was rejected; compensate owned operation".into())),
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

/// Called only after a fresh Spawned acknowledgement, persisted PID and actual
/// registry installation. This records ownership transfer, NOT writer quiescence.
pub(crate) async fn hand_off(
    repo: &dyn RouteRepo,
    launch: TaskLaunch,
    terminal_id: String,
    pid: u32,
) -> Result<()> {
    let op = launch.operation().clone();
    write_in_tx_typed(repo, move |tx| Box::pin(async move {
        let changed = sqlx::query("UPDATE operations SET tx_output_json=json_set(tx_output_json,'$.data.terminal_launch.state','handed_off','$.data.terminal_launch.pid',?1) WHERE id=?2 AND lease_owner=?3 AND phase='spawn_started' AND json_extract(tx_output_json,'$.data.terminal_launch.state')='requested' AND json_extract(tx_output_json,'$.data.terminal_launch.terminal_id')=?4 AND EXISTS(SELECT 1 FROM terminals WHERE id=?4 AND pid=?1 AND card_id=operations.target_id)")
            .bind(i64::from(pid)).bind(op.id).bind(op.lease_owner).bind(terminal_id).execute(&mut **tx).await?.rows_affected();
        if changed != 1 { return Err(CalmError::Conflict("terminal ownership handoff changed; retain prepared operation for reconciliation".into())); }
        Ok(())
    })).await
}

/// A complete negative reply only authorizes cleanup after this durable CAS.
pub(crate) async fn reject_no_child(
    repo: &dyn RouteRepo,
    launch: &TaskLaunch,
    terminal_id: &str,
    supervisor_sock: &Path,
) -> Result<()> {
    let op = launch.operation().clone();
    if op.lease_owner.as_deref().is_none_or(str::is_empty) || !supervisor_sock.is_absolute() {
        return Err(CalmError::Conflict(
            "negative launch acknowledgement has no lease or absolute endpoint".into(),
        ));
    }
    let terminal_id = terminal_id.to_owned();
    let socket = supervisor_sock.to_string_lossy().into_owned();
    write_in_tx_typed(repo, move |tx| Box::pin(async move {
        let changed = sqlx::query("UPDATE operations SET tx_output_json=json_set(tx_output_json,'$.data.terminal_launch.state','rejected') \
            WHERE id=?1 AND lease_owner=?2 AND lease_until_ms>=?5 AND phase='spawn_started' AND \
            json_extract(tx_output_json,'$.data.terminal_launch.version')=1 AND \
            json_extract(tx_output_json,'$.data.terminal_launch.state')='requested' AND \
            json_extract(tx_output_json,'$.data.terminal_launch.terminal_id')=?3 AND \
            json_extract(tx_output_json,'$.data.terminal_launch.supervisor_sock')=?4 AND \
            json_extract(tx_output_json,'$.data.terminal_id')=?3 AND target_type='card' AND EXISTS(SELECT 1 FROM \
            terminals WHERE id=?3 AND card_id=operations.target_id AND pid IS NULL)")
            .bind(op.id).bind(op.lease_owner).bind(terminal_id).bind(socket).bind(crate::model::now_ms())
            .execute(&mut **tx).await?.rows_affected();
        if changed != 1 {
            return Err(CalmError::Conflict("negative launch acknowledgement changed; retain prepared resources".into()));
        }
        Ok(())
    })).await
}

/// The owning checkpoint takes precedence over synthetic boot exit evidence.
pub(crate) fn rejected(output: &Value, terminal_id: &str) -> Result<bool> {
    match RequestState::read(output)? {
        Some(RequestState::Rejected {
            terminal_id: recorded,
            supervisor_sock,
            ..
        }) => {
            if recorded != terminal_id || !supervisor_sock.is_absolute() {
                return Err(CalmError::Conflict(
                    "rejected launch identity or endpoint changed; retain resources".into(),
                ));
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub(crate) fn require_not_rejected(output: &Value, terminal_id: &str) -> Result<()> {
    if rejected(output, terminal_id)? {
        return Err(CalmError::Conflict(
            "terminal launch was rejected; compensate owned operation".into(),
        ));
    }
    Ok(())
}

/// Re-fetch because the renderer commits its receipt after the adapter's output
/// was loaded. Never authorize using its stale in-memory launch state.
pub(crate) async fn rejected_for_terminal(repo: &dyn RouteRepo, terminal_id: &str) -> Result<bool> {
    let terminal_id = terminal_id.to_owned();
    write_in_tx_typed(repo, move |tx| Box::pin(async move {
        let rows: Vec<String> = sqlx::query_scalar("SELECT o.tx_output_json FROM operations o JOIN terminals t ON t.card_id=o.target_id WHERE t.id=?1 AND \
            o.target_type='card' AND o.kind IN ('terminal-worker','claude-worker','codex-worker') LIMIT 2")
            .bind(&terminal_id).fetch_all(&mut **tx).await?;
        if rows.len() > 1 { return Err(CalmError::Conflict("conflicting launch ownership; retain resources".into())); }
        match rows.first() {
            Some(row) => rejected(&serde_json::from_str::<Value>(row)?["data"], &terminal_id),
            None => Ok(false),
        }
    })).await
}
