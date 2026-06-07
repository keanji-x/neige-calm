//! SQLite-backed `Repo` implementation. **Owned by Track A.**
//!
//! Implements every method on the `Repo` trait against a `sqlx::SqlitePool`.
//! The pool is opened with `PRAGMA foreign_keys = ON` per-connection, the
//! bundled migrations under `migrations/` are run on `open()`, and every
//! observable behavior of `MockRepo` (cascades, sort defaulting, not-found
//! semantics, overlay upsert by unique key) is replicated here.
//!
//! ## Sync engine — internal layout
//!
//! Every entity write the trait exposes (`cove_create`, `wave_update`,
//! `card_create`, ...) is implemented as a thin wrapper around a `_tx`-
//! suffixed free function that takes `&mut Transaction<'_, Sqlite>` and
//! does the actual SQL. The wrappers each open their own one-shot
//! transaction (the existing single-call semantics), but the `_tx`
//! functions can also be **composed inside** `Repo::write_with_event`'s
//! closure so the entity write and the `INSERT INTO events ...` run in
//! the same transaction. See `db::mod`'s sync-engine comment.

use std::collections::HashMap;
use std::str::FromStr;

use async_trait::async_trait;
use futures::future::BoxFuture;
use sqlx::ConnectOptions;
use sqlx::Executor;
use sqlx::QueryBuilder;
use sqlx::Row;
use sqlx::Sqlite;
use sqlx::SqlitePool;
use sqlx::Transaction;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::{
    CardCodexThreadRow, Repo, RepoEventWrite, RepoOutOfDomain, RepoRead, RepoSyncDomainRaw,
    SharedCodexDaemonRecord, SharedCodexDaemonUpdate, WaveEvent, WriteInTxFn, WriteWithEventFn,
    WriteWithEventsFn,
};
use crate::card_kind::validate_card_kind_global;
use crate::card_role_cache::CardRoleCache;
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, EventBus, EventScope, SYNC_EVENT_VERSION};
use crate::ids::{ActorId, WaveId};
use crate::model::*;
use crate::runtime_repo::{
    AgentProvider, CardId as RuntimeCardId, CardRuntime, Result as RuntimeResult, RunStatus,
    RuntimeId, RuntimeInit, RuntimeKind, RuntimeRepo, RuntimeRepoError, ThreadAttribution,
    Tx as RuntimeTx,
};
use crate::validation::{
    CLAUDE_PAYLOAD_SCHEMA_VERSION, CODEX_PAYLOAD_SCHEMA_VERSION, TERMINAL_PAYLOAD_SCHEMA_VERSION,
};
use crate::wave_cove_cache::WaveCoveCache;

pub struct SqlxRepo {
    pool: SqlitePool,
    /// PR3 (#136) — write-through role cache local to the repo so the
    /// gated `RepoSyncDomainRaw` trait methods (`card_create` /
    /// `card_delete`) can call the `_tx` helpers without every test
    /// fixture having to hand a cache in. Production writes go through
    /// `AppState::card_role_cache` — a separate `Arc<DashMap<…>>`
    /// instance also kept in sync via the `_tx` helpers when the
    /// production `write_with_event` path runs. Both caches converge
    /// on whatever the `cards` table holds, since `seed_from_db`
    /// fully repopulates from sqlite. The duplication is intentional:
    /// `enforce_role` only ever consults the cache passed in at the
    /// call site, so AppState's view stays authoritative for
    /// production while the repo-local view backs the test-only raw
    /// path.
    card_role_cache: CardRoleCache,
    /// #234 — write-through `WaveId -> CoveId` cache, same rationale as
    /// `card_role_cache` above: the raw `RepoSyncDomainRaw` wave write
    /// paths (`wave_create` / `wave_delete`) keep this in sync via the
    /// `_tx` helpers, while production `write_with_event` callers thread
    /// `AppState::wave_cove_cache` (a separate instance that
    /// `AppState::new` seeds from the same pool). Both converge on
    /// the persisted `waves` table.
    wave_cove_cache: WaveCoveCache,
}

impl SqlxRepo {
    /// Open / create the SQLite DB at `url`, run pending migrations, and
    /// enable foreign-key enforcement per-connection.
    ///
    /// Accepts both `sqlite::memory:` (used in tests) and on-disk
    /// `sqlite://path?mode=rwc` URLs.
    pub async fn open(url: &str) -> Result<Self> {
        let mut opts = SqliteConnectOptions::from_str(url)
            .map_err(|e| CalmError::Internal(format!("invalid sqlite url {url:?}: {e}")))?
            .create_if_missing(true)
            .foreign_keys(true);
        // Reduce noise from sqlx's per-statement logging at info; keep debug.
        opts = opts.log_statements(tracing::log::LevelFilter::Debug);

        let pool = SqlitePoolOptions::new()
            // Belt-and-braces: also re-issue the pragmas on every fresh
            // connection in case connect options are silently dropped for
            // some URL forms (e.g. memory).
            .after_connect(|conn, _meta| {
                Box::pin(async move {
                    conn.execute("PRAGMA foreign_keys = ON;").await?;
                    conn.execute("PRAGMA busy_timeout = 5000;").await?;
                    conn.execute("PRAGMA journal_mode = WAL;").await?;
                    Ok(())
                })
            })
            .connect_with(opts)
            .await?;

        let migrator = sqlx::migrate!("./migrations");

        // Tier-A upgrade stability boundary (`docs/upgrade-stability.md`):
        // refuse to boot when the DB carries a migration row that this
        // binary doesn't know about. Downgrade is unsupported — an older
        // binary opening a newer DB must fail loudly here rather than
        // continue against a schema it can't reason about. sqlx 0.8.x's
        // own `run()` would also refuse (via `MigrateError::VersionMissing`
        // unless `set_ignore_missing(true)` is set), but we check first so
        // (a) the error message wording is owned by us, not sqlx, and (b)
        // sqlx never gets a chance to apply any pending known migration
        // before we've rejected the open.
        check_no_unknown_future_migrations(&pool, &migrator).await?;

        migrator
            .run(&pool)
            .await
            .map_err(|e| CalmError::Internal(format!("migrate: {e}")))?;

        // PR3 (#136): seed the repo-local role cache from the freshly-
        // migrated table. This is the backing store for the gated raw
        // path's `card_create_tx` / `card_delete_tx` calls; the
        // production write path uses `AppState::card_role_cache`,
        // which `AppState::new` re-seeds from the same pool.
        let card_role_cache = CardRoleCache::new();
        card_role_cache.seed_from_db(&pool).await?;
        let wave_cove_cache = WaveCoveCache::new();
        wave_cove_cache.seed_from_db(&pool).await?;

        Ok(Self {
            pool,
            card_role_cache,
            wave_cove_cache,
        })
    }

    /// Direct access to the pool for tests / fixtures / sync-engine
    /// integration tests that need to `SELECT` from the `events` table
    /// outside the `Repo` trait surface.
    ///
    /// Marked `#[doc(hidden)]` because production code must go through
    /// the trait (so a future swap to a non-sqlite backend stays
    /// possible). Integration tests under `tests/` need real access for
    /// replay / atomicity assertions; that's what this surface is for.
    #[doc(hidden)]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// PR3 (#136) — borrow the repo's role cache. `AppState::new` clones
    /// this into its own field so the production write path's `enforce_role`
    /// lookup sees the same map as the repo's `_tx` write-through.
    /// `CardRoleCache: Clone` is cheap (`Arc<DashMap<…>>` under the hood).
    pub fn card_role_cache(&self) -> &CardRoleCache {
        &self.card_role_cache
    }

    /// #234 — borrow the repo's wave→cove cache. Mirrors
    /// [`card_role_cache`](Self::card_role_cache). `AppState::new`
    /// re-seeds its own clone from the same pool.
    pub fn wave_cove_cache(&self) -> &WaveCoveCache {
        &self.wave_cove_cache
    }

    /// **Private.** The raw events-table insert. Lives off the trait per
    /// design doc §1.4: only `Repo::write_with_event` and
    /// `Repo::log_pure_event` may reach this path, so the commit-then-emit
    /// invariant is unbypassable from the route / plugin host layers.
    ///
    /// Returns the auto-incremented row id, which is then stamped onto
    /// the `BroadcastEnvelope` the wrapper emits on the bus.
    ///
    /// PR2 of #136:
    ///   * `actor` is typed [`ActorId`] and stored as `serde_json::to_string(&actor)`
    ///     in the `events.actor` TEXT column (forward-compatible with future
    ///     actor enrichment).
    ///   * `scope` is decomposed into the four `events.scope_*` columns added
    ///     in migration 0007. `EventScope::System` writes `scope_kind='system'`
    ///     with NULL ancestor cols; the other variants populate whatever
    ///     prefix of the cove → wave → card chain they carry.
    async fn event_append_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        actor: &ActorId,
        scope: &EventScope,
        correlation: Option<&str>,
        event: &Event,
    ) -> Result<i64> {
        let kind = event.kind_tag();
        let payload = event.payload_value();
        let payload_text = serde_json::to_string(&payload)?;
        let actor_text = serde_json::to_string(actor)?;
        let at = now_ms();
        let scope_kind = scope.kind();
        let scope_cove = scope.cove_id().map(|c| c.as_str());
        let scope_wave = scope.wave_id().map(|w| w.as_str());
        let scope_card = scope.card_id().map(|c| c.as_str());
        let row = sqlx::query(
            r#"INSERT INTO events (
                   kind, payload, actor, at, correlation, event_version,
                   scope_kind, scope_cove, scope_wave, scope_card
               )
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
               RETURNING id"#,
        )
        .bind(kind)
        .bind(&payload_text)
        .bind(&actor_text)
        .bind(at)
        .bind(correlation)
        .bind(SYNC_EVENT_VERSION)
        .bind(scope_kind)
        .bind(scope_cove)
        .bind(scope_wave)
        .bind(scope_card)
        .fetch_one(&mut **tx)
        .await?;
        let id: i64 = row.try_get("id")?;
        Ok(id)
    }

    /// `#[cfg(test)]`-gated raw appender for fixture seeding / replay
    /// loaders. Bypasses the wrapper deliberately so test scaffolds can
    /// reconstruct an event stream verbatim (id-stamped) without driving
    /// the full handler stack.
    #[cfg(test)]
    pub async fn event_append_fixture(
        &self,
        actor: ActorId,
        scope: EventScope,
        correlation: Option<&str>,
        event: &Event,
    ) -> Result<i64> {
        let mut tx = self.pool.begin().await?;
        let id = Self::event_append_in_tx(&mut tx, &actor, &scope, correlation, event).await?;
        tx.commit().await?;
        Ok(id)
    }
}

impl Repo for SqlxRepo {
    fn sqlite_pool(&self) -> Option<SqlitePool> {
        Some(self.pool.clone())
    }
}

pub(crate) async fn event_append_for_operation_tx(
    tx: &mut Transaction<'_, Sqlite>,
    actor: &ActorId,
    scope: &EventScope,
    correlation: Option<&str>,
    event: &Event,
) -> Result<i64> {
    SqlxRepo::event_append_in_tx(tx, actor, scope, correlation, event).await
}

// ---- helpers -----------------------------------------------------------------

/// Tier-A upgrade stability guard: refuse to boot when `_sqlx_migrations`
/// contains a `version` not known to the binary's embedded `Migrator`.
///
/// This is the "old binary reading new DB" case from
/// `docs/upgrade-stability.md`. Downgrade is unsupported — once the user's
/// data has been migrated forward, an older binary must not continue
/// against a schema it can't reason about.
///
/// Behavior:
///
/// * The `_sqlx_migrations` table may not exist on a brand-new DB (sqlx
///   creates it on first `run()`). Treat absence as "no applied migrations
///   yet" — opens normally.
/// * `success = false` rows are still included in the diff: their `version`
///   is what matters for "binary doesn't know about". A half-applied future
///   migration is still a future migration.
/// * If multiple unknown versions exist, the error names the lowest one
///   (most useful for debugging: it's the first row that drifted past
///   what this binary knows). The remaining unknown versions are appended
///   in parentheses so operators can see the full extent of the drift.
async fn check_no_unknown_future_migrations(
    pool: &SqlitePool,
    migrator: &sqlx::migrate::Migrator,
) -> Result<()> {
    // Does `_sqlx_migrations` exist? `sqlite_master` is always present.
    // We pre-check existence rather than catching the "no such table"
    // error so we don't conflate it with a real driver failure.
    let table_exists: Option<(String,)> = sqlx::query_as(
        r#"SELECT name FROM sqlite_master
           WHERE type = 'table' AND name = '_sqlx_migrations'"#,
    )
    .fetch_optional(pool)
    .await?;
    if table_exists.is_none() {
        return Ok(());
    }

    let applied: Vec<(i64,)> =
        sqlx::query_as(r#"SELECT version FROM _sqlx_migrations ORDER BY version ASC"#)
            .fetch_all(pool)
            .await?;

    let known: std::collections::HashSet<i64> = migrator.iter().map(|m| m.version).collect();
    let mut unknown: Vec<i64> = applied
        .into_iter()
        .map(|(v,)| v)
        .filter(|v| !known.contains(v))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    unknown.sort_unstable();
    let lowest = unknown[0];
    let detail = if unknown.len() == 1 {
        String::new()
    } else {
        // List the remaining unknown versions so operators can see the
        // full forward-drift surface without grepping the DB themselves.
        let rest: Vec<String> = unknown[1..].iter().map(|v| v.to_string()).collect();
        format!(" (additional unknown versions: {})", rest.join(", "))
    };
    Err(CalmError::Internal(format!(
        "database has migration {lowest} applied that this binary doesn't know about \
         — refusing to boot; downgrade is not supported{detail}"
    )))
}

/// Compute the next sort value (max + 1) within a scoped table.
///
/// `scope_sql` is appended verbatim after `FROM <table>`; supply `""` for
/// global scope, or `"WHERE cove_id = ?1"` etc. Bind a single optional
/// scope parameter via `scope_id`.
async fn next_sort_scoped_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    table: &str,
    scope_sql: &str,
    scope_id: Option<&str>,
) -> Result<f64> {
    let sql = format!("SELECT COALESCE(MAX(sort), 0.0) + 1.0 AS s FROM {table} {scope_sql}");
    let mut q = sqlx::query(&sql);
    if let Some(id) = scope_id {
        q = q.bind(id);
    }
    let row = q.fetch_one(&mut **tx).await?;
    Ok(row.try_get::<f64, _>("s")?)
}

// ---------------------------------------------------------------------------
// `_tx` helpers — composable inside `Repo::write_with_event` closures.
// ---------------------------------------------------------------------------

pub async fn cove_create_tx(tx: &mut Transaction<'_, Sqlite>, p: NewCove) -> Result<Cove> {
    let sort = match p.sort {
        Some(s) => s,
        None => next_sort_scoped_in_tx(tx, "coves", "", None).await?,
    };
    let now = now_ms();
    let id = new_id();
    // Issue #175: user-facing creates always land as `CoveKind::User`.
    // The `coves.kind` column was added in migration 0009 with DEFAULT
    // 'user'; we bind the variant explicitly here (mirroring the
    // `card_create_with_id_tx` pattern that binds `CardRole::Plain`)
    // so the storage shape stays self-documenting and a future kind
    // addition surfaces here as a compile error rather than silently
    // accepting the DB default. The system cove is minted exclusively
    // via `cove_create_system_tx` below.
    sqlx::query(
        r#"INSERT INTO coves (id, name, color, sort, kind, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"#,
    )
    .bind(&id)
    .bind(&p.name)
    .bind(&p.color)
    .bind(sort)
    .bind(CoveKind::User)
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(Cove {
        id: id.into(),
        name: p.name,
        color: p.color,
        sort,
        kind: CoveKind::User,
        created_at: now,
        updated_at: now,
    })
}

/// Issue #175 — mint the singleton system cove that hosts the default
/// Today terminal's wave + card. The unique partial index on
/// `coves(kind) WHERE kind = 'system'` from migration 0009 enforces the
/// at-most-one invariant DB-side; the upsert endpoint
/// (`POST /api/coves/system`) checks for existence before calling this
/// helper, so a healthy production path never trips the index. We
/// don't translate a uniqueness violation into a typed conflict here
/// — if two callers race past the existence check we want the txn to
/// roll back and the loser to retry via the upsert endpoint, which
/// will re-read the now-existing row.
///
/// `name`, `color`, and `sort` are sentinel values the user never sees
/// (system coves are filtered out of `GET /api/coves`). They exist
/// because the underlying columns are `NOT NULL`; the chosen sentinels
/// (`name = 'system'`, `color = '#000'`, `sort = -1.0`) are documented
/// here so a debugger landing on a system row knows it's looking at
/// scaffolding, not user data.
pub async fn cove_create_system_tx(tx: &mut Transaction<'_, Sqlite>) -> Result<Cove> {
    let now = now_ms();
    let id = new_id();
    // Sort sentinel: -1.0 places the system cove below any user cove
    // (which start at 1.0 via `next_sort_scoped_in_tx`) if a debugger
    // ever asks for `coves ORDER BY sort`. Hidden from `GET /api/coves`
    // either way; this is just a debugger-friendly default.
    let sort = -1.0_f64;
    sqlx::query(
        r#"INSERT INTO coves (id, name, color, sort, kind, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"#,
    )
    .bind(&id)
    .bind("system")
    .bind("#000")
    .bind(sort)
    .bind(CoveKind::System)
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(Cove {
        id: id.into(),
        name: "system".into(),
        color: "#000".into(),
        sort,
        kind: CoveKind::System,
        created_at: now,
        updated_at: now,
    })
}

pub async fn cove_update_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    p: CovePatch,
) -> Result<Cove> {
    let mut c = sqlx::query_as::<_, Cove>(
        r#"SELECT id, name, color, sort, kind, created_at, updated_at
           FROM coves WHERE id = ?1"#,
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| CalmError::NotFound(format!("cove {id}")))?;

    if let Some(v) = p.name {
        c.name = v;
    }
    if let Some(v) = p.color {
        c.color = v;
    }
    if let Some(v) = p.sort {
        c.sort = v;
    }
    c.updated_at = now_ms();

    // `kind` is intentionally absent from `CovePatch` — issue #175
    // forbids re-tagging a cove between user/system through the regular
    // PATCH surface. The system cove is minted exactly once via
    // `cove_create_system_tx` and never demoted; user coves stay user.
    sqlx::query(
        r#"UPDATE coves SET name = ?1, color = ?2, sort = ?3, updated_at = ?4
           WHERE id = ?5"#,
    )
    .bind(&c.name)
    .bind(&c.color)
    .bind(c.sort)
    .bind(c.updated_at)
    .bind(&c.id)
    .execute(&mut **tx)
    .await?;
    Ok(c)
}

pub async fn cove_delete_tx(tx: &mut Transaction<'_, Sqlite>, id: &str) -> Result<()> {
    let res = sqlx::query("DELETE FROM coves WHERE id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    if res.rows_affected() == 0 {
        return Err(CalmError::NotFound(format!("cove {id}")));
    }
    Ok(())
}

/// Issue #250 PR 2 — in-tx variant of [`SqlxRepo::cove_folder_create`].
///
/// Needed because the wave-create path with `attach_folder = true`
/// claims a folder and writes the wave row in the **same** transaction:
/// either both land or neither does. The route layer
/// (`routes::waves::create_wave`) hands path normalization +
/// conflict-classification responsibilities here (mirror of the route
/// layer in `routes::cove_folders::create_folder`), but the conflict
/// scan reuses the existing in-memory pass over `cove_folders_list_all`
/// inside the same tx so a concurrent claim from another connection is
/// detected by the UNIQUE constraint at INSERT time. Returns the
/// inserted row; the caller emits whatever event/cache write it needs.
pub async fn cove_folder_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    cove_id: &str,
    path: &str,
) -> Result<CoveFolder> {
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM coves WHERE id = ?1")
        .bind(cove_id)
        .fetch_optional(&mut **tx)
        .await?;
    if exists.is_none() {
        return Err(CalmError::NotFound(format!("cove {cove_id}")));
    }
    let now = now_ms();
    let res =
        sqlx::query("INSERT INTO cove_folders (cove_id, path, created_at) VALUES (?1, ?2, ?3)")
            .bind(cove_id)
            .bind(path)
            .bind(now)
            .execute(&mut **tx)
            .await;
    match res {
        Ok(out) => Ok(CoveFolder {
            id: out.last_insert_rowid(),
            cove_id: cove_id.to_string().into(),
            path: path.to_string(),
            created_at: now,
        }),
        Err(sqlx::Error::Database(dbe)) if dbe.message().contains("UNIQUE") => Err(
            CalmError::Conflict(format!("cove_folders.path already claims `{path}`")),
        ),
        Err(e) => Err(e.into()),
    }
}

/// Issue #250 PR 2 — in-tx variant of `cove_folders_list_all`. Used by
/// the wave-create `attach_folder = true` path so the conflict scan
/// reads consistent state alongside the row insert. SQLite serializes
/// writers anyway, but routing through the same tx future-proofs the
/// path against per-connection isolation surprises.
pub async fn cove_folders_list_all_tx(tx: &mut Transaction<'_, Sqlite>) -> Result<Vec<CoveFolder>> {
    let rows = sqlx::query_as::<_, CoveFolder>(
        r#"SELECT id, cove_id, path, created_at
           FROM cove_folders ORDER BY path ASC"#,
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows)
}

pub async fn wave_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    p: NewWave,
    wave_cove_cache: &WaveCoveCache,
) -> Result<Wave> {
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM coves WHERE id = ?1")
        .bind(&p.cove_id)
        .fetch_optional(&mut **tx)
        .await?;
    if exists.is_none() {
        return Err(CalmError::NotFound(format!("cove {}", p.cove_id)));
    }

    let sort = match p.sort {
        Some(s) => s,
        None => {
            next_sort_scoped_in_tx(tx, "waves", "WHERE cove_id = ?1", Some(p.cove_id.as_ref()))
                .await?
        }
    };
    let now = now_ms();
    let id = new_id();
    // Issue #145 — new waves seed at `lifecycle = 'draft'`. The DB
    // DEFAULT in migration 0012 also pins this, but stamping it
    // explicitly here matches the "required field, no Option" model:
    // every wave-create path declares the seed lifecycle in code so a
    // future change to the seed value can't be reached by skipping
    // the column from the INSERT list.
    let lifecycle = crate::model::WaveLifecycle::Draft;
    // Issue #250 PR 2 — `cwd` lands on the row verbatim from `NewWave`.
    // The route layer (`POST /api/waves`) already validated absolute-
    // path shape + cove-folder ownership; this writer stays mechanical.
    // `terminal_at` is `NULL` on every fresh wave (Draft is non-terminal
    // by construction; `WaveLifecycle::is_terminal` returns false for it).
    sqlx::query(
        r#"INSERT INTO waves
           (id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd, terminal_at, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, ?6, NULL, ?7, ?8)"#,
    )
    .bind(&id)
    .bind(&p.cove_id)
    .bind(&p.title)
    .bind(sort)
    .bind(lifecycle)
    .bind(&p.cwd)
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    // #234 — write-through into the wave→cove cache. Same semantics as
    // the `card_role_cache` write-through in `card_create_with_id_tx`: a
    // follow-up emit inside the same `write_with_event` closure can
    // see the freshly-minted binding via `enforce_role`'s lookup.
    let wave_id: WaveId = id.clone().into();
    wave_cove_cache.insert(wave_id.clone(), p.cove_id.clone());
    Ok(Wave {
        id: wave_id,
        cove_id: p.cove_id,
        title: p.title,
        sort,
        archived_at: None,
        pinned_at: None,
        lifecycle,
        cwd: p.cwd,
        terminal_at: None,
        created_at: now,
        updated_at: now,
    })
}

pub async fn wave_update_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    p: WavePatch,
) -> Result<Wave> {
    let mut w = sqlx::query_as::<_, Wave>(
        r#"SELECT id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd, terminal_at, created_at, updated_at
           FROM waves WHERE id = ?1"#,
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| CalmError::NotFound(format!("wave {id}")))?;

    if let Some(v) = p.title {
        w.title = v;
    }
    if let Some(v) = p.sort {
        w.sort = v;
    }
    if let Some(v) = p.archived_at {
        w.archived_at = v;
    }
    if let Some(v) = p.pinned_at {
        w.pinned_at = v;
    }
    // Issue #145 — `WavePatch.lifecycle` is applied here, but the
    // transition is validated by `validate_transition` at the call
    // site (REST handler / MCP tool), *outside* the DB layer. Routing
    // the validator through the route boundary (rather than this
    // function) keeps `wave_update_tx` a pure mechanical row write
    // and avoids threading `ActorId` through every call site that
    // patches the row. Production code paths that mutate
    // `lifecycle` must call `validate_transition` first.
    //
    // Issue #250 PR 2 — `terminal_at` rides on the lifecycle column:
    // when this patch advances the wave into a terminal state we
    // stamp the current time; when it reopens a terminal wave
    // (terminal → planning, the only legal reopen edge today) we
    // clear `terminal_at` back to NULL. A patch that doesn't touch
    // `lifecycle` leaves `terminal_at` alone — that matches the
    // archive precedent (changing `title` doesn't bump `archived_at`).
    // The stamp happens inside the same transaction as the wave row
    // update and the caller's `WaveLifecycleChanged` event, so a
    // mid-tx crash leaves none of them behind.
    if let Some(new_lifecycle) = p.lifecycle {
        if new_lifecycle != w.lifecycle {
            if new_lifecycle.is_terminal() {
                w.terminal_at = Some(now_ms());
            } else if w.lifecycle.is_terminal() {
                // Reopen (terminal → non-terminal). Today the only
                // legal edge here is `terminal → planning` (user-
                // driven, gated by `validate_transition`). Clearing
                // the stamp ensures a reopened wave doesn't render
                // with a stale terminal date on the calendar.
                w.terminal_at = None;
            }
        }
        w.lifecycle = new_lifecycle;
    }
    w.updated_at = now_ms();

    sqlx::query(
        r#"UPDATE waves
           SET title = ?1, sort = ?2, archived_at = ?3, pinned_at = ?4,
               lifecycle = ?5, terminal_at = ?6, updated_at = ?7
           WHERE id = ?8"#,
    )
    .bind(&w.title)
    .bind(w.sort)
    .bind(w.archived_at)
    .bind(w.pinned_at)
    .bind(w.lifecycle)
    .bind(w.terminal_at)
    .bind(w.updated_at)
    .bind(&w.id)
    .execute(&mut **tx)
    .await?;
    Ok(w)
}

pub async fn wave_delete_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    wave_cove_cache: &WaveCoveCache,
) -> Result<()> {
    let res = sqlx::query("DELETE FROM waves WHERE id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    if res.rows_affected() == 0 {
        return Err(CalmError::NotFound(format!("wave {id}")));
    }
    // #234 — keep the wave→cove cache in lockstep with the table. Mirror
    // of the card-delete-side write-through in `card_delete_tx`.
    wave_cove_cache.remove(&WaveId::from(id));
    Ok(())
}

/// Card-row insert that lets the caller pre-mint the row id.
///
/// Carved out from `card_create_tx` so atomic-card endpoints (terminal,
/// codex) can stamp the soon-to-exist card id into per-card sidecar paths
/// (e.g. `codex_homes_dir.join(card_id)`) *before* the row hits the DB,
/// without re-fetching the row after insert. The standalone
/// [`card_create_tx`] wrapper preserves the original "mint inside the
/// helper" contract for every other caller.
pub async fn card_create_with_id_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: String,
    p: NewCard,
    role: CardRole,
    // Issue #229 PR A — explicit, required: every call site must decide
    // whether the card is user-deletable. Per `[[required-over-option]]`
    // an `Option<bool>` with a serde default would silently hide the
    // wrong default at any future callsite (kernel-owned cards minted
    // as deletable would be a security regression). The three live
    // callers cover the policy:
    //   * `card_create_tx`              → `true`  (plain user cards)
    //   * dispatcher worker terminals    → `true`  (workers are user-facing)
    //   * `card_with_codex_create_tx`    → caller decides (`false` for spec)
    deletable: bool,
    card_role_cache: &CardRoleCache,
) -> Result<Card> {
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM waves WHERE id = ?1")
        .bind(&p.wave_id)
        .fetch_optional(&mut **tx)
        .await?;
    if exists.is_none() {
        return Err(CalmError::NotFound(format!("wave {}", p.wave_id)));
    }

    let sort = match p.sort {
        Some(s) => s,
        None => {
            next_sort_scoped_in_tx(tx, "cards", "WHERE wave_id = ?1", Some(p.wave_id.as_ref()))
                .await?
        }
    };
    let now = now_ms();
    let payload_text = serde_json::to_string(&p.payload)?;
    // `role` lands in the `cards.role` column added by migration 0008
    // (PR3, #136). PR3 callers always pass `CardRole::Plain`; PR6 will
    // pass `CardRole::Spec` from the wave-create path, PR5 will pass
    // `CardRole::Worker` from the dispatcher.
    //
    // `deletable` lands in the column added by migration 0013 (#229 PR A).
    // SQLite has no native bool; we encode as `1` / `0`, matching the
    // column's `INTEGER NOT NULL DEFAULT 1` shape. sqlx maps `bool ↔ i64`
    // transparently via its `Encode<Sqlite>` impl, so the bind is direct.
    sqlx::query(
        r#"INSERT INTO cards
               (id, wave_id, kind, sort, payload, role, deletable, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"#,
    )
    .bind(&id)
    .bind(&p.wave_id)
    .bind(&p.kind)
    .bind(sort)
    .bind(&payload_text)
    .bind(role)
    .bind(deletable)
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    // PR3 (#136) — write-through into the role cache. The cache update
    // happens *inside* the surrounding `write_with_event` transaction
    // so a follow-up emit in the same closure can see the freshly
    // minted role via `enforce_role`'s lookup. A txn rollback leaves a
    // stale entry; that's acceptable per the cache's documented
    // semantics — `enforce_role` denies in the only direction that
    // matters (unknown card) and the next boot's `seed_from_db` will
    // overwrite stale entries from the persisted truth.
    let card_id: CardId = id.into();
    card_role_cache.insert(card_id.clone(), role, p.wave_id.clone());
    Ok(Card {
        id: card_id,
        wave_id: p.wave_id,
        kind: p.kind,
        sort,
        payload: p.payload,
        deletable,
        created_at: now,
        updated_at: now,
    })
}

pub async fn card_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    p: NewCard,
    card_role_cache: &CardRoleCache,
) -> Result<Card> {
    // Plain user cards are user-deletable by default — the user added
    // them via REST and can remove them the same way. Spec / report
    // cards take the explicit `false` route via
    // `card_with_codex_create_tx`.
    card_create_with_id_tx(tx, new_id(), p, CardRole::Plain, true, card_role_cache).await
}

pub async fn card_update_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    p: CardPatch,
) -> Result<Card> {
    let mut c = sqlx::query_as::<_, Card>(
        r#"SELECT id, wave_id, kind, sort, payload, deletable, created_at, updated_at
           FROM cards WHERE id = ?1"#,
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;

    if let Some(v) = p.kind {
        c.kind = v;
    }
    if let Some(v) = p.sort {
        c.sort = v;
    }
    if let Some(v) = p.payload {
        c.payload = v;
    }
    // Issue #229 PR A — `p.deletable` is intentionally ignored here.
    // The route handler in `routes/cards.rs::update_card` returns 400
    // when a client sends the field; the field exists on `CardPatch`
    // only to make that 400 explicit (rather than serde silently
    // dropping an unknown field). The UPDATE statement below also
    // doesn't touch the `deletable` column — defense in depth.
    c.updated_at = now_ms();
    let payload_text = serde_json::to_string(&c.payload)?;

    sqlx::query(
        r#"UPDATE cards SET kind = ?1, sort = ?2, payload = ?3, updated_at = ?4
           WHERE id = ?5"#,
    )
    .bind(&c.kind)
    .bind(c.sort)
    .bind(&payload_text)
    .bind(c.updated_at)
    .bind(&c.id)
    .execute(&mut **tx)
    .await?;
    Ok(c)
}

/// Issue #247 PR1 — wave-report-specific transactional update that
/// rewrites both the legacy `payload` JSON column AND the new opaque
/// CRDT blob in `body_crdt` in one statement. Wraps [`card_update_tx`]
/// for the JSON+timestamps path, then re-runs a single UPDATE to
/// stamp the blob. Both writes happen inside the supplied `tx` so a
/// rollback drops them together — the JSON cache and the CRDT
/// authoritative bytes never drift.
///
/// `body_crdt` is the `automerge::AutoCommit::save()` bytes from
/// `wave_report_doc::ReportDoc::to_bytes`; the kernel never
/// interprets the column outside of the round-trip via that module.
///
/// This is a **wave-report-only** seam. Plain / terminal / codex /
/// plugin cards continue going through `card_update_tx`, which never
/// touches `body_crdt` — the column stays NULL on those rows forever.
pub async fn card_update_with_crdt_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    p: CardPatch,
    body_crdt: Vec<u8>,
) -> Result<Card> {
    // Reuse the existing JSON+timestamps update path so the two
    // codepaths can't drift on what `updated_at` / payload-text
    // semantics look like.
    let card = card_update_tx(tx, id, p).await?;
    // Second statement: stamp the opaque CRDT bytes onto the row.
    // Split into its own UPDATE (rather than extending the one above)
    // so plain `card_update_tx` callers never sqlx-bind a `Vec<u8>`
    // they don't care about. The combined cost is one extra UPDATE
    // per wave-report write, which is dominated by the surrounding
    // event-emit work.
    sqlx::query(r#"UPDATE cards SET body_crdt = ?1 WHERE id = ?2"#)
        .bind(&body_crdt)
        .bind(&card.id)
        .execute(&mut **tx)
        .await?;
    Ok(card)
}

/// Issue #247 PR1 — read the opaque CRDT blob for a card inside an
/// open transaction. Returns `None` in either of two cases:
///
///   * the card row doesn't exist (fetched via `fetch_optional` —
///     no `NotFound` is raised, the absent row collapses into the
///     same "no blob to load" signal as a NULL column), or
///   * the row exists but `body_crdt` IS NULL (every pre-PR1 row,
///     plus non-wave-report cards which never get initialized).
///
/// Returns `Some(bytes)` for any row whose first post-PR1 write has
/// run through `card_update_with_crdt_tx`.
///
/// Read inside the same tx as the update so a concurrent writer
/// can't slip a blob in between this read and our `to_bytes` write
/// (the wave-report write path is the only writer of the column
/// today, but pinning the read to the tx is cheap and matches the
/// pattern the rest of `*_tx` uses).
pub async fn card_body_crdt_get_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<Option<Vec<u8>>> {
    let row: Option<(Option<Vec<u8>>,)> =
        sqlx::query_as(r#"SELECT body_crdt FROM cards WHERE id = ?1"#)
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?;
    Ok(row.and_then(|(blob,)| blob))
}

pub async fn card_delete_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    card_role_cache: &CardRoleCache,
) -> Result<()> {
    let res = sqlx::query("DELETE FROM cards WHERE id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    if res.rows_affected() == 0 {
        return Err(CalmError::NotFound(format!("card {id}")));
    }
    // Not reached when a wave/cove delete cascades cards via FK — those
    // paths sweep card overlays in their own txn via
    // overlay_delete_card_overlays_by_wave_tx / overlay_delete_subtree_by_cove_tx.
    overlay_delete_by_entity_tx(tx, "card", id).await?;
    // PR3 (#136) — keep the role cache in lockstep with the table.
    // Like the insert-side write-through, this happens before commit;
    // a txn rollback would leave the cache temporarily missing an
    // entry. The consequence is at worst an `enforce_role` deny on a
    // re-emit that would have been allowed (the card still exists),
    // which is the *safe* failure mode for an auth gate.
    card_role_cache.remove(&CardId::from(id));
    Ok(())
}

pub async fn terminal_delete_tx(tx: &mut Transaction<'_, Sqlite>, id: &str) -> Result<()> {
    let res = sqlx::query("DELETE FROM terminals WHERE id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    if res.rows_affected() == 0 {
        return Err(CalmError::NotFound(format!("terminal {id}")));
    }
    Ok(())
}

/// Transactional terminal-row insert. Structural twin of the `terminal_create`
/// method on `SqlxRepo` — same parent-card-exists and per-card uniqueness
/// pre-checks, same `NotFound` / `Conflict` mapping — but composable inside
/// `Repo::write_with_event` closures alongside the card write.
///
/// Currently only invoked from `card_with_terminal_create_tx`; the standalone
/// `RepoOutOfDomain::terminal_create` path still talks to the pool directly so
/// the existing `POST /api/cards/:id/terminal` recipe keeps its behavior
/// untouched until #13 PR2 swaps it out.
pub async fn terminal_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    p: NewTerminal,
) -> Result<Terminal> {
    // Parent card must exist; surface as NotFound to mirror MockRepo.
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM cards WHERE id = ?1")
        .bind(&p.card_id)
        .fetch_optional(&mut **tx)
        .await?;
    if exists.is_none() {
        return Err(CalmError::NotFound(format!("card {}", p.card_id)));
    }
    // Per-card uniqueness — surface as Conflict to mirror MockRepo
    // (the schema also enforces this via UNIQUE on terminals.card_id).
    let dup: Option<(String,)> = sqlx::query_as("SELECT id FROM terminals WHERE card_id = ?1")
        .bind(&p.card_id)
        .fetch_optional(&mut **tx)
        .await?;
    if dup.is_some() {
        return Err(CalmError::Conflict(format!(
            "terminal already exists for card {}",
            p.card_id
        )));
    }

    let now = now_ms();
    let id = new_id();
    let env_text = serde_json::to_string(&p.env)?;
    // #177 — theme is a write-once row invariant. Render the
    // `(r, g, b)` tuples to comma-decimal once at row creation so
    // every spawn path that reads this row can use the theme with zero
    // allocation.
    let theme_fg = p.theme.fg_arg();
    let theme_bg = p.theme.bg_arg();
    sqlx::query(
        r#"INSERT INTO terminals
               (id, card_id, program, cwd, env, pid, theme_fg, theme_bg, created_at)
           VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?8)"#,
    )
    .bind(&id)
    .bind(&p.card_id)
    .bind(&p.program)
    .bind(&p.cwd)
    .bind(&env_text)
    .bind(&theme_fg)
    .bind(&theme_bg)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(Terminal {
        id,
        card_id: p.card_id,
        program: p.program,
        cwd: p.cwd,
        env: p.env,
        pid: None,
        theme_fg,
        theme_bg,
        exit_code: None,
        signal_killed: false,
        created_at: now,
    })
}

/// Atomically create a `terminal`-kind card AND its associated terminal row
/// inside a single transaction. Runtime identity is written to `runtimes`;
/// API/WS responses project the legacy payload fields at read time.
///
/// This is the kernel side of #13's plan to collapse today's 3-step
/// terminal-card recipe (card-add → terminal-create → card-update) into one
/// atomic db helper. PR1 just lands this helper; PR2 will wire it to a new
/// `POST /api/waves/:id/terminal-cards` endpoint and delete the old recipe.
///
/// On any failure the surrounding transaction rolls back, so partial state
/// (card without terminal, or terminal without runtime row) is impossible.
#[allow(clippy::too_many_arguments)]
pub async fn card_with_terminal_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: String,
    runtime_id: &str,
    wave_id: WaveId,
    sort: Option<f64>,
    program: String,
    cwd: String,
    env: serde_json::Value,
    role: CardRole,
    // Issue #229 PR A — required deletable bit, threaded through to
    // `card_create_with_id_tx`. Dispatcher's worker-terminal path passes
    // `true` (workers are user-facing — users can close them); the
    // direct `POST /api/waves/:id/terminal-cards` path passes `true` for
    // the same reason. Future kernel-owned terminal cards (none today)
    // would pass `false`.
    deletable: bool,
    card_role_cache: &CardRoleCache,
    // #177 — host browser's theme RGB, written onto the terminal row
    // alongside the card so every spawn path reads it from the row and
    // stamps consistent `--terminal-fg/-bg` argv (closes the WS auto-
    // revive race observed in PR #193).
    theme: crate::routes::theme::RequestTheme,
) -> Result<(Card, Terminal)> {
    // 1. Card row with placeholder payload — schemaVersion is stamped in
    //    step 5 once we have the terminal row.
    //
    // PR2 of #136: card id is now pre-minted by the caller (same pattern
    // the codex helper has had since #117) so the surrounding
    // `write_with_event` can stamp `EventScope::Card { card, .. }` on
    // the audit row without racing the txn.
    //
    // PR3 (#136): the user-facing `POST /api/waves/:id/terminal-cards`
    // route passes `CardRole::Plain`; PR6 (#136) — the dispatcher's
    // worker-terminal path passes `CardRole::Worker`. The cache
    // write-through inside `card_create_with_id_tx` keeps the role
    // visible to `enforce_role` calls later in the same tx.
    let card = card_create_with_id_tx(
        tx,
        card_id,
        NewCard {
            wave_id,
            kind: "terminal".into(),
            sort,
            payload: serde_json::Value::Null,
        },
        role,
        deletable,
        card_role_cache,
    )
    .await?;

    // 2. Terminal row, parented to the card.
    let term = terminal_create_tx(
        tx,
        NewTerminal {
            card_id: card.id.clone(),
            program,
            cwd,
            env,
            theme,
        },
    )
    .await?;

    // 3. Build the canonical terminal-card payload.
    let payload = serde_json::json!({
        "schemaVersion": TERMINAL_PAYLOAD_SCHEMA_VERSION,
    });

    // 4. Defense-in-depth: payload validation. The boundary call in
    //    `routes/cards.rs:141` already enforces this for direct create, but
    //    composing inside the kernel means we run our own check rather than
    //    trusting a payload we built ourselves.
    validate_card_kind_global("terminal", &payload)?;

    // 5. Re-stamp the card with the real payload.
    let card = card_update_tx(
        tx,
        card.id.as_ref(),
        CardPatch {
            kind: None,
            sort: None,
            payload: Some(payload),
            // #229 PR A — kernel-internal callers never patch
            // `deletable`; the route handler 400s clients that try.
            deletable: None,
        },
    )
    .await?;

    let runtime_init = RuntimeInit {
        id: runtime_id.to_string(),
        card_id: card.id.to_string(),
        kind: RuntimeKind::Terminal,
        agent_provider: None,
        status: RunStatus::Starting,
        terminal_run_id: Some(term.id.clone()),
        thread_id: None,
        session_id: None,
        active_turn_id: None,
        handle_state_json: None,
        lease_owner: None,
        lease_until_ms: None,
        now_ms: now_ms(),
    };
    if let Some(existing) = runtime_get_active_for_card_tx(tx, card.id.as_ref()).await? {
        runtime_supersede_tx(tx, &existing.id, runtime_init).await?;
    } else {
        runtime_start_tx(tx, runtime_init).await?;
    }

    Ok((card, term))
}

/// Issue #310 followup — atomically delete a card + its backing terminal
/// row inside a single tx, in the order the `RESTRICT` FK demands
/// (terminal first, then card). The structural inverse of
/// [`card_with_terminal_create_tx`] / [`card_with_codex_create_tx`].
///
/// **Use site** is the dispatcher's post-commit failure cleanup: when
/// `per-card CODEX_HOME seeding` or `spawn_daemon_with_parts` returns
/// Err *after* the row-creation tx has already committed, the worker
/// card + terminal row are orphans — the runtime references a terminal
/// whose daemon never came up, and a retry with the same
/// `idempotency_key` would short-circuit on the abandoned row instead
/// of trying again. Rolling both rows back here lets the retry succeed.
///
/// **Idempotent shape.** Each delete swallows `NotFound` so a caller
/// that races the orphan sweeper (which deletes terminals out from
/// under us on a 30-60s cadence) still completes cleanly. The card
/// delete may still surface `NotFound` if the sweeper additionally
/// reaped the card — same shape as the route handler in
/// `routes/cards.rs::delete_card`, where the comment notes the same
/// race is acceptable.
///
/// `card_role_cache` is threaded through so the cache stays in
/// lockstep with the row delete — same write-through invariant
/// `card_delete_tx` itself enforces.
pub async fn card_with_terminal_rollback_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    terminal_id: &str,
    card_role_cache: &CardRoleCache,
) -> Result<()> {
    // Order matters — the FK on `terminals.card_id` is `ON DELETE RESTRICT`
    // since migration 0011, so the card delete would fail with a FK
    // violation if the terminal row still existed.
    match terminal_delete_tx(tx, terminal_id).await {
        Ok(()) => {}
        Err(CalmError::NotFound(_)) => {}
        Err(e) => return Err(e),
    }
    match card_delete_tx(tx, card_id, card_role_cache).await {
        Ok(()) => {}
        Err(CalmError::NotFound(_)) => {}
        Err(e) => return Err(e),
    }
    Ok(())
}

/// Atomically create a `codex`-kind card, its associated terminal row, and
/// the initial `Starting` runtime row inside a single transaction. Runtime
/// identity is written to `runtimes`; API/WS responses project the legacy
/// payload fields at read time.
///
/// Twin of [`card_with_terminal_create_tx`] for the codex-card flow (#117).
/// Differs in two places from the terminal helper:
///
///   1. The caller pre-mints `card_id` (option C in the design doc) so the
///      handler can derive per-card filesystem paths (`CODEX_HOME =
///      <codex_homes_dir>/<card_id>/`) before the row hits the DB. The
///      pre-mint avoids a post-commit "stamp env" round-trip that option B
///      would have required, and keeps a single `card.added` envelope on
///      the bus.
///   2. The canonical payload carries `cwd` when non-empty — the frontend's
///      `codex.tsx` placeholder reads it for status text while the daemon
///      boots. Terminal cards have no such field.
///
/// `program` is hardwired to `"codex"`. The caller still owns env
/// composition (CODEX_HOME / NEIGE_CARD_ID / proxy vars) since those
/// require `AppState` and a settings snapshot that the db layer shouldn't
/// see.
///
/// On any failure the surrounding transaction rolls back; a partial state
/// (card without terminal, or terminal without runtime row) is impossible.
/// PR7a (#136) — third return slot is `Some(raw_token)` for Spec/Worker
/// cards, `None` for Plain. The caller is expected to thread the raw
/// value into the codex daemon's `NEIGE_MCP_TOKEN` env var immediately
/// and discard it — the hash is persisted in `card_mcp_tokens`, but the
/// raw form is unrecoverable on a kernel restart (by design).
#[allow(clippy::too_many_arguments)]
pub async fn card_with_codex_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: String,
    runtime_id: &str,
    wave_id: WaveId,
    sort: Option<f64>,
    cwd: String,
    env: serde_json::Value,
    prompt: Option<String>,
    icon_bg: Option<String>,
    icon_fg: Option<String>,
    role: CardRole,
    // Issue #229 PR A — required deletable bit. The wave-create route
    // passes `false` (the spec card is kernel-owned, must survive
    // direct REST / plugin-callback delete attempts). The plain
    // user-facing `POST /api/waves/:id/codex-cards` route passes `true`.
    deletable: bool,
    card_role_cache: &CardRoleCache,
    // #177 — host browser's theme RGB; written onto the terminal row
    // in the same transaction so the codex daemon's spawn argv is
    // deterministic regardless of which spawn path lands it.
    theme: crate::routes::theme::RequestTheme,
) -> Result<(Card, Terminal, Option<String>)> {
    // 1. Card row with placeholder payload — schemaVersion and UI hints
    //    are stamped in step 5 once we have the terminal row.
    //
    // PR3 (#136): the user-facing `POST /api/waves/:id/codex-cards`
    // route passes `CardRole::Plain`. PR6 (#136) — the wave-create
    // route passes `CardRole::Spec` so the auto-minted spec card is
    // recognized by `enforce_role` as a `WaveUpdated`-permitted
    // emitter. PR5's dispatcher will pass `CardRole::Worker` once it
    // moves off the standalone insert path. The cache write-through
    // inside `card_create_with_id_tx` keeps the role visible to
    // `enforce_role` calls later in the same tx.
    let card = card_create_with_id_tx(
        tx,
        card_id,
        NewCard {
            wave_id,
            kind: "codex".into(),
            sort,
            payload: serde_json::Value::Null,
        },
        role,
        deletable,
        card_role_cache,
    )
    .await?;

    // 2. Terminal row, parented to the card. `program == "codex"` always —
    //    the codex CLI runs in the PTY directly (see `routes::codex_cards`).
    let term = terminal_create_tx(
        tx,
        NewTerminal {
            card_id: card.id.clone(),
            program: "codex".into(),
            cwd: cwd.clone(),
            env,
            theme,
        },
    )
    .await?;

    // 3. Build the canonical codex-card payload. `cwd` is omitted when the
    //    caller passed an empty string — the frontend treats a missing
    //    `cwd` as "show no path hint" rather than "show an empty path".
    let mut payload = serde_json::Map::new();
    payload.insert(
        "schemaVersion".into(),
        serde_json::Value::from(CODEX_PAYLOAD_SCHEMA_VERSION),
    );
    if !cwd.is_empty() {
        payload.insert("cwd".into(), serde_json::Value::String(cwd));
    }
    // `prompt` — surfaces to the `legacy auto-submit` subscriber, which
    // gates auto-Enter on this being a non-empty string. An empty /
    // missing value here is the "user spawned codex without a hands-free
    // prompt" path, identical to pre-#110 behaviour. Trimmed and empty-
    // filtered so the subscriber's `.filter(|s| !s.is_empty())` is the
    // single source of truth.
    if let Some(p) = prompt.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        payload.insert("prompt".into(), serde_json::Value::String(p.to_string()));
    }
    if let Some(c) = icon_bg.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        payload.insert("icon_bg".into(), serde_json::Value::String(c.to_string()));
    }
    if let Some(c) = icon_fg.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        payload.insert("icon_fg".into(), serde_json::Value::String(c.to_string()));
    }
    let payload = serde_json::Value::Object(payload);

    // 4. Defense-in-depth: payload validation. The boundary call in
    //    `routes/cards.rs` enforces this for direct create; composing
    //    inside the kernel means we re-run the check on the payload we
    //    just built.
    validate_card_kind_global("codex", &payload)?;

    // 5. Re-stamp the card with the real payload.
    let card = card_update_tx(
        tx,
        card.id.as_ref(),
        CardPatch {
            kind: None,
            sort: None,
            payload: Some(payload),
            // #229 PR A — kernel-internal callers never patch
            // `deletable`; the route handler 400s clients that try.
            deletable: None,
        },
    )
    .await?;

    // 6. PR7a (#136) — when the card is Spec/Worker, mint a fresh per-card
    //    MCP token, store the hash in `card_mcp_tokens` inside the same tx
    //    (FK enforced — the card row above is the parent), and return the
    //    raw value to the caller so it can be threaded into the codex
    //    daemon's `NEIGE_MCP_TOKEN` env var. Plain cards return `None`.
    //
    //    Doing this here (rather than at the route layer) keeps the
    //    invariant atomic: a committed card row whose role is Spec/Worker
    //    will *always* have a matching token row, and a rolled-back tx
    //    drops both together.
    let mcp_token = if matches!(role, CardRole::Spec | CardRole::Worker) {
        let token = crate::mcp_server::auth::CardMcpToken::generate();
        let hashed = crate::mcp_server::auth::hash_token(token.as_str());
        card_mcp_token_set_tx(tx, card.id.as_ref(), &hashed).await?;
        Some(token.into_inner())
    } else {
        None
    };

    let runtime_init = RuntimeInit {
        id: runtime_id.to_string(),
        card_id: card.id.to_string(),
        kind: RuntimeKind::CodexCard,
        agent_provider: Some(AgentProvider::Codex),
        status: RunStatus::Starting,
        terminal_run_id: Some(term.id.clone()),
        thread_id: None,
        session_id: None,
        active_turn_id: None,
        handle_state_json: None,
        lease_owner: None,
        lease_until_ms: None,
        now_ms: now_ms(),
    };
    runtime_start_tx(tx, runtime_init).await?;

    Ok((card, term, mcp_token))
}

/// Atomically create a `claude`-kind worker card AND its associated terminal
/// row. Claude cards are PTY-backed like codex cards, but intentionally have
/// no MCP token/config path; completion observability comes solely from
/// Claude hook events ingested through `/internal/claude/hook`.
#[allow(clippy::too_many_arguments)]
pub async fn card_with_claude_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: String,
    runtime_id: &str,
    wave_id: WaveId,
    sort: Option<f64>,
    program: String,
    cwd: String,
    env: serde_json::Value,
    prompt: Option<String>,
    icon_bg: Option<String>,
    icon_fg: Option<String>,
    settings_path: String,
    claude_session_id: String,
    role: CardRole,
    deletable: bool,
    card_role_cache: &CardRoleCache,
    theme: crate::routes::theme::RequestTheme,
) -> Result<(Card, Terminal)> {
    let card = card_create_with_id_tx(
        tx,
        card_id,
        NewCard {
            wave_id,
            kind: "claude".into(),
            sort,
            payload: serde_json::Value::Null,
        },
        role,
        deletable,
        card_role_cache,
    )
    .await?;

    let term = terminal_create_tx(
        tx,
        NewTerminal {
            card_id: card.id.clone(),
            program,
            cwd: cwd.clone(),
            env,
            theme,
        },
    )
    .await?;

    let mut payload = serde_json::Map::new();
    payload.insert(
        "schemaVersion".into(),
        serde_json::Value::from(CLAUDE_PAYLOAD_SCHEMA_VERSION),
    );
    payload.insert(
        "settings_path".into(),
        serde_json::Value::String(settings_path),
    );
    if !cwd.is_empty() {
        payload.insert("cwd".into(), serde_json::Value::String(cwd));
    }
    if let Some(p) = prompt.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        payload.insert("prompt".into(), serde_json::Value::String(p.to_string()));
    }
    if let Some(c) = icon_bg.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        payload.insert("icon_bg".into(), serde_json::Value::String(c.to_string()));
    }
    if let Some(c) = icon_fg.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        payload.insert("icon_fg".into(), serde_json::Value::String(c.to_string()));
    }
    let payload = serde_json::Value::Object(payload);
    validate_card_kind_global("claude", &payload)?;

    let card = card_update_tx(
        tx,
        card.id.as_ref(),
        CardPatch {
            kind: None,
            sort: None,
            payload: Some(payload),
            deletable: None,
        },
    )
    .await?;

    let runtime_init = RuntimeInit {
        id: runtime_id.to_string(),
        card_id: card.id.to_string(),
        kind: RuntimeKind::ClaudeCard,
        agent_provider: Some(AgentProvider::Claude),
        status: RunStatus::Starting,
        terminal_run_id: Some(term.id.clone()),
        thread_id: None,
        session_id: Some(claude_session_id),
        active_turn_id: None,
        handle_state_json: None,
        lease_owner: None,
        lease_until_ms: None,
        now_ms: now_ms(),
    };
    if let Some(existing) = runtime_get_active_for_card_tx(tx, card.id.as_ref()).await? {
        runtime_supersede_tx(tx, &existing.id, runtime_init).await?;
    } else {
        runtime_start_tx(tx, runtime_init).await?;
    }

    Ok((card, term))
}

/// PR7a (#136) — insert (or replace) a per-card MCP token row in the
/// supplied transaction. The raw token is never persisted; the caller
/// passes `hash_token(raw)` and keeps the raw value only in memory long
/// enough to thread it into the env map handed to the codex daemon.
///
/// `card_id` must reference a real row in `cards` — the FK constraint
/// in migration 0010 fails the tx otherwise. The standard call site is
/// `card_with_codex_create_tx`, where the card row is created moments
/// earlier in the same tx, so the FK is satisfied by construction.
pub async fn card_mcp_token_set_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    hashed_token: &str,
) -> Result<()> {
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO card_mcp_tokens (card_id, hashed_token, created_at)
           VALUES (?1, ?2, ?3)
           ON CONFLICT(card_id) DO UPDATE SET
               hashed_token = excluded.hashed_token,
               created_at   = excluded.created_at"#,
    )
    .bind(card_id)
    .bind(hashed_token)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Deprecated legacy compatibility helper.
///
/// PR3 write-demotes `card_codex_threads`: normal runtime identity writes
/// must go to `runtimes` instead. Keep this for migrations, tests, and
/// rollback paths that intentionally preserve pre-PR3 semantics.
pub async fn card_codex_thread_upsert_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    thread_id: &str,
    role: CardRole,
    wave_id: Option<&str>,
) -> Result<()> {
    let now = now_ms();
    sqlx::query(
        r#"INSERT INTO card_codex_threads
               (thread_id, card_id, role, wave_id, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?5)
           ON CONFLICT(card_id) DO UPDATE SET
               thread_id  = excluded.thread_id,
               role       = excluded.role,
               wave_id    = excluded.wave_id,
               updated_at = excluded.updated_at"#,
    )
    .bind(thread_id)
    .bind(card_id)
    .bind(role)
    .bind(wave_id)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn card_codex_thread_delete_by_card_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
) -> Result<()> {
    sqlx::query("DELETE FROM card_codex_threads WHERE card_id = ?1")
        .bind(card_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

fn runtime_kind_to_db(kind: &RuntimeKind) -> &'static str {
    match kind {
        RuntimeKind::Terminal => "terminal",
        RuntimeKind::CodexCard => "codex",
        RuntimeKind::ClaudeCard => "claude",
        RuntimeKind::SharedSpec => "shared-spec",
    }
}

fn runtime_kind_from_db(value: &str) -> RuntimeResult<RuntimeKind> {
    match value {
        "terminal" => Ok(RuntimeKind::Terminal),
        "codex" => Ok(RuntimeKind::CodexCard),
        "claude" => Ok(RuntimeKind::ClaudeCard),
        "shared-spec" => Ok(RuntimeKind::SharedSpec),
        other => Err(RuntimeRepoError::Message {
            message: format!("unknown runtime kind {other:?}"),
        }),
    }
}

fn agent_provider_to_db(provider: &AgentProvider) -> &'static str {
    match provider {
        AgentProvider::Codex => "codex",
        AgentProvider::Claude => "claude",
    }
}

fn agent_provider_from_db(value: &str) -> RuntimeResult<AgentProvider> {
    match value {
        "codex" => Ok(AgentProvider::Codex),
        "claude" => Ok(AgentProvider::Claude),
        other => Err(RuntimeRepoError::Message {
            message: format!("unknown runtime agent provider {other:?}"),
        }),
    }
}

fn run_status_to_db(status: &RunStatus) -> &'static str {
    match status {
        RunStatus::Starting => "starting",
        RunStatus::Running => "running",
        RunStatus::Idle => "idle",
        RunStatus::TurnPending => "turn_pending",
        RunStatus::Failed => "failed",
        RunStatus::Exited => "exited",
        RunStatus::Superseded => "superseded",
    }
}

fn run_status_from_db(value: &str) -> RuntimeResult<RunStatus> {
    match value {
        "starting" => Ok(RunStatus::Starting),
        "running" => Ok(RunStatus::Running),
        "idle" => Ok(RunStatus::Idle),
        "turn_pending" => Ok(RunStatus::TurnPending),
        "failed" => Ok(RunStatus::Failed),
        "exited" => Ok(RunStatus::Exited),
        "superseded" => Ok(RunStatus::Superseded),
        other => Err(RuntimeRepoError::Message {
            message: format!("unknown runtime status {other:?}"),
        }),
    }
}

fn runtime_message(message: impl Into<String>) -> RuntimeRepoError {
    RuntimeRepoError::Message {
        message: message.into(),
    }
}

fn runtime_status_transition_allowed(from: &RunStatus, to: &RunStatus) -> bool {
    match from {
        RunStatus::Starting => matches!(
            to,
            RunStatus::Running
                | RunStatus::Idle
                | RunStatus::TurnPending
                | RunStatus::Failed
                | RunStatus::Exited
        ),
        RunStatus::Running => matches!(to, RunStatus::Idle | RunStatus::Failed | RunStatus::Exited),
        RunStatus::Idle => matches!(
            to,
            RunStatus::Running | RunStatus::Failed | RunStatus::Exited
        ),
        RunStatus::TurnPending => {
            matches!(
                to,
                RunStatus::Running | RunStatus::Failed | RunStatus::Exited
            )
        }
        RunStatus::Failed | RunStatus::Exited | RunStatus::Superseded => false,
    }
}

fn ensure_runtime_status_transition(
    id: &RuntimeId,
    from: &RunStatus,
    to: &RunStatus,
) -> RuntimeResult<()> {
    if runtime_status_transition_allowed(from, to) {
        Ok(())
    } else {
        Err(RuntimeRepoError::IllegalStatusTransition {
            id: id.clone(),
            attempted: to.clone(),
        })
    }
}

async fn runtime_current_status_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
) -> RuntimeResult<RunStatus> {
    let row = sqlx::query("SELECT status FROM runtimes WHERE id = ?1")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?;
    let Some(row) = row else {
        return Err(runtime_message(format!("runtime {id} not found")));
    };
    run_status_from_db(row.try_get::<String, _>("status")?.as_str())
}

fn card_runtime_from_row(row: &sqlx::sqlite::SqliteRow) -> RuntimeResult<CardRuntime> {
    let kind = runtime_kind_from_db(row.try_get::<String, _>("kind")?.as_str())?;
    let agent_provider = row
        .try_get::<Option<String>, _>("agent_provider")?
        .as_deref()
        .map(agent_provider_from_db)
        .transpose()?;
    let status = run_status_from_db(row.try_get::<String, _>("status")?.as_str())?;
    let handle_state_json = row
        .try_get::<Option<String>, _>("handle_state_json")?
        .as_deref()
        .map(serde_json::from_str)
        .transpose()?;

    Ok(CardRuntime {
        id: row.try_get("id")?,
        card_id: row.try_get("card_id")?,
        kind,
        agent_provider,
        status,
        terminal_run_id: row.try_get("terminal_run_id")?,
        terminal_ref: None,
        thread_id: row.try_get("thread_id")?,
        session_id: row.try_get("session_id")?,
        active_turn_id: row.try_get("active_turn_id")?,
        handle_state_json,
        lease_owner: row.try_get("lease_owner")?,
        lease_until_ms: row.try_get("lease_until_ms")?,
        created_at_ms: row.try_get("created_at_ms")?,
        updated_at_ms: row.try_get("updated_at_ms")?,
        completed_at_ms: row.try_get("completed_at_ms")?,
    })
}

async fn runtime_get_by_id_from_pool(
    pool: &SqlitePool,
    id: &RuntimeId,
) -> RuntimeResult<Option<CardRuntime>> {
    let row = sqlx::query(
        r#"SELECT id, card_id, kind, agent_provider, status, terminal_run_id,
                  thread_id, session_id, active_turn_id, handle_state_json,
                  lease_owner, lease_until_ms, created_at_ms, updated_at_ms,
                  completed_at_ms
           FROM runtimes
           WHERE id = ?1"#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(card_runtime_from_row).transpose()
}

async fn runtime_get_active_for_card_from_pool(
    pool: &SqlitePool,
    card_id: &str,
) -> RuntimeResult<Option<CardRuntime>> {
    let row = sqlx::query(
        r#"SELECT id, card_id, kind, agent_provider, status, terminal_run_id,
                  thread_id, session_id, active_turn_id, handle_state_json,
                  lease_owner, lease_until_ms, created_at_ms, updated_at_ms,
                  completed_at_ms
           FROM runtimes
           WHERE card_id = ?1
             AND status IN ('starting', 'running', 'idle', 'turn_pending')
           ORDER BY updated_at_ms DESC, created_at_ms DESC, id DESC
           LIMIT 1"#,
    )
    .bind(card_id)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(card_runtime_from_row).transpose()
}

async fn runtime_get_projectable_for_card_from_pool(
    pool: &SqlitePool,
    card_id: &str,
) -> RuntimeResult<Option<CardRuntime>> {
    // Project the card's CURRENT identity. We include terminal-status rows
    // (failed/exited) so a card whose runtime just exited still surfaces its
    // last-known thread_id/terminal_id/etc. for UI and history. 'superseded'
    // is excluded — a superseded runtime has been replaced by a new active
    // row, which the ORDER BY will pick up. Active states sort first.
    let row = sqlx::query(
        r#"SELECT id, card_id, kind, agent_provider, status, terminal_run_id,
                  thread_id, session_id, active_turn_id, handle_state_json,
                  lease_owner, lease_until_ms, created_at_ms, updated_at_ms,
                  completed_at_ms
           FROM runtimes
           WHERE card_id = ?1
             AND status != 'superseded'
           ORDER BY
             CASE
                 WHEN status IN ('starting', 'running', 'idle', 'turn_pending') THEN 0
                 ELSE 1
             END ASC,
             updated_at_ms DESC, created_at_ms DESC, id DESC
           LIMIT 1"#,
    )
    .bind(card_id)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(card_runtime_from_row).transpose()
}

async fn runtime_get_projectable_for_cards_from_pool(
    pool: &SqlitePool,
    card_ids: &[RuntimeCardId],
) -> RuntimeResult<HashMap<RuntimeCardId, CardRuntime>> {
    if card_ids.is_empty() {
        return Ok(HashMap::new());
    }

    // See `runtime_get_projectable_for_card_from_pool` for the projection
    // semantics — include terminal-state rows so last-known identity surfaces,
    // exclude 'superseded' so the replacement active row is preferred.
    let mut query = QueryBuilder::<Sqlite>::new(
        r#"SELECT id, card_id, kind, agent_provider, status, terminal_run_id,
                  thread_id, session_id, active_turn_id, handle_state_json,
                  lease_owner, lease_until_ms, created_at_ms, updated_at_ms,
                  completed_at_ms
           FROM runtimes
           WHERE status != 'superseded'
             AND card_id IN ("#,
    );
    let mut separated = query.separated(", ");
    for card_id in card_ids {
        separated.push_bind(card_id);
    }
    separated.push_unseparated(
        r#") ORDER BY card_id ASC,
             CASE
                 WHEN status IN ('starting', 'running', 'idle', 'turn_pending') THEN 0
                 ELSE 1
             END ASC,
             updated_at_ms DESC, created_at_ms DESC, id DESC"#,
    );

    let rows = query.build().fetch_all(pool).await?;
    let mut out = HashMap::new();
    for row in rows {
        let runtime = card_runtime_from_row(&row)?;
        let card_id = runtime.card_id.clone();
        out.entry(card_id).or_insert(runtime);
    }
    Ok(out)
}

async fn runtime_get_active_by_thread_from_pool(
    pool: &SqlitePool,
    provider: AgentProvider,
    thread_id: &str,
) -> RuntimeResult<Option<CardRuntime>> {
    let row = sqlx::query(
        r#"SELECT id, card_id, kind, agent_provider, status, terminal_run_id,
                  thread_id, session_id, active_turn_id, handle_state_json,
                  lease_owner, lease_until_ms, created_at_ms, updated_at_ms,
                  completed_at_ms
           FROM runtimes
           WHERE agent_provider = ?1
             AND thread_id = ?2
             AND status IN ('starting', 'running', 'idle', 'turn_pending')
           ORDER BY updated_at_ms DESC, created_at_ms DESC, id DESC
           LIMIT 1"#,
    )
    .bind(agent_provider_to_db(&provider))
    .bind(thread_id)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(card_runtime_from_row).transpose()
}

async fn runtime_active_shared_thread_attribution_from_pool(
    pool: &SqlitePool,
) -> RuntimeResult<Vec<(String, String)>> {
    sqlx::query_as::<_, (String, String)>(
        r#"SELECT thread_id, card_id
           FROM runtimes
           WHERE kind IN ('shared-spec', 'codex')
             AND agent_provider = 'codex'
             AND thread_id IS NOT NULL
             AND status IN ('starting', 'running', 'idle', 'turn_pending')
           ORDER BY created_at_ms ASC, card_id ASC"#,
    )
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

async fn runtimes_active_for_kind_from_pool(
    pool: &SqlitePool,
    kind: RuntimeKind,
) -> RuntimeResult<Vec<CardRuntime>> {
    let rows = sqlx::query(
        r#"SELECT id, card_id, kind, agent_provider, status, terminal_run_id,
                  thread_id, session_id, active_turn_id, handle_state_json,
                  lease_owner, lease_until_ms, created_at_ms, updated_at_ms,
                  completed_at_ms
           FROM runtimes
           WHERE kind = ?1
             AND status IN ('starting', 'running', 'idle', 'turn_pending')
           ORDER BY created_at_ms ASC, card_id ASC"#,
    )
    .bind(runtime_kind_to_db(&kind))
    .fetch_all(pool)
    .await?;
    rows.iter().map(card_runtime_from_row).collect()
}

pub async fn runtime_get_by_id_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
) -> RuntimeResult<Option<CardRuntime>> {
    let row = sqlx::query(
        r#"SELECT id, card_id, kind, agent_provider, status, terminal_run_id,
                  thread_id, session_id, active_turn_id, handle_state_json,
                  lease_owner, lease_until_ms, created_at_ms, updated_at_ms,
                  completed_at_ms
           FROM runtimes
           WHERE id = ?1"#,
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    row.as_ref().map(card_runtime_from_row).transpose()
}

pub async fn runtime_get_active_for_card_tx(
    tx: &mut RuntimeTx<'_>,
    card_id: &str,
) -> RuntimeResult<Option<CardRuntime>> {
    let row = sqlx::query(
        r#"SELECT id, card_id, kind, agent_provider, status, terminal_run_id,
                  thread_id, session_id, active_turn_id, handle_state_json,
                  lease_owner, lease_until_ms, created_at_ms, updated_at_ms,
                  completed_at_ms
           FROM runtimes
           WHERE card_id = ?1
             AND status IN ('starting', 'running', 'idle', 'turn_pending')
           ORDER BY updated_at_ms DESC, created_at_ms DESC, id DESC
           LIMIT 1"#,
    )
    .bind(card_id)
    .fetch_optional(&mut **tx)
    .await?;
    row.as_ref().map(card_runtime_from_row).transpose()
}

pub async fn runtime_start_tx(
    tx: &mut RuntimeTx<'_>,
    init: RuntimeInit,
) -> RuntimeResult<CardRuntime> {
    let kind = runtime_kind_to_db(&init.kind);
    let agent_provider = init.agent_provider.as_ref().map(agent_provider_to_db);
    let status = run_status_to_db(&init.status);
    let handle_state_json = init
        .handle_state_json
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;

    sqlx::query(
        r#"INSERT INTO runtimes (
               id, card_id, kind, agent_provider, status, terminal_run_id,
               thread_id, session_id, active_turn_id, handle_state_json,
               lease_owner, lease_until_ms, created_at_ms, updated_at_ms,
               completed_at_ms
           )
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13, NULL)"#,
    )
    .bind(&init.id)
    .bind(&init.card_id)
    .bind(kind)
    .bind(agent_provider)
    .bind(status)
    .bind(&init.terminal_run_id)
    .bind(&init.thread_id)
    .bind(&init.session_id)
    .bind(&init.active_turn_id)
    .bind(&handle_state_json)
    .bind(&init.lease_owner)
    .bind(init.lease_until_ms)
    .bind(init.now_ms)
    .execute(&mut **tx)
    .await?;

    runtime_get_by_id_tx(tx, &init.id)
        .await?
        .ok_or_else(|| runtime_message(format!("runtime {} missing after insert", init.id)))
}

pub async fn runtime_supersede_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
    new_init: RuntimeInit,
) -> RuntimeResult<CardRuntime> {
    let res = sqlx::query(
        r#"UPDATE runtimes
              SET status = 'superseded',
                  updated_at_ms = ?1,
                  completed_at_ms = COALESCE(completed_at_ms, ?1)
            WHERE id = ?2
              AND status IN ('starting', 'running', 'idle', 'turn_pending')"#,
    )
    .bind(new_init.now_ms)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(runtime_message(format!(
            "active runtime {id} not found for supersede"
        )));
    }

    runtime_start_tx(tx, new_init).await
}

pub async fn runtime_set_status_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
    status: RunStatus,
) -> RuntimeResult<()> {
    if status == RunStatus::Superseded {
        return Err(RuntimeRepoError::IllegalStatusTransition {
            id: id.clone(),
            attempted: status,
        });
    }

    let current = runtime_current_status_tx(tx, id).await?;
    ensure_runtime_status_transition(id, &current, &status)?;

    let now = now_ms();
    let res = sqlx::query(
        r#"UPDATE runtimes
              SET status = ?1,
                  updated_at_ms = ?2
            WHERE id = ?3"#,
    )
    .bind(run_status_to_db(&status))
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(runtime_message(format!("runtime {id} not found")));
    }
    Ok(())
}

pub async fn runtime_set_status_for_card_tx(
    tx: &mut RuntimeTx<'_>,
    card_id: &str,
    status: RunStatus,
) -> RuntimeResult<()> {
    let Some(runtime) = runtime_get_active_for_card_tx(tx, card_id).await? else {
        return Ok(());
    };
    runtime_set_status_tx(tx, &runtime.id, status).await
}

pub async fn runtime_bind_attribution_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
    attr: ThreadAttribution,
) -> RuntimeResult<()> {
    if &attr.runtime_id != id {
        return Err(runtime_message(format!(
            "runtime attribution id mismatch: arg={id}, attr={}",
            attr.runtime_id
        )));
    }

    let now = now_ms();
    let res = sqlx::query(
        r#"UPDATE runtimes
              SET agent_provider = ?1,
                  thread_id = ?2,
                  session_id = ?3,
                  active_turn_id = ?4,
                  updated_at_ms = ?5
            WHERE id = ?6"#,
    )
    .bind(agent_provider_to_db(&attr.provider))
    .bind(&attr.thread_id)
    .bind(&attr.session_id)
    .bind(&attr.active_turn_id)
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(runtime_message(format!("runtime {id} not found")));
    }
    Ok(())
}

pub async fn runtime_clear_terminal_run_id_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
) -> RuntimeResult<()> {
    let now = now_ms();
    let res = sqlx::query(
        r#"UPDATE runtimes
              SET terminal_run_id = NULL, updated_at_ms = ?1
            WHERE id = ?2"#,
    )
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(runtime_message(format!("runtime {id} not found")));
    }
    Ok(())
}

pub async fn runtime_set_handle_state_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
    state: Option<serde_json::Value>,
) -> RuntimeResult<()> {
    let state_text = state.as_ref().map(serde_json::to_string).transpose()?;
    let now = now_ms();
    let res = sqlx::query(
        r#"UPDATE runtimes
              SET handle_state_json = ?1,
                  updated_at_ms = ?2
            WHERE id = ?3"#,
    )
    .bind(&state_text)
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(runtime_message(format!("runtime {id} not found")));
    }
    Ok(())
}

pub async fn runtime_set_active_turn_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
    turn_id: Option<&str>,
) -> RuntimeResult<()> {
    let now = now_ms();
    let res = sqlx::query(
        r#"UPDATE runtimes
              SET active_turn_id = ?1,
                  updated_at_ms = ?2
            WHERE id = ?3"#,
    )
    .bind(turn_id)
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(runtime_message(format!("runtime {id} not found")));
    }
    Ok(())
}

pub async fn runtime_complete_tx(
    tx: &mut RuntimeTx<'_>,
    id: &RuntimeId,
    terminal_status: RunStatus,
) -> RuntimeResult<()> {
    if !matches!(terminal_status, RunStatus::Failed | RunStatus::Exited) {
        return Err(RuntimeRepoError::IllegalStatusTransition {
            id: id.clone(),
            attempted: terminal_status,
        });
    }

    let current = runtime_current_status_tx(tx, id).await?;
    ensure_runtime_status_transition(id, &current, &terminal_status)?;

    let now = now_ms();
    let res = sqlx::query(
        r#"UPDATE runtimes
              SET status = ?1,
                  updated_at_ms = ?2,
                  completed_at_ms = ?2
            WHERE id = ?3"#,
    )
    .bind(run_status_to_db(&terminal_status))
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(runtime_message(format!("runtime {id} not found")));
    }
    Ok(())
}

pub async fn runtime_complete_for_card_tx(
    tx: &mut RuntimeTx<'_>,
    card_id: &str,
    terminal_status: RunStatus,
) -> RuntimeResult<()> {
    let Some(runtime) = runtime_get_active_for_card_tx(tx, card_id).await? else {
        return Ok(());
    };
    runtime_complete_tx(tx, &runtime.id, terminal_status).await
}

pub async fn runtime_get_active_for_terminal_tx(
    tx: &mut RuntimeTx<'_>,
    terminal_id: &str,
) -> RuntimeResult<Option<CardRuntime>> {
    let row = sqlx::query(
        r#"SELECT id, card_id, kind, agent_provider, status, terminal_run_id,
                  thread_id, session_id, active_turn_id, handle_state_json,
                  lease_owner, lease_until_ms, created_at_ms, updated_at_ms,
                  completed_at_ms
           FROM runtimes
           WHERE terminal_run_id = ?1
             AND status IN ('starting', 'running', 'idle', 'turn_pending')
           ORDER BY updated_at_ms DESC, created_at_ms DESC, id DESC
           LIMIT 1"#,
    )
    .bind(terminal_id)
    .fetch_optional(&mut **tx)
    .await?;
    row.as_ref().map(card_runtime_from_row).transpose()
}

pub async fn runtime_complete_for_terminal_tx(
    tx: &mut RuntimeTx<'_>,
    terminal_id: &str,
    terminal_status: RunStatus,
) -> RuntimeResult<()> {
    let Some(runtime) = runtime_get_active_for_terminal_tx(tx, terminal_id).await? else {
        return Ok(());
    };
    runtime_complete_tx(tx, &runtime.id, terminal_status).await
}

pub async fn overlay_upsert_tx(tx: &mut Transaction<'_, Sqlite>, p: NewOverlay) -> Result<Overlay> {
    let now = now_ms();
    let new_id_str = new_id();
    let payload_text = serde_json::to_string(&p.payload)?;
    let row = sqlx::query_as::<_, Overlay>(
        r#"INSERT INTO overlays
               (id, plugin_id, entity_kind, entity_id, kind, payload, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
           ON CONFLICT(plugin_id, entity_kind, entity_id, kind)
             DO UPDATE SET payload = excluded.payload,
                           updated_at = excluded.updated_at
           RETURNING id, plugin_id, entity_kind, entity_id, kind, payload, updated_at"#,
    )
    .bind(&new_id_str)
    .bind(&p.plugin_id)
    .bind(&p.entity_kind)
    .bind(&p.entity_id)
    .bind(&p.kind)
    .bind(&payload_text)
    .bind(now)
    .fetch_one(&mut **tx)
    .await?;
    Ok(row)
}

pub async fn overlay_delete_tx(
    tx: &mut Transaction<'_, Sqlite>,
    plugin_id: &str,
    entity_kind: &str,
    entity_id: &str,
    kind: &str,
) -> Result<()> {
    let res = sqlx::query(
        r#"DELETE FROM overlays
           WHERE plugin_id = ?1 AND entity_kind = ?2 AND entity_id = ?3 AND kind = ?4"#,
    )
    .bind(plugin_id)
    .bind(entity_kind)
    .bind(entity_id)
    .bind(kind)
    .execute(&mut **tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(CalmError::NotFound("overlay".into()));
    }
    Ok(())
}

/// Drop every overlay row addressed at the given `(entity_kind, entity_id)`,
/// across all `plugin_id`s and all `kind`s. Used by the card / wave / cove
/// delete paths to keep the table from growing orphans; the `overlays`
/// schema has no FK because SQLite can't express polymorphic ones.
pub async fn overlay_delete_by_entity_tx(
    tx: &mut Transaction<'_, Sqlite>,
    entity_kind: &str,
    entity_id: &str,
) -> Result<u64> {
    let res = sqlx::query("DELETE FROM overlays WHERE entity_kind = ?1 AND entity_id = ?2")
        .bind(entity_kind)
        .bind(entity_id)
        .execute(&mut **tx)
        .await?;
    Ok(res.rows_affected())
}

/// Drop every card-scoped overlay for cards still belonging to the given
/// wave, in the caller's transaction. Race-safe replacement for "read
/// cards_by_wave, loop, sweep" — the IN subquery sees the same DB state
/// the subsequent `wave_delete_tx` cascade will see, so a card+overlay
/// created between an outside-tx snapshot and the txn commit is still
/// caught.
pub async fn overlay_delete_card_overlays_by_wave_tx(
    tx: &mut Transaction<'_, Sqlite>,
    wave_id: &str,
) -> Result<u64> {
    let res = sqlx::query(
        r#"DELETE FROM overlays
           WHERE entity_kind = 'card'
             AND entity_id IN (SELECT id FROM cards WHERE wave_id = ?1)"#,
    )
    .bind(wave_id)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected())
}

/// Drop every card-scoped and wave-scoped (`'wave'` + `'view'`) overlay
/// under the cove, in the caller's transaction. Same race-safe shape as
/// `overlay_delete_card_overlays_by_wave_tx`. Caller still needs to
/// sweep `('cove', cove_id)` separately — plugins may address overlays
/// at the cove kind.
pub async fn overlay_delete_subtree_by_cove_tx(
    tx: &mut Transaction<'_, Sqlite>,
    cove_id: &str,
) -> Result<u64> {
    // Three statements, accumulated count. Could be one nested query but
    // splitting keeps each delete legible and individually optimizable.
    let cards = sqlx::query(
        r#"DELETE FROM overlays
           WHERE entity_kind = 'card'
             AND entity_id IN (
               SELECT c.id FROM cards c
               JOIN waves w ON w.id = c.wave_id
               WHERE w.cove_id = ?1
             )"#,
    )
    .bind(cove_id)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    let waves = sqlx::query(
        r#"DELETE FROM overlays
           WHERE entity_kind IN ('wave', 'view')
             AND entity_id IN (SELECT id FROM waves WHERE cove_id = ?1)"#,
    )
    .bind(cove_id)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    Ok(cards + waves)
}

// ---------------------------------------------------------------------------
// Sub-trait impls — thin pool-wrapping wrappers around the `_tx` helpers,
// plus the read-side methods that don't need transaction composition.
//
// `Repo` (and `RouteRepo`) are picked up via the blanket impls in `db/mod`
// once all four sub-traits are implemented.
// ---------------------------------------------------------------------------

#[async_trait]
impl RepoRead for SqlxRepo {
    // ---------------------------------------------------------------- coves
    async fn coves_list(&self) -> Result<Vec<Cove>> {
        let rows = sqlx::query_as::<_, Cove>(
            r#"SELECT id, name, color, sort, kind, created_at, updated_at
               FROM coves ORDER BY sort ASC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn coves_list_user_visible(&self) -> Result<Vec<Cove>> {
        // Issue #175 — default surface for `GET /api/coves`. Filters out
        // the singleton system cove that hosts the default Today
        // terminal's wave + card. Pre-#175 callers that want every row
        // (debug surfaces, integration tests asserting on the system
        // cove's existence) use `coves_list` directly.
        let rows = sqlx::query_as::<_, Cove>(
            r#"SELECT id, name, color, sort, kind, created_at, updated_at
               FROM coves WHERE kind = 'user' ORDER BY sort ASC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn cove_get(&self, id: &str) -> Result<Option<Cove>> {
        let row = sqlx::query_as::<_, Cove>(
            r#"SELECT id, name, color, sort, kind, created_at, updated_at
               FROM coves WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn cove_get_system(&self) -> Result<Option<Cove>> {
        // Issue #175 — return the singleton system cove if it exists,
        // `None` before the first call to the `POST /api/coves/system`
        // upsert endpoint. Backed by the partial unique index on
        // `coves(kind) WHERE kind = 'system'` from migration 0009 —
        // there is at most one such row.
        let row = sqlx::query_as::<_, Cove>(
            r#"SELECT id, name, color, sort, kind, created_at, updated_at
               FROM coves WHERE kind = 'system' LIMIT 1"#,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    // -------------------------------------------------------- cove_folders
    async fn cove_folders_by_cove(&self, cove_id: &str) -> Result<Vec<CoveFolder>> {
        let rows = sqlx::query_as::<_, CoveFolder>(
            r#"SELECT id, cove_id, path, created_at
               FROM cove_folders WHERE cove_id = ?1 ORDER BY path ASC"#,
        )
        .bind(cove_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn cove_folders_list_all(&self) -> Result<Vec<CoveFolder>> {
        let rows = sqlx::query_as::<_, CoveFolder>(
            r#"SELECT id, cove_id, path, created_at
               FROM cove_folders ORDER BY path ASC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn cove_folder_get(&self, id: i64) -> Result<Option<CoveFolder>> {
        let row = sqlx::query_as::<_, CoveFolder>(
            r#"SELECT id, cove_id, path, created_at
               FROM cove_folders WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    // ---------------------------------------------------------------- waves
    async fn waves_by_cove(&self, cove_id: &str) -> Result<Vec<Wave>> {
        let rows = sqlx::query_as::<_, Wave>(
            r#"SELECT id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd, terminal_at, created_at, updated_at
               FROM waves WHERE cove_id = ?1 ORDER BY sort ASC"#,
        )
        .bind(cove_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn wave_get(&self, id: &str) -> Result<Option<Wave>> {
        let row = sqlx::query_as::<_, Wave>(
            r#"SELECT id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd, terminal_at, created_at, updated_at
               FROM waves WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
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
            "SELECT id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd, terminal_at, \
             created_at, updated_at FROM waves",
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

        let mut q = sqlx::query_as::<_, Wave>(&sql);
        if let Some(c) = cove_id {
            q = q.bind(c);
        }
        if let Some(u) = until {
            q = q.bind(u);
        }
        if let Some(s) = since {
            q = q.bind(s);
        }
        Ok(q.fetch_all(&self.pool).await?)
    }

    async fn wave_detail(&self, id: &str) -> Result<Option<WaveDetail>> {
        let mut tx = self.pool.begin().await?;
        let wave = sqlx::query_as::<_, Wave>(
            r#"SELECT id, cove_id, title, sort, archived_at, pinned_at, lifecycle, cwd, terminal_at, created_at, updated_at
               FROM waves WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(wave) = wave else {
            return Ok(None);
        };

        let cards = sqlx::query_as::<_, Card>(
            r#"SELECT id, wave_id, kind, sort, payload, deletable, created_at, updated_at
               FROM cards WHERE wave_id = ?1 ORDER BY sort ASC"#,
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;

        // Overlays scoped to this wave or any of its cards. One query: a
        // wave-scoped row plus an IN-list on card ids built at the SQL level
        // using a `cards` subquery so we avoid a parameter explosion.
        let overlays = sqlx::query_as::<_, Overlay>(
            r#"SELECT id, plugin_id, entity_kind, entity_id, kind, payload, updated_at
               FROM overlays
               WHERE (entity_kind = 'wave' AND entity_id = ?1)
                  OR (entity_kind = 'card'
                      AND entity_id IN (SELECT id FROM cards WHERE wave_id = ?1))"#,
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(Some(WaveDetail {
            wave,
            cards,
            overlays,
        }))
    }

    // ---------------------------------------------------------------- cards
    async fn cards_by_wave(&self, wave_id: &str) -> Result<Vec<Card>> {
        let rows = sqlx::query_as::<_, Card>(
            r#"SELECT id, wave_id, kind, sort, payload, deletable, created_at, updated_at
               FROM cards WHERE wave_id = ?1 ORDER BY sort ASC"#,
        )
        .bind(wave_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn card_get(&self, id: &str) -> Result<Option<Card>> {
        let row = sqlx::query_as::<_, Card>(
            r#"SELECT id, wave_id, kind, sort, payload, deletable, created_at, updated_at
               FROM cards WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn card_role_get(&self, id: &str) -> Result<Option<CardRole>> {
        let row: Option<(CardRole,)> = sqlx::query_as("SELECT role FROM cards WHERE id = ?1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|(role,)| role))
    }

    async fn card_codex_thread_get_by_thread(
        &self,
        thread_id: &str,
    ) -> Result<Option<CardCodexThreadRow>> {
        let row = sqlx::query_as::<_, (String, String, CardRole, Option<String>, i64, i64)>(
            r#"SELECT thread_id, card_id, role, wave_id, created_at, updated_at
               FROM card_codex_threads
               WHERE thread_id = ?1"#,
        )
        .bind(thread_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(
            |(thread_id, card_id, role, wave_id, created_at, updated_at)| CardCodexThreadRow {
                thread_id,
                card_id,
                role,
                wave_id,
                created_at,
                updated_at,
            },
        ))
    }

    async fn card_codex_thread_get_by_card(
        &self,
        card_id: &str,
    ) -> Result<Option<CardCodexThreadRow>> {
        let row = sqlx::query_as::<_, (String, String, CardRole, Option<String>, i64, i64)>(
            r#"SELECT thread_id, card_id, role, wave_id, created_at, updated_at
               FROM card_codex_threads
               WHERE card_id = ?1"#,
        )
        .bind(card_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(
            |(thread_id, card_id, role, wave_id, created_at, updated_at)| CardCodexThreadRow {
                thread_id,
                card_id,
                role,
                wave_id,
                created_at,
                updated_at,
            },
        ))
    }

    async fn card_codex_threads_active(&self) -> Result<Vec<CardCodexThreadRow>> {
        let rows = sqlx::query_as::<_, (String, String, CardRole, Option<String>, i64, i64)>(
            r#"SELECT thread_id, card_id, role, wave_id, created_at, updated_at
               FROM card_codex_threads
               ORDER BY created_at ASC, card_id ASC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(thread_id, card_id, role, wave_id, created_at, updated_at)| CardCodexThreadRow {
                    thread_id,
                    card_id,
                    role,
                    wave_id,
                    created_at,
                    updated_at,
                },
            )
            .collect())
    }

    async fn card_codex_threads_active_shared_only(&self) -> Result<Vec<CardCodexThreadRow>> {
        // Deprecated fallback for PR2b's runtime-first read switch. New
        // callers should prefer `RuntimeRepo::runtime_active_shared_thread_attribution`.
        let rows = sqlx::query_as::<_, (String, String, CardRole, Option<String>, i64, i64)>(
            r#"SELECT ct.thread_id,
                      ct.card_id,
                      ct.role,
                      ct.wave_id,
                      ct.created_at,
                      ct.updated_at
               FROM card_codex_threads ct
               JOIN cards c ON c.id = ct.card_id
               WHERE COALESCE(json_extract(c.payload, '$.codex_source'), 'legacy') = 'shared'
               ORDER BY ct.created_at ASC, ct.card_id ASC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(thread_id, card_id, role, wave_id, created_at, updated_at)| CardCodexThreadRow {
                    thread_id,
                    card_id,
                    role,
                    wave_id,
                    created_at,
                    updated_at,
                },
            )
            .collect())
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
        let rows = sqlx::query_as::<_, Overlay>(
            r#"SELECT id, plugin_id, entity_kind, entity_id, kind, payload, updated_at
               FROM overlays WHERE entity_kind = ?1 AND entity_id = ?2"#,
        )
        .bind(entity_kind)
        .bind(entity_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn overlays_by_kind(&self, entity_kind: &str) -> Result<Vec<Overlay>> {
        let rows = sqlx::query_as::<_, Overlay>(
            r#"SELECT id, plugin_id, entity_kind, entity_id, kind, payload, updated_at
               FROM overlays WHERE entity_kind = ?1"#,
        )
        .bind(entity_kind)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
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
        // Orphan: this terminal's card has no active runtime, AND the row
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
                   SELECT 1 FROM runtimes r
                   WHERE r.card_id = t.card_id
                     AND r.status IN ('starting', 'running', 'idle', 'turn_pending')
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
        let rows: Vec<(String, String, String, Option<i64>)> = sqlx::query_as(
            r#"SELECT c.id,
                      c.wave_id,
                      r.terminal_run_id,
                      json_extract(c.payload, '$.push_watermark')
               FROM cards c
               JOIN waves w ON w.id = c.wave_id
               JOIN runtimes r ON r.card_id = c.id
                   AND r.kind = 'shared-spec'
                   AND r.thread_id IS NULL
                   AND r.status IN ('starting', 'running', 'idle', 'turn_pending')
               JOIN terminals t ON t.id = r.terminal_run_id
               WHERE c.role = 'spec'
                 AND t.exit_code IS NULL
                 AND COALESCE(t.signal_killed, 0) = 0
                 AND NOT EXISTS (
                       SELECT 1
                         FROM card_codex_threads ct
                        WHERE ct.card_id = c.id
                 )
                 AND w.lifecycle NOT IN ('done', 'canceled', 'failed')
               ORDER BY c.created_at ASC, c.id ASC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(card_id, wave_id, terminal_id, watermark)| {
                (card_id, wave_id, terminal_id, watermark.unwrap_or(0))
            })
            .collect())
    }

    async fn legacy_spec_cards_for_boot_cleanup(&self) -> Result<Vec<Card>> {
        let rows = sqlx::query_as::<_, Card>(
            r#"SELECT c.id,
                      c.wave_id,
                      c.kind,
                      c.sort,
                      c.payload,
                      c.deletable,
                      c.created_at,
                      c.updated_at
               FROM cards c
               JOIN waves w ON w.id = c.wave_id
               WHERE c.role = 'spec'
                 AND NOT EXISTS (
                       SELECT 1
                         FROM runtimes r
                        WHERE r.card_id = c.id
                          AND r.kind = 'shared-spec'
                          AND r.status IN ('starting', 'running', 'idle', 'turn_pending')
                 )
                 AND w.lifecycle NOT IN ('done', 'canceled', 'failed')
               ORDER BY c.created_at ASC, c.id ASC"#,
        )
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
}

#[async_trait]
impl RuntimeRepo for SqlxRepo {
    async fn runtime_get_active_by_thread(
        &self,
        provider: AgentProvider,
        thread_id: &str,
    ) -> RuntimeResult<Option<CardRuntime>> {
        runtime_get_active_by_thread_from_pool(&self.pool, provider, thread_id).await
    }

    async fn runtime_get_active_for_card(
        &self,
        card_id: &crate::runtime_repo::CardId,
    ) -> RuntimeResult<Option<CardRuntime>> {
        runtime_get_active_for_card_from_pool(&self.pool, card_id).await
    }

    async fn runtime_get_projectable_for_card(
        &self,
        card_id: &crate::runtime_repo::CardId,
    ) -> RuntimeResult<Option<CardRuntime>> {
        runtime_get_projectable_for_card_from_pool(&self.pool, card_id).await
    }

    async fn runtime_get_projectable_for_cards(
        &self,
        card_ids: &[crate::runtime_repo::CardId],
    ) -> RuntimeResult<HashMap<crate::runtime_repo::CardId, CardRuntime>> {
        runtime_get_projectable_for_cards_from_pool(&self.pool, card_ids).await
    }

    async fn runtime_active_shared_thread_attribution(
        &self,
    ) -> RuntimeResult<Vec<(String, String)>> {
        runtime_active_shared_thread_attribution_from_pool(&self.pool).await
    }

    async fn runtimes_active_for_kind(&self, kind: RuntimeKind) -> RuntimeResult<Vec<CardRuntime>> {
        runtimes_active_for_kind_from_pool(&self.pool, kind).await
    }

    async fn runtime_get_by_id(&self, id: &RuntimeId) -> RuntimeResult<Option<CardRuntime>> {
        runtime_get_by_id_from_pool(&self.pool, id).await
    }

    async fn runtime_start_tx(
        &self,
        tx: &mut RuntimeTx<'_>,
        init: RuntimeInit,
    ) -> RuntimeResult<CardRuntime> {
        runtime_start_tx(tx, init).await
    }

    async fn runtime_supersede_tx(
        &self,
        tx: &mut RuntimeTx<'_>,
        id: &RuntimeId,
        new_init: RuntimeInit,
    ) -> RuntimeResult<CardRuntime> {
        runtime_supersede_tx(tx, id, new_init).await
    }

    async fn runtime_set_status_tx(
        &self,
        tx: &mut RuntimeTx<'_>,
        id: &RuntimeId,
        status: RunStatus,
    ) -> RuntimeResult<()> {
        runtime_set_status_tx(tx, id, status).await
    }

    async fn runtime_set_status_for_card(
        &self,
        card_id: &str,
        status: RunStatus,
    ) -> RuntimeResult<()> {
        let mut tx = self.pool.begin().await?;
        runtime_set_status_for_card_tx(&mut tx, card_id, status).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn runtime_set_status_for_card_tx(
        &self,
        tx: &mut RuntimeTx<'_>,
        card_id: &str,
        status: RunStatus,
    ) -> RuntimeResult<()> {
        runtime_set_status_for_card_tx(tx, card_id, status).await
    }

    async fn runtime_bind_attribution_tx(
        &self,
        tx: &mut RuntimeTx<'_>,
        id: &RuntimeId,
        attr: ThreadAttribution,
    ) -> RuntimeResult<()> {
        runtime_bind_attribution_tx(tx, id, attr).await
    }

    async fn runtime_clear_terminal_run_id_tx(
        &self,
        tx: &mut RuntimeTx<'_>,
        id: &RuntimeId,
    ) -> RuntimeResult<()> {
        runtime_clear_terminal_run_id_tx(tx, id).await
    }

    async fn runtime_set_handle_state_tx(
        &self,
        tx: &mut RuntimeTx<'_>,
        id: &RuntimeId,
        state: Option<serde_json::Value>,
    ) -> RuntimeResult<()> {
        runtime_set_handle_state_tx(tx, id, state).await
    }

    async fn runtime_set_active_turn_tx(
        &self,
        tx: &mut RuntimeTx<'_>,
        id: &RuntimeId,
        turn_id: Option<&str>,
    ) -> RuntimeResult<()> {
        runtime_set_active_turn_tx(tx, id, turn_id).await
    }

    async fn runtime_complete_tx(
        &self,
        tx: &mut RuntimeTx<'_>,
        id: &RuntimeId,
        terminal_status: RunStatus,
    ) -> RuntimeResult<()> {
        runtime_complete_tx(tx, id, terminal_status).await
    }

    async fn runtime_complete_for_card(
        &self,
        card_id: &str,
        terminal_status: RunStatus,
    ) -> RuntimeResult<()> {
        let mut tx = self.pool.begin().await?;
        runtime_complete_for_card_tx(&mut tx, card_id, terminal_status).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn runtime_complete_for_card_tx(
        &self,
        tx: &mut RuntimeTx<'_>,
        card_id: &str,
        terminal_status: RunStatus,
    ) -> RuntimeResult<()> {
        runtime_complete_for_card_tx(tx, card_id, terminal_status).await
    }

    async fn runtime_complete_for_terminal(
        &self,
        terminal_id: &str,
        terminal_status: RunStatus,
    ) -> RuntimeResult<()> {
        let mut tx = self.pool.begin().await?;
        runtime_complete_for_terminal_tx(&mut tx, terminal_id, terminal_status).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn runtime_complete_for_terminal_tx(
        &self,
        tx: &mut RuntimeTx<'_>,
        terminal_id: &str,
        terminal_status: RunStatus,
    ) -> RuntimeResult<()> {
        runtime_complete_for_terminal_tx(tx, terminal_id, terminal_status).await
    }

    async fn runtimes_recover_orphans_on_boot(&self) -> RuntimeResult<Vec<CardRuntime>> {
        let now = now_ms();
        let stale_before = now - 60_000;
        let rows = sqlx::query(
            r#"SELECT id, card_id, kind, agent_provider, status, terminal_run_id,
                      thread_id, session_id, active_turn_id, handle_state_json,
                      lease_owner, lease_until_ms, created_at_ms, updated_at_ms,
                      completed_at_ms
               FROM runtimes
               WHERE status IN ('starting', 'running', 'idle', 'turn_pending')
                 AND (lease_until_ms IS NULL OR lease_until_ms < ?1)
                 AND updated_at_ms < ?2
               ORDER BY updated_at_ms ASC, created_at_ms ASC, id ASC"#,
        )
        .bind(now)
        .bind(stale_before)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(card_runtime_from_row).collect()
    }
}

// ---------------------------------------------------------------------------
// RepoSyncDomainRaw — raw entity writes for the in-scope sync domain.
// Gated: not reachable via the `RouteRepo` trait object that handlers see;
// only callable via the explicit `AppState::raw_repo()` escape hatch.
// ---------------------------------------------------------------------------

#[async_trait]
impl RepoSyncDomainRaw for SqlxRepo {
    // ---------------------------------------------------------------- coves
    async fn cove_create(&self, p: NewCove) -> Result<Cove> {
        let mut tx = self.pool.begin().await?;
        let out = cove_create_tx(&mut tx, p).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn cove_update(&self, id: &str, p: CovePatch) -> Result<Cove> {
        let mut tx = self.pool.begin().await?;
        let out = cove_update_tx(&mut tx, id, p).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn cove_delete(&self, id: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        overlay_delete_subtree_by_cove_tx(&mut tx, id).await?;
        overlay_delete_by_entity_tx(&mut tx, "cove", id).await?;
        cove_delete_tx(&mut tx, id).await?;
        tx.commit().await?;
        Ok(())
    }

    // ---------------------------------------------------------------- waves
    async fn wave_create(&self, p: NewWave) -> Result<Wave> {
        let mut tx = self.pool.begin().await?;
        let out = wave_create_tx(&mut tx, p, &self.wave_cove_cache).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn wave_update(&self, id: &str, p: WavePatch) -> Result<Wave> {
        let mut tx = self.pool.begin().await?;
        let out = wave_update_tx(&mut tx, id, p).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn wave_delete(&self, id: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        overlay_delete_card_overlays_by_wave_tx(&mut tx, id).await?;
        overlay_delete_by_entity_tx(&mut tx, "wave", id).await?;
        overlay_delete_by_entity_tx(&mut tx, "view", id).await?;
        wave_delete_tx(&mut tx, id, &self.wave_cove_cache).await?;
        tx.commit().await?;
        Ok(())
    }

    // ---------------------------------------------------------------- cards
    async fn card_create(&self, p: NewCard) -> Result<Card> {
        let mut tx = self.pool.begin().await?;
        let out = card_create_tx(&mut tx, p, &self.card_role_cache).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn card_update(&self, id: &str, p: CardPatch) -> Result<Card> {
        let mut tx = self.pool.begin().await?;
        let out = card_update_tx(&mut tx, id, p).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn card_delete(&self, id: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        card_delete_tx(&mut tx, id, &self.card_role_cache).await?;
        tx.commit().await?;
        Ok(())
    }

    // -------------------------------------------------------------- overlays
    async fn overlay_upsert(&self, p: NewOverlay) -> Result<Overlay> {
        let mut tx = self.pool.begin().await?;
        let out = overlay_upsert_tx(&mut tx, p).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn overlay_delete(
        &self,
        plugin_id: &str,
        entity_kind: &str,
        entity_id: &str,
        kind: &str,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        overlay_delete_tx(&mut tx, plugin_id, entity_kind, entity_id, kind).await?;
        tx.commit().await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// RepoOutOfDomain — operational writes that intentionally bypass the event
// log: terminal lifecycle, plugin install/config, app-global settings. See
// db/mod.rs module doc for the sync-domain vs. out-of-domain split.
// ---------------------------------------------------------------------------

#[async_trait]
impl RepoOutOfDomain for SqlxRepo {
    // ------------------------------------------------------------- terminals
    async fn terminal_create(&self, p: NewTerminal) -> Result<Terminal> {
        // Parent card must exist; surface as NotFound to mirror MockRepo.
        let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM cards WHERE id = ?1")
            .bind(&p.card_id)
            .fetch_optional(&self.pool)
            .await?;
        if exists.is_none() {
            return Err(CalmError::NotFound(format!("card {}", p.card_id)));
        }
        // Per-card uniqueness — surface as Conflict to mirror MockRepo
        // (the schema also enforces this via UNIQUE on terminals.card_id).
        let dup: Option<(String,)> = sqlx::query_as("SELECT id FROM terminals WHERE card_id = ?1")
            .bind(&p.card_id)
            .fetch_optional(&self.pool)
            .await?;
        if dup.is_some() {
            return Err(CalmError::Conflict(format!(
                "terminal already exists for card {}",
                p.card_id
            )));
        }

        let now = now_ms();
        let id = new_id();
        let env_text = serde_json::to_string(&p.env)?;
        // #177 — render theme RGB once at row-creation; persisted in
        // comma-decimal form so every spawn-path read is a zero-alloc
        // string slice.
        let theme_fg = p.theme.fg_arg();
        let theme_bg = p.theme.bg_arg();
        sqlx::query(
            r#"INSERT INTO terminals
                   (id, card_id, program, cwd, env, pid, theme_fg, theme_bg, created_at)
               VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?8)"#,
        )
        .bind(&id)
        .bind(&p.card_id)
        .bind(&p.program)
        .bind(&p.cwd)
        .bind(&env_text)
        .bind(&theme_fg)
        .bind(&theme_bg)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(Terminal {
            id,
            card_id: p.card_id,
            program: p.program,
            cwd: p.cwd,
            env: p.env,
            pid: None,
            theme_fg,
            theme_bg,
            exit_code: None,
            signal_killed: false,
            created_at: now,
        })
    }

    async fn terminal_set_pid(&self, id: &str, pid: Option<u32>) -> Result<()> {
        // Cast to i64 for sqlite's INTEGER affinity; u32 is well within range.
        let pid_i64: Option<i64> = pid.map(|p| p as i64);
        let res = sqlx::query("UPDATE terminals SET pid = ?1 WHERE id = ?2")
            .bind(pid_i64)
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("terminal {id}")));
        }
        Ok(())
    }

    async fn terminal_set_exit(
        &self,
        id: &str,
        exit_code: Option<i32>,
        signal_killed: bool,
    ) -> Result<()> {
        // #306 — single UPDATE; the two columns are written together so
        // a reader never sees a mismatched intermediate state. The
        // mutual-exclusion invariant (signal_killed=true ⇒ exit_code=None)
        // is the writer's responsibility — see daemon `spawn_child_waiter`.
        let res =
            sqlx::query("UPDATE terminals SET exit_code = ?1, signal_killed = ?2 WHERE id = ?3")
                .bind(exit_code)
                .bind(if signal_killed { 1_i64 } else { 0_i64 })
                .bind(id)
                .execute(&self.pool)
                .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("terminal {id}")));
        }
        Ok(())
    }

    async fn terminal_clear_exit_for_spawn(&self, id: &str) -> Result<()> {
        let res = sqlx::query(
            "UPDATE terminals SET pid = NULL, exit_code = NULL, signal_killed = 0 WHERE id = ?1",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("terminal {id}")));
        }
        Ok(())
    }

    async fn terminal_delete(&self, id: &str) -> Result<()> {
        let res = sqlx::query("DELETE FROM terminals WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("terminal {id}")));
        }
        Ok(())
    }

    async fn spec_card_set_push_watermark(&self, card_id: &str, watermark: i64) -> Result<()> {
        // JSON merge so we only touch `payload.push_watermark` — never clobber
        // `appserver_sock` / any other field.
        // `json_set(p, '$.k', v)` upserts the key in place.
        // A missing row is silently a no-op (the wave was deleted between
        // the dispatcher's bump and the persist; nothing to do).
        //
        // #313 problem #1 round-3 (N1) — MONOTONIC GUARD via the WHERE
        // clause. Two persisters can race to this UPDATE:
        //
        //   * `Dispatcher::push_to_spec` on a successful `Issued` —
        //     persists `max_envelope_id` (highest id of the just-issued
        //     coalesced turn), under the per-wave push lock.
        //   * The consumer task's `flush_push_queue` via the installed
        //     [`WatermarkSink`] — persists the max id from the drained
        //     queue, NOT under the same lock (the queue lock is
        //     different).
        //
        // Both happen during normal operation. If `flush_push_queue`'s
        // SQL is slow enough that a later `Issued` for a higher
        // `max_envelope_id` lands AND persists FIRST, an unguarded
        // `json_set` here could LOWER the stored watermark — a regression
        // that boot catch-up would then mistake for "we never delivered
        // ids in [old, new]" and re-push.
        //
        // The `CASE` preserves the stored watermark when it is already
        // at-or-above the proposed one. SQLite evaluates the WHERE and CASE
        // atomically under the same row lock, so the read-modify-write race
        // is closed.
        let now = now_ms();
        let _ = sqlx::query(
            r#"UPDATE cards
                  SET payload    = json_set(
                                      COALESCE(payload, '{}'),
                                      '$.push_watermark',
                                      CASE
                                          WHEN COALESCE(json_extract(payload, '$.push_watermark'), 0) < ?1
                                          THEN ?1
                                          ELSE COALESCE(json_extract(payload, '$.push_watermark'), 0)
                                      END
                                   ),
                      updated_at = ?2
                WHERE id = ?3
                  AND COALESCE(json_extract(payload, '$.push_watermark'), 0) < ?1"#,
        )
        .bind(watermark)
        .bind(now)
        .bind(card_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn card_codex_thread_upsert(
        &self,
        card_id: &str,
        thread_id: &str,
        role: CardRole,
        wave_id: Option<&str>,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        card_codex_thread_upsert_tx(&mut tx, card_id, thread_id, role, wave_id).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn card_codex_thread_delete_by_card(&self, card_id: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        card_codex_thread_delete_by_card_tx(&mut tx, card_id).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn shared_daemon_runtime_set(&self, update: SharedCodexDaemonUpdate) -> Result<()> {
        let now = now_ms();
        let start_time = update
            .process_start_time
            .and_then(|v| i64::try_from(v).ok());
        sqlx::query(
            r#"INSERT INTO shared_codex_daemon
                   (id, state, pid, pgid, sock_path, codex_home_path, process_start_time,
                    boot_id, started_at, updated_at, restart_count, last_error,
                    daemon_env_signature)
               VALUES
                   (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                    CASE WHEN ?10 THEN 1 ELSE 0 END, ?11, ?12)
               ON CONFLICT(id) DO UPDATE SET
                   state = excluded.state,
                   pid = excluded.pid,
                   pgid = excluded.pgid,
                   sock_path = excluded.sock_path,
                   codex_home_path = excluded.codex_home_path,
                   process_start_time = excluded.process_start_time,
                   boot_id = excluded.boot_id,
                   started_at = excluded.started_at,
                   updated_at = excluded.updated_at,
                   restart_count = shared_codex_daemon.restart_count
                       + CASE WHEN ?10 THEN 1 ELSE 0 END,
                   last_error = excluded.last_error,
                   daemon_env_signature = excluded.daemon_env_signature"#,
        )
        .bind(&update.state)
        .bind(update.pid)
        .bind(update.pgid)
        .bind(&update.sock_path)
        .bind(&update.codex_home_path)
        .bind(start_time)
        .bind(&update.boot_id)
        .bind(update.started_at)
        .bind(now)
        .bind(update.increment_restart_count)
        .bind(&update.last_error)
        .bind(&update.daemon_env_signature)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn shared_daemon_record_event(&self, action: &str, error: Option<&str>) -> Result<()> {
        let now = now_ms();
        let last_error = error.map(|e| format!("{action}: {e}"));
        sqlx::query(
            r#"UPDATE shared_codex_daemon
                  SET updated_at = ?1,
                      last_error = COALESCE(?2, last_error)
                WHERE id = 1"#,
        )
        .bind(now)
        .bind(last_error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---- spec push queue (#318 INV-3 / R2-B1) ---------------------------

    async fn spec_card_enqueue_observation(
        &self,
        card_id: &str,
        envelope_id: i64,
        text: &str,
    ) -> Result<i64> {
        // Persist-first: the caller (`SpecPusher::push_observation`)
        // INSERTs here BEFORE pushing the in-memory `VecDeque` entry and
        // BEFORE returning `Ok(PushOutcome::Enqueued)`, so a crash
        // between the persist and the in-memory push leaves a row that
        // boot-takeover's `spec_card_queued_observations` can rehydrate.
        //
        // The FK (`card_id REFERENCES cards(id) ON DELETE CASCADE`) is
        // enforced by `PRAGMA foreign_keys = ON` (set per-connection in
        // `SqlxRepo::open`); an INSERT against a non-existent card_id
        // fails with `SQLITE_CONSTRAINT_FOREIGNKEY` rather than silently
        // orphaning a row.
        // #325 — no `enqueued_at`/wall-clock column on `spec_push_queue`:
        // nothing reads it (FIFO is established by the AUTOINCREMENT `id`),
        // so persisting a `now_ms()` per row was dead bytes. See the
        // migration header for the followup story.
        let row = sqlx::query(
            r#"INSERT INTO spec_push_queue (card_id, envelope_id, text)
               VALUES (?1, ?2, ?3)
               RETURNING id"#,
        )
        .bind(card_id)
        .bind(envelope_id)
        .bind(text)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<i64, _>("id"))
    }

    async fn spec_card_queued_observations(
        &self,
        card_id: &str,
    ) -> Result<Vec<(i64, i64, String)>> {
        // Ordered by id so the caller's rehydrated in-memory queue
        // preserves the original enqueue order. The composite index
        // `idx_spec_push_queue_card_id_id` covers this scan.
        let rows = sqlx::query(
            r#"SELECT id, envelope_id, text
                 FROM spec_push_queue
                WHERE card_id = ?1
                ORDER BY id ASC"#,
        )
        .bind(card_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                (
                    r.get::<i64, _>("id"),
                    r.get::<i64, _>("envelope_id"),
                    r.get::<String, _>("text"),
                )
            })
            .collect())
    }

    async fn spec_card_dequeue_observations(&self, ids: &[i64]) -> Result<()> {
        // Empty-input fast path so callers don't have to special-case
        // "nothing drained".
        if ids.is_empty() {
            return Ok(());
        }
        // Variadic `?, ?, …` placeholder list. The queue is per-card and
        // batch sizes are bounded by what fit into one coalesced
        // `turn/start` (small — observations are wave events), so the
        // dynamic SQL stays well under any `SQLITE_MAX_COMPOUND_SELECT`
        // / parameter-count limit. A single batched DELETE keeps the
        // flush path to one round-trip.
        let placeholders = std::iter::repeat_n("?", ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("DELETE FROM spec_push_queue WHERE id IN ({placeholders})");
        let mut q = sqlx::query(&sql);
        for id in ids {
            q = q.bind(id);
        }
        let _ = q.execute(&self.pool).await?;
        Ok(())
    }

    // --------------------------------------------------------------- plugins
    async fn plugin_install(&self, p: NewPlugin) -> Result<Plugin> {
        let manifest_text = serde_json::to_string(&p.manifest)?;
        let user_config_text = serde_json::to_string(&p.user_config)?;
        let now = now_ms();
        let row = sqlx::query_as::<_, Plugin>(
            r#"INSERT INTO plugins
                   (id, version, install_path, manifest, enabled, user_config,
                    installed_at, updated_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
               ON CONFLICT(id) DO UPDATE SET
                   version      = excluded.version,
                   install_path = excluded.install_path,
                   manifest     = excluded.manifest,
                   enabled      = excluded.enabled,
                   user_config  = excluded.user_config,
                   updated_at   = excluded.updated_at
               RETURNING id, version, install_path, manifest, enabled, user_config,
                         installed_at, updated_at"#,
        )
        .bind(&p.id)
        .bind(&p.version)
        .bind(&p.install_path)
        .bind(&manifest_text)
        .bind(p.enabled)
        .bind(&user_config_text)
        .bind(now)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    async fn plugin_update_enabled(&self, id: &str, enabled: bool) -> Result<Plugin> {
        let now = now_ms();
        let res = sqlx::query(r#"UPDATE plugins SET enabled = ?1, updated_at = ?2 WHERE id = ?3"#)
            .bind(enabled)
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("plugin {id}")));
        }
        self.plugin_get_by_id(id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))
    }

    async fn plugin_update_user_config(
        &self,
        id: &str,
        user_config: serde_json::Value,
    ) -> Result<Plugin> {
        let now = now_ms();
        let user_config_text = serde_json::to_string(&user_config)?;
        let res =
            sqlx::query(r#"UPDATE plugins SET user_config = ?1, updated_at = ?2 WHERE id = ?3"#)
                .bind(&user_config_text)
                .bind(now)
                .bind(id)
                .execute(&self.pool)
                .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("plugin {id}")));
        }
        self.plugin_get_by_id(id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))
    }

    async fn plugin_update_manifest(
        &self,
        id: &str,
        manifest: serde_json::Value,
    ) -> Result<Plugin> {
        let now = now_ms();
        let manifest_text = serde_json::to_string(&manifest)?;
        let res = sqlx::query(r#"UPDATE plugins SET manifest = ?1, updated_at = ?2 WHERE id = ?3"#)
            .bind(&manifest_text)
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("plugin {id}")));
        }
        self.plugin_get_by_id(id)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))
    }

    async fn plugin_delete(&self, id: &str) -> Result<()> {
        let res = sqlx::query("DELETE FROM plugins WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("plugin {id}")));
        }
        Ok(())
    }

    async fn overlays_clear_by_plugin(&self, plugin_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM overlays WHERE plugin_id = ?1")
            .bind(plugin_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn plugin_kv_clear(&self, plugin_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM plugin_kv WHERE plugin_id = ?1")
            .bind(plugin_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // -------------------------------------------------------- plugin tokens
    async fn plugin_token_set(
        &self,
        plugin_id: &str,
        hashed_token: &str,
        expires_at: i64,
    ) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO plugin_tokens (plugin_id, hashed_token, expires_at)
               VALUES (?1, ?2, ?3)
               ON CONFLICT(plugin_id) DO UPDATE SET
                   hashed_token = excluded.hashed_token,
                   expires_at   = excluded.expires_at"#,
        )
        .bind(plugin_id)
        .bind(hashed_token)
        .bind(expires_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn plugin_token_delete(&self, plugin_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM plugin_tokens WHERE plugin_id = ?1")
            .bind(plugin_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // -------------------------------------------------------- plugin kv
    async fn plugin_kv_set(
        &self,
        plugin_id: &str,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<()> {
        let text = serde_json::to_string(value)?;
        let now = now_ms();
        sqlx::query(
            r#"INSERT INTO plugin_kv (plugin_id, key, value, updated_at)
               VALUES (?1, ?2, ?3, ?4)
               ON CONFLICT(plugin_id, key) DO UPDATE SET
                   value      = excluded.value,
                   updated_at = excluded.updated_at"#,
        )
        .bind(plugin_id)
        .bind(key)
        .bind(&text)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn plugin_kv_delete(&self, plugin_id: &str, key: &str) -> Result<()> {
        sqlx::query("DELETE FROM plugin_kv WHERE plugin_id = ?1 AND key = ?2")
            .bind(plugin_id)
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // -------------------------------------------------------------- settings
    async fn settings_upsert(&self, key: &str, value: &str) -> Result<()> {
        let now = now_ms();
        sqlx::query(
            r#"INSERT INTO settings (key, value, updated_at)
               VALUES (?1, ?2, ?3)
               ON CONFLICT(key) DO UPDATE SET
                   value      = excluded.value,
                   updated_at = excluded.updated_at"#,
        )
        .bind(key)
        .bind(value)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn settings_delete(&self, key: &str) -> Result<()> {
        sqlx::query("DELETE FROM settings WHERE key = ?1")
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ----------------------------------------------------- cove_folders
    async fn cove_folder_create(&self, cove_id: &str, path: &str) -> Result<CoveFolder> {
        // Parent cove must exist; surface as NotFound to mirror the
        // terminal_create precedent above (FK error message would be
        // less actionable for the REST caller).
        let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM coves WHERE id = ?1")
            .bind(cove_id)
            .fetch_optional(&self.pool)
            .await?;
        if exists.is_none() {
            return Err(CalmError::NotFound(format!("cove {cove_id}")));
        }
        let now = now_ms();
        // The UNIQUE constraint on `path` is the backstop here. The
        // route layer has already done equality / ancestor / descendant
        // conflict detection so a real-world INSERT failing the
        // UNIQUE is a race (concurrent claim of the same path). Bubble
        // it up as the generic Conflict so the surface is honest.
        let res =
            sqlx::query("INSERT INTO cove_folders (cove_id, path, created_at) VALUES (?1, ?2, ?3)")
                .bind(cove_id)
                .bind(path)
                .bind(now)
                .execute(&self.pool)
                .await;
        match res {
            Ok(out) => Ok(CoveFolder {
                id: out.last_insert_rowid(),
                cove_id: cove_id.to_string().into(),
                path: path.to_string(),
                created_at: now,
            }),
            Err(sqlx::Error::Database(dbe)) if dbe.message().contains("UNIQUE") => Err(
                CalmError::Conflict(format!("cove_folders.path already claims `{path}`")),
            ),
            Err(e) => Err(e.into()),
        }
    }

    async fn cove_folder_delete(&self, id: i64) -> Result<()> {
        let res = sqlx::query("DELETE FROM cove_folders WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("cove_folder {id}")));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// RepoEventWrite — the eventized write path. Every public write that the
// sync engine cares about lands here: `write_with_event` (atomic entity-
// write + event-log), `log_pure_event` (entity-less event log), and the
// `events_*` cursor queries used by replay.
// ---------------------------------------------------------------------------

#[allow(deprecated)]
#[async_trait]
impl RepoEventWrite for SqlxRepo {
    async fn write_with_event(
        &self,
        actor: ActorId,
        scope: EventScope,
        correlation: Option<&str>,
        bus: &EventBus,
        write: &crate::state::WriteContext,
        f: WriteWithEventFn<'_>,
    ) -> Result<i64> {
        let mut tx = self.pool.begin().await?;
        // Run the caller-supplied entity write.
        let fut: BoxFuture<'_, Result<Event>> = f(&mut tx);
        let event = match fut.await {
            Ok(ev) => ev,
            Err(e) => {
                // Rollback is implicit on `tx` drop, but be explicit so the
                // intent reads clearly.
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        // PR3 (#136) — authorization gate. Runs after the closure
        // produces an event so the closure can mint per-row roles
        // (e.g. `card_create_with_id_tx` writes through the cache)
        // before the gate checks them. Violations roll back: no
        // entity write, no event row, no broadcast.
        if let Err(violation) = crate::role_gate::enforce_role(
            &actor,
            &event,
            &scope,
            write.role_cache(),
            write.cove_cache(),
        ) {
            let _ = tx.rollback().await;
            return Err(CalmError::Forbidden(violation.to_string()));
        }
        // Persist the event in the same txn.
        let event_id =
            match Self::event_append_in_tx(&mut tx, &actor, &scope, correlation, &event).await {
                Ok(id) => id,
                Err(e) => {
                    let _ = tx.rollback().await;
                    return Err(e);
                }
            };
        // Commit before any externally-visible side effect.
        tx.commit().await?;
        // Commit-then-emit invariant: now (and only now) do we broadcast.
        bus.emit_envelope(BroadcastEnvelope {
            id: event_id,
            event_version: SYNC_EVENT_VERSION,
            actor,
            scope,
            event,
        });
        Ok(event_id)
    }

    async fn write_with_events(
        &self,
        actor: ActorId,
        correlation: Option<&str>,
        bus: &EventBus,
        write: &crate::state::WriteContext,
        f: WriteWithEventsFn<'_>,
    ) -> Result<Vec<i64>> {
        let mut tx = self.pool.begin().await?;
        // Run the caller-supplied entity write — closure returns one
        // or more (scope, event) pairs for this tx.
        let fut: BoxFuture<'_, Result<Vec<(EventScope, Event)>>> = f(&mut tx);
        let events = match fut.await {
            Ok(v) => v,
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        // Contract: at least one event per tx. An empty vec is a
        // caller bug — refuse to commit so the closure's writes
        // disappear with the rollback.
        if events.is_empty() {
            let _ = tx.rollback().await;
            return Err(CalmError::Internal(
                "write_with_events: closure returned an empty event batch".into(),
            ));
        }
        // PR3 (#136) — authorization gate, per event. The cache is
        // already write-through for any role insert the closure
        // performed, so a wave-create-with-spec-card batch can mint
        // the spec card in the closure and immediately have its
        // role visible to the `WaveUpdated` enforce_role call below.
        for (scope, event) in &events {
            if let Err(violation) = crate::role_gate::enforce_role(
                &actor,
                event,
                scope,
                write.role_cache(),
                write.cove_cache(),
            ) {
                let _ = tx.rollback().await;
                return Err(CalmError::Forbidden(violation.to_string()));
            }
        }
        // Persist every event in the same txn, in order.
        let mut event_ids: Vec<i64> = Vec::with_capacity(events.len());
        for (scope, event) in &events {
            match Self::event_append_in_tx(&mut tx, &actor, scope, correlation, event).await {
                Ok(id) => event_ids.push(id),
                Err(e) => {
                    let _ = tx.rollback().await;
                    return Err(e);
                }
            }
        }
        // Commit before any externally-visible side effect.
        tx.commit().await?;
        // Commit-then-emit invariant: broadcast in the same order the
        // closure produced.
        for (id, (scope, event)) in event_ids.iter().zip(events) {
            bus.emit_envelope(BroadcastEnvelope {
                id: *id,
                event_version: SYNC_EVENT_VERSION,
                actor: actor.clone(),
                scope,
                event,
            });
        }
        Ok(event_ids)
    }

    async fn log_pure_event(
        &self,
        actor: ActorId,
        scope: EventScope,
        correlation: Option<&str>,
        bus: &EventBus,
        card_role_cache: &CardRoleCache,
        wave_cove_cache: &WaveCoveCache,
        event: Event,
    ) -> Result<i64> {
        let mut tx = self.pool.begin().await?;
        // PR3 (#136) — gate. Pure events don't have an entity write to
        // populate the cache from, so the role lookup uses the cache's
        // current contents. `log_pure_event` callers (codex hook
        // ingest, plugin state transitions) always supply a real actor
        // identity; the gate's defense-in-depth checks (empty
        // CardId, unknown card) still apply.
        if let Err(violation) =
            crate::role_gate::enforce_role(&actor, &event, &scope, card_role_cache, wave_cove_cache)
        {
            let _ = tx.rollback().await;
            return Err(CalmError::Forbidden(violation.to_string()));
        }
        let event_id =
            match Self::event_append_in_tx(&mut tx, &actor, &scope, correlation, &event).await {
                Ok(id) => id,
                Err(e) => {
                    let _ = tx.rollback().await;
                    return Err(e);
                }
            };
        tx.commit().await?;
        bus.emit_envelope(BroadcastEnvelope {
            id: event_id,
            event_version: SYNC_EVENT_VERSION,
            actor,
            scope,
            event,
        });
        Ok(event_id)
    }

    /// Issue #310 — event-less tx wrapper. Runs the caller-supplied
    /// closure inside one sqlx transaction; commits on `Ok(())`, rolls
    /// back on `Err(_)`. No event row is appended to the `events` log;
    /// no broadcast is emitted. The caller is responsible for
    /// broadcasting any downstream event via `log_pure_event` after
    /// this returns. See [`crate::db::WriteInTxFn`] for the rationale.
    async fn write_in_tx(&self, f: WriteInTxFn<'_>) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let fut: BoxFuture<'_, Result<()>> = f(&mut tx);
        match fut.await {
            Ok(()) => {}
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        }
        tx.commit().await?;
        Ok(())
    }

    async fn events_since(
        &self,
        since_id: i64,
        limit: Option<i64>,
    ) -> Result<Vec<(i64, u32, EventScope, Event)>> {
        // `LIMIT -1` is sqlite's "no limit" sentinel; using `?` binding lets
        // us keep one SQL string regardless of caller intent. Callers that
        // pass `None` want every row > since_id.
        let cap = limit.unwrap_or(-1);
        // `event_version` is selected so the replay path can stamp the
        // envelope with the version persisted on the row, not the current
        // `SYNC_EVENT_VERSION` constant — old rows that predate migration
        // 0006 backfill to `1` via the column default, and any future row
        // written under a newer envelope schema must round-trip its own
        // version, not the kernel's.
        //
        // `scope_*` columns (migration 0007) reconstruct the typed
        // `EventScope`. Rows that predate the migration carry
        // `scope_kind='system'` (column default) with NULL ancestor cols,
        // which `EventScope::from_row` collapses to `EventScope::System`.
        // The same fallback covers any malformed row whose declared
        // `scope_kind` doesn't line up with its ancestor cols — replay
        // never strands a client on a malformed scope.
        type ScopeRow = (
            i64,            // id
            String,         // kind
            String,         // payload
            u32,            // event_version
            Option<String>, // scope_kind
            Option<String>, // scope_cove
            Option<String>, // scope_wave
            Option<String>, // scope_card
        );
        let rows: Vec<ScopeRow> = sqlx::query_as(
            r#"SELECT id, kind, payload, event_version,
                      scope_kind, scope_cove, scope_wave, scope_card
               FROM events
               WHERE id > ?1
               ORDER BY id ASC
               LIMIT ?2"#,
        )
        .bind(since_id)
        .bind(cap)
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for (id, kind, payload_text, event_version, sk, sc, sw, scard) in rows {
            let payload: serde_json::Value = match serde_json::from_str(&payload_text) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(
                        id, kind = %kind, error = %e,
                        "events_since: skipping row with malformed payload JSON",
                    );
                    continue;
                }
            };
            let scope = EventScope::from_row(
                sk.as_deref(),
                sc.as_deref(),
                sw.as_deref(),
                scard.as_deref(),
            );
            match Event::from_kind_and_payload(&kind, payload) {
                Ok(ev) => out.push((id, event_version, scope, ev)),
                Err(e) => {
                    tracing::error!(
                        id, kind = %kind, error = %e,
                        "events_since: skipping row that no longer matches Event enum",
                    );
                }
            }
        }
        Ok(out)
    }

    async fn events_for_wave(&self, wave_id: &str, kinds: &[&str]) -> Result<Vec<WaveEvent>> {
        if kinds.is_empty() {
            return Ok(Vec::new());
        }

        type ScopeRow = (
            i64,            // id
            String,         // kind
            String,         // payload
            String,         // actor
            i64,            // at
            Option<String>, // scope_kind
            Option<String>, // scope_cove
            Option<String>, // scope_wave
            Option<String>, // scope_card
        );

        let mut query = QueryBuilder::<Sqlite>::new(
            r#"SELECT id, kind, payload, actor, at,
                      scope_kind, scope_cove, scope_wave, scope_card
               FROM events
               WHERE scope_wave = "#,
        );
        query.push_bind(wave_id);
        query.push(" AND kind IN (");
        let mut separated = query.separated(", ");
        for kind in kinds {
            separated.push_bind(*kind);
        }
        separated.push_unseparated(") ORDER BY id ASC");

        let rows: Vec<ScopeRow> = query.build_query_as().fetch_all(&self.pool).await?;

        let mut out = Vec::with_capacity(rows.len());
        for (id, kind, payload_text, actor_text, at, sk, sc, sw, scard) in rows {
            let payload: serde_json::Value = match serde_json::from_str(&payload_text) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(
                        id, kind = %kind, error = %e,
                        "events_for_wave: skipping row with malformed payload JSON",
                    );
                    continue;
                }
            };
            let actor: ActorId = match serde_json::from_str(&actor_text) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(
                        id, kind = %kind, error = %e,
                        "events_for_wave: skipping row with malformed actor JSON",
                    );
                    continue;
                }
            };
            let scope = EventScope::from_row(
                sk.as_deref(),
                sc.as_deref(),
                sw.as_deref(),
                scard.as_deref(),
            );
            match Event::from_kind_and_payload(&kind, payload) {
                Ok(event) => out.push(WaveEvent {
                    id,
                    at,
                    actor,
                    scope,
                    event,
                }),
                Err(e) => {
                    tracing::error!(
                        id, kind = %kind, error = %e,
                        "events_for_wave: skipping row that no longer matches Event enum",
                    );
                }
            }
        }
        Ok(out)
    }

    async fn events_earliest_id(&self) -> Result<Option<i64>> {
        // `MIN(id)` over an empty table returns a single `NULL` row. Reading
        // the column as `Option<i64>` surfaces that as `None`; non-empty
        // tables return `Some(min)`.
        let row: (Option<i64>,) = sqlx::query_as("SELECT MIN(id) FROM events")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0)
    }

    async fn events_latest_id(&self) -> Result<Option<i64>> {
        // Mirror of `events_earliest_id`: `MAX(id)` over an empty table
        // returns a single `NULL` row, surfaced as `None` here. Used by
        // the WS handler to detect a client cursor that's ahead of the
        // server's actual log tip (see the `events_latest_id` trait
        // docstring for the reset detection contract). Issue #290.
        let row: (Option<i64>,) = sqlx::query_as("SELECT MAX(id) FROM events")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0)
    }

    async fn events_latest_id_for_wave(&self, wave_id: &str) -> Result<Option<i64>> {
        // `MAX(id) … WHERE scope_wave = ?1`. Filters on the dedicated
        // column added in migration 0007. Rows whose `scope_kind` is
        // `'card'` are included automatically — the `EventScope::from_row`
        // contract puts the card's parent wave in `scope_wave` for any
        // card-scoped event, so the dispatcher's catch-up filter and this
        // query agree on "events in scope for this wave".
        //
        // `MAX()` over an empty filtered set returns one `NULL` row,
        // surfaced as `None` here — the call site maps that to `0` (the
        // `events.id` "no row" sentinel) on the
        // `SpecPushAbandoned.last_envelope_id` payload.
        let row: (Option<i64>,) =
            sqlx::query_as("SELECT MAX(id) FROM events WHERE scope_wave = ?1")
                .bind(wave_id)
                .fetch_one(&self.pool)
                .await?;
        Ok(row.0)
    }
}
