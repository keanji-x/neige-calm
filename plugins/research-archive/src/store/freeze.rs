//! The freeze: the Store derives each planned index's membership from the
//! stored observations. The runner only decides when to freeze.

use sqlx::{Row, SqliteConnection};

use super::write::{insert_run_key, load_plan, require_open};
use super::{Result, Store, StoreError};
use crate::catalog::EndpointId;
use crate::membership::constituents_members;
use crate::types::{AsOf, Key, Membership, Origin, RunId, Source, UnixMs};

/// The `ok` constituents observation a membership comes from.
struct ConstituentsRow {
    run_id: RunId,
    attempt: i64,
    fetched_at: UnixMs,
    body: Vec<u8>,
}

impl Store {
    /// Freezes the run's universe, all or nothing, and returns what it
    /// wrote. For each planned `index-symbol` key, in key order:
    ///
    /// - **observed** when this run has an `ok` constituents observation of
    ///   it: the latest by attempt;
    /// - else **carried** from the run of the same source with the latest
    ///   earlier `as_of` that has one (its latest by attempt). That row is
    ///   an original observation by construction, never a carry;
    /// - else **none**.
    ///
    /// Members are parsed from that body with
    /// [`constituents_members`](crate::membership::constituents_members); a
    /// stored `ok` body that fails it (only possible if a writer bypassed
    /// `record_observation`) refuses the freeze. So does a `frozen_at`
    /// outside the run's window or before the `fetched_at` of a body used.
    /// One `run_membership` row per index and one `run_key` row (`origin =
    /// index:<sym>`) per member are written. A second freeze is refused
    /// with `AlreadyFrozen`. A run that plans no index has nothing to freeze:
    /// the call succeeds and writes nothing.
    pub async fn freeze_membership(
        &self,
        run_id: RunId,
        frozen_at: UnixMs,
    ) -> Result<Vec<(Key, Membership)>> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let window = require_open(&mut tx, run_id).await?;
        if !window.contains(frozen_at) {
            return Err(StoreError::OutsideWindow {
                run: run_id,
                at: frozen_at,
            });
        }
        let frozen: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM run_membership WHERE run_id = ?)")
                .bind(run_id.0)
                .fetch_one(&mut *tx)
                .await?;
        if frozen {
            return Err(StoreError::AlreadyFrozen(run_id));
        }
        let plan = load_plan(&mut tx, run_id, false).await?;
        if plan.indexes.is_empty() {
            return Ok(Vec::new());
        }
        let endpoint = self
            .catalog
            .membership()
            .ok_or(StoreError::NoMembershipEndpoint(run_id))?;
        let mut written = Vec::with_capacity(plan.indexes.len());
        for index in plan.indexes {
            let own = latest_ok(&mut tx, endpoint, &index, Scope::Run(run_id)).await?;
            let earlier = match own {
                Some(_) => None,
                None => {
                    let scope = Scope::EarlierThan(plan.source, plan.as_of);
                    latest_ok(&mut tx, endpoint, &index, scope).await?
                }
            };
            let membership = match (own, earlier) {
                (Some(o), _) => Membership::Observed {
                    members: members_of(&index, &o, frozen_at)?,
                },
                (None, Some(o)) => Membership::Carried {
                    source: o.run_id,
                    members: members_of(&index, &o, frozen_at)?,
                },
                (None, None) => Membership::None,
            };
            let source_run_id = match &membership {
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
            .bind(source_run_id)
            .bind(frozen_at.0)
            .execute(&mut *tx)
            .await?;
            let origin = Origin::Index(index.clone());
            for member in membership.members() {
                insert_run_key(&mut tx, run_id, member, &origin).await?;
            }
            written.push((index, membership));
        }
        tx.commit().await?;
        Ok(written)
    }
}

/// Where to look for an `ok` constituents observation.
enum Scope {
    /// This run.
    Run(RunId),
    /// Runs of this source with an earlier `as_of`; the latest day wins.
    EarlierThan(Source, AsOf),
}

/// The latest `ok` observation of `endpoint` for `index` in `scope`: the
/// latest `as_of`, then the highest attempt.
async fn latest_ok(
    conn: &mut SqliteConnection,
    endpoint: EndpointId,
    index: &Key,
    scope: Scope,
) -> Result<Option<ConstituentsRow>> {
    let select = "SELECT o.run_id, o.attempt, o.fetched_at, b.body
         FROM observation o
         JOIN run r ON r.run_id = o.run_id
         JOIN blob b ON b.sha256 = o.sha256
         WHERE o.endpoint_id = ? AND o.key = ? AND o.status = 'ok'";
    let row = match scope {
        Scope::Run(run) => {
            let sql = format!("{select} AND r.run_id = ? ORDER BY o.attempt DESC LIMIT 1");
            sqlx::query(&sql)
                .bind(endpoint.as_str())
                .bind(index.as_str())
                .bind(run.0)
                .fetch_optional(conn)
                .await?
        }
        Scope::EarlierThan(source, as_of) => {
            let sql = format!(
                "{select} AND r.source = ? AND r.as_of < ?
                 ORDER BY r.as_of DESC, o.attempt DESC LIMIT 1"
            );
            sqlx::query(&sql)
                .bind(endpoint.as_str())
                .bind(index.as_str())
                .bind(source.as_str())
                .bind(as_of.to_string())
                .fetch_optional(conn)
                .await?
        }
    };
    row.map(|r| {
        Ok(ConstituentsRow {
            run_id: RunId(r.try_get("run_id")?),
            attempt: r.try_get("attempt")?,
            fetched_at: UnixMs(r.try_get("fetched_at")?),
            body: r.try_get("body")?,
        })
    })
    .transpose()
}

/// The members in the body of `origin`, refusing look-ahead and bodies that are
/// not a complete membership.
fn members_of(index: &Key, origin: &ConstituentsRow, frozen_at: UnixMs) -> Result<Vec<Key>> {
    if origin.fetched_at > frozen_at {
        return Err(StoreError::FreezeBeforeObservation {
            index: index.clone(),
            frozen_at,
            fetched_at: origin.fetched_at,
        });
    }
    constituents_members(&origin.body).map_err(|reason| StoreError::StoredMembershipInvalid {
        source_run: origin.run_id,
        index: index.clone(),
        attempt: origin.attempt,
        reason,
    })
}
