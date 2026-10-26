//! The only admitted durable state in A is Required. There is deliberately no
//! completion writer or successful-apply representation to fake an upgrade.
use super::super::{SqliteDb, begin_immediate_tx};
use crate::error::{CalmError, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::SqliteConnection;

const PROTOCOL_VERSION: i64 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct RequiredUpgrade {
    pub deployment_id: String,
    pub schema_sha256: String,
    pub protocol_version: i64,
    pub state: String,
    pub prepared_at_ms: i64,
}

#[derive(sqlx::FromRow)]
struct StoredMarker {
    singleton: i64,
    #[sqlx(flatten)]
    marker: RequiredUpgrade,
}

pub(super) fn refusal(reason: impl std::fmt::Display) -> CalmError {
    CalmError::Internal(format!("conversation upgrade: {reason}; refusing boot"))
}

/// Pin the full ordered schema contract, not merely the highest version. SQLx
/// separately verifies the installed checksums before this value is consulted.
pub(super) fn schema_sha256() -> String {
    let mut digest = Sha256::new();
    for migration in crate::MIGRATOR.iter() {
        digest.update(migration.version.to_be_bytes());
        digest.update(migration.checksum.as_ref());
    }
    format!("{:x}", digest.finalize())
}

pub(super) async fn read(connection: &mut SqliteConnection) -> Result<Option<RequiredUpgrade>> {
    let rows: Vec<StoredMarker> = sqlx::query_as("SELECT singleton,deployment_id,schema_sha256,protocol_version,state,prepared_at_ms FROM conversation_upgrade")
        .fetch_all(connection).await?;
    match rows.as_slice() {
        [] => Ok(None),
        [
            StoredMarker {
                singleton: 1,
                marker,
            },
        ] => Ok(Some(marker.clone())),
        _ => Err(refusal("malformed marker cardinality")),
    }
}

impl RequiredUpgrade {
    pub(super) fn validate(&self, deployment: &str) -> Result<()> {
        let canonical_id =
            uuid::Uuid::parse_str(deployment).is_ok_and(|id| id.to_string() == deployment);
        if !canonical_id
            || self.deployment_id != deployment
            || self.schema_sha256 != schema_sha256()
            || self.protocol_version != PROTOCOL_VERSION
            || self.state != "required"
            || self.prepared_at_ms < 0
        {
            return Err(refusal("malformed or mismatched required marker"));
        }
        Ok(())
    }
}

pub(super) async fn prepare(db: &SqliteDb, deployment: &str) -> Result<RequiredUpgrade> {
    let mut tx = begin_immediate_tx(db).await?;
    if let Some(existing) = read(&mut tx).await? {
        existing.validate(deployment)?;
        tx.commit().await?;
        return Ok(existing);
    }
    let marker = RequiredUpgrade {
        deployment_id: deployment.into(),
        schema_sha256: schema_sha256(),
        protocol_version: PROTOCOL_VERSION,
        state: "required".into(),
        prepared_at_ms: crate::model::now_ms(),
    };
    marker.validate(deployment)?;
    sqlx::query("INSERT INTO conversation_upgrade(singleton,deployment_id,schema_sha256,protocol_version,state,prepared_at_ms) VALUES (1,?1,?2,?3,?4,?5)")
        .bind(&marker.deployment_id).bind(&marker.schema_sha256)
        .bind(marker.protocol_version).bind(&marker.state).bind(marker.prepared_at_ms)
        .execute(&mut **tx).await?;
    tx.commit().await?;
    Ok(marker)
}
