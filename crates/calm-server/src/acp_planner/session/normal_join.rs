//! Genuine normal joins are durable independently of a Human Keep decision.
use super::*;
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct NormalJoin {
    version: u8,
    worker: String,
    client: String,
    thread: String,
    turn: String,
    native: String,
    input: String,
    registration: String,
}
impl NormalJoin {
    fn for_receipt(receipt: &AcpSubmission, registration: String) -> Self {
        Self {
            version: 1,
            worker: receipt.worker_session_id.clone(),
            client: receipt.client_id.clone(),
            thread: receipt.thread_id.clone(),
            turn: receipt.turn_id.clone(),
            native: receipt.native_session_id.clone(),
            input: receipt.input_json.clone(),
            registration,
        }
    }
}
async fn evidence_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    params: &SessionParams,
    expected: &AcpSubmission,
) -> Result<Option<NormalJoin>> {
    let current: Option<String> = sqlx::query_scalar("SELECT session_id FROM cards WHERE id=?1")
        .bind(&params.card_id)
        .fetch_optional(&mut **tx)
        .await?
        .flatten();
    if current.as_deref() != Some(params.worker_session_id.as_str()) {
        return Ok(None);
    };
    let Some((receipt, _)) = super::recovery::canonical_receipt_tx(
        tx,
        &params.card_id,
        &params.worker_session_id,
        &params.provider,
    )
    .await?
    else {
        return Ok(None);
    };
    if !matches!(
        receipt.state,
        crate::db::sqlite::AcpSubmissionState::Unknown
    ) || receipt.outcome_json.is_some()
        || receipt.worker_session_id != expected.worker_session_id
        || receipt.client_id != expected.client_id
        || receipt.thread_id != expected.thread_id
        || receipt.turn_id != expected.turn_id
        || receipt.native_session_id != expected.native_session_id
        || receipt.input_json != expected.input_json
    {
        return Ok(None);
    };
    let registration: String = sqlx::query_scalar(
        "SELECT registration_digest FROM acp_managed_sessions WHERE worker_session_id=?1",
    )
    .bind(&receipt.worker_session_id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(Some(NormalJoin::for_receipt(&receipt, registration)))
}
/// Only callers that have joined the exact original task with CleanUnknown may record this.
pub(super) async fn record(params: &SessionParams, receipt: &AcpSubmission) -> Result<()> {
    // A retained normal join is immutable evidence, not permission to execute.
    // Already-recorded evidence needs no writer; every new witness still uses
    // the protected current-association validation below.
    let db = params
        .repo
        .sqlite_db()
        .ok_or(CalmError::ExecutionUnavailable)?;
    let stored: Option<(String, String)> = sqlx::query_as(concat!(
        "SELECT j.evidence_json,m.registration_digest FROM acp_normal_joins j JOIN ",
        "acp_managed_sessions m ON m.worker_session_id=j.worker_session_id ",
        "WHERE j.worker_session_id=?1 AND j.client_id=?2",
    ))
    .bind(&receipt.worker_session_id)
    .bind(&receipt.client_id)
    .fetch_optional(db.read_pool())
    .await?;
    if let Some((stored, registration)) = stored {
        let expected = NormalJoin::for_receipt(receipt, registration);
        if serde_json::from_str::<NormalJoin>(&stored).ok().as_ref() == Some(&expected) {
            return Ok(());
        }
    }
    let mut tx = crate::db::sqlite::begin_immediate_tx(
        &params
            .repo
            .sqlite_db()
            .ok_or(CalmError::ExecutionUnavailable)?,
    )
    .await?;
    let Some(evidence) = evidence_tx(&mut tx, params, receipt).await? else {
        return Ok(());
    };
    let json = serde_json::to_string(&evidence)?;
    sqlx::query(concat!("INSERT INTO acp_normal_joins(worker_session_id,client_id,evidence_json) VALUES(?1,?2,?3) ON ","CONFLICT(worker_session_id,client_id) DO NOTHING"))
        .bind(&evidence.worker).bind(&evidence.client).bind(&json).execute(&mut **tx).await?;
    let stored: String = sqlx::query_scalar(
        "SELECT evidence_json FROM acp_normal_joins WHERE worker_session_id=?1 AND client_id=?2",
    )
    .bind(&evidence.worker)
    .bind(&evidence.client)
    .fetch_one(&mut **tx)
    .await?;
    if serde_json::from_str::<NormalJoin>(&stored)? != evidence {
        return Err(CalmError::Conflict(
            "ACP normal join witness changed".into(),
        ));
    };
    tx.commit().await?;
    Ok(())
}
impl AcpPlannerSession {
    /// A new explicit Human Keep can consume a genuine witness after restart.
    /// Ordinary boot recovery remains Held; process sweeps cannot enter this path.
    pub(crate) async fn restore_normal_join_after_keep(&self) -> Result<()> {
        let shared = &self.shared;
        let _issue = shared.issue.lock().await;
        if shared.driver.lock().expect("ACP driver").is_some() {
            return Ok(());
        };
        let params = &shared.params;
        let db = params
            .repo
            .sqlite_db()
            .ok_or(CalmError::ExecutionUnavailable)?;
        let mut tx = crate::db::sqlite::begin_immediate_tx(&db).await?;
        let Some((receipt, association)) = super::recovery::canonical_receipt_tx(
            &mut tx,
            &params.card_id,
            &params.worker_session_id,
            &params.provider,
        )
        .await?
        else {
            return Ok(());
        };
        let Some(expected) = evidence_tx(&mut tx, params, &receipt).await? else {
            return Ok(());
        };
        let stored:Option<String>=sqlx::query_scalar("SELECT evidence_json FROM acp_normal_joins WHERE worker_session_id=?1 AND client_id=?2")
            .bind(&receipt.worker_session_id).bind(&receipt.client_id).fetch_optional(&mut **tx).await?;
        let Some(stored) = stored else { return Ok(()) };
        if serde_json::from_str::<NormalJoin>(&stored)? != expected {
            return Err(CalmError::Conflict(
                "ACP normal join witness no longer matches".into(),
            ));
        };
        tx.rollback().await?;
        revoke(params).await?;
        super::super::lifecycle::stop_session(
            &params.host,
            &params.worker_session_id,
            crate::provider_execution::StopReason::Shutdown,
        )
        .await?;
        // Recheck the exact witness and current association after fallible cleanup.
        let mut tx = crate::db::sqlite::begin_immediate_tx(&db).await?;
        if evidence_tx(&mut tx, params, &receipt).await?.as_ref() != Some(&expected) {
            return Err(CalmError::Conflict(
                "ACP normal join association changed".into(),
            ));
        };
        tx.rollback().await?;
        execution_end::install(shared, DriverSlot::from_normal_join(receipt, association));
        Ok(())
    }
}
