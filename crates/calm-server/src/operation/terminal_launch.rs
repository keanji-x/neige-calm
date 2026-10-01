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
    Stopped {
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
            | Self::HandedOff { version, .. }
            | Self::Stopped { version, .. } => *version,
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
    if matches!(
        kind,
        "codex-worker"
            | "claude-worker"
            | "terminal-worker"
            | "terminal-create"
            | "codex-create"
            | "claude-create"
    ) {
        let data = output
            .data
            .as_object_mut()
            .ok_or_else(|| CalmError::Conflict("worker preparation data is missing".into()))?;
        data.entry("terminal_launch").or_insert_with(fresh_state);
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) enum Launch {
    Task(TaskLaunch),
    Terminal(super::Operation),
}
impl Launch {
    fn operation(&self) -> &super::Operation {
        match self {
            Self::Task(t) => t.operation(),
            Self::Terminal(op) => op,
        }
    }
    pub(crate) async fn run_observed<T, F>(
        self,
        repo: &dyn RouteRepo,
        effect: F,
    ) -> std::result::Result<T, super::task_launch::LaunchFailure>
    where
        T: Send + 'static,
        F: std::future::Future<Output = Result<T>> + Send + 'static,
    {
        match self {
            Self::Task(t) => t.run_observed(repo, effect).await,
            Self::Terminal(op) => {
                write_in_tx_typed(repo, move |tx| {
                    Box::pin(async move {
                        let valid: bool = sqlx::query_scalar(
                            r#"
SELECT EXISTS(SELECT 1 FROM operations WHERE id=?1 AND lease_owner=?2
AND phase='spawn_started'
AND json_extract(tx_output_json,'$.data.terminal_launch.state')='requested')
"#,
                        )
                        .bind(op.id)
                        .bind(op.lease_owner)
                        .fetch_one(&mut **tx)
                        .await?;
                        if !valid {
                            return Err(CalmError::Conflict(
                                "terminal launch ownership changed".into(),
                            ));
                        }
                        Ok(())
                    })
                })
                .await?;
                effect
                    .await
                    .map_err(|error| super::task_launch::LaunchFailure {
                        error,
                        effect_started: true,
                    })
            }
        }
    }
}

pub(crate) enum TerminalStart {
    Unbound,
    Fresh(Box<Launch>),
    AttachOnly(PathBuf),
}

pub(crate) async fn resolve(
    repo: &dyn RouteRepo,
    terminal_id: &str,
    supervisor_sock: &Path,
    launch: Option<TaskLaunch>,
) -> Result<TerminalStart> {
    resolve_for_native_session(repo, terminal_id, supervisor_sock, launch, None).await
}

pub(crate) async fn resolve_for_native_session(
    repo: &dyn RouteRepo,
    terminal_id: &str,
    supervisor_sock: &Path,
    launch: Option<TaskLaunch>,
    native_permit: Option<super::execution_manager::WritePermit>,
) -> Result<TerminalStart> {
    let terminal_id = terminal_id.to_owned();
    let sock = if supervisor_sock.is_absolute() {
        supervisor_sock.to_path_buf()
    } else {
        std::env::current_dir()?.join(supervisor_sock)
    };
    write_in_tx_typed(repo, move |tx| Box::pin(async move {
        let managed = super::execution_manager::authorize_native_session_tx(tx, &terminal_id, native_permit.as_ref()).await?;
        let card: String = sqlx::query_scalar("SELECT card_id FROM terminals WHERE id=?1")
            .bind(&terminal_id).fetch_optional(&mut **tx).await?
            .ok_or_else(|| CalmError::NotFound(format!("terminal {terminal_id}")))?;
        let rows: Vec<(String,String,Option<String>,String)> = sqlx::query_as(
            "SELECT id,phase,lease_owner,tx_output_json FROM operations WHERE (target_type='card' AND target_id=?1 OR json_extract(tx_output_json,'$.data.card_id')=?1) AND kind IN ('codex-worker','claude-worker','terminal-worker','terminal-create','codex-create','claude-create') LIMIT 2"
        ).bind(&card).fetch_all(&mut **tx).await?;
        if rows.len() > 1 { return Err(CalmError::Conflict("terminal has conflicting worker operation ownership".into())); }
        let Some((op_id, phase, owner, output)) = rows.into_iter().next() else {
            if launch.is_some() { return Err(CalmError::Conflict("task launch operation does not own this terminal".into())); }
            // Historical rows carry no authority to create another writer.
            // UI can attach to a surviving process; a missing process needs a new operation.
            return Ok(TerminalStart::AttachOnly(sock));
        };
        let output: Value = serde_json::from_str(&output)?;
        match RequestState::read(&output["data"])? {
            Some(RequestState::Requested {terminal_id: recorded, supervisor_sock,..} | RequestState::HandedOff {terminal_id: recorded, supervisor_sock,..} | RequestState::Stopped {terminal_id:recorded,supervisor_sock,..}) => {
                if recorded != terminal_id { return Err(CalmError::Conflict("terminal launch identity disagrees with prepared operation".into())); }
                Ok(TerminalStart::AttachOnly(supervisor_sock))
            }
            Some(RequestState::NotRequested {..}) => {
                if managed && native_permit.is_none() { return Ok(TerminalStart::AttachOnly(sock)); }
                let launch = match launch {
                    Some(launch)=>Launch::Task(launch),
                    None=> {
                        let row=sqlx::query("SELECT * FROM operations WHERE id=?1 AND kind IN ('terminal-create','codex-create','claude-create')").bind(&op_id).fetch_optional(&mut **tx).await?;
                        let Some(row)=row else {return Ok(TerminalStart::AttachOnly(sock));};
                        let op=super::repo_sqlite::operation_from_row(&row)?;
                        if phase != "spawn_started" || owner.is_none() {return Ok(TerminalStart::AttachOnly(sock));}
                        Launch::Terminal(op)
                    }
                };
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

pub(crate) async fn reset_unissued(repo: &dyn RouteRepo, launch: &Launch) -> Result<()> {
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
    launch: Launch,
    terminal_id: String,
    pid: u32,
) -> Result<()> {
    let op = launch.operation().clone();
    write_in_tx_typed(repo, move |tx| Box::pin(async move {
        let changed = sqlx::query("UPDATE operations SET tx_output_json=json_set(tx_output_json,'$.data.terminal_launch.state','handed_off','$.data.terminal_launch.pid',?1) WHERE id=?2 AND lease_owner=?3 AND phase='spawn_started' AND json_extract(tx_output_json,'$.data.terminal_launch.state')='requested' AND json_extract(tx_output_json,'$.data.terminal_launch.terminal_id')=?4 AND EXISTS(SELECT 1 FROM terminals WHERE id=?4 AND pid=?1 AND card_id=json_extract(operations.tx_output_json,'$.data.card_id'))")
            .bind(i64::from(pid)).bind(op.id).bind(op.lease_owner).bind(&terminal_id).execute(&mut **tx).await?.rows_affected();
        if changed != 1 { return Err(CalmError::Conflict("terminal ownership handoff changed; retain prepared operation for reconciliation".into())); }
        sqlx::query("UPDATE workspace_leases SET holder_phase='running' WHERE holder_kind='terminal' AND holder_id=?1 AND state='held'")
            .bind(&terminal_id).execute(&mut **tx).await?;
        Ok(())
    })).await
}
