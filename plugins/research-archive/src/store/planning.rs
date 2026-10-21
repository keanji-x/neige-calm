//! Store-owned v3 pair selection and frozen request resolution.
use super::{Result, Store};
use crate::catalog::{Catalog, KeyKind, Method};
use crate::types::{AsOf, Key, Origin, RunId};
use sqlx::SqliteConnection;

pub(super) async fn append_pairs(
    conn: &mut SqliteConnection,
    run: RunId,
    key: &Key,
    origin: &Origin,
    catalog: &Catalog,
) -> Result<()> {
    let is_v3: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM run_plan_v3 WHERE run_id=?)")
        .bind(run.0)
        .fetch_one(&mut *conn)
        .await?;
    if !is_v3 {
        return Ok(());
    }
    let as_of: String = sqlx::query_scalar("SELECT as_of FROM run WHERE run_id=?")
        .bind(run.0)
        .fetch_one(&mut *conn)
        .await?;
    let as_of = AsOf::parse(&as_of)?;
    for e in catalog.endpoints() {
        let applies = match origin {
            Origin::Config | Origin::Index(_) => e.key_kind == KeyKind::Security,
            Origin::IndexSymbol => e.key_kind == KeyKind::Index,
            Origin::Market => e.key_kind == KeyKind::Market,
            Origin::Benchmark => e.method == Method::Quote,
        };
        if !applies {
            continue;
        }
        let request = serde_json::to_string(&e.resolve(key.as_str(), as_of)).expect("string pairs");
        sqlx::query("INSERT INTO run_pair(run_id,key,endpoint_id,request_json,capture_tier)
            SELECT ?1,?2,?3,?4,?5 WHERE EXISTS(SELECT 1 FROM run_endpoint WHERE run_id=?1 AND endpoint_id=?3)
            AND NOT EXISTS(SELECT 1 FROM run_pair WHERE run_id=?1 AND key=?2 AND endpoint_id=?3)")
            .bind(run.0).bind(key.as_str()).bind(e.id.as_str()).bind(request).bind(e.tier.as_str())
            .execute(&mut *conn).await?;
    }
    Ok(())
}

impl Store {
    /// The immutable request for a v3 pair; legacy requests retain their old recipe.
    pub async fn pair_request(
        &self,
        run: RunId,
        key: &Key,
        endpoint: crate::catalog::EndpointId,
    ) -> Result<Vec<(String, String)>> {
        let mut tx = self.pool.begin().await?;
        let request: Option<String> = sqlx::query_scalar(
            "SELECT request_json FROM run_pair WHERE run_id=? AND key=? AND endpoint_id=?",
        )
        .bind(run.0)
        .bind(key.as_str())
        .bind(endpoint.as_str())
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(request) = request {
            return serde_json::from_str(&request).map_err(|_| {
                super::StoreError::Sqlite(sqlx::Error::Protocol(
                    "invalid frozen pair request".into(),
                ))
            });
        }
        let is_v3: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM run_plan_v3 WHERE run_id=?)")
                .bind(run.0)
                .fetch_one(&mut *tx)
                .await?;
        if is_v3 {
            return Err(super::StoreError::NotPlanned {
                run,
                key: key.clone(),
                endpoint: endpoint.to_string(),
            });
        }
        let plan = super::write::load_plan(&mut tx, run, false).await?;
        let e = self
            .catalog
            .get(endpoint.as_str())
            .expect("catalog endpoint");
        Ok(e.resolve(key.as_str(), plan.as_of))
    }
}
