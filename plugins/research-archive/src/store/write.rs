//! The Store's writes. Each public method is one `BEGIN IMMEDIATE`
//! transaction: it commits whole or leaves nothing behind. The guard
//! triggers back every check here at the file level.

use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection};

use super::{Result, Store, StoreError};
use crate::catalog::EndpointId;
use crate::membership::constituents_members;
use crate::types::{AsOf, Finish, Key, Origin, Outcome, RunId, RunOutcome, Source, UnixMs, Window};

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
        if !req.window.contains(req.started_at) {
            return Err(StoreError::PlanOutsideWindow {
                started_at: req.started_at,
            });
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
        let (body, error) = match outcome {
            Outcome::Ok { body } => (Some(body), None),
            Outcome::Failed { body, error, .. } => (body.as_ref(), Some(error)),
        };
        if let Outcome::Ok { body } = outcome
            && self.catalog.membership() == Some(endpoint)
        {
            constituents_members(body).map_err(|reason| StoreError::IncompleteMembership {
                index: key.clone(),
                reason,
            })?;
        }
        let sha256 = body.map(|b| hex::encode(Sha256::digest(b)));
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let window = require_open(&mut tx, run_id).await?;
        if !window.contains(fetched_at) {
            return Err(StoreError::OutsideWindow {
                run: run_id,
                at: fetched_at,
            });
        }
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
        .bind(error.map(|e| e.class.as_str()))
        .bind(error.map(|e| e.code.as_str()))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(u32::try_from(attempt).expect("attempt counts stay far below u32::MAX"))
    }

    /// Ends a run that has not ended. `Complete` needs what the Store can
    /// check: every planned index frozen and every planned pair with at
    /// least one row. Whether the failures left are final (the attempt cap)
    /// is the runner's call.
    pub async fn end_run(
        &self,
        run_id: RunId,
        finished_at: UnixMs,
        outcome: Finish,
        reason: Option<&str>,
    ) -> Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        require_open(&mut tx, run_id).await?;
        if outcome == Finish::Complete {
            require_complete(&mut tx, run_id).await?;
        }
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

pub(super) async fn insert_run_key(
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

/// Refuses a run that does not exist or has ended; returns its window.
pub(super) async fn require_open(conn: &mut SqliteConnection, run_id: RunId) -> Result<Window> {
    let row = sqlx::query(
        "SELECT r.window_start, r.window_end,
                EXISTS (SELECT 1 FROM run_end e WHERE e.run_id = r.run_id) AS ended
         FROM run r WHERE r.run_id = ?",
    )
    .bind(run_id.0)
    .fetch_optional(conn)
    .await?
    .ok_or(StoreError::UnknownRun(run_id))?;
    if row.try_get::<bool, _>("ended")? {
        return Err(StoreError::RunEnded(run_id));
    }
    Ok(Window::new(
        UnixMs(row.try_get("window_start")?),
        UnixMs(row.try_get("window_end")?),
    )?)
}

/// Refuses `complete` for a run with an unfrozen planned index or a planned
/// pair without any row.
async fn require_complete(conn: &mut SqliteConnection, run_id: RunId) -> Result<()> {
    let row = sqlx::query(&format!(
        "SELECT
            NOT EXISTS (SELECT 1 FROM run_key k
                        WHERE k.run_id = ?1 AND k.origin = 'index-symbol'
                          AND NOT EXISTS (SELECT 1 FROM run_membership m
                                          WHERE m.run_id = k.run_id AND m.index_sym = k.key))
                AS frozen,
            (SELECT COUNT(*) FROM ({PLANNED_PAIRS}) p
             WHERE p.run_id = ?1
               AND NOT EXISTS (SELECT 1 FROM observation o
                               WHERE o.run_id = p.run_id AND o.key = p.key
                                 AND o.endpoint_id = p.endpoint_id)) AS missing"
    ))
    .bind(run_id.0)
    .fetch_one(conn)
    .await?;
    let frozen: bool = row.try_get("frozen")?;
    let missing: i64 = row.try_get("missing")?;
    if frozen && missing == 0 {
        Ok(())
    } else {
        Err(StoreError::NotComplete {
            run: run_id,
            frozen,
            pairs_without_rows: usize::try_from(missing).expect("a COUNT is never negative"),
        })
    }
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
