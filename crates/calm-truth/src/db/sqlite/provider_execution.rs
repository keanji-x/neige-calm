//! Durable provider-owner claims. Callers compose these CAS writes with Engine writes.
//! Settlement and owner-completion evidence belong to the owner, not this SQL layer.
use crate::error::{CalmError, Result};
use serde::{Deserialize, Serialize};
use sqlx::{Executor, Sqlite, Transaction};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub enum Version {
    V1,
}
impl TryFrom<u64> for Version {
    type Error = &'static str;
    fn try_from(value: u64) -> std::result::Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::V1),
            _ => Err("unsupported provider execution version"),
        }
    }
}
impl From<Version> for u64 {
    fn from(_: Version) -> Self {
        1
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Purpose {
    Turn { turn_id: String },
    Compaction { turn_id: String },
    RewindCheck,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Release {
    NotRequired,
    Held,
    Releasing { authorized_at_ms: i64 },
    Released { authorized_at_ms: i64 },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    Finished,
    Interrupted,
    Shutdown,
    TrackRetired,
    KernelRestarted,
    DriverLost,
    ReleaseRefused,
    SetupFailed,
    OutputGap,
    StdinTorn,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    Observing,
    Stopping { reason: StopReason, at_ms: i64 },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub version: Version,
    pub execution_id: String,
    pub purpose: Purpose,
    pub release: Release,
    pub decision: Decision,
}

/// Keep the exact bytes read/written alongside the record for whole-record CAS.
/// A stale claim must be re-read; it never authorizes adopting another execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    record: Record,
    json: String,
}
impl Claim {
    pub fn record(&self) -> &Record {
        &self.record
    }
    fn decode(json: String) -> Result<Self> {
        Ok(Self {
            record: serde_json::from_str(&json)?,
            json,
        })
    }
    fn encode(record: Record) -> Result<Self> {
        Ok(Self {
            json: serde_json::to_string(&record)?,
            record,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Cas<T> {
    Applied(T),
    LostOwnership,
}

/// Missing session is RowNotFound; only a present session with NULL permits admission.
pub async fn load<'a>(
    executor: impl Executor<'a, Database = Sqlite>,
    worker_session_id: &str,
) -> Result<Option<Claim>> {
    let json: Option<String> =
        sqlx::query_scalar("SELECT provider_execution_json FROM worker_sessions WHERE id = ?")
            .bind(worker_session_id)
            .fetch_one(executor)
            .await?;
    json.map(Claim::decode).transpose()
}

/// Durable sweep inventory, including both Observing and Stopping claims.
pub async fn list<'a>(
    executor: impl Executor<'a, Database = Sqlite>,
) -> Result<Vec<(String, Claim)>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, provider_execution_json FROM worker_sessions
         WHERE provider_execution_json IS NOT NULL ORDER BY id",
    )
    .fetch_all(executor)
    .await?;
    rows.into_iter()
        .map(|(id, json)| Ok((id, Claim::decode(json)?)))
        .collect()
}

/// Admission cannot start already releasing, released, or stopping.
pub enum InitialRelease {
    NotRequired,
    Held,
}

/// ① Must share the caller's Engine::admit transaction.
pub async fn admit(
    tx: &mut Transaction<'_, Sqlite>,
    session: &str,
    execution_id: String,
    purpose: Purpose,
    release: InitialRelease,
) -> Result<Cas<Claim>> {
    let claim = Claim::encode(Record {
        version: Version::V1,
        execution_id,
        purpose,
        release: match release {
            InitialRelease::NotRequired => Release::NotRequired,
            InitialRelease::Held => Release::Held,
        },
        decision: Decision::Observing,
    })?;
    let changed = sqlx::query(
        "UPDATE worker_sessions SET provider_execution_json = ?
         WHERE id = ? AND provider_execution_json IS NULL",
    )
    .bind(&claim.json)
    .bind(session)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    Ok(if changed == 1 {
        Cas::Applied(claim)
    } else {
        Cas::LostOwnership
    })
}

/// ② Authorize once, then finish using the persisted authorization timestamp.
/// The owner alone calls this; sweep never advances release.
pub async fn advance_release(
    tx: &mut Transaction<'_, Sqlite>,
    session: &str,
    expected: &Claim,
    authorized_at_ms: i64,
) -> Result<Cas<Claim>> {
    let mut record = expected.record.clone();
    record.release = match record.release {
        Release::Held => Release::Releasing { authorized_at_ms },
        Release::Releasing {
            authorized_at_ms: at,
        } if at == authorized_at_ms => Release::Released {
            authorized_at_ms: at,
        },
        _ => return Err(CalmError::BadRequest("invalid provider release transition")),
    };
    replace(tx, session, expected, record).await
}

/// ③ Compose with Engine::request(Stop); stopping is irreversible.
pub async fn stop(
    tx: &mut Transaction<'_, Sqlite>,
    session: &str,
    expected: &Claim,
    reason: StopReason,
    at_ms: i64,
) -> Result<Cas<Claim>> {
    if expected.record.decision != Decision::Observing {
        return Err(CalmError::BadRequest(
            "provider execution is already stopping",
        ));
    }
    let mut record = expected.record.clone();
    record.decision = Decision::Stopping { reason, at_ms };
    replace(tx, session, expected, record).await
}

async fn replace(
    tx: &mut Transaction<'_, Sqlite>,
    session: &str,
    expected: &Claim,
    record: Record,
) -> Result<Cas<Claim>> {
    let next = Claim::encode(record)?;
    match compare_exchange(tx, session, expected, Some(&next.json)).await? {
        Cas::Applied(()) => Ok(Cas::Applied(next)),
        Cas::LostOwnership => Ok(Cas::LostOwnership),
    }
}

/// ④ Caller must first confirm settlement and owner completion, then clear and
/// request Retire in this transaction. NULL alone opens the next admission.
pub async fn clear_on_completion(
    tx: &mut Transaction<'_, Sqlite>,
    session: &str,
    expected: &Claim,
) -> Result<Cas<()>> {
    compare_exchange(tx, session, expected, None).await
}

async fn compare_exchange(
    tx: &mut Transaction<'_, Sqlite>,
    session: &str,
    expected: &Claim,
    next: Option<&str>,
) -> Result<Cas<()>> {
    let changed = sqlx::query(
        "UPDATE worker_sessions SET provider_execution_json = ?
         WHERE id = ? AND provider_execution_json = ?",
    )
    .bind(next)
    .bind(session)
    .bind(&expected.json)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    Ok(if changed == 1 {
        Cas::Applied(())
    } else {
        Cas::LostOwnership
    })
}
