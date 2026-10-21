//! Explicit v2→v3 upgrade, separate from ordinary opens and the frozen v1→v2 path.
use super::{OpenError, Store, insert_endpoint, schema, schema_v3};
use crate::catalog::Catalog;
use sqlx::{Connection, sqlite::SqliteConnectOptions};
use std::path::Path;

impl Store {
    /// Caller must hold archive.lock. Never creates a missing archive.
    pub async fn upgrade_market_catalog(path: &Path) -> Result<Store, OpenError> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(false)
            .foreign_keys(false)
            .busy_timeout(std::time::Duration::from_secs(5));
        let mut conn = sqlx::SqliteConnection::connect_with(&options).await?;
        let mut tx = conn.begin_with("BEGIN IMMEDIATE").await?;
        // Revalidate under the transaction; the CLI lock excludes cooperating writers.
        let version = schema::user_version(&mut tx).await?;
        if version != 2 && version != 3 {
            return Err(OpenError::UserVersion {
                found: version,
                expected: 2,
            });
        }
        let expected = if version == 2 {
            schema::reference_v2().await?
        } else {
            schema::reference().await?
        };
        let catalog = if version == 2 {
            Catalog::released_v2()
        } else {
            Catalog::compiled()
        };
        let found = schema::read_schema(&mut tx).await?;
        if let Some(drift) = schema::SchemaDrift::between(&expected.0, &found) {
            return Err(OpenError::Schema(drift));
        }
        if let Some(drift) = Store::catalog_drift_for(&mut tx, &catalog).await? {
            return Err(OpenError::Catalog(drift));
        }
        if version == 2 {
            sqlx::Executor::execute(&mut *tx, schema_v3::MIGRATION_V3).await?;
            for endpoint in Catalog::compiled().endpoints().iter().skip(8) {
                insert_endpoint(&mut tx, endpoint).await?;
            }
        }
        let expected = schema::reference().await?;
        let found = schema::read_schema(&mut tx).await?;
        if let Some(drift) = schema::SchemaDrift::between(&expected.0, &found) {
            return Err(OpenError::Schema(drift));
        }
        if let Some(drift) = Store::catalog_drift_for(&mut tx, &Catalog::compiled()).await? {
            return Err(OpenError::Catalog(drift));
        }
        let violations = sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&mut *tx)
            .await?;
        if !violations.is_empty() {
            return Err(OpenError::Sqlite(sqlx::Error::Protocol(
                "upgrade foreign key check failed".into(),
            )));
        }
        tx.commit().await?;
        sqlx::Executor::execute(&mut conn, "PRAGMA foreign_keys = ON").await?;
        let enabled: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&mut conn)
            .await?;
        if enabled != 1 {
            return Err(OpenError::Sqlite(sqlx::Error::Protocol(
                "foreign keys not enabled".into(),
            )));
        }
        conn.close().await?;
        Store::open(path).await
    }
}
