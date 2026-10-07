//! Managed ACP prompt dispatch receipts. No receipt authorizes repeating a native prompt.
use crate::error::{CalmError, Result};
use sqlx::{FromRow, SqlitePool};

#[derive(Clone, Copy, Debug, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
pub enum AcpSubmissionState {
    Sending,
    Completed,
    Unknown,
}

#[derive(Clone, Debug, FromRow)]
pub struct AcpSubmission {
    pub worker_session_id: String,
    pub client_id: String,
    pub turn_id: String,
    pub thread_id: String,
    pub native_session_id: String,
    pub input_json: String,
    pub state: AcpSubmissionState,
    pub outcome_json: Option<String>,
}

/// Claim only a fresh managed session, or the exact registration that already owns it.
pub async fn acp_registration_claim(
    pool: &SqlitePool,
    worker: &str,
    digest: &str,
    native_bound: bool,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    let prior: Option<String> = sqlx::query_scalar(
        "SELECT registration_digest FROM acp_managed_sessions WHERE worker_session_id=?1",
    )
    .bind(worker)
    .fetch_optional(&mut *tx)
    .await?;
    match prior {
        Some(prior) if prior == digest => {}
        Some(_) => {
            return Err(CalmError::Conflict(
                "ACP registration changed; reset the conversation explicitly",
            ));
        }
        None if native_bound => {
            return Err(CalmError::Conflict(
                "ACP cannot adopt a native session it did not create",
            ));
        }
        None => {
            sqlx::query("INSERT INTO acp_managed_sessions(worker_session_id,registration_digest) VALUES(?1,?2)").bind(worker).bind(digest).execute(&mut *tx).await?;
        }
    }
    tx.commit().await?;
    Ok(())
}
pub async fn acp_submission_get(
    pool: &SqlitePool,
    worker: &str,
    client: &str,
) -> Result<Option<AcpSubmission>> {
    Ok(sqlx::query_as(concat!("SELECT ","worker_session_id,client_id,turn_id,thread_id,native_session_id,input_json,state,outcome_j","son FROM acp_submissions WHERE worker_session_id=?1 AND client_id=?2")).bind(worker).bind(client).fetch_optional(pool).await?)
}
pub async fn acp_submission_unresolved(pool: &SqlitePool, worker: &str) -> Result<bool> {
    Ok(sqlx::query_scalar::<_,i64>("SELECT EXISTS(SELECT 1 FROM acp_submissions WHERE worker_session_id=?1 AND state IN ('sending','unknown'))").bind(worker).fetch_one(pool).await? != 0)
}
pub async fn acp_submission_prepare(
    pool: &SqlitePool,
    receipt: &AcpSubmission,
    at_ms: i64,
) -> Result<()> {
    let inserted = sqlx::query(concat!("INSERT INTO ","acp_submissions(worker_session_id,client_id,turn_id,thread_id,native_session_id,input_json",",state,created_at_ms) SELECT ?1,?2,?3,?4,?5,?6,'sending',?7 WHERE EXISTS(SELECT 1 FROM ","worker_sessions WHERE id=?1 AND card_id IS NOT NULL AND provider='opencode' AND ","contract='planner' AND thread_id=?4 AND agent_session_id=?5 AND state IN ","('starting','running','idle','turn_pending'))"))
        .bind(&receipt.worker_session_id).bind(&receipt.client_id).bind(&receipt.turn_id).bind(&receipt.thread_id).bind(&receipt.native_session_id).bind(&receipt.input_json).bind(at_ms).execute(pool).await?;
    if inserted.rows_affected() != 1 {
        return Err(CalmError::Conflict(
            "ACP submission owner changed before dispatch",
        ));
    }
    Ok(())
}
pub async fn acp_submission_finish(
    pool: &SqlitePool,
    worker: &str,
    client: &str,
    outcome: Option<&str>,
) -> Result<()> {
    let state = if outcome.is_some() {
        "completed"
    } else {
        "unknown"
    };
    let result = sqlx::query(concat!(
        "UPDATE acp_submissions SET state=?3,outcome_json=?4 WHERE worker_session_id=?1 AND ",
        "client_id=?2 AND state IN ('sending','unknown')"
    ))
    .bind(worker)
    .bind(client)
    .bind(state)
    .bind(outcome)
    .execute(pool)
    .await?;
    if result.rows_affected() != 1 {
        return Err(CalmError::Conflict(
            "ACP submission fence changed before settlement",
        ));
    }
    Ok(())
}
