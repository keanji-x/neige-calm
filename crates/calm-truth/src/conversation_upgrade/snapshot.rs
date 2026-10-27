use super::*;
use sha2::{Digest, Sha256};
use sqlx::{Connection, SqliteConnection, sqlite::SqliteConnectOptions};
use std::{fs::File, io::Read, path::Path};

pub struct OfflineSnapshot {
    connection: SqliteConnection,
    input: FrozenBackup,
    digest: ([u8; 32], u64),
}
impl OfflineSnapshot {
    #[cfg(test)]
    pub(super) fn connection_for_test(&mut self) -> &mut SqliteConnection {
        &mut self.connection
    }
    pub async fn open(input: FrozenBackup) -> Result<Self, SnapshotError> {
        if input.path.to_str().is_none() {
            return Err(SnapshotError::UnsupportedInput("non-UTF-8 database path"));
        }
        if !input.path.is_absolute()
            || input.operator.trim().is_empty()
            || input.freeze_evidence.trim().is_empty()
        {
            return Err(SnapshotError::UnsupportedInput(
                "absolute frozen backup and external evidence required",
            ));
        }
        let digest = hash(&input.path)?;
        let options = SqliteConnectOptions::new()
            .filename(&input.path)
            .read_only(true)
            .create_if_missing(false)
            .immutable(true)
            .pragma("query_only", "ON")
            .pragma("trusted_schema", "OFF");
        let mut connection = SqliteConnection::connect_with(&options)
            .await
            .map_err(|e| partial("open backup", e))?;
        sqlx::query("BEGIN")
            .execute(&mut connection)
            .await
            .map_err(|e| partial("read transaction", e))?;
        Ok(Self {
            connection,
            input,
            digest,
        })
    }
    /// Consumes and closes the sole read connection before declaring coverage.
    pub async fn inventory(mut self) -> Result<CompleteBlockedAudit, SnapshotError> {
        let inventory = schema::inventory(&mut self.connection).await;
        self.connection
            .close()
            .await
            .map_err(|e| partial("close backup", e))?;
        let inventory = inventory?;
        if hash(&self.input.path)? != self.digest {
            return Err(partial("backup bytes", PartialReason::SourceChanged));
        }
        Ok(CompleteBlockedAudit {
            inventory,
            input: self.input,
            source_sha256: self.digest.0,
            source_len: self.digest.1,
        })
    }
}
fn hash(path: &Path) -> Result<([u8; 32], u64), SnapshotError> {
    let mut file = File::open(path).map_err(|e| partial("backup file", e))?;
    if !file
        .metadata()
        .map_err(|e| partial("backup metadata", e))?
        .is_file()
    {
        return Err(SnapshotError::UnsupportedInput(
            "regular backup file required",
        ));
    }
    let mut digest = Sha256::new();
    let mut length = 0;
    let mut buf = [0; 65536];
    loop {
        let n = file.read(&mut buf).map_err(|e| partial("backup hash", e))?;
        if n == 0 {
            break;
        }
        digest.update(&buf[..n]);
        length += n as u64;
    }
    Ok((digest.finalize().into(), length))
}
