//! Kernel transaction and event adapters for the Calendar domain service.
use super::PLUGIN_ID;
use crate::{
    db::{RouteRepo, write_in_tx_typed, write_with_events_typed},
    error::{CalmError, Result},
    event::EventBus,
    mcp_server::registry::AppContext,
    model::Track,
    state::WriteContext,
};
use async_trait::async_trait;
use plugin::builtin::calendar::ports::{CommitMode, Mutation, Storage, Transaction};
use serde_json::Value;
use sqlx::{Sqlite, Transaction as SqlTransaction};
use std::sync::Arc;
pub struct Adapter {
    repo: Arc<dyn RouteRepo>,
    events: EventBus,
    write: WriteContext,
    host: Arc<tokio::sync::OnceCell<Arc<crate::plugin_host::PluginHost>>>,
}
impl Adapter {
    pub fn new(ctx: &AppContext) -> Self {
        Self {
            repo: ctx.repo.clone(),
            events: ctx.events.clone(),
            write: ctx.write.clone(),
            host: ctx.plugin_host.clone(),
        }
    }
}
struct Tx<'a, 't> {
    tx: &'a mut SqlTransaction<'t, Sqlite>,
}
#[async_trait]
impl Transaction<CalmError> for Tx<'_, '_> {
    fn new_id(&self) -> String {
        crate::model::new_id()
    }
    fn now_ms(&self) -> i64 {
        crate::model::now_ms()
    }
    async fn read(&mut self, key: &str) -> Result<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT value FROM plugin_kv WHERE plugin_id=? AND key=?")
                .bind(PLUGIN_ID)
                .bind(key)
                .fetch_optional(&mut **self.tx)
                .await?,
        )
    }
    async fn put(&mut self, key: &str, value: &str) -> Result<()> {
        sqlx::query("INSERT INTO plugin_kv(plugin_id,key,value,updated_at) VALUES(?,?,?,?) \
        ON CONFLICT(plugin_id,key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at")
            .bind(PLUGIN_ID).bind(key).bind(value).bind(crate::model::now_ms()).execute(&mut **self.tx).await?;
        Ok(())
    }
    async fn track_is_open(&mut self, track: &str) -> Result<bool> {
        Ok(
            sqlx::query_scalar::<_, i64>("SELECT 1 FROM tracks WHERE id = ? AND closed_at IS NULL")
                .bind(track)
                .fetch_optional(&mut **self.tx)
                .await?
                .is_some(),
        )
    }
}
#[async_trait]
impl Storage for Adapter {
    type Error = CalmError;
    async fn list(&self, prefix: &str) -> Result<Vec<(String, Value)>> {
        Ok(self.repo.plugin_kv_list(PLUGIN_ID, prefix).await?)
    }
    async fn get(&self, key: &str) -> Result<Option<Value>> {
        Ok(self.repo.plugin_kv_get(PLUGIN_ID, key).await?)
    }
    async fn track(&self, id: &str) -> Result<Option<Track>> {
        Ok(self.repo.track_get(id).await?)
    }
    async fn is_running(&self) -> bool {
        match self.host.get() {
            Some(host) => host.running_plugin_ids().await.contains(PLUGIN_ID),
            None => false,
        }
    }
    async fn commit(&self, mode: CommitMode, mutation: Mutation<CalmError>) -> Result<Value> {
        match mode {
            CommitMode::Events(actor) => write_with_events_typed(
                self.repo.as_ref(),
                actor,
                None,
                &self.events,
                &self.write,
                move |tx| Box::pin(async move { mutation(&mut Tx { tx }).await }),
            )
            .await
            .map(|(value, _)| value),
            CommitMode::Silent => {
                write_in_tx_typed(self.repo.as_ref(), move |tx| {
                    Box::pin(async move {
                        let (value, effects) = mutation(&mut Tx { tx }).await?;
                        if !effects.is_empty() {
                            return Err(CalmError::Internal(
                                "silent calendar mutation produced events".into(),
                            ));
                        }
                        Ok(value)
                    })
                })
                .await
            }
        }
    }
}
