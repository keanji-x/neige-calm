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
    #[sqlx(rename = "kept_cleanup_json")]
    pub kept_cleanup: Option<sqlx::types::Json<super::AcpKeptCleanup>>,
}

#[derive(Debug, thiserror::Error)]
pub enum AcpRegistrationError {
    #[error("ACP registration changed; reset the conversation explicitly")]
    Changed,
    #[error("ACP cannot adopt a native session it did not create")]
    UnownedNative,
    #[error(transparent)]
    Storage(#[from] crate::error::TruthError),
}
impl From<AcpRegistrationError> for crate::error::TruthError {
    fn from(error: AcpRegistrationError) -> Self {
        match error {
            AcpRegistrationError::Storage(error) => error,
            mismatch @ (AcpRegistrationError::Changed | AcpRegistrationError::UnownedNative) => {
                Self::Conflict(mismatch.to_string())
            }
        }
    }
}
impl From<sqlx::Error> for AcpRegistrationError {
    fn from(error: sqlx::Error) -> Self {
        Self::Storage(error.into())
    }
}
/// Claim only a fresh managed session, or the exact registration that already owns it.
pub async fn acp_registration_claim(
    pool: &crate::db::sqlite::SqliteDb,
    worker: &str,
    digest: &str,
    native_bound: bool,
) -> std::result::Result<(), AcpRegistrationError> {
    let mut tx = super::begin_immediate_tx(pool).await?;
    use crate::decision_gate::{PrincipalDecisionGate, WriteTx};
    let id = crate::worker::WorkerSessionId::from(worker);
    let session = tx
        .read_worker_session(&id)
        .await?
        .ok_or_else(|| CalmError::Conflict("ACP registration owner changed"))?;
    let principal = crate::worker::Principal::Agent {
        session_id: id,
        track_id: session.track_id.clone(),
        area_id: tx
            .read_track_area(&session.track_id)
            .await?
            .ok_or_else(|| CalmError::Conflict("ACP registration track changed"))?,
    };
    if !PrincipalDecisionGate::new(principal)
        .recorder_grant(&mut *tx, &session.track_id)
        .await?
    {
        return Err(CalmError::Conflict("ACP registration owner changed").into());
    }
    let prior: Option<String> = sqlx::query_scalar(
        "SELECT registration_digest FROM acp_managed_sessions WHERE worker_session_id=?1",
    )
    .bind(worker)
    .fetch_optional(&mut **tx)
    .await?;
    match prior {
        Some(prior) if prior == digest => {}
        Some(_) => {
            return Err(AcpRegistrationError::Changed);
        }
        None if native_bound => {
            return Err(AcpRegistrationError::UnownedNative);
        }
        None => {
            sqlx::query("INSERT INTO acp_managed_sessions(worker_session_id,registration_digest) VALUES(?1,?2)").bind(worker).bind(digest).execute(&mut **tx).await?;
        }
    }
    tx.commit().await?;
    Ok(())
}
pub async fn acp_submission_get<'e, E>(
    executor: E,
    worker: &str,
    client: &str,
) -> Result<Option<AcpSubmission>>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    Ok(sqlx::query_as(concat!("SELECT ","worker_session_id,client_id,turn_id,thread_id,native_session_id,input_json,state,outcome_j","son,kept_cleanup_json FROM acp_submissions WHERE worker_session_id=?1 AND client_id=?2")).bind(worker).bind(client).fetch_optional(executor).await?)
}
pub async fn acp_submission_unresolved(pool: &SqlitePool, worker: &str) -> Result<bool> {
    let mut connection = pool.acquire().await?;
    acp_submission_unresolved_on(&mut connection, worker).await
}
pub async fn acp_submission_unresolved_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    worker: &str,
) -> Result<bool> {
    acp_submission_unresolved_on(tx, worker).await
}
async fn acp_submission_unresolved_on(
    connection: &mut sqlx::SqliteConnection,
    worker: &str,
) -> Result<bool> {
    let sql = format!(
        "SELECT EXISTS(SELECT 1 FROM acp_submissions WHERE worker_session_id=?1 AND ({ACP_BLOCKING_RECEIPT_SQL}))"
    );
    Ok(sqlx::query_scalar::<_, i64>(&sql)
        .bind(worker)
        .fetch_one(connection)
        .await?
        != 0)
}
/// The journal's authoritative blocking predicate, also used by recovery.
/// The unique index is its durable SQL counterpart.
pub const ACP_BLOCKING_RECEIPT_SQL: &str =
    "state = 'sending' OR (state = 'unknown' AND kept_cleanup_json IS NULL)";
pub async fn acp_submission_other_blocking_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    worker: &str,
    client: &str,
) -> Result<bool> {
    let sql = format!(
        concat!(
            "SELECT EXISTS(SELECT 1 FROM acp_submissions WHERE worker_session_id=?1 AND client_id!=?2 AND ",
            "({blocking}))",
        ),
        blocking = ACP_BLOCKING_RECEIPT_SQL
    );
    Ok(sqlx::query_scalar::<_, bool>(&sql)
        .bind(worker)
        .bind(client)
        .fetch_one(&mut **tx)
        .await?)
}
/// The persisted strings of every provider speaking ACP: only their sessions own submissions.
fn acp_provider_db_strings() -> String {
    super::session_row::agent_provider_db_strings(|provider| {
        provider.protocol() == Some(calm_types::runtime::AgentProtocol::Acp)
    })
}
#[derive(Debug, thiserror::Error)]
pub enum AcpPrepareError {
    #[error("ACP receipt refused before insertion: {0}")]
    BeforeReceipt(crate::error::TruthError),
    #[error("ACP receipt commit is uncertain: {0}")]
    CommitUncertain(crate::error::TruthError),
}
pub async fn acp_submission_prepare(
    pool: &crate::db::sqlite::SqliteDb,
    receipt: &AcpSubmission,
    at_ms: i64,
) -> std::result::Result<(), AcpPrepareError> {
    let mut tx = super::begin_immediate_tx(pool)
        .await
        .map_err(AcpPrepareError::BeforeReceipt)?;
    prepare_tx(&mut tx, receipt, at_ms)
        .await
        .map_err(AcpPrepareError::BeforeReceipt)?;
    tx.commit()
        .await
        .map_err(|error| AcpPrepareError::CommitUncertain(error.into()))
}
async fn prepare_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    receipt: &AcpSubmission,
    at_ms: i64,
) -> Result<()> {
    let input = super::AcpSubmissionInput::decode(&receipt.input_json)?;
    match &input {
        super::AcpSubmissionInput::Canonical(association) => {
            super::acp_submission_input::validate_tx(tx, receipt, association).await?;
        }
        super::AcpSubmissionInput::Historical(_) => {
            let card: Option<String> =
                sqlx::query_scalar("SELECT card_id FROM worker_sessions WHERE id=?1")
                    .bind(&receipt.worker_session_id)
                    .fetch_one(&mut **tx)
                    .await?;
            let card = card.ok_or(CalmError::Conflict("ACP historical receipt has no card"))?;
            if super::conversation::card_has_canonical_storage(tx, &card).await? {
                return Err(CalmError::Conflict(
                    "ACP historical receipt cannot execute canonical storage",
                ));
            }
        }
    }
    if acp_submission_unresolved_tx(tx, &receipt.worker_session_id).await? {
        return Err(CalmError::Conflict("ACP receipt remains blocking"));
    }
    let inserted = sqlx::query(concat!("INSERT INTO ","acp_submissions(worker_session_id,client_id,turn_id,thread_id,native_session_id,input_json",",state,created_at_ms) SELECT ?1,?2,?3,?4,?5,?6,'sending',?7 WHERE EXISTS(SELECT 1 FROM ","worker_sessions ws JOIN cards c ON c.session_id=ws.id AND c.id=ws.card_id WHERE ws.id=?1 ","AND ws.provider IN (SELECT value FROM json_each(?8)) AND ws.mode='resumable' AND c.track_id=ws.track_id ","AND ((ws.contract='planner' AND c.role='planner' AND json_extract(c.payload,'$.planner_provider')=ws.provider) ","OR (ws.contract='assistant' AND c.role='assistant' AND c.kind='codex' ","AND json_extract(c.payload,'$.harness_profile')='assistant' ","AND json_extract(c.payload,'$.assistant_provider')=ws.provider)) ","AND ws.thread_id=?4 AND ws.agent_session_id=?5 AND ws.state IN ","('starting','running','idle','turn_pending'))"))
        .bind(&receipt.worker_session_id).bind(&receipt.client_id).bind(&receipt.turn_id).bind(&receipt.thread_id).bind(&receipt.native_session_id).bind(&receipt.input_json).bind(at_ms).bind(acp_provider_db_strings()).execute(&mut **tx).await?;
    if inserted.rows_affected() != 1 {
        return Err(CalmError::Conflict(
            "ACP submission owner changed before dispatch",
        ));
    }
    Ok(())
}
pub async fn acp_submission_finish(
    pool: &crate::db::sqlite::SqliteDb,
    worker: &str,
    client: &str,
    outcome: Option<&str>,
) -> Result<()> {
    let mut tx = super::begin_immediate_tx(pool).await?;
    let state = if outcome.is_some() {
        "completed"
    } else {
        "unknown"
    };
    let result = sqlx::query(concat!(
        "UPDATE acp_submissions SET state=?3,outcome_json=?4 WHERE worker_session_id=?1 AND ",
        "client_id=?2 AND (state IN ('sending','unknown') OR ",
        "(state='completed' AND ?3='completed' AND outcome_json=?4 COLLATE BINARY))"
    ))
    .bind(worker)
    .bind(client)
    .bind(state)
    .bind(outcome)
    .execute(&mut **tx)
    .await?;
    if result.rows_affected() != 1 {
        return Err(CalmError::Conflict(
            "ACP submission fence changed before settlement",
        ));
    }
    tx.commit().await?;
    Ok(())
}
