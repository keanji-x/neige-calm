//! SQLite execution store and registered consumer projections, committed atomically.
use crate::db::sqlite::SqliteDb;
use async_trait::async_trait;
use calm_truth::db::sqlite::workspace_execution as sql;
use sqlx::{Sqlite, Transaction};
use std::{collections::HashMap, sync::Arc};
use workspace_runtime::execution::*;

/// Consumer projections execute inside the store's transaction; SQL belongs to calm-truth.
/// Every consumer requires registration, including an explicit no-op when appropriate.
#[async_trait]
pub trait ExecutionProjection: Send + Sync {
    async fn spawned(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        alias: &str,
        now: i64,
    ) -> Result<(), sqlx::Error>;
    async fn settled(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        alias: &str,
        settlement: &Settlement,
        now: i64,
    ) -> Result<(), sqlx::Error>;
}

#[derive(Clone)]
pub struct SqliteExecutionStore {
    pool: SqliteDb,
    #[cfg(feature = "fixtures")]
    faults: Arc<crate::test_seams::ExecutionRecoveryFaults>,
    projections: HashMap<ExecutionConsumer, Arc<dyn ExecutionProjection>>,
}
impl SqliteExecutionStore {
    pub fn new(pool: SqliteDb) -> Self {
        Self {
            pool,
            #[cfg(feature = "fixtures")]
            faults: Default::default(),
            projections: HashMap::new(),
        }
    }
    #[cfg(feature = "fixtures")]
    pub fn with_faults(mut self, faults: Arc<crate::test_seams::ExecutionRecoveryFaults>) -> Self {
        self.faults = faults;
        self
    }
    pub fn with_projection(
        mut self,
        consumer: ExecutionConsumer,
        projection: Arc<dyn ExecutionProjection>,
    ) -> Self {
        assert!(
            self.projections.insert(consumer, projection).is_none(),
            "duplicate execution projection"
        );
        self
    }
    fn projection(
        &self,
        consumer: &ExecutionConsumer,
    ) -> Result<&dyn ExecutionProjection, StoreError> {
        self.projections
            .get(consumer)
            .map(AsRef::as_ref)
            .ok_or_else(|| {
                StoreError(format!(
                    "missing execution projection for consumer: {}",
                    consumer.0
                ))
            })
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
        self.projection(&intent.metadata.consumer)?;
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
        let Some(row) = sql::load(self.pool.read_pool(), id.as_str())
            .await
            .map_err(error)?
        else {
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
    async fn spawned(&self, id: &ExecutionId) -> Result<(), StoreError> {
        #[cfg(feature = "fixtures")]
        self.faults.check("spawned")?;
        let mut tx = calm_truth::db::sqlite::begin_immediate_tx(&self.pool)
            .await
            .map_err(error)?;
        let metadata: ExecutionMetadata =
            serde_json::from_str(&sql::intent_tx(&mut tx, id.as_str()).await.map_err(error)?)
                .map_err(error)?;
        self.projection(&metadata.consumer)?
            .spawned(
                &mut tx,
                &metadata.alias.0,
                chrono::Utc::now().timestamp_millis(),
            )
            .await
            .map_err(error)?;
        tx.commit().await.map_err(error)
    }
    async fn settle(&self, id: &ExecutionId, settlement: &Settlement) -> Result<(), StoreError> {
        #[cfg(feature = "fixtures")]
        self.faults.check("settle")?;
        let mut tx = calm_truth::db::sqlite::begin_immediate_tx(&self.pool)
            .await
            .map_err(error)?;
        let now = chrono::Utc::now().timestamp_millis();
        sql::settle_tx(
            &mut tx,
            id.as_str(),
            &serde_json::to_string(settlement).map_err(error)?,
            now,
        )
        .await
        .map_err(error)?;
        let metadata: ExecutionMetadata =
            serde_json::from_str(&sql::intent_tx(&mut tx, id.as_str()).await.map_err(error)?)
                .map_err(error)?;
        self.projection(&metadata.consumer)?
            .settled(&mut tx, &metadata.alias.0, settlement, now)
            .await
            .map_err(error)?;
        tx.commit().await.map_err(error)
    }
    async fn finish(&self, id: &ExecutionId) -> Result<(), StoreError> {
        #[cfg(feature = "fixtures")]
        self.faults.check("finish")?;
        sql::finish(&self.pool, id.as_str()).await.map_err(error)
    }
    async fn pending(&self) -> Result<Vec<ExecutionId>, StoreError> {
        #[cfg(feature = "fixtures")]
        self.faults.check("pending")?;
        sql::pending(self.pool.read_pool())
            .await
            .map_err(error)?
            .into_iter()
            .map(|id| ExecutionId::try_from(id).map_err(error))
            .collect()
    }
}

#[cfg(test)]
pub(crate) mod tests;
