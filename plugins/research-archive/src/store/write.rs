//! The Store's writes. Each public method is one `BEGIN IMMEDIATE`
//! transaction: it commits whole or leaves nothing behind. The guard
//! triggers back every check here at the file level.

use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection};

use super::{Result, Store, StoreError};
use crate::catalog::EndpointId;
use crate::types::{
    AsOf, Finish, Key, Membership, Origin, Outcome, RunId, RunOutcome, Source, UnixMs, Window,
};

/// The key × endpoint pairs a run plans, as rows `(run_id, key,
/// endpoint_id)`: each `run_key` row joined to the endpoints of its kind. An
/// `index-symbol` key takes the index endpoints; `config` and `index:<sym>`
/// keys take the security endpoints. The one definition of that rule.
pub(super) const PLANNED_PAIRS: &str = "
    SELECT DISTINCT rk.run_id AS run_id, rk.key AS key, e.endpoint_id AS endpoint_id
    FROM run_key rk JOIN endpoint e ON e.key_kind =
        CASE rk.origin WHEN 'index-symbol' THEN 'index' ELSE 'security' END";

/// What `plan_run` writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanRequest {
    pub source: Source,
    pub as_of: AsOf,
    pub window: Window,
    /// Wall time of the plan; stored as `run.started_at`.
    pub started_at: UnixMs,
    /// The research list as configured now (`origin = config`).
    pub config: Vec<Key>,
    /// The configured index symbols (`origin = index-symbol`).
    pub indexes: Vec<Key>,
}

/// A run's committed plan, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunPlan {
    pub run_id: RunId,
    pub source: Source,
    pub as_of: AsOf,
    pub window: Window,
    pub started_at: UnixMs,
    /// Sorted `config` keys.
    pub config: Vec<Key>,
    /// Sorted `index-symbol` keys.
    pub indexes: Vec<Key>,
    /// Whether the freeze has committed.
    pub frozen: bool,
    /// The run's end, if written.
    pub ended: Option<RunOutcome>,
    /// Whether this call wrote the plan (false: an existing plan was read).
    pub created: bool,
}

impl Store {
    /// Writes the `run` row, its `config` keys and its `index-symbol` keys
    /// atomically. Idempotent per `(source, as_of)`: when a run exists for
    /// the day, its stored plan is returned and nothing is written.
    pub async fn plan_run(&self, req: &PlanRequest) -> Result<RunPlan> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(run_id) = find_run(&mut tx, req.source, req.as_of).await? {
            let plan = load_plan(&mut tx, run_id, false).await?;
            tx.commit().await?;
            return Ok(plan);
        }
        let run_id = insert_run(&mut tx, req.source, req.as_of, req.window, req.started_at).await?;
        for key in &req.config {
            insert_run_key(&mut tx, run_id, key, &Origin::Config).await?;
        }
        for key in &req.indexes {
            insert_run_key(&mut tx, run_id, key, &Origin::IndexSymbol).await?;
        }
        let plan = load_plan(&mut tx, run_id, true).await?;
        tx.commit().await?;
        Ok(plan)
    }

    /// The stored plan of `run_id`.
    pub async fn plan(&self, run_id: RunId) -> Result<RunPlan> {
        let mut conn = self.pool.acquire().await?;
        load_plan(&mut conn, run_id, false).await
    }

    /// Records one attempt: the body's blob (deduplicated by content) and the
    /// observation row. The attempt number is the next one for `(run, key,
    /// endpoint)`, computed inside the transaction, and is returned. The
    /// pair must be planned and the run must not have ended.
    pub async fn record_observation(
        &self,
        run_id: RunId,
        key: &Key,
        endpoint: EndpointId,
        fetched_at: UnixMs,
        outcome: &Outcome,
    ) -> Result<u32> {
        let (body, error_class, error_code) = match outcome {
            Outcome::Ok { body } => (Some(body), None, None),
            Outcome::Failed {
                body,
                error_class,
                error_code,
                ..
            } => (body.as_ref(), error_class.as_deref(), error_code.as_deref()),
        };
        let sha256 = body.map(|b| hex::encode(Sha256::digest(b)));
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        require_open(&mut tx, run_id).await?;
        let planned: bool = sqlx::query_scalar(&format!(
            "SELECT EXISTS (SELECT 1 FROM ({PLANNED_PAIRS}) p
             WHERE p.run_id = ? AND p.key = ? AND p.endpoint_id = ?)"
        ))
        .bind(run_id.0)
        .bind(key.as_str())
        .bind(endpoint.as_str())
        .fetch_one(&mut *tx)
        .await?;
        if !planned {
            return Err(StoreError::NotPlanned {
                run: run_id,
                key: key.clone(),
                endpoint: endpoint.as_str().to_owned(),
            });
        }
        if let (Some(sha), Some(body)) = (&sha256, body) {
            // A body already stored is ignored by the `blob_dedup` guard.
            sqlx::query("INSERT INTO blob (sha256, body) VALUES (?, ?)")
                .bind(sha)
                .bind(body.as_slice())
                .execute(&mut *tx)
                .await?;
        }
        let attempt: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(attempt), 0) + 1 FROM observation
             WHERE run_id = ? AND key = ? AND endpoint_id = ?",
        )
        .bind(run_id.0)
        .bind(key.as_str())
        .bind(endpoint.as_str())
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO observation (run_id, key, endpoint_id, attempt, fetched_at, status,
                                      sha256, error_class, error_code)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(run_id.0)
        .bind(key.as_str())
        .bind(endpoint.as_str())
        .bind(attempt)
        .bind(fetched_at.0)
        .bind(outcome.status().as_str())
        .bind(sha256)
        .bind(error_class)
        .bind(error_code)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(u32::try_from(attempt).expect("attempt counts stay far below u32::MAX"))
    }

    /// Freezes the run's universe: one `run_membership` row per planned
    /// index, plus each member as a `run_key` row with `origin =
    /// index:<sym>`, all or nothing. `indexes` must name exactly the planned
    /// index symbols. An `observed` index needs an `ok` constituents
    /// observation in this run; a `carried` one needs it in `source`, an
    /// earlier run (lower id). A second freeze of a run is refused with
    /// `AlreadyFrozen` and writes nothing. A run that plans no index has
    /// nothing to freeze: the call succeeds and writes nothing.
    pub async fn freeze_membership(
        &self,
        run_id: RunId,
        frozen_at: UnixMs,
        indexes: &[(Key, Membership)],
    ) -> Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        require_open(&mut tx, run_id).await?;
        let frozen: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM run_membership WHERE run_id = ?)")
                .bind(run_id.0)
                .fetch_one(&mut *tx)
                .await?;
        if frozen {
            return Err(StoreError::AlreadyFrozen(run_id));
        }
        let planned = load_plan(&mut tx, run_id, false).await?.indexes;
        let mut given: Vec<Key> = indexes.iter().map(|(k, _)| k.clone()).collect();
        given.sort();
        if given != planned {
            return Err(StoreError::FreezeIndexes {
                run: run_id,
                planned,
                given,
            });
        }
        for (index, membership) in indexes {
            let source = match membership {
                Membership::Observed { .. } => Some(run_id),
                Membership::Carried { source, .. } if *source >= run_id => {
                    return Err(StoreError::CarryNotEarlier {
                        run: run_id,
                        index: index.clone(),
                    });
                }
                Membership::Carried { source, .. } => Some(*source),
                Membership::None => None,
            };
            if let Some(source) = source
                && !has_ok_constituents(&mut tx, source, index).await?
            {
                return Err(StoreError::NoMembershipObservation {
                    source_run: source,
                    index: index.clone(),
                });
            }
            let carried_from = match membership {
                Membership::Carried { source, .. } => Some(source.0),
                _ => None,
            };
            sqlx::query(
                "INSERT INTO run_membership (run_id, index_sym, mode, source_run_id, frozen_at)
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(run_id.0)
            .bind(index.as_str())
            .bind(membership.mode())
            .bind(carried_from)
            .bind(frozen_at.0)
            .execute(&mut *tx)
            .await?;
            let origin = Origin::Index(index.clone());
            for member in membership.members() {
                insert_run_key(&mut tx, run_id, member, &origin).await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    /// Ends a run that has not ended.
    pub async fn end_run(
        &self,
        run_id: RunId,
        finished_at: UnixMs,
        outcome: Finish,
        reason: Option<&str>,
    ) -> Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        require_open(&mut tx, run_id).await?;
        insert_run_end(&mut tx, run_id, finished_at, outcome.into(), reason).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Ends as `partial` (reason `dangling`) every run whose window has
    /// closed (`window_end ≤ now`) and that has no `run_end`. Returns them.
    pub async fn close_dangling_runs(&self, now: UnixMs) -> Result<Vec<RunId>> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let ids: Vec<i64> = sqlx::query_scalar(
            "SELECT run_id FROM run
             WHERE window_end <= ? AND run_id NOT IN (SELECT run_id FROM run_end)
             ORDER BY run_id",
        )
        .bind(now.0)
        .fetch_all(&mut *tx)
        .await?;
        for &id in &ids {
            insert_run_end(
                &mut tx,
                RunId(id),
                now,
                RunOutcome::Partial,
                Some("dangling"),
            )
            .await?;
        }
        tx.commit().await?;
        Ok(ids.into_iter().map(RunId).collect())
    }

    /// Records a trading day whose window closed with no run: a `run` row
    /// (`started_at = now`, no keys) and its `run_end(missed)`, together.
    /// Refused while the window is open or when the day already has a run.
    pub async fn record_missed(
        &self,
        source: Source,
        as_of: AsOf,
        window: Window,
        now: UnixMs,
    ) -> Result<RunId> {
        if now < window.end() {
            return Err(StoreError::WindowOpen);
        }
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(existing) = find_run(&mut tx, source, as_of).await? {
            return Err(StoreError::RunExists(existing));
        }
        let run_id = insert_run(&mut tx, source, as_of, window, now).await?;
        insert_run_end(&mut tx, run_id, now, RunOutcome::Missed, None).await?;
        tx.commit().await?;
        Ok(run_id)
    }
}

async fn find_run(
    conn: &mut SqliteConnection,
    source: Source,
    as_of: AsOf,
) -> Result<Option<RunId>> {
    let id: Option<i64> =
        sqlx::query_scalar("SELECT run_id FROM run WHERE source = ? AND as_of = ?")
            .bind(source.as_str())
            .bind(as_of.to_string())
            .fetch_optional(conn)
            .await?;
    Ok(id.map(RunId))
}

async fn insert_run(
    conn: &mut SqliteConnection,
    source: Source,
    as_of: AsOf,
    window: Window,
    started_at: UnixMs,
) -> Result<RunId> {
    let id: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(run_id), 0) + 1 FROM run")
        .fetch_one(&mut *conn)
        .await?;
    sqlx::query(
        "INSERT INTO run (run_id, source, as_of, window_start, window_end, started_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(source.as_str())
    .bind(as_of.to_string())
    .bind(window.start().0)
    .bind(window.end().0)
    .bind(started_at.0)
    .execute(conn)
    .await?;
    Ok(RunId(id))
}

async fn insert_run_key(
    conn: &mut SqliteConnection,
    run_id: RunId,
    key: &Key,
    origin: &Origin,
) -> Result<()> {
    sqlx::query("INSERT INTO run_key (run_id, key, origin) VALUES (?, ?, ?)")
        .bind(run_id.0)
        .bind(key.as_str())
        .bind(origin.to_sql())
        .execute(conn)
        .await?;
    Ok(())
}

async fn insert_run_end(
    conn: &mut SqliteConnection,
    run_id: RunId,
    finished_at: UnixMs,
    outcome: RunOutcome,
    reason: Option<&str>,
) -> Result<()> {
    sqlx::query("INSERT INTO run_end (run_id, finished_at, outcome, reason) VALUES (?, ?, ?, ?)")
        .bind(run_id.0)
        .bind(finished_at.0)
        .bind(outcome.as_str())
        .bind(reason)
        .execute(conn)
        .await?;
    Ok(())
}

/// Refuses a run that does not exist or has ended.
async fn require_open(conn: &mut SqliteConnection, run_id: RunId) -> Result<()> {
    let row = sqlx::query(
        "SELECT (SELECT COUNT(*) FROM run WHERE run_id = ?1) AS exists_,
                (SELECT COUNT(*) FROM run_end WHERE run_id = ?1) AS ended",
    )
    .bind(run_id.0)
    .fetch_one(conn)
    .await?;
    if row.try_get::<i64, _>("exists_")? == 0 {
        return Err(StoreError::UnknownRun(run_id));
    }
    if row.try_get::<i64, _>("ended")? > 0 {
        return Err(StoreError::RunEnded(run_id));
    }
    Ok(())
}

/// Whether `run_id` holds an `ok` observation of an index endpoint for `index`.
async fn has_ok_constituents(
    conn: &mut SqliteConnection,
    run_id: RunId,
    index: &Key,
) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM observation o JOIN endpoint e ON e.endpoint_id = o.endpoint_id
            WHERE o.run_id = ? AND o.key = ? AND o.status = 'ok' AND e.key_kind = 'index'
        )",
    )
    .bind(run_id.0)
    .bind(index.as_str())
    .fetch_one(conn)
    .await?)
}

pub(super) async fn load_plan(
    conn: &mut SqliteConnection,
    run_id: RunId,
    created: bool,
) -> Result<RunPlan> {
    let row = sqlx::query(
        "SELECT r.source, r.as_of, r.window_start, r.window_end, r.started_at, e.outcome,
                EXISTS (SELECT 1 FROM run_membership m WHERE m.run_id = r.run_id) AS frozen
         FROM run r LEFT JOIN run_end e ON e.run_id = r.run_id WHERE r.run_id = ?",
    )
    .bind(run_id.0)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(StoreError::UnknownRun(run_id))?;
    let keys = sqlx::query(
        "SELECT key, origin FROM run_key
         WHERE run_id = ? AND origin IN ('config', 'index-symbol') ORDER BY key",
    )
    .bind(run_id.0)
    .fetch_all(&mut *conn)
    .await?;
    let mut config = Vec::new();
    let mut indexes = Vec::new();
    for k in &keys {
        let key = Key::new(k.try_get::<String, _>("key")?)?;
        match Origin::parse(&k.try_get::<String, _>("origin")?)? {
            Origin::Config => config.push(key),
            _ => indexes.push(key),
        }
    }
    let ended: Option<String> = row.try_get("outcome")?;
    Ok(RunPlan {
        run_id,
        source: Source::parse(&row.try_get::<String, _>("source")?)?,
        as_of: AsOf::parse(&row.try_get::<String, _>("as_of")?)?,
        window: Window::new(
            UnixMs(row.try_get("window_start")?),
            UnixMs(row.try_get("window_end")?),
        )?,
        started_at: UnixMs(row.try_get("started_at")?),
        config,
        indexes,
        frozen: row.try_get("frozen")?,
        ended: ended.as_deref().map(RunOutcome::parse).transpose()?,
        created,
    })
}
