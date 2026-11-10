//! Conditional daemon-row writes. Native observation and settlement proof belong
//! to the shared owner; this layer never observes or signals processes.
use super::SqliteDb;
use crate::db::{
    DaemonRecovery, RecoveryWrite, SharedCodexDaemonRecord, SharedCodexDaemonUpdate,
    SharedDaemonState,
};
use crate::error::{Result, TruthError};
use sqlx::{Row, SqlitePool};

pub(super) async fn read(pool: &SqlitePool, backend: &str) -> Result<SharedCodexDaemonRecord> {
    let row = sqlx::query("SELECT * FROM codex_backend_daemons WHERE backend_id=?")
        .bind(backend)
        .fetch_one(pool)
        .await?;
    Ok(SharedCodexDaemonRecord {
        recovery: DaemonRecovery::decode(
            row.try_get("recovery_phase")?,
            row.try_get("recovery_id")?,
        )?,
        state: SharedDaemonState::try_from(row.try_get::<String, _>("state")?)
            .map_err(TruthError::Internal)?,
        pid: row.try_get("pid")?,
        pgid: row.try_get("pgid")?,
        paths_are_legacy_display: row.try_get("paths_are_legacy_display")?,
        sock_path: row.try_get("sock_path")?,
        codex_home_path: row.try_get("codex_home_path")?,
        process_start_time: row
            .try_get::<Option<i64>, _>("process_start_time")?
            .map(u64::try_from)
            .transpose()
            .map_err(|_| TruthError::Internal("negative daemon birth".into()))?,
        boot_id: row.try_get("boot_id")?,
        started_at: row.try_get("started_at")?,
        updated_at: row.try_get("updated_at")?,
        restart_count: row.try_get("restart_count")?,
        last_error: row.try_get("last_error")?,
        daemon_env_signature: row.try_get("daemon_env_signature")?,
    })
}

pub(super) async fn write(
    db: &SqliteDb,
    backend: &str,
    update: SharedCodexDaemonUpdate,
) -> Result<()> {
    let (mode, next, expected) = match update.recovery {
        RecoveryWrite::Legacy => (
            "legacy",
            DaemonRecovery::LegacyUntracked,
            DaemonRecovery::LegacyUntracked,
        ),
        RecoveryWrite::Begin { id } => (
            "begin",
            DaemonRecovery::Pending { id },
            DaemonRecovery::Clear,
        ),
        RecoveryWrite::ClaimLegacy { id } => (
            "claim",
            DaemonRecovery::Recorded { id },
            DaemonRecovery::LegacyUntracked,
        ),
        RecoveryWrite::Publish { id } => (
            "publish",
            DaemonRecovery::Recorded { id },
            DaemonRecovery::Pending { id },
        ),
        RecoveryWrite::Preserve { expected } => ("preserve", expected, expected),
        RecoveryWrite::CancelReserved { id } => (
            "cancel_reserved",
            DaemonRecovery::Clear,
            DaemonRecovery::Pending { id },
        ),
        RecoveryWrite::Settle { id } => (
            "settle",
            DaemonRecovery::Clear,
            DaemonRecovery::Recorded { id },
        ),
    };
    let (phase, id) = next.encode();
    let (expected_phase, expected_id) = expected.encode();
    let start = update
        .process_start_time
        .map(i64::try_from)
        .transpose()
        .map_err(|_| TruthError::Internal("daemon birth out of range".into()))?;
    let affected = db.writer().execute(sqlx::query(r#"
        INSERT INTO codex_backend_daemons
            (backend_id,state,pid,pgid,sock_path,codex_home_path,process_start_time,boot_id,
             started_at,updated_at,restart_count,last_error,daemon_env_signature,
             paths_are_legacy_display,recovery_phase,recovery_id)
        SELECT ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,CASE WHEN ?11 THEN 1 ELSE 0 END,?12,?13,?14,?15,?16
        WHERE ?17='legacy' OR EXISTS (SELECT 1 FROM codex_backend_daemons WHERE backend_id=?1)
        ON CONFLICT(backend_id) DO UPDATE SET
            state=excluded.state, pid=excluded.pid, pgid=excluded.pgid,
            sock_path=excluded.sock_path, codex_home_path=excluded.codex_home_path,
            process_start_time=excluded.process_start_time, boot_id=excluded.boot_id,
            started_at=excluded.started_at, updated_at=excluded.updated_at,
            restart_count=codex_backend_daemons.restart_count+CASE WHEN ?11 THEN 1 ELSE 0 END,
            last_error=excluded.last_error, daemon_env_signature=excluded.daemon_env_signature,
            paths_are_legacy_display=excluded.paths_are_legacy_display,
            recovery_phase=excluded.recovery_phase, recovery_id=excluded.recovery_id
        WHERE
            (?17='legacy' AND codex_backend_daemons.recovery_phase='legacy')
            OR (?17='begin' AND codex_backend_daemons.recovery_phase IN ('legacy','clear')
                AND codex_backend_daemons.pid IS NULL AND codex_backend_daemons.pgid IS NULL
                AND codex_backend_daemons.process_start_time IS NULL AND codex_backend_daemons.boot_id IS NULL)
            OR (?17='settle' AND codex_backend_daemons.recovery_phase='recorded'
                AND codex_backend_daemons.recovery_id=?19)
            OR (?17='cancel_reserved' AND codex_backend_daemons.recovery_phase='pending'
                AND codex_backend_daemons.recovery_id=?19)
            OR (?17='publish' AND codex_backend_daemons.recovery_phase='pending'
                AND codex_backend_daemons.recovery_id=?16)
            OR (
                ((?17='claim' AND codex_backend_daemons.recovery_phase='legacy')
                 OR (?17='publish' AND codex_backend_daemons.recovery_phase='recorded' AND codex_backend_daemons.recovery_id=?16)
                 OR (?17='preserve' AND codex_backend_daemons.recovery_phase=?18 AND codex_backend_daemons.recovery_id IS ?19))
                AND codex_backend_daemons.pid IS excluded.pid
                AND codex_backend_daemons.pgid IS excluded.pgid
                AND codex_backend_daemons.process_start_time IS excluded.process_start_time
                AND codex_backend_daemons.boot_id IS excluded.boot_id
                AND codex_backend_daemons.started_at IS excluded.started_at
                AND codex_backend_daemons.sock_path IS excluded.sock_path
                AND codex_backend_daemons.codex_home_path IS excluded.codex_home_path
                AND codex_backend_daemons.paths_are_legacy_display=excluded.paths_are_legacy_display
            )
        "#)
        .bind(backend).bind(update.state.as_db_str()).bind(update.pid).bind(update.pgid)
        .bind(update.sock_path).bind(update.codex_home_path).bind(start).bind(update.boot_id)
        .bind(update.started_at).bind(crate::model::now_ms()).bind(update.increment_restart_count)
        .bind(update.last_error).bind(update.daemon_env_signature).bind(update.paths_are_legacy_display)
        .bind(phase).bind(id).bind(mode).bind(expected_phase).bind(expected_id)).await?;
    if affected.rows_affected() != 1 {
        return Err(TruthError::Conflict("shared daemon recovery scope changed"));
    }
    Ok(())
}
