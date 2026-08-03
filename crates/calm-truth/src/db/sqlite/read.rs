use async_trait::async_trait;
use sqlx::Row;

use super::task::TASK_COLUMNS;
use super::{
    SqlxRepo, derive_session_identity, session_get_by_active_token_hash, session_get_by_id,
};
use crate::card_role_cache::CardRoleCache;
use crate::db::{RepoRead, SessionCardIdentity, SharedCodexDaemonRecord, WorkspaceLease};
use crate::error::{CalmError, Result};
use crate::ids::{CardId, CoveId, WaveId};
use crate::model::*;
use crate::session_projection_repo::WorkerSessionKind;
use crate::wave_cove_cache::WaveCoveCache;
use calm_types::worker::{WorkerSession, WorkerSessionId};

/// Row shape of the single-statement `wave_detail` read (#1016).
///
/// The wave columns decode through the usual [`crate::db::rows::WaveRow`]
/// mirror; `cards` and `overlays` ride along as JSON arrays built by
/// `group_concat` so that all three come from ONE implicit transaction
/// without the row multiplication a join would cause (a wave-scoped overlay
/// would pair with every card).
#[derive(sqlx::FromRow)]
struct WaveDetailRow {
    #[sqlx(flatten)]
    wave: crate::db::rows::WaveRow,
    cards_json: String,
    overlays_json: String,
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
        // not free on big waves, and the first cut of this statement built
        // the array with `json_group_array(json_object(…))`, which made
        // sqlite parse every `payload` into JSONB and render it back to text
        // before the row even left the database. Release build, in-memory
        // sqlite, 30 calls per point:
        //
        //                          json_object   printf   3 SELECTs
        //     8 cards × 2 KB           0.62 ms   0.51 ms    0.79 ms
        //    60 cards × 20 KB         28.3  ms  16.8  ms   11.2  ms
        //   200 cards × 8 KB          41.1  ms  24.1  ms   17.0  ms
        //
        // (Allocation is unchanged by the switch — 8.8 / 11.8 MiB per call on
        // the two big fixtures either way, against 7.6 / 10.2 for three
        // SELECTs: the extra copy is the aggregate blob itself.)
        //
        // Assembling each element with ONE `printf` and splicing the stored
        // `payload` TEXT in verbatim takes ~40% off the tail: sqlite copies
        // the bytes instead of parsing and re-rendering them, and the
        // left-associative `||` chain that would copy the growing element
        // string once per operand never happens. Two shapes that look like
        // they should help were measured and do NOT, so don't re-try them:
        // shipping `payload` as an escaped JSON *string* (`json_quote`, or
        // `json_object` with a bare TEXT argument) only moves the parse to
        // Rust and pays an escape + unescape round trip on top — 21.5 /
        // 32.6 ms on the two big fixtures; and joining `cards` as rows
        // instead of aggregating re-evaluates the correlated `overlays`
        // subquery once per card — 37.8 / 157.4 ms.
        //
        // What remains is ~1.6× the three-SELECT shape, whose fetch is
        // cheaper only because each `payload` crosses the boundary on its
        // own instead of inside one big blob. That is the price of the
        // snapshot this statement exists to provide, and small waves — the
        // overwhelmingly common shape — are still FASTER than three round
        // trips.
        //
        // `cards` / `overlays` are built to be exactly the public `Card` /
        // `Overlay` serde representation, so they decode without a second
        // row-mirror to keep in sync. Adding a column to `cards` /
        // `overlays` means adding a field to the format string AND its
        // argument here, the same audit the explicit SELECT lists already
        // required. What each conversion depends on (all pinned by
        // `wave_detail_json_shape_tests`, on the bundled sqlite 3.46.0):
        //
        //   * every TEXT column goes through `json_quote`, which emits a
        //     JSON string literal with quotes, backslashes, newlines and
        //     control characters escaped, and `null` for a NULL column
        //     (`title`). It is a no-op only on values carrying sqlite's JSON
        //     subtype, which a plain column reference never does.
        //
        //   * `deletable` — INTEGER in sqlite, `bool` in the model, so a
        //     `CASE` renders the JSON keyword.
        //
        //   * `sort` — REAL in sqlite, `f64` in the model, rendered with
        //     `%!.17g`: 17 significant digits, the round-trip width for
        //     binary64. `json_object` would have rendered it `%!0.15g`
        //     (`jsonAppendSqlValue`) with no round-trip fallback, silently
        //     turning `1.0000000000000002` into `1.0`. Nothing errors when
        //     that happens: `total_cmp` below then orders two neighbouring
        //     cards wrong, and the web client writes back what it read on
        //     the next reorder (`WaveList.tsx`) — an unlogged rewrite of
        //     persisted data. Pinned by `wave_detail_sort_precision_tests`.
        //
        //   * `payload` is spliced RAW, which requires the column to hold
        //     valid JSON text — and unlike the `json(c.payload)` this
        //     replaced, the read no longer checks. Text that is not a single
        //     JSON value would not merely garble the array: `{}},{"id":…`
        //     closes the card object and opens another one, i.e. it could
        //     fabricate a card. Gating the splice on `json_valid` in the
        //     query gives most of the win back (24.8 / 34.8 ms — barely
        //     better than `json_object`), so the check lives on the write
        //     side instead: migration 0070 adds BEFORE INSERT / UPDATE
        //     triggers that ABORT a payload `json_valid` rejects, on both
        //     tables, the same defense-in-depth shape as
        //     `cards_role_validate_*`. Pinned by
        //     `wave_detail_json_shape_tests::payload_write_of_invalid_json_is_refused`.
        //
        //   * `printf('[%s]', …)` over an empty group renders `[]`, because
        //     `group_concat` of no rows is NULL and `%s` renders NULL as the
        //     empty string. Pinned by the empty-wave case.
        let row = sqlx::query_as::<_, WaveDetailRow>(
            r#"SELECT w.id, w.cove_id, w.title, w.sort, w.archived_at, w.pinned_at, w.lifecycle,
                      w.cwd, w.workflow_id, w.purpose, w.workflow_input, w.terminal_at,
                      w.created_at, w.updated_at,
                      (SELECT printf('[%s]', group_concat(printf(
                           '{"id":%s,"wave_id":%s,"kind":%s,"sort":%!.17g,"payload":%s,"title":%s,"deletable":%s,"created_at":%d,"updated_at":%d}',
                           json_quote(c.id), json_quote(c.wave_id), json_quote(c.kind), c.sort,
                           c.payload, json_quote(c.title),
                           CASE WHEN c.deletable THEN 'true' ELSE 'false' END,
                           c.created_at, c.updated_at)))
                       FROM cards c WHERE c.wave_id = w.id) AS cards_json,
                      (SELECT printf('[%s]', group_concat(printf(
                           '{"id":%s,"plugin_id":%s,"entity_kind":%s,"entity_id":%s,"kind":%s,"payload":%s,"updated_at":%d}',
                           json_quote(o.id), json_quote(o.plugin_id), json_quote(o.entity_kind),
                           json_quote(o.entity_id), json_quote(o.kind), o.payload,
                           o.updated_at)))
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

        let mut cards: Vec<Card> = serde_json::from_str(&row.cards_json)?;
        // `ORDER BY sort ASC` used to sit in the cards SELECT; an aggregate
        // ORDER BY would pin us to sqlite >= 3.44, and sorting here is
        // equivalent (stable, so ties keep scan order).
        cards.sort_by(|a, b| a.sort.total_cmp(&b.sort));
        let overlays: Vec<Overlay> = serde_json::from_str(&row.overlays_json)?;

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
            super::evaluate_schedulability(&mut conn, wave_id, &declarations, &local).await?;
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
