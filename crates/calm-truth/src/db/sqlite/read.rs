use async_trait::async_trait;
use sqlx::Row;

use super::task::TASK_COLUMNS;
use super::{
    SqlxRepo, derive_session_identity, session_get_by_active_token_hash, session_get_by_id,
};
use crate::card_role_cache::CardRoleCache;
use crate::db::{
    COVE_TASK_SUMMARY_MAX_WAVES, RepoRead, SessionCardIdentity, SharedCodexDaemonRecord,
    WorkspaceLease,
};
use crate::error::{CalmError, Result};
use crate::ids::{CardId, CoveId, WaveId};
use crate::model::*;
use crate::session_projection_repo::WorkerSessionKind;
use crate::wave_cove_cache::WaveCoveCache;
use calm_types::worker::{WorkerSession, WorkerSessionId};

/// Row shape of the single-statement `wave_detail` read (#1016).
///
/// The wave columns decode through the usual [`crate::db::rows::WaveRow`]
/// mirror; `cards` and `overlays` ride along as JSON arrays produced by
/// `json_group_array` so that all three come from ONE implicit transaction
/// without the row multiplication a join would cause (a wave-scoped overlay
/// would pair with every card).
#[derive(sqlx::FromRow)]
struct WaveDetailRow {
    #[sqlx(flatten)]
    wave: crate::db::rows::WaveRow,
    cards_json: String,
    overlays_json: String,
}

/// Row shape of the one-statement cove summary. Cove totals repeat on every
/// returned wave row; an existing empty cove produces exactly one row with a
/// NULL `wave_id`, while a missing cove produces no rows.
#[derive(sqlx::FromRow)]
struct CoveTaskSummaryRow {
    total_pending: i64,
    total_in_flight: i64,
    total_done: i64,
    total_failed: i64,
    total_canceled: i64,
    total_legacy_live: i64,
    total_block_live: i64,
    total_spec_live: i64,
    total_user_live: i64,
    truncated: i64,
    wave_id: Option<String>,
    title: Option<String>,
    lifecycle: Option<String>,
    parent_wave_id: Option<String>,
    spec_task_ceiling: Option<i64>,
    tree_task_budget: Option<i64>,
    pending: Option<i64>,
    in_flight: Option<i64>,
    done: Option<i64>,
    failed: Option<i64>,
    canceled: Option<i64>,
    legacy_live: Option<i64>,
    block_live: Option<i64>,
    spec_live: Option<i64>,
    user_live: Option<i64>,
}

/// One static statement owns all four pieces of the contract: the `coves`
/// anchor distinguishes missing from empty, window totals are calculated over
/// every `wave_counts` row, `ROW_NUMBER` applies the stable sort/limit only to
/// the returned rows, and `wave_count` yields `truncated` from the same SQLite
/// snapshot. Do not split this into autocommit reads.
pub(super) const COVE_TASK_SUMMARY_SQL: &str = concat!(
    r#"
WITH wave_counts AS (
  SELECT
    w.id AS wave_id,
    w.title,
    w.lifecycle,
    w.parent_wave_id,
    w.spec_task_ceiling,
    w.tree_task_budget,
    w.updated_at,
    SUM(CASE WHEN t.status = 'pending' THEN 1 ELSE 0 END) AS pending,
    SUM(CASE WHEN t.status IN ('dispatched','running','verifying') THEN 1 ELSE 0 END) AS in_flight,
    SUM(CASE WHEN t.status = 'done' THEN 1 ELSE 0 END) AS done,
    SUM(CASE WHEN t.status = 'failed' THEN 1 ELSE 0 END) AS failed,
    SUM(CASE WHEN t.status = 'canceled' THEN 1 ELSE 0 END) AS canceled,
    SUM(CASE WHEN "#,
    legacy_live_spec_predicate!("t"),
    r#" THEN 1 ELSE 0 END) AS legacy_live,
    SUM(CASE WHEN t.declared_by = 'spec' AND t.origin = 'block'
                  AND t.status NOT IN ('done','failed','canceled') THEN 1 ELSE 0 END) AS block_live,
    SUM(CASE WHEN t.declared_by = 'spec'
                  AND t.status NOT IN ('done','failed','canceled') THEN 1 ELSE 0 END) AS spec_live,
    SUM(CASE WHEN t.declared_by = 'user'
                  AND t.status NOT IN ('done','failed','canceled') THEN 1 ELSE 0 END) AS user_live
  FROM waves w
  LEFT JOIN tasks t ON t.wave_id = w.id
  WHERE w.cove_id = ?1
  GROUP BY w.id, w.title, w.lifecycle, w.parent_wave_id,
           w.spec_task_ceiling, w.tree_task_budget, w.updated_at
), ranked AS (
  SELECT
    wave_counts.*,
    SUM(pending) OVER () AS total_pending,
    SUM(in_flight) OVER () AS total_in_flight,
    SUM(done) OVER () AS total_done,
    SUM(failed) OVER () AS total_failed,
    SUM(canceled) OVER () AS total_canceled,
    SUM(legacy_live) OVER () AS total_legacy_live,
    SUM(block_live) OVER () AS total_block_live,
    SUM(spec_live) OVER () AS total_spec_live,
    SUM(user_live) OVER () AS total_user_live,
    COUNT(*) OVER () AS wave_count,
    ROW_NUMBER() OVER (
      ORDER BY legacy_live DESC, updated_at DESC, wave_id ASC
    ) AS ordinal
  FROM wave_counts
)
SELECT
  COALESCE(r.total_pending, 0) AS total_pending,
  COALESCE(r.total_in_flight, 0) AS total_in_flight,
  COALESCE(r.total_done, 0) AS total_done,
  COALESCE(r.total_failed, 0) AS total_failed,
  COALESCE(r.total_canceled, 0) AS total_canceled,
  COALESCE(r.total_legacy_live, 0) AS total_legacy_live,
  COALESCE(r.total_block_live, 0) AS total_block_live,
  COALESCE(r.total_spec_live, 0) AS total_spec_live,
  COALESCE(r.total_user_live, 0) AS total_user_live,
  CASE WHEN COALESCE(r.wave_count, 0) > ?2 THEN 1 ELSE 0 END AS truncated,
  r.wave_id,
  r.title,
  r.lifecycle,
  r.parent_wave_id,
  r.spec_task_ceiling,
  r.tree_task_budget,
  r.pending,
  r.in_flight,
  r.done,
  r.failed,
  r.canceled,
  r.legacy_live,
  r.block_live,
  r.spec_live,
  r.user_live
FROM coves c
LEFT JOIN ranked r ON r.ordinal <= ?2
WHERE c.id = ?1
ORDER BY r.ordinal
"#
);

pub(super) async fn cove_task_summary_on(
    conn: &mut sqlx::SqliteConnection,
    cove_id: &str,
) -> Result<Option<CoveTaskSummary>> {
    let rows = sqlx::query_as::<_, CoveTaskSummaryRow>(COVE_TASK_SUMMARY_SQL)
        .bind(cove_id)
        .bind(COVE_TASK_SUMMARY_MAX_WAVES)
        .fetch_all(conn)
        .await?;
    let Some(first) = rows.first() else {
        return Ok(None);
    };

    let totals = TaskSummaryCounts {
        pending: first.total_pending,
        in_flight: first.total_in_flight,
        done: first.total_done,
        failed: first.total_failed,
        canceled: first.total_canceled,
        legacy_live: first.total_legacy_live,
        block_live: first.total_block_live,
        spec_live: first.total_spec_live,
        user_live: first.total_user_live,
    };
    let truncated = first.truncated != 0;
    let waves = rows
        .into_iter()
        .filter_map(|row| {
            let wave_id = row.wave_id.clone()?;
            Some((wave_id, row))
        })
        .map(|(wave_id, row)| {
            let lifecycle = row
                .lifecycle
                .ok_or_else(|| {
                    CalmError::Internal(format!("summary wave {wave_id} has no lifecycle"))
                })
                .and_then(|value| WaveLifecycle::try_from(value).map_err(CalmError::Internal))?;
            Ok(WaveTaskSummary {
                wave_id,
                title: row.title.unwrap_or_default(),
                lifecycle,
                parent_wave_id: row.parent_wave_id,
                spec_task_ceiling: row.spec_task_ceiling,
                tree_task_budget: row.tree_task_budget,
                counts: TaskSummaryCounts {
                    pending: row.pending.unwrap_or_default(),
                    in_flight: row.in_flight.unwrap_or_default(),
                    done: row.done.unwrap_or_default(),
                    failed: row.failed.unwrap_or_default(),
                    canceled: row.canceled.unwrap_or_default(),
                    legacy_live: row.legacy_live.unwrap_or_default(),
                    block_live: row.block_live.unwrap_or_default(),
                    spec_live: row.spec_live.unwrap_or_default(),
                    user_live: row.user_live.unwrap_or_default(),
                },
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(Some(CoveTaskSummary {
        totals,
        waves,
        truncated,
    }))
}

#[async_trait]
impl RepoRead for SqlxRepo {
    // ---------------------------------------------------------------- coves
    async fn coves_list(&self) -> Result<Vec<Cove>> {
        let rows = sqlx::query_as::<_, crate::db::rows::CoveRow>(
            r#"SELECT id, name, color, sort, kind, created_at, updated_at
               FROM coves ORDER BY sort ASC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Cove::from).collect())
    }

    async fn coves_list_user_visible(&self) -> Result<Vec<Cove>> {
        // Issue #175 — default surface for `GET /api/coves`. Filters out
        // the singleton system cove that hosts the default Today
        // terminal's wave + card. Pre-#175 callers that want every row
        // (debug surfaces, integration tests asserting on the system
        // cove's existence) use `coves_list` directly.
        let rows = sqlx::query_as::<_, crate::db::rows::CoveRow>(
            r#"SELECT id, name, color, sort, kind, created_at, updated_at
               FROM coves WHERE kind = 'user' ORDER BY sort ASC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Cove::from).collect())
    }

    async fn cove_get(&self, id: &str) -> Result<Option<Cove>> {
        let row = sqlx::query_as::<_, crate::db::rows::CoveRow>(
            r#"SELECT id, name, color, sort, kind, created_at, updated_at
               FROM coves WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Cove::from))
    }

    async fn cove_task_summary(&self, cove_id: &str) -> Result<Option<CoveTaskSummary>> {
        // Exactly one autocommit statement: no explicit transaction and no
        // existence preflight. See `COVE_TASK_SUMMARY_SQL`.
        let mut conn = self.pool.acquire().await?;
        cove_task_summary_on(&mut conn, cove_id).await
    }

    async fn cove_get_system(&self) -> Result<Option<Cove>> {
        // Issue #175 — return the singleton system cove if it exists,
        // `None` before the first call to the `POST /api/coves/system`
        // upsert endpoint. Backed by the partial unique index on
        // `coves(kind) WHERE kind = 'system'` from migration 0009 —
        // there is at most one such row.
        let row = sqlx::query_as::<_, crate::db::rows::CoveRow>(
            r#"SELECT id, name, color, sort, kind, created_at, updated_at
               FROM coves WHERE kind = 'system' LIMIT 1"#,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Cove::from))
    }

    // -------------------------------------------------------- cove_folders
    async fn cove_folders_by_cove(&self, cove_id: &str) -> Result<Vec<CoveFolder>> {
        let rows = sqlx::query_as::<_, crate::db::rows::CoveFolderRow>(
            r#"SELECT id, cove_id, path, repo_identity, repo_identity_probed_at, created_at
               FROM cove_folders WHERE cove_id = ?1 ORDER BY path ASC"#,
        )
        .bind(cove_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(CoveFolder::from).collect())
    }

    async fn cove_folders_list_all(&self) -> Result<Vec<CoveFolder>> {
        let rows = sqlx::query_as::<_, crate::db::rows::CoveFolderRow>(
            r#"SELECT id, cove_id, path, repo_identity, repo_identity_probed_at, created_at
               FROM cove_folders ORDER BY path ASC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(CoveFolder::from).collect())
    }

    async fn cove_folder_get(&self, id: i64) -> Result<Option<CoveFolder>> {
        let row = sqlx::query_as::<_, crate::db::rows::CoveFolderRow>(
            r#"SELECT id, cove_id, path, repo_identity, repo_identity_probed_at, created_at
               FROM cove_folders WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(CoveFolder::from))
    }

    // ---------------------------------------------------------------- waves
    async fn waves_by_cove(&self, cove_id: &str) -> Result<Vec<Wave>> {
        let rows = sqlx::query_as::<_, crate::db::rows::WaveRow>(
            r#"SELECT id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd, workflow_id, purpose, workflow_input, terminal_at, created_at, updated_at
               FROM waves WHERE cove_id = ?1 ORDER BY sort ASC"#,
        )
        .bind(cove_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Wave::from).collect())
    }

    async fn wave_get(&self, id: &str) -> Result<Option<Wave>> {
        let row = sqlx::query_as::<_, crate::db::rows::WaveRow>(
            r#"SELECT id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd, workflow_id, purpose, workflow_input, terminal_at, created_at, updated_at
               FROM waves WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Wave::from))
    }

    async fn waves_window(
        &self,
        cove_id: Option<&str>,
        since: Option<i64>,
        until: Option<i64>,
    ) -> Result<Vec<Wave>> {
        // Build the WHERE clause dynamically because sqlx doesn't have
        // good "optional bind" ergonomics — every binding has to be
        // either materialized or excluded from the query string. The
        // three predicates compose in any combination:
        //   * `cove_id`     : `cove_id = ?`
        //   * `until`       : `created_at <= ?`
        //   * `since`       : `(terminal_at IS NULL OR terminal_at >= ?)`
        let mut sql = String::from(
            "SELECT id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd, workflow_id, purpose, workflow_input, \
             terminal_at, created_at, updated_at FROM waves",
        );
        let mut where_clauses: Vec<&str> = Vec::new();
        if cove_id.is_some() {
            where_clauses.push("cove_id = ?");
        }
        if until.is_some() {
            where_clauses.push("created_at <= ?");
        }
        if since.is_some() {
            where_clauses.push("(terminal_at IS NULL OR terminal_at >= ?)");
        }
        if !where_clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&where_clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY created_at ASC, id ASC");

        let mut q = sqlx::query_as::<_, crate::db::rows::WaveRow>(&sql);
        if let Some(c) = cove_id {
            q = q.bind(c);
        }
        if let Some(u) = until {
            q = q.bind(u);
        }
        if let Some(s) = since {
            q = q.bind(s);
        }
        Ok(q.fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(Wave::from)
            .collect())
    }

    async fn wave_detail(&self, id: &str) -> Result<Option<WaveDetail>> {
        // ONE statement, no explicit transaction (#1016).
        //
        // Why not a deferred (`pool.begin()`) tx, which is what this used to
        // be: it held R(waves)+R(cards) while parking on `overlays` and
        // cycled with the IMMEDIATE writer of `DELETE /api/waves/:id`
        // (overlays -> tasks -> waves), aborting the writer with the
        // non-retryable `SQLITE_LOCKED` (6) — see
        // `deferred_read_tx_deadlock_repro`. That gap is real only on a
        // SHARED-CACHE database with table-granularity locks, i.e. the
        // in-memory sqlite CI and `make dev-fresh` run on. The production
        // file database (PRIVATECACHE + WAL) gives readers an MVCC snapshot
        // that never blocks, so no cycle exists there either way.
        //
        // Why not three separate autocommit statements (the first #1016
        // attempt): autocommit does break the cycle — a blocked autocommit
        // statement unwinds its implicit transaction, releasing every table
        // lock it took, before sqlx parks in `unlock_notify` — but splitting
        // the read into three statements throws away cross-statement
        // consistency (a card could appear whose overlays were read from an
        // older version, or vice versa) on EVERY deployment, including the
        // production one that never had the problem. That is a pure loss.
        //
        // A single statement is both: it is autocommit (so it can never be
        // the lock-HOLDING waiter that closes a cycle) AND it is one
        // implicit transaction (so wave, cards and overlays all come from
        // one version of the database). The lock order is unchanged —
        // waves, then cards, then overlays — so the repro above still parks
        // on `overlays`, it just holds nothing while parked.
        //
        // The rejected third option was `begin_immediate_tx`: also
        // cycle-free (it parks at BEGIN holding nothing) and snapshot-
        // consistent, but it takes the writer slot, which would serialize
        // every wave-detail read against every writer on the production
        // database too. Paying a real production cost to close a gap that
        // does not exist in production is the trade this comment exists to
        // refuse.
        //
        // COST, measured rather than asserted (#1016 review). Aggregating
        // every card and overlay into one JSON string and parsing it whole is
        // NOT free on big waves. Release build, in-memory sqlite, 30 calls
        // per point, one-statement vs. the old three-SELECT deferred tx:
        //
        //     8 cards × 2 KB payload   0.62 ms vs 0.79 ms
        //    60 cards × 20 KB payload 28.3  ms vs 11.2  ms
        //   200 cards × 8 KB payload  41.1  ms vs 17.0  ms
        //
        // Small waves — the overwhelmingly common shape — get FASTER: one
        // round trip beats three. Waves carrying large payloads (report /
        // spec cards) get 2–3.5× slower, because the row is materialized as
        // text and then parsed into the same values a second time. That is a
        // real regression on the tail, and the reason it is accepted here is
        // that the alternative shapes are worse in kind, not in degree: three
        // autocommit statements drop cross-statement consistency on every
        // deployment, and the deferred tx is the deadlock this issue exists
        // to remove. If the tail ever matters, the fix is to stop shipping
        // `payload` through the aggregate (fetch card bodies separately)
        // rather than to reopen the transaction question.
        //
        // What was tried and REVERTED, so it does not get re-tried: building
        // each element with one `printf` and splicing the stored `payload`
        // TEXT in verbatim takes ~40% off the tail (16.8 / 24.1 ms on the two
        // big fixtures). It is not worth it. A raw splice makes the array's
        // STRUCTURE depend on bytes nobody re-validates: `{}},{"id":…` in a
        // payload closes the card object and opens another one, i.e. it
        // fabricates a card, silently and with no error anywhere. Write-side
        // `json_valid` triggers (migration 0070, reverted with it) do not
        // close that — they cannot see disk corruption, a hand-edited row, or
        // a restored bad backup, and `spec_harness_wave_vcs`'s
        // `transcript_refresh_failure_from_corrupt_card_payload…` shows the
        // codebase deliberately EXERCISES a corrupt payload and expects the
        // read to fail loudly rather than degrade into structure. `json()`
        // below is constructively safe instead: sqlite parses the payload and
        // re-renders it, so corrupt text can only ever raise "malformed JSON"
        // — it can never become another card.
        //
        // `cards` / `overlays` come back as JSON arrays shaped exactly like
        // the public `Card` / `Overlay` serde representation, so they decode
        // without a second row-mirror to keep in sync. Adding a column to
        // `cards` / `overlays` means adding it here, the same audit the
        // previous explicit SELECT lists already required. Two columns need
        // an explicit fixup on the way into JSON (both pinned by
        // `wave_detail_json_shape_tests`, on the bundled sqlite 3.46.0):
        //
        //   * `deletable` — INTEGER in sqlite, `bool` in the model.
        //
        //   * `sort` — REAL in sqlite, `f64` in the model. `json_object`
        //     renders a FLOAT argument with `%!0.15g` (`jsonAppendSqlValue`
        //     in the bundled sqlite 3.46.0) and, unlike `sqlite3QuoteValue`,
        //     has NO "reparse and fall back to `%!0.20e` if it does not
        //     round-trip" branch. 15 significant digits is not enough for
        //     f64: `json_object('s', 1.0000000000000002)` yields `1.0`.
        //     Rendering through `printf('%!.17g', …)` instead — 17
        //     significant digits, the round-trip width for binary64 — and
        //     splicing the result in as a JSON number via `json()` keeps the
        //     value bit-exact. Pinned by
        //     `wave_detail_sort_precision_tests`.
        //
        //     This matters beyond a cosmetic digit: two adjacent cards would
        //     collapse onto one `sort`, the total order below would then fall
        //     through to the `id` tiebreak (i.e. the wrong order half the
        //     time), and the web client writes the value it read back to the
        //     DB when reordering cards (`WaveList.tsx`) — a silent, unlogged
        //     rewrite of persisted data.
        //
        //     (`payload` needs no such care: `json(c.payload)` re-renders the
        //     stored TEXT without reparsing its numbers into f64.)
        //
        // TEXT columns and NULL `title` need no fixup at all — `json_object`
        // escapes its TEXT arguments and renders a NULL argument as JSON
        // `null`. An empty group yields `[]`, not `null`, because
        // `json_group_array` over zero rows is an empty array.
        let row = sqlx::query_as::<_, WaveDetailRow>(
            r#"SELECT w.id, w.cove_id, w.title, w.sort, w.archived_at, w.pinned_at, w.lifecycle,
                      w.cwd, w.workflow_id, w.purpose, w.workflow_input, w.terminal_at,
                      w.created_at, w.updated_at,
                      (SELECT json_group_array(json_object(
                           'id', c.id, 'wave_id', c.wave_id, 'kind', c.kind,
                           'sort', json(printf('%!.17g', c.sort)),
                           'payload', json(c.payload), 'title', c.title,
                           'deletable', json(CASE WHEN c.deletable THEN 'true' ELSE 'false' END),
                           'created_at', c.created_at, 'updated_at', c.updated_at))
                       FROM cards c WHERE c.wave_id = w.id) AS cards_json,
                      (SELECT json_group_array(json_object(
                           'id', o.id, 'plugin_id', o.plugin_id, 'entity_kind', o.entity_kind,
                           'entity_id', o.entity_id, 'kind', o.kind, 'payload', json(o.payload),
                           'updated_at', o.updated_at))
                       FROM overlays o
                       WHERE (o.entity_kind = 'wave' AND o.entity_id = w.id)
                          OR (o.entity_kind = 'card'
                              AND o.entity_id IN
                                  (SELECT c2.id FROM cards c2 WHERE c2.wave_id = w.id)))
                          AS overlays_json
               FROM waves w WHERE w.id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };

        // ORDER (#1016 review). `json_group_array` above takes its input in
        // an ARBITRARY order — sqlite documents it as unspecified and free to
        // change between releases, and an `ORDER BY` in the subquery does not
        // constrain it (`https://www.sqlite.org/lang_aggfunc.html`). So the
        // order of both arrays as they arrive is a fact about the current
        // query plan, not about the data, and it has to be imposed here.
        //
        // The bug this replaces: `cards` was sorted by `sort` ALONE and
        // `overlays` was not sorted at all. A sort by a NON-unique key only
        // permutes within tie groups, so every card sharing a `sort` kept
        // whatever order the scan produced — and `sort` is client-assigned, so
        // ties are normal, not exotic. Today `idx_cards_wave (wave_id, sort)`
        // happens to make that scan order look reasonable; a different plan
        // unmakes it silently, with no error anywhere.
        //
        // The fix is that each comparator below is a TOTAL order — no two
        // distinct rows compare `Equal` — which is exactly the property that
        // makes the sorted result independent of the input permutation. That
        // is what turns "sqlite may reorder its aggregate input" from a
        // correctness risk into a non-event; it does NOT depend on the sort
        // being stable.
        //
        //   * cards    — `(sort, id)`. `id` is the PK, so the pair is unique.
        //     `total_cmp` orders every f64 bit pattern (NaN cannot reach here
        //     anyway: sqlite stores NaN as NULL and `cards.sort` is NOT NULL).
        //   * overlays — `(entity_kind, entity_id, plugin_id, kind)`, exactly
        //     the table's UNIQUE key, so uniqueness is DB-enforced. This is a
        //     NEW guarantee — the pre-#1016 three-SELECT shape had no ORDER BY
        //     on overlays either. Grouping an entity's overlays together beats
        //     ordering by a random uuid `id`.
        //
        // Why not an in-aggregate `ORDER BY` (sqlite >= 3.44, and the bundled
        // 3.46.0 does support it — this was measured, not assumed): it costs
        // ~28% on payload-heavy waves and cannot be indexed away.
        // `EXPLAIN QUERY PLAN` reports `USE TEMP B-TREE FOR
        // <aggregate>(ORDER BY)` even when the ORDER BY is exactly the index
        // order (`c.sort` alone against `idx_cards_wave`), i.e. 3.46 never
        // elides the sorter, so every ~20 KB element string is copied through
        // a temp b-tree. Sorting a `Vec` that is already nearly ordered is
        // far cheaper than buffering the payloads twice, and the guarantee is
        // identical because the key is total. Both orders pinned by
        // `wave_detail_order_tests`, which is RED if either key is weakened
        // to a non-unique one.
        let mut cards: Vec<Card> = serde_json::from_str(&row.cards_json)?;
        cards.sort_by(|a, b| {
            a.sort
                .total_cmp(&b.sort)
                .then_with(|| a.id.as_str().cmp(b.id.as_str()))
        });
        let mut overlays: Vec<Overlay> = serde_json::from_str(&row.overlays_json)?;
        overlays.sort_by(|a, b| {
            (&a.entity_kind, &a.entity_id, &a.plugin_id, &a.kind).cmp(&(
                &b.entity_kind,
                &b.entity_id,
                &b.plugin_id,
                &b.kind,
            ))
        });

        Ok(Some(WaveDetail {
            wave: Wave::from(row.wave),
            cards,
            overlays,
        }))
    }

    // ---------------------------------------------------------------- tasks
    async fn tasks_by_wave(&self, wave_id: &str) -> Result<Vec<Task>> {
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM tasks WHERE wave_id = ?1 \
             ORDER BY priority DESC, created_at_ms ASC, key ASC"
        );
        let rows = sqlx::query_as::<_, Task>(&sql)
            .bind(wave_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows)
    }

    async fn task_get(&self, id: &str) -> Result<Option<Task>> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1");
        let row = sqlx::query_as::<_, Task>(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row)
    }

    async fn tasks_nonterminal(&self) -> Result<Vec<Task>> {
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM tasks \
             WHERE status IN ('pending', 'dispatched', 'running', 'verifying') \
             ORDER BY wave_id ASC, priority DESC, created_at_ms ASC, key ASC"
        );
        let rows = sqlx::query_as::<_, Task>(&sql)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows)
    }

    async fn task_contexts_by_dst_wave(
        &self,
        dst_wave_id: &str,
    ) -> Result<Vec<crate::db::TaskContextRow>> {
        let rows = sqlx::query_as::<_, (String, String, Option<String>, i64)>(
            r#"SELECT DISTINCT t.id, t.wave_id, t.claim_context_json, t.context_closure_truncated
               FROM task_ref_index i
               JOIN tasks t ON t.id = i.task_id
               WHERE i.dst_wave_id = ?1
                 AND t.status IN ('dispatched','running','verifying')
                 AND t.context_stale_at_ms IS NULL
               ORDER BY t.id"#,
        )
        .bind(dst_wave_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(task_id, wave_id, claim_context_json, closure_truncated)| {
                    crate::db::TaskContextRow {
                        task_id,
                        wave_id,
                        claim_context_json,
                        closure_truncated: closure_truncated != 0,
                    }
                },
            )
            .collect())
    }

    async fn task_contexts_inflight_fresh(&self) -> Result<Vec<crate::db::TaskContextRow>> {
        let rows = sqlx::query_as::<_, (String, String, Option<String>, i64)>(
            r#"SELECT id, wave_id, claim_context_json, context_closure_truncated
               FROM tasks
               WHERE status IN ('dispatched','running','verifying')
                 AND context_stale_at_ms IS NULL
               ORDER BY id"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(task_id, wave_id, claim_context_json, closure_truncated)| {
                    crate::db::TaskContextRow {
                        task_id,
                        wave_id,
                        claim_context_json,
                        closure_truncated: closure_truncated != 0,
                    }
                },
            )
            .collect())
    }

    async fn operation_idempotency_key_by_id(&self, op_id: &str) -> Result<Option<String>> {
        let row: Option<Option<String>> =
            sqlx::query_scalar("SELECT idempotency_key FROM operations WHERE id = ?1")
                .bind(op_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.flatten())
    }

    // ---------------------------------------------------------------- cards
    async fn cards_by_wave(&self, wave_id: &str) -> Result<Vec<Card>> {
        // Keep this ORDER BY aligned with wave_vcs::cards_for_wave_tx; tests pin
        // the sort ASC, id ASC tie-break for duplicate worker run keys.
        let rows = sqlx::query_as::<_, crate::db::rows::CardRow>(
            r#"SELECT id, wave_id, kind, sort, payload, title, deletable, created_at, updated_at
               FROM cards WHERE wave_id = ?1 ORDER BY sort ASC, id ASC"#,
        )
        .bind(wave_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Card::from).collect())
    }

    async fn wave_report_cards_by_cove(&self, cove_id: &str) -> Result<Vec<Card>> {
        let rows = sqlx::query_as::<_, crate::db::rows::CardRow>(
            r#"SELECT id, wave_id, kind, sort, payload, title, deletable, created_at, updated_at
               FROM cards
               WHERE kind = 'wave-report'
                 AND wave_id IN (SELECT id FROM waves WHERE cove_id = ?1)
               ORDER BY wave_id ASC, id ASC"#,
        )
        .bind(cove_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Card::from).collect())
    }

    async fn card_get(&self, id: &str) -> Result<Option<Card>> {
        let row = sqlx::query_as::<_, crate::db::rows::CardRow>(
            r#"SELECT id, wave_id, kind, sort, payload, title, deletable, created_at, updated_at
               FROM cards WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Card::from))
    }

    async fn card_get_with_body_crdt(&self, id: &str) -> Result<Option<(Card, Option<Vec<u8>>)>> {
        #[derive(sqlx::FromRow)]
        struct CardWithCrdtRow {
            #[sqlx(flatten)]
            card: crate::db::rows::CardRow,
            body_crdt: Option<Vec<u8>>,
        }
        // Single SELECT = a self-consistent row snapshot: payload and
        // body_crdt can never tear against each other.
        let row: Option<CardWithCrdtRow> = sqlx::query_as(
            r#"SELECT id, wave_id, kind, sort, payload, title, deletable, created_at, updated_at,
                      body_crdt
               FROM cards WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| (Card::from(row.card), row.body_crdt)))
    }

    async fn task_diagnostics(
        &self,
        wave_id: &str,
        blocks: &[calm_types::wave_report::ReportBlock],
    ) -> Result<Vec<super::BlockVerdict>> {
        // No explicit transaction (#1016) — same trade as `wave_detail`, but
        // this predicate genuinely cannot collapse into one statement: it
        // loops over each declaration's references issuing a data-dependent
        // lookup per reference, and it is the SAME function the write path
        // runs inside its IMMEDIATE transaction. Folding it into one SQL
        // statement would mean either a giant generated query or forking the
        // predicate in two — and "one DB-aware schedulability predicate" is
        // worth more than a snapshot on a diagnostics render.
        //
        // What WAS collapsed is the part the verdict is computed from: policy,
        // ceiling, in-flight occupancy, in-flight keys and the wave's cove now
        // come from a single statement, so a displayed capacity /
        // schedulability can no longer be stitched together from four versions
        // of the database. Two remain — the frozen-declaration scan and the
        // per-reference lookups. Not just "stale": a task deleted or driven
        // terminal between the core snapshot and the frozen scan can yield a
        // verdict set that contradicts itself (`schedulable = true` for a key
        // whose slot the same call counted as occupied). That skew predates
        // #1016 and survives it; `evaluate_schedulability`'s doc comment spells
        // out the exact interleaving and why this diagnostics-only surface
        // tolerates it while the write path does not.
        let mut conn = self.pool.acquire().await?;
        let (declarations, local) =
            calm_types::report_blocks::tasks::project_task_declarations(blocks);
        let diagnostics =
            super::evaluate_schedulability(&mut conn, wave_id, &declarations, &local, true).await?;
        Ok(diagnostics)
    }

    async fn card_role_get(&self, id: &str) -> Result<Option<CardRole>> {
        // #679 PR1 — `CardRole` lost its `sqlx::Type` derive when it moved
        // to calm-types; decode TEXT and parse via `TryFrom<String>`.
        let row: Option<(String,)> = sqlx::query_as("SELECT role FROM cards WHERE id = ?1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|(role,)| {
            CardRole::try_from(role)
                .map_err(|e| CalmError::Internal(format!("cards.role decode: {e}")))
        })
        .transpose()
    }

    async fn harness_item_list_by_card(
        &self,
        card_id: &str,
        after_id: i64,
        limit: i64,
        descending: bool,
    ) -> Result<Vec<HarnessItem>> {
        let (sql, cursor) = if descending {
            (
                r#"SELECT id, runtime_id, card_id, wave_id, thread_id, turn_id,
                          item_uuid, item_type, method, params, created_at_ms
                   FROM harness_items
                   WHERE card_id = ?1 AND id < ?2
                   ORDER BY id DESC
                   LIMIT ?3"#,
                if after_id == 0 { i64::MAX } else { after_id },
            )
        } else {
            (
                r#"SELECT id, runtime_id, card_id, wave_id, thread_id, turn_id,
                          item_uuid, item_type, method, params, created_at_ms
                   FROM harness_items
                   WHERE card_id = ?1 AND id > ?2
                   ORDER BY id ASC
                   LIMIT ?3"#,
                after_id,
            )
        };
        let mut rows = sqlx::query_as::<_, crate::db::rows::HarnessItemRow>(sql)
            .bind(card_id)
            .bind(cursor)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        if descending {
            rows.reverse();
        }
        Ok(rows.into_iter().map(HarnessItem::from).collect())
    }

    async fn worker_flow_item_list_by_card(
        &self,
        card_id: &str,
        after_id: i64,
        limit: i64,
        descending: bool,
    ) -> Result<Vec<crate::db::rows::WorkerFlowItemRow>> {
        // Clamp the page size to a defensible ceiling so a caller passing a
        // huge (or non-positive) limit cannot scan the whole table.
        let limit = limit.clamp(1, 500);
        let (sql, cursor) = if descending {
            (
                r#"SELECT id, card_id, runtime_id, wave_id, worker_session_id,
                          kind, payload, created_at_ms
                   FROM worker_flow_items
                   WHERE card_id = ?1 AND id < ?2
                   ORDER BY id DESC
                   LIMIT ?3"#,
                if after_id == 0 { i64::MAX } else { after_id },
            )
        } else {
            (
                r#"SELECT id, card_id, runtime_id, wave_id, worker_session_id,
                          kind, payload, created_at_ms
                   FROM worker_flow_items
                   WHERE card_id = ?1 AND id > ?2
                   ORDER BY id ASC
                   LIMIT ?3"#,
                after_id,
            )
        };
        let mut rows = sqlx::query_as::<_, crate::db::rows::WorkerFlowItemRow>(sql)
            .bind(card_id)
            .bind(cursor)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        if descending {
            rows.reverse();
        }
        Ok(rows)
    }

    async fn worker_flow_cursor_get(
        &self,
        card_id: &str,
        source_kind: &str,
    ) -> Result<Option<crate::db::rows::WorkerFlowCursor>> {
        let row = sqlx::query_as::<_, crate::db::rows::WorkerFlowCursor>(
            r#"SELECT card_id, source_kind, source_path, record_index,
                      byte_offset, last_source_uuid, last_line_hash, updated_at_ms
               FROM worker_flow_cursors
               WHERE card_id = ?1 AND source_kind = ?2"#,
        )
        .bind(card_id)
        .bind(source_kind)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn shared_daemon_runtime_get(&self) -> Result<SharedCodexDaemonRecord> {
        let row = sqlx::query_as::<
            _,
            (
                String,
                Option<i32>,
                Option<i32>,
                Option<String>,
                Option<String>,
                Option<i64>,
                Option<String>,
                Option<i64>,
                i64,
                i64,
                Option<String>,
                Option<String>,
            ),
        >(
            r#"SELECT state, pid, pgid, sock_path, codex_home_path, process_start_time,
                      boot_id, started_at, updated_at, restart_count, last_error,
                      daemon_env_signature
               FROM shared_codex_daemon
               WHERE id = 1"#,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(SharedCodexDaemonRecord {
            state: row.0,
            pid: row.1,
            pgid: row.2,
            sock_path: row.3,
            codex_home_path: row.4,
            process_start_time: row.5.and_then(|v| u64::try_from(v).ok()),
            boot_id: row.6,
            started_at: row.7,
            updated_at: row.8,
            restart_count: row.9,
            last_error: row.10,
            daemon_env_signature: row.11,
        })
    }

    // -------------------------------------------------------------- overlays
    async fn overlays_for(&self, entity_kind: &str, entity_id: &str) -> Result<Vec<Overlay>> {
        let rows = sqlx::query_as::<_, crate::db::rows::OverlayRow>(
            r#"SELECT id, plugin_id, entity_kind, entity_id, kind, payload, updated_at
               FROM overlays WHERE entity_kind = ?1 AND entity_id = ?2"#,
        )
        .bind(entity_kind)
        .bind(entity_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Overlay::from).collect())
    }

    async fn overlays_by_kind(&self, entity_kind: &str) -> Result<Vec<Overlay>> {
        let rows = sqlx::query_as::<_, crate::db::rows::OverlayRow>(
            r#"SELECT id, plugin_id, entity_kind, entity_id, kind, payload, updated_at
               FROM overlays WHERE entity_kind = ?1"#,
        )
        .bind(entity_kind)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Overlay::from).collect())
    }

    // ------------------------------------------------------------- terminals
    async fn terminal_get(&self, id: &str) -> Result<Option<Terminal>> {
        let row = sqlx::query_as::<_, Terminal>(
            r#"SELECT id, card_id, program, cwd, env, pid,
                      theme_fg, theme_bg, exit_code, signal_killed, created_at
               FROM terminals WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn terminal_get_by_card(&self, card_id: &str) -> Result<Option<Terminal>> {
        let row = sqlx::query_as::<_, Terminal>(
            r#"SELECT id, card_id, program, cwd, env, pid,
                      theme_fg, theme_bg, exit_code, signal_killed, created_at
               FROM terminals WHERE card_id = ?1"#,
        )
        .bind(card_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn terminals_orphaned(&self, grace_seconds: i64) -> Result<Vec<Terminal>> {
        // Orphan: this terminal's card has no active worker_session, AND the row
        // was created more than `grace_seconds` ago.
        //
        // `created_at` is unix ms; the grace bound is `now_ms - grace_seconds * 1000`.
        let cutoff = now_ms() - grace_seconds.saturating_mul(1000);
        let rows = sqlx::query_as::<_, Terminal>(
            r#"SELECT t.id, t.card_id, t.program, t.cwd, t.env,
                      t.pid,
                      t.theme_fg, t.theme_bg,
                      t.exit_code, t.signal_killed,
                      t.created_at
               FROM terminals t
               WHERE NOT EXISTS (
                   SELECT 1 FROM worker_sessions ws
                   WHERE ws.card_id = t.card_id
                     AND ws.state IN ('starting', 'running', 'idle', 'turn_pending')
               )
               AND t.created_at < ?1"#,
        )
        .bind(cutoff)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn terminals_running(&self) -> Result<Vec<Terminal>> {
        let rows = sqlx::query_as::<_, Terminal>(
            r#"SELECT id, card_id, program, cwd, env,
                      pid,
                      theme_fg, theme_bg,
                      exit_code, signal_killed,
                      created_at
               FROM terminals
               WHERE exit_code IS NULL AND signal_killed = 0"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn shared_spec_cards_for_initial_prompt_takeover(
        &self,
    ) -> Result<Vec<(String, String, String, i64)>> {
        let (provider, _mode, contract) = derive_session_identity(&WorkerSessionKind::SharedSpec);
        // Join `terminals` and require a LIVE row so a card whose TUI was
        // already reaped (reconcile_supervisor_on_boot marked it exited,
        // or a SIGKILL set signal_killed=1) is NOT re-registered into the
        // pending FIFO. A dead TUI can never emit thread/started, so
        // re-registering would leave the entry stranded until TTL expiry
        // — and worse, the entry would absorb a later thread/started
        // attribution intended for a different empty card (until
        // on_thread_started's stale-front-drop catches it). This was the
        // R7 P2 #1 followup; CI reproduced it because the terminal gets
        // reaped before the next boot's takeover query runs.
        let rows: Vec<(String, String, String, i64)> = sqlx::query_as(
            r#"SELECT c.id,
                      c.wave_id,
                      ws.terminal_run_id,
                      0
               FROM cards c
               JOIN waves w ON w.id = c.wave_id
               JOIN worker_sessions ws ON ws.id = c.session_id
                   AND ws.provider = ?1
                   AND ws.contract = ?2
                   AND ws.thread_id IS NULL
                   AND ws.state IN ('starting','running','idle','turn_pending')
               JOIN terminals t ON t.id = ws.terminal_run_id
               WHERE c.role = 'spec'
                 AND t.exit_code IS NULL
                 AND COALESCE(t.signal_killed, 0) = 0
                 AND NOT EXISTS (
                       SELECT 1
                         FROM worker_sessions hws
                         JOIN cards hc ON hc.session_id = hws.id
                        WHERE hc.id = c.id
                          AND hws.provider = ?3
                          AND hws.contract = ?4
                          AND hws.state IN ('starting','running','idle','turn_pending')
                          AND hws.handle_state_json IS NOT NULL
                          AND json_extract(hws.handle_state_json, '$.mode') = 'harness'
                 )
                 AND w.lifecycle NOT IN ('done', 'canceled', 'failed')
               ORDER BY c.created_at ASC, c.id ASC"#,
        )
        .bind(provider.as_db_str())
        .bind(contract.as_db_str())
        .bind(provider.as_db_str())
        .bind(contract.as_db_str())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    // --------------------------------------------------------------- plugins
    async fn plugins_list(&self) -> Result<Vec<Plugin>> {
        self.plugins_list_all().await
    }

    async fn plugins_list_all(&self) -> Result<Vec<Plugin>> {
        let rows = sqlx::query_as::<_, Plugin>(
            r#"SELECT id, version, install_path, manifest, enabled, user_config,
                      installed_at, updated_at
               FROM plugins
               ORDER BY id ASC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn plugin_get_by_id(&self, id: &str) -> Result<Option<Plugin>> {
        let row = sqlx::query_as::<_, Plugin>(
            r#"SELECT id, version, install_path, manifest, enabled, user_config,
                      installed_at, updated_at
               FROM plugins WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn plugin_token_get(&self, plugin_id: &str) -> Result<Option<(String, i64)>> {
        let row: Option<(String, i64)> = sqlx::query_as(
            r#"SELECT hashed_token, expires_at FROM plugin_tokens WHERE plugin_id = ?1"#,
        )
        .bind(plugin_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn plugin_kv_get(&self, plugin_id: &str, key: &str) -> Result<Option<serde_json::Value>> {
        let row: Option<(String,)> =
            sqlx::query_as(r#"SELECT value FROM plugin_kv WHERE plugin_id = ?1 AND key = ?2"#)
                .bind(plugin_id)
                .bind(key)
                .fetch_optional(&self.pool)
                .await?;
        match row {
            Some((text,)) => Ok(Some(serde_json::from_str(&text)?)),
            None => Ok(None),
        }
    }

    async fn plugin_kv_list(
        &self,
        plugin_id: &str,
        prefix: &str,
    ) -> Result<Vec<(String, serde_json::Value)>> {
        let mut escaped = String::with_capacity(prefix.len() + 2);
        for ch in prefix.chars() {
            if ch == '%' || ch == '_' || ch == '\\' {
                escaped.push('\\');
            }
            escaped.push(ch);
        }
        escaped.push('%');
        let rows: Vec<(String, String)> = sqlx::query_as(
            r#"SELECT key, value FROM plugin_kv
               WHERE plugin_id = ?1 AND key LIKE ?2 ESCAPE '\'
               ORDER BY key ASC"#,
        )
        .bind(plugin_id)
        .bind(&escaped)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for (k, v) in rows {
            out.push((k, serde_json::from_str(&v)?));
        }
        Ok(out)
    }

    // -------------------------------------------------------------- settings
    async fn settings_get_all(&self) -> Result<Vec<(String, String)>> {
        let rows: Vec<(String, String)> =
            sqlx::query_as(r#"SELECT key, value FROM settings ORDER BY key ASC"#)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows)
    }

    // ------------------------------------------------------------ role cache
    async fn seed_card_role_cache(&self, cache: &CardRoleCache) -> Result<()> {
        cache.seed_from_db(&self.pool).await
    }

    // ------------------------------------------------------- wave-cove cache
    async fn seed_wave_cove_cache(&self, cache: &WaveCoveCache) -> Result<()> {
        cache.seed_from_db(&self.pool).await
    }

    // ----------------------------------------------------------- mcp tokens
    async fn card_mcp_token_lookup_by_hash(
        &self,
        hashed_token: &str,
    ) -> Result<Option<(String, String)>> {
        // PR7a.1 (#136 followup) — return `(card_id, hashed_token)` so
        // the handshake can run a constant-time compare on the stored
        // hash. The `WHERE` clause already filtered on the hash, so the
        // returned column is the same value the caller passed in; we
        // still echo it back rather than hand off the input — that way
        // a future migration that changes column storage (e.g. hex →
        // bytes) doesn't break the contract silently.
        let row: Option<(String, String)> = sqlx::query_as(
            r#"SELECT card_id, hashed_token FROM card_mcp_tokens WHERE hashed_token = ?1"#,
        )
        .bind(hashed_token)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn card_identity_get_by_session(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionCardIdentity>> {
        let rows = sqlx::query(
            r#"SELECT c.id, c.role, c.wave_id, w.cove_id
               FROM cards c
               JOIN waves w ON w.id = c.wave_id
              WHERE c.session_id = ?1
              ORDER BY c.updated_at DESC, c.created_at DESC, c.id DESC
              LIMIT 2"#,
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;
        match rows.as_slice() {
            [] => Ok(None),
            [row] => {
                let role = CardRole::try_from(row.try_get::<String, _>("role")?)
                    .map_err(|e| CalmError::Internal(format!("cards.role decode: {e}")))?;
                Ok(Some(SessionCardIdentity {
                    card_id: CardId(row.try_get("id")?),
                    role,
                    wave_id: WaveId(row.try_get("wave_id")?),
                    cove_id: CoveId(row.try_get("cove_id")?),
                }))
            }
            _ => Err(CalmError::Internal(format!(
                "multiple cards linked to worker session {session_id}"
            ))),
        }
    }

    async fn workspace_lease_for_card(&self, card_id: &str) -> Result<Option<WorkspaceLease>> {
        let row = sqlx::query(
            r#"SELECT lease_id, card_id, wave_id, path, state
               FROM workspace_leases
               WHERE card_id = ?1
                 AND state = 'held'
               ORDER BY created_at_ms DESC, lease_id DESC
               LIMIT 1"#,
        )
        .bind(card_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(WorkspaceLease {
                lease_id: row.try_get("lease_id")?,
                card_id: row.try_get("card_id")?,
                wave_id: row.try_get("wave_id")?,
                path: row.try_get("path")?,
                state: row.try_get("state")?,
            })
        })
        .transpose()
    }

    async fn session_get_by_active_token_hash(
        &self,
        hashed_token: &str,
    ) -> Result<Option<WorkerSession>> {
        session_get_by_active_token_hash(&self.pool, hashed_token).await
    }

    async fn session_get_by_id(&self, id: &WorkerSessionId) -> Result<Option<WorkerSession>> {
        session_get_by_id(&self.pool, id).await
    }

    async fn card_mcp_token_exists_for_card(&self, card_id: &str) -> Result<bool> {
        let row: Option<(i64,)> =
            sqlx::query_as(r#"SELECT 1 FROM card_mcp_tokens WHERE card_id = ?1 LIMIT 1"#)
                .bind(card_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.is_some())
    }
}
