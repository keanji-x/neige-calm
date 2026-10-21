//! The Store's writes. Each public method is one `BEGIN IMMEDIATE`
//! transaction: it commits whole or leaves nothing behind. The guard
//! triggers back every check here at the file level.

use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection};

use super::{Result, Store, StoreError};
use crate::catalog::EndpointId;
use crate::membership::constituents_members;
use crate::types::{
    AsOf, ErrorClass, FailureClass, Finish, Key, Origin, Recorded, Response, RunId, RunOutcome,
    Source, Status, UnixMs, Window,
};

/// How a response is stored: its status, its bytes (if kept) and its
/// `(error_class, error_code)` (if a failure).
struct Classified<'r> {
    status: Status,
    body: Option<&'r [u8]>,
    error: Option<(&'static str, String)>,
}

impl<'r> Classified<'r> {
    fn ok(body: &'r [u8]) -> Classified<'r> {
        Classified {
            status: Status::Ok,
            body: Some(body),
            error: None,
        }
    }
}

/// The stored `error_class` of a membership body that fails the
/// completeness rule. Only the Store writes it; no caller can state it.
const INCOMPLETE_MEMBERSHIP: &str = "incomplete_membership";

/// The key × endpoint pairs a run plans, as rows `(run_id, key,
/// endpoint_id)`: each `run_key` row joined to the endpoints of its kind. An
/// `index-symbol` key takes the index endpoints; `config` and `index:<sym>`
/// keys take the security endpoints. The one definition of that rule.
pub(super) const PLANNED_PAIRS: &str = "
    SELECT DISTINCT rk.run_id AS run_id, rk.key AS key, e.endpoint_id AS endpoint_id, 'core' AS capture_tier
    FROM run_key rk JOIN run_endpoint re ON re.run_id = rk.run_id
    JOIN endpoint e ON e.endpoint_id = re.endpoint_id AND e.key_kind =
        CASE rk.origin WHEN 'index-symbol' THEN 'index' ELSE 'security' END
    WHERE NOT EXISTS(SELECT 1 FROM run_plan_v3 v WHERE v.run_id = rk.run_id)
    UNION ALL SELECT run_id,key,endpoint_id,capture_tier FROM run_pair";

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
    /// Additional comparison securities, bars only.
    pub benchmarks: Vec<Key>,
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
    /// Additional comparison securities, bars only.
    pub benchmarks: Vec<Key>,
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
    /// the day, its stored plan is returned and nothing is written. A new
    /// plan's `started_at` must be inside its window, and its window must
    /// not overlap or contradict the `as_of` order of another run of the
    /// source.
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
        require_window_order(&mut tx, req.source, req.as_of, req.window).await?;
        let run_id = insert_run(&mut tx, req.source, req.as_of, req.window, req.started_at).await?;
        sqlx::query("INSERT INTO run_plan_v3(run_id) VALUES (?)")
            .bind(run_id.0)
            .execute(&mut *tx)
            .await?;
        for key in &req.config {
            insert_run_key(&mut tx, run_id, key, &Origin::Config).await?;
            super::planning::append_pairs(&mut tx, run_id, key, &Origin::Config, &self.catalog)
                .await?;
        }
        for key in &req.indexes {
            insert_run_key(&mut tx, run_id, key, &Origin::IndexSymbol).await?;
            super::planning::append_pairs(
                &mut tx,
                run_id,
                key,
                &Origin::IndexSymbol,
                &self.catalog,
            )
            .await?;
        }
        if self.catalog.get("daily_candlesticks").is_some() {
            for key in &req.benchmarks {
                insert_run_key(&mut tx, run_id, key, &Origin::Benchmark).await?;
                super::planning::append_pairs(
                    &mut tx,
                    run_id,
                    key,
                    &Origin::Benchmark,
                    &self.catalog,
                )
                .await?;
            }
        }
        if self.catalog.get("finance_calendar").is_some() {
            let key = Key::new("market:US")?;
            insert_run_key(&mut tx, run_id, &key, &Origin::Market).await?;
            super::planning::append_pairs(&mut tx, run_id, &key, &Origin::Market, &self.catalog)
                .await?;
        }
        let plan = load_plan(&mut tx, run_id, true).await?;
        tx.commit().await?;
        Ok(plan)
    }

    /// The stored plan of `run_id`.
    pub async fn plan(&self, run_id: RunId) -> Result<RunPlan> {
        // One read transaction: the run row and its keys from one snapshot.
        let mut tx = self.pool.begin().await?;
        let plan = load_plan(&mut tx, run_id, false).await?;
        tx.commit().await?;
        Ok(plan)
    }

    /// Records one attempt: the body's blob (deduplicated by content) and the
    /// observation row, and returns the attempt number and the status the
    /// Store derived. The attempt number is the next one for `(run, key,
    /// endpoint)`, computed inside the transaction. The pair must be
    /// planned, the run must not have ended, and `fetched_at` must be inside
    /// the window and not before the run's clock (see [`RunClock`]), so
    /// attempt order is time order within a run.
    ///
    /// The caller never classifies a body. A body on a security endpoint is
    /// `ok`; a body on the membership endpoint is `ok` when it is a complete
    /// membership, else `permanent` / `incomplete_membership` with the rule
    /// that failed as `error_code`, and the bytes kept for audit. A failure
    /// without a body keeps no bytes.
    pub async fn record_observation(
        &self,
        run_id: RunId,
        key: &Key,
        endpoint: EndpointId,
        fetched_at: UnixMs,
        response: &Response,
    ) -> Result<Recorded> {
        let Classified {
            status,
            body,
            error,
        } = self.classify(endpoint, response)?;
        let sha256 = body.map(|b| hex::encode(Sha256::digest(b)));
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let clock = require_open(&mut tx, run_id).await?;
        clock.inside_window(fetched_at)?;
        clock.not_before_mark(fetched_at)?;
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
                .bind(body)
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
        .bind(status.as_str())
        .bind(sha256)
        .bind(error.as_ref().map(|(class, _)| *class))
        .bind(error.as_ref().map(|(_, code)| code.as_str()))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Recorded {
            attempt: u32::try_from(attempt).expect("attempt counts stay far below u32::MAX"),
            status,
        })
    }

    /// The stored `(status, body, (error_class, error_code))` of a response
    /// on `endpoint`. The only place a body is classified.
    fn classify<'r>(&self, endpoint: EndpointId, response: &'r Response) -> Result<Classified<'r>> {
        match response {
            Response::Body(body) if endpoint == self.catalog.membership() => {
                match constituents_members(body) {
                    Ok(_) => Ok(Classified::ok(body)),
                    Err(reason) => Ok(Classified {
                        status: Status::Failed(FailureClass::Permanent),
                        body: Some(body),
                        error: Some((INCOMPLETE_MEMBERSHIP, reason.code().to_owned())),
                    }),
                }
            }
            Response::Body(body) => Ok(Classified::ok(body)),
            Response::Failed { class, error } => {
                let restricted = self
                    .catalog
                    .get(endpoint.as_str())
                    .is_some_and(|e| e.dc_restrict.is_some());
                if error.class == ErrorClass::DcRegionRestricted && !restricted {
                    return Err(StoreError::ErrorClassForEndpoint {
                        endpoint: endpoint.as_str().to_owned(),
                        class: error.class.as_str(),
                    });
                }
                Ok(Classified {
                    status: Status::Failed(*class),
                    body: None,
                    error: Some((error.class.as_str(), error.code.clone())),
                })
            }
        }
    }

    /// Ends a run that has not ended, at a `finished_at` not before the
    /// run's clock. `Complete` needs what the Store can
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
        let clock = require_open(&mut tx, run_id).await?;
        clock.not_before_mark(finished_at)?;
        if outcome == Finish::Complete {
            require_complete(&mut tx, run_id, None).await?;
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
    /// Refused while the window is open, when the day already has a run, or
    /// when its window conflicts with another run of the source.
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
        require_window_order(&mut tx, source, as_of, window).await?;
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

/// Refuses a window that overlaps, or contradicts the `as_of` order of, any
/// run of the same source: an earlier day's window must end at or before
/// this one starts, and a later day's must start at or after this one ends.
/// So across runs of a source, `fetched_at` order is `as_of` order.
async fn require_window_order(
    conn: &mut SqliteConnection,
    source: Source,
    as_of: AsOf,
    window: Window,
) -> Result<()> {
    let conflict: Option<i64> = sqlx::query_scalar(
        "SELECT run_id FROM run
         WHERE source = ?1
           AND ((as_of < ?2 AND window_end > ?3) OR (as_of > ?2 AND window_start < ?4))
         ORDER BY run_id LIMIT 1",
    )
    .bind(source.as_str())
    .bind(as_of.to_string())
    .bind(window.start().0)
    .bind(window.end().0)
    .fetch_optional(conn)
    .await?;
    match conflict {
        Some(id) => Err(StoreError::WindowConflict { with: RunId(id) }),
        None => Ok(()),
    }
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
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT INTO run_endpoint (run_id, endpoint_id) SELECT ?, endpoint_id FROM endpoint",
    )
    .bind(id)
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

/// An open run's window and clock.
///
/// **A run's clock is monotonic.** Its high-water mark is the latest of
/// `started_at`, every `frozen_at` and every `fetched_at` in the run (an
/// ended run takes no writes, so `finished_at` never raises it). Every
/// `fetched_at`, `frozen_at` and `finished_at` written to the run must be
/// at or after the mark. So within a run, attempt order is time order, no
/// row predates the plan, and an index member's observations come after the
/// freeze that planned it.
pub(super) struct RunClock {
    pub(super) run_id: RunId,
    pub(super) window: Window,
    pub(super) mark: UnixMs,
}

impl RunClock {
    /// Refuses `at` before the mark. Equal is allowed.
    pub(super) fn not_before_mark(&self, at: UnixMs) -> Result<()> {
        if at < self.mark {
            Err(StoreError::ClockWentBack {
                run: self.run_id,
                at,
                mark: self.mark,
            })
        } else {
            Ok(())
        }
    }

    /// Refuses `at` outside `[window_start, window_end)`.
    pub(super) fn inside_window(&self, at: UnixMs) -> Result<()> {
        if self.window.contains(at) {
            Ok(())
        } else {
            Err(StoreError::OutsideWindow {
                run: self.run_id,
                at,
            })
        }
    }
}

/// Refuses a run that does not exist or has ended; returns its clock.
pub(super) async fn require_open(conn: &mut SqliteConnection, run_id: RunId) -> Result<RunClock> {
    let row = sqlx::query(
        "SELECT r.window_start, r.window_end,
                EXISTS (SELECT 1 FROM run_end e WHERE e.run_id = r.run_id) AS ended,
                MAX(r.started_at,
                    COALESCE((SELECT finished_at FROM run_core_complete c WHERE c.run_id=r.run_id), r.started_at),
                    COALESCE((SELECT MAX(o.fetched_at) FROM observation o
                              WHERE o.run_id = r.run_id), r.started_at),
                    COALESCE((SELECT MAX(m.frozen_at) FROM run_membership m
                              WHERE m.run_id = r.run_id), r.started_at)) AS mark
         FROM run r WHERE r.run_id = ?",
    )
    .bind(run_id.0)
    .fetch_optional(conn)
    .await?
    .ok_or(StoreError::UnknownRun(run_id))?;
    if row.try_get::<bool, _>("ended")? {
        return Err(StoreError::RunEnded(run_id));
    }
    Ok(RunClock {
        run_id,
        window: Window::new(
            UnixMs(row.try_get("window_start")?),
            UnixMs(row.try_get("window_end")?),
        )?,
        mark: UnixMs(row.try_get("mark")?),
    })
}

/// Refuses `complete` for a run with an unfrozen planned index or a planned
/// pair without any row.
pub(super) async fn require_complete(
    conn: &mut SqliteConnection,
    run_id: RunId,
    selected: Option<crate::catalog::CaptureTier>,
) -> Result<()> {
    let mut sql = sqlx::QueryBuilder::<sqlx::Sqlite>::new("WITH selected_run AS (SELECT ");
    sql.push_bind(run_id.0);
    sql.push(format!(" AS run_id) SELECT
            NOT EXISTS (SELECT 1 FROM run_key k
                        WHERE k.run_id = (SELECT run_id FROM selected_run) AND k.origin = 'index-symbol'
                          AND NOT EXISTS (SELECT 1 FROM run_membership m
                                          WHERE m.run_id = k.run_id AND m.index_sym = k.key))
                AS frozen,
            (SELECT COUNT(*) FROM ({PLANNED_PAIRS}) p
             WHERE p.run_id = (SELECT run_id FROM selected_run)
               AND NOT EXISTS (SELECT 1 FROM observation o
                               WHERE o.run_id = p.run_id AND o.key = p.key
                                 AND o.endpoint_id = p.endpoint_id)"
    ));
    if let Some(tier) = selected {
        sql.push(" AND p.capture_tier = ").push_bind(tier.as_str());
    }
    sql.push(") AS missing");
    let row = sql.build().fetch_one(conn).await?;
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
         WHERE run_id = ? AND origin IN ('config', 'index-symbol', 'benchmark') ORDER BY key",
    )
    .bind(run_id.0)
    .fetch_all(&mut *conn)
    .await?;
    let mut config = Vec::new();
    let mut indexes = Vec::new();
    let mut benchmarks = Vec::new();
    for k in &keys {
        let key = Key::new(k.try_get::<String, _>("key")?)?;
        match Origin::parse(&k.try_get::<String, _>("origin")?)? {
            Origin::Config => config.push(key),
            Origin::Benchmark => benchmarks.push(key),
            Origin::Market => {}
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
        benchmarks,
        indexes,
        frozen: row.try_get("frozen")?,
        ended: ended.as_deref().map(RunOutcome::parse).transpose()?,
        created,
    })
}
