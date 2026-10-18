//! SQLite implementation of the execution Store port; projections arrive with consumers.
use async_trait::async_trait;
use calm_truth::db::sqlite::workspace_execution as sql;
use sqlx::{Sqlite, SqlitePool, Transaction};
use workspace_runtime::execution::*;

#[derive(Clone)]
pub struct SqliteExecutionStore {
    pool: SqlitePool,
}
impl SqliteExecutionStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}
fn error(error: impl std::fmt::Display) -> StoreError {
    StoreError(error.to_string())
}

#[async_trait]
impl Store for SqliteExecutionStore {
    type Tx<'t> = Transaction<'t, Sqlite>;
    async fn insert(
        &self,
        tx: &mut Self::Tx<'_>,
        intent: &ExecutionIntent,
    ) -> Result<(), StoreError> {
        sql::insert(
            tx,
            intent.metadata.id.as_str(),
            &intent.metadata.track.0,
            &intent.metadata.alias.0,
            &serde_json::to_string(&intent.metadata).map_err(error)?,
            &serde_json::to_string(&intent.secrets).map_err(error)?,
        )
        .await
        .map_err(error)
    }
    async fn request(
        &self,
        tx: &mut Self::Tx<'_>,
        id: &ExecutionId,
        desired: Desired,
    ) -> Result<(), StoreError> {
        let desired = match desired {
            Desired::Run => "run",
            Desired::Stop => "stop",
            Desired::Retire => "retire",
        };
        sql::request(tx, id.as_str(), desired).await.map_err(error)
    }
    async fn load(&self, id: &ExecutionId) -> Result<Option<Stored>, StoreError> {
        let Some(row) = sql::load(&self.pool, id.as_str()).await.map_err(error)? else {
            return Ok(None);
        };
        let intent = serde_json::from_str(&row.intent_json).map_err(error)?;
        let settlement = row
            .settlement_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(error)?;
        Ok(Some(match row.secrets_json {
            Some(secrets) => Stored::Active {
                intent,
                secrets: serde_json::from_str(&secrets).map_err(error)?,
                desired: match row.desired.as_str() {
                    "run" => Desired::Run,
                    "stop" => Desired::Stop,
                    "retire" => Desired::Retire,
                    _ => return Err(StoreError("invalid desired".into())),
                },
                settlement,
            },
            None => Stored::Finished {
                intent,
                settlement: settlement
                    .ok_or_else(|| StoreError("finished execution lacks settlement".into()))?,
            },
        }))
    }
    async fn spawned(&self, _: &ExecutionId) -> Result<(), StoreError> {
        // E2a has no consumer projections.
        Ok(())
    }
    async fn settle(&self, id: &ExecutionId, settlement: &Settlement) -> Result<(), StoreError> {
        sql::settle(
            &self.pool,
            id.as_str(),
            &serde_json::to_string(settlement).map_err(error)?,
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .map_err(error)
    }
    async fn finish(&self, id: &ExecutionId) -> Result<(), StoreError> {
        sql::finish(&self.pool, id.as_str()).await.map_err(error)
    }
    async fn pending(&self) -> Result<Vec<ExecutionId>, StoreError> {
        sql::pending(&self.pool)
            .await
            .map_err(error)?
            .into_iter()
            .map(|id| ExecutionId::try_from(id).map_err(error))
            .collect()
    }
}

#[cfg(test)]
mod tests;
