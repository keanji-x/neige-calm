//! Exact registered-execution evidence retained across carrier retirement.
use crate::error::{CalmError, Result};
use workspace_runtime::execution::{ExecutionMetadata, Settlement};

/// Captured from the execution owner; absence of an owner claim alone is not proof.
pub(crate) struct Quiescence {
    rows: Vec<(String, String, Option<String>)>,
    session: String,
    track: String,
    consumer: &'static str,
    alias: super::AliasPrefix,
}
impl Quiescence {
    pub(crate) async fn recheck_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    ) -> Result<()> {
        let rows = execution_rows(&mut **tx, &self.session, self.alias).await?;
        if rows != self.rows {
            return Err(CalmError::Conflict(
                "Provider execution evidence changed".into(),
            ));
        }
        validate_rows(&rows, &self.session, &self.track, self.consumer, self.alias)
    }
}
async fn execution_rows<'a>(
    executor: impl sqlx::Executor<'a, Database = sqlx::Sqlite>,
    session: &str,
    alias: super::AliasPrefix,
) -> Result<Vec<(String, String, Option<String>)>> {
    let prefix = alias.alias(session, "");
    Ok(sqlx::query_as(concat!(
        "SELECT execution_id,intent_json,settlement_json FROM workspace_executions WHERE ",
        "substr(alias,1,length(?1))=?1 ORDER BY execution_id"
    ))
    .bind(prefix)
    .fetch_all(executor)
    .await?)
}
fn validate_rows(
    rows: &[(String, String, Option<String>)],
    session: &str,
    track: &str,
    consumer: &str,
    alias: super::AliasPrefix,
) -> Result<()> {
    if rows.is_empty() {
        return Err(CalmError::Conflict(
            "Provider execution absence is unproven".into(),
        ));
    }
    for (id, intent, settlement) in rows {
        let metadata: ExecutionMetadata = serde_json::from_str(intent)?;
        let settled: Option<Settlement> = settlement
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?;
        if metadata.id.as_str() != id
            || metadata.consumer.0 != consumer
            || metadata.track.0 != track
            || metadata.alias.0 != alias.alias(session, id)
            || !settled.is_some_and(|s| s.descendants_complete())
        {
            return Err(CalmError::Conflict(
                "Provider execution quiescence is unproven".into(),
            ));
        }
    }
    Ok(())
}

impl Quiescence {
    pub(crate) async fn capture(
        pool: &sqlx::SqlitePool,
        session: &str,
        track: &str,
        consumer: &'static str,
        alias: super::AliasPrefix,
    ) -> Result<Self> {
        let rows = execution_rows(pool, session, alias).await?;
        validate_rows(&rows, session, track, consumer, alias)?;
        Ok(Self {
            rows,
            session: session.into(),
            track: track.into(),
            consumer,
            alias,
        })
    }
}
