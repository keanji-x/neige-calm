//! The Store's reads: the point-in-time value, the universe at a time, and
//! a run's coverage.

use sqlx::Row;

use super::write::PLANNED_PAIRS;
use super::{Result, Store};
use crate::catalog::EndpointId;
use crate::types::{AsOf, Key, Origin, RunId, Source, Status, UnixMs};

/// The one order of `ok` rows `o`, latest first: by `fetched_at`, then the
/// higher run id, then the higher attempt. Within a run the clock is
/// monotonic, so this is also attempt order. `read_at` and the freeze both
/// use it, so "the latest ok" has one meaning.
pub(super) const LATEST_OK: &str = "o.fetched_at DESC, o.run_id DESC, o.attempt DESC";

/// The value of a key × endpoint at a time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PointRead {
    /// The exact stored bytes. Empty bytes are a value, not an absence.
    pub body: Vec<u8>,
    pub fetched_at: UnixMs,
    /// The trading day of the run that fetched it.
    pub as_of: AsOf,
}

/// Universe membership at a time, from one run's `run_key` rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Universe {
    pub run_id: RunId,
    pub as_of: AsOf,
    /// Every `(key, origin)` fact visible at the time, sorted. A key with
    /// several origins appears once per origin.
    pub members: Vec<(Key, Origin)>,
    /// Planned indexes whose membership was not frozen by the time; their
    /// members are absent from `members`.
    pub pending: Vec<Key>,
}

impl Universe {
    /// The distinct keys: each key counts once, whatever its origins.
    pub fn keys(&self) -> std::collections::BTreeSet<&Key> {
        self.members.iter().map(|(k, _)| k).collect()
    }
}

/// One planned key × endpoint pair of a run and what it has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairCoverage {
    pub key: Key,
    pub endpoint: EndpointId,
    /// Observation rows of any status.
    pub attempts: u32,
    /// Whether any of them is `ok`.
    pub has_ok: bool,
}

impl Store {
    /// The latest `ok` observation of `key` × `endpoint` with `fetched_at ≤
    /// t`, as `(body, fetched_at, as_of)`. Failure rows never win; a later
    /// `ok` with an empty body wins over an earlier full one. Ties on
    /// `fetched_at` go to the higher run id, then the higher attempt.
    pub async fn read_at(
        &self,
        key: &Key,
        endpoint: EndpointId,
        t: UnixMs,
    ) -> Result<Option<PointRead>> {
        let row = sqlx::query(&format!(
            "SELECT b.body, o.fetched_at, r.as_of
             FROM observation o
             JOIN blob b ON b.sha256 = o.sha256
             JOIN run r ON r.run_id = o.run_id
             WHERE o.key = ? AND o.endpoint_id = ? AND o.status = 'ok' AND o.fetched_at <= ?
             ORDER BY {LATEST_OK}
             LIMIT 1"
        ))
        .bind(key.as_str())
        .bind(endpoint.as_str())
        .bind(t.0)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| {
            Ok(PointRead {
                body: r.try_get("body")?,
                fetched_at: UnixMs(r.try_get("fetched_at")?),
                as_of: AsOf::parse(&r.try_get::<String, _>("as_of")?)?,
            })
        })
        .transpose()
    }

    /// The universe at `t`: the run with the latest `started_at ≤ t` (ties
    /// to the higher run id) that is not a `missed` day. Its `config` and
    /// `index-symbol` keys were written with the run row; its `index:<sym>`
    /// keys count only when that index's `run_membership.frozen_at ≤ t`.
    /// `None` when no such run exists.
    pub async fn universe_at(&self, t: UnixMs) -> Result<Option<Universe>> {
        // One read transaction: every query sees the same snapshot.
        let mut conn = self.pool.begin().await?;
        let run = sqlx::query(
            "SELECT r.run_id, r.as_of FROM run r
             WHERE r.started_at <= ?
               AND r.run_id NOT IN (SELECT run_id FROM run_end WHERE outcome = 'missed')
             ORDER BY r.started_at DESC, r.run_id DESC LIMIT 1",
        )
        .bind(t.0)
        .fetch_optional(&mut *conn)
        .await?;
        let Some(run) = run else { return Ok(None) };
        let run_id = RunId(run.try_get("run_id")?);
        let as_of = AsOf::parse(&run.try_get::<String, _>("as_of")?)?;
        let rows = sqlx::query(
            "SELECT k.key, k.origin FROM run_key k
             WHERE k.run_id = ?1 AND (
                k.origin IN ('config', 'index-symbol')
                OR EXISTS (SELECT 1 FROM run_membership m
                           WHERE m.run_id = k.run_id AND k.origin = 'index:' || m.index_sym
                             AND m.frozen_at <= ?2))",
        )
        .bind(run_id.0)
        .bind(t.0)
        .fetch_all(&mut *conn)
        .await?;
        let mut members = Vec::with_capacity(rows.len());
        for r in &rows {
            members.push((
                Key::new(r.try_get::<String, _>("key")?)?,
                Origin::parse(&r.try_get::<String, _>("origin")?)?,
            ));
        }
        members.sort();
        let pending: Vec<String> = sqlx::query_scalar(
            "SELECT k.key FROM run_key k
             WHERE k.run_id = ?1 AND k.origin = 'index-symbol'
               AND NOT EXISTS (SELECT 1 FROM run_membership m
                               WHERE m.run_id = k.run_id AND m.index_sym = k.key
                                 AND m.frozen_at <= ?2)
             ORDER BY k.key",
        )
        .bind(run_id.0)
        .bind(t.0)
        .fetch_all(&mut *conn)
        .await?;
        conn.commit().await?;
        Ok(Some(Universe {
            run_id,
            as_of,
            members,
            pending: pending
                .into_iter()
                .map(Key::new)
                .collect::<std::result::Result<_, _>>()?,
        }))
    }

    /// Every planned key × endpoint pair of a run (each distinct key once,
    /// with the endpoints of its kinds), with its attempt count and whether
    /// one is `ok`. Index members appear once the freeze has committed.
    pub async fn coverage(&self, run_id: RunId) -> Result<Vec<PairCoverage>> {
        let rows = sqlx::query(&format!(
            "SELECT p.key, p.endpoint_id,
                    COUNT(o.attempt) AS attempts,
                    COALESCE(MAX(o.status = 'ok'), 0) AS has_ok
             FROM ({PLANNED_PAIRS}) p
             LEFT JOIN observation o
               ON o.run_id = p.run_id AND o.key = p.key AND o.endpoint_id = p.endpoint_id
             WHERE p.run_id = ?
             GROUP BY p.key, p.endpoint_id
             ORDER BY p.key, p.endpoint_id"
        ))
        .bind(run_id.0)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(PairCoverage {
                    key: Key::new(r.try_get::<String, _>("key")?)?,
                    endpoint: self.endpoint_id(&r.try_get::<String, _>("endpoint_id")?)?,
                    attempts: u32::try_from(r.try_get::<i64, _>("attempts")?)
                        .expect("a COUNT is never negative"),
                    has_ok: r.try_get("has_ok")?,
                })
            })
            .collect()
    }

    /// When the run's membership was frozen, or `None` while it is not.
    pub async fn frozen_at(&self, run_id: RunId) -> Result<Option<UnixMs>> {
        let at: Option<i64> =
            sqlx::query_scalar("SELECT MIN(frozen_at) FROM run_membership WHERE run_id = ?")
                .bind(run_id.0)
                .fetch_one(&self.pool)
                .await?;
        Ok(at.map(UnixMs))
    }

    /// The earliest `as_of` of any run of `source`, or `None` before the
    /// first run. Gap days are marked `missed` only from there on.
    pub async fn first_as_of(&self, source: Source) -> Result<Option<AsOf>> {
        let first: Option<String> =
            sqlx::query_scalar("SELECT MIN(as_of) FROM run WHERE source = ?")
                .bind(source.as_str())
                .fetch_one(&self.pool)
                .await?;
        Ok(first.as_deref().map(AsOf::parse).transpose()?)
    }

    /// The most frequent `(error_class, error_code)` among a run's rows with
    /// a status in `among`, with its row count (ties to the smaller class,
    /// then code). `None` when there is none.
    pub async fn dominant_failure(
        &self,
        run_id: RunId,
        among: &[Status],
    ) -> Result<Option<(String, String, u32)>> {
        if among.is_empty() {
            return Ok(None);
        }
        let marks = vec!["?"; among.len()].join(", ");
        let sql = format!(
            "SELECT error_class, error_code, COUNT(*) AS n FROM observation
             WHERE run_id = ? AND error_class IS NOT NULL AND status IN ({marks})
             GROUP BY error_class, error_code
             ORDER BY n DESC, error_class, error_code LIMIT 1"
        );
        let mut query = sqlx::query(&sql).bind(run_id.0);
        for status in among {
            query = query.bind(status.as_str());
        }
        let row = query.fetch_optional(&self.pool).await?;
        row.map(|r| {
            Ok((
                r.try_get("error_class")?,
                r.try_get("error_code")?,
                u32::try_from(r.try_get::<i64, _>("n")?).expect("a COUNT is never negative"),
            ))
        })
        .transpose()
    }

    /// The status of every observation row of a pair in a run, by attempt.
    pub async fn attempts(
        &self,
        run_id: RunId,
        key: &Key,
        endpoint: EndpointId,
    ) -> Result<Vec<(u32, UnixMs, Status)>> {
        let rows = sqlx::query(
            "SELECT attempt, fetched_at, status FROM observation
             WHERE run_id = ? AND key = ? AND endpoint_id = ? ORDER BY attempt",
        )
        .bind(run_id.0)
        .bind(key.as_str())
        .bind(endpoint.as_str())
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok((
                    u32::try_from(r.try_get::<i64, _>("attempt")?)
                        .expect("the attempt CHECK keeps it positive"),
                    UnixMs(r.try_get("fetched_at")?),
                    Status::parse(&r.try_get::<String, _>("status")?)?,
                ))
            })
            .collect()
    }
}
