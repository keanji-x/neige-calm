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

use std::str::FromStr;

use async_trait::async_trait;
use futures::future::BoxFuture;
use sqlx::ConnectOptions;
use sqlx::Executor;
use sqlx::Row;
use sqlx::Sqlite;
use sqlx::SqlitePool;
use sqlx::Transaction;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::{
    RepoEventWrite, RepoOutOfDomain, RepoRead, RepoSyncDomainRaw, WriteWithEventFn,
    WriteWithEventsFn,
};
use crate::card_role_cache::CardRoleCache;
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, EventBus, EventScope, SYNC_EVENT_VERSION};
use crate::ids::ActorId;
use crate::model::*;
use crate::validation::{
    CODEX_PAYLOAD_SCHEMA_VERSION, TERMINAL_PAYLOAD_SCHEMA_VERSION, validate_card_payload,
};

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
            // Belt-and-braces: also re-issue the pragma on every fresh
            // connection in case `foreign_keys(true)` is silently dropped
            // for some URL forms (e.g. memory).
            .after_connect(|conn, _meta| {
                Box::pin(async move {
                    conn.execute("PRAGMA foreign_keys = ON;").await?;
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

        Ok(Self {
            pool,
            card_role_cache,
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

pub async fn wave_create_tx(tx: &mut Transaction<'_, Sqlite>, p: NewWave) -> Result<Wave> {
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
    sqlx::query(
        r#"INSERT INTO waves
           (id, cove_id, title, sort, archived_at, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6)"#,
    )
    .bind(&id)
    .bind(&p.cove_id)
    .bind(&p.title)
    .bind(sort)
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(Wave {
        id: id.into(),
        cove_id: p.cove_id,
        title: p.title,
        sort,
        archived_at: None,
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
        r#"SELECT id, cove_id, title, sort, archived_at, created_at, updated_at
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
    w.updated_at = now_ms();

    sqlx::query(
        r#"UPDATE waves SET title = ?1, sort = ?2, archived_at = ?3, updated_at = ?4
           WHERE id = ?5"#,
    )
    .bind(&w.title)
    .bind(w.sort)
    .bind(w.archived_at)
    .bind(w.updated_at)
    .bind(&w.id)
    .execute(&mut **tx)
    .await?;
    Ok(w)
}

pub async fn wave_delete_tx(tx: &mut Transaction<'_, Sqlite>, id: &str) -> Result<()> {
    let res = sqlx::query("DELETE FROM waves WHERE id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    if res.rows_affected() == 0 {
        return Err(CalmError::NotFound(format!("wave {id}")));
    }
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
    sqlx::query(
        r#"INSERT INTO cards (id, wave_id, kind, sort, payload, role, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"#,
    )
    .bind(&id)
    .bind(&p.wave_id)
    .bind(&p.kind)
    .bind(sort)
    .bind(&payload_text)
    .bind(role)
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
    card_role_cache.insert(card_id.clone(), role);
    Ok(Card {
        id: card_id,
        wave_id: p.wave_id,
        kind: p.kind,
        sort,
        payload: p.payload,
        created_at: now,
        updated_at: now,
    })
}

pub async fn card_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    p: NewCard,
    card_role_cache: &CardRoleCache,
) -> Result<Card> {
    card_create_with_id_tx(tx, new_id(), p, CardRole::Plain, card_role_cache).await
}

pub async fn card_update_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    p: CardPatch,
) -> Result<Card> {
    let mut c = sqlx::query_as::<_, Card>(
        r#"SELECT id, wave_id, kind, sort, payload, created_at, updated_at
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
    sqlx::query(
        r#"INSERT INTO terminals
               (id, card_id, program, cwd, env, daemon_handle, pid, created_at)
           VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL, ?6)"#,
    )
    .bind(&id)
    .bind(&p.card_id)
    .bind(&p.program)
    .bind(&p.cwd)
    .bind(&env_text)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(Terminal {
        id,
        card_id: p.card_id,
        program: p.program,
        cwd: p.cwd,
        env: p.env,
        daemon_handle: None,
        pid: None,
        created_at: now,
    })
}

/// Atomically create a `terminal`-kind card AND its associated terminal row
/// inside a single transaction, stamping the terminal id onto the card's
/// payload before returning.
///
/// This is the kernel side of #13's plan to collapse today's 3-step
/// terminal-card recipe (card-add → terminal-create → card-update) into one
/// atomic db helper. PR1 just lands this helper; PR2 will wire it to a new
/// `POST /api/waves/:id/terminal-cards` endpoint and delete the old recipe.
///
/// On any failure the surrounding transaction rolls back, so partial state
/// (card without terminal, or terminal without payload link) is impossible.
#[allow(clippy::too_many_arguments)]
pub async fn card_with_terminal_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: String,
    wave_id: WaveId,
    sort: Option<f64>,
    program: String,
    cwd: String,
    env: serde_json::Value,
    role: CardRole,
    card_role_cache: &CardRoleCache,
) -> Result<(Card, Terminal)> {
    // 1. Card row with placeholder payload — the terminal_id and schemaVersion
    //    are stamped in step 5 once we have the terminal row.
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
        },
    )
    .await?;

    // 3. Build the canonical terminal-card payload.
    let payload = serde_json::json!({
        "schemaVersion": TERMINAL_PAYLOAD_SCHEMA_VERSION,
        "terminal_id": term.id,
    });

    // 4. Defense-in-depth: payload validation. The boundary call in
    //    `routes/cards.rs:141` already enforces this for direct create, but
    //    composing inside the kernel means we run our own check rather than
    //    trusting a payload we built ourselves.
    validate_card_payload("terminal", &payload)?;

    // 5. Re-stamp the card with the real payload.
    let card = card_update_tx(
        tx,
        card.id.as_ref(),
        CardPatch {
            kind: None,
            sort: None,
            payload: Some(payload),
        },
    )
    .await?;

    Ok((card, term))
}

/// Atomically create a `codex`-kind card AND its associated terminal row
/// inside a single transaction, stamping `terminal_id` (+ optional `cwd`)
/// onto the card's payload before returning.
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
/// (card without terminal, or terminal without payload link) is impossible.
/// PR7a (#136) — third return slot is `Some(raw_token)` for Spec/Worker
/// cards, `None` for Plain. The caller is expected to thread the raw
/// value into the codex daemon's `NEIGE_MCP_TOKEN` env var immediately
/// and discard it — the hash is persisted in `card_mcp_tokens`, but the
/// raw form is unrecoverable on a kernel restart (by design).
#[allow(clippy::too_many_arguments)]
pub async fn card_with_codex_create_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: String,
    wave_id: WaveId,
    sort: Option<f64>,
    cwd: String,
    env: serde_json::Value,
    prompt: Option<String>,
    role: CardRole,
    card_role_cache: &CardRoleCache,
) -> Result<(Card, Terminal, Option<String>)> {
    // 1. Card row with placeholder payload — the terminal_id, cwd, and
    //    schemaVersion fields are stamped in step 5 once we have the
    //    terminal row.
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
    payload.insert(
        "terminal_id".into(),
        serde_json::Value::String(term.id.clone()),
    );
    if !cwd.is_empty() {
        payload.insert("cwd".into(), serde_json::Value::String(cwd));
    }
    // `prompt` — surfaces to the `codex_auto_submit` subscriber, which
    // gates auto-Enter on this being a non-empty string. An empty /
    // missing value here is the "user spawned codex without a hands-free
    // prompt" path, identical to pre-#110 behaviour. Trimmed and empty-
    // filtered so the subscriber's `.filter(|s| !s.is_empty())` is the
    // single source of truth.
    if let Some(p) = prompt.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        payload.insert("prompt".into(), serde_json::Value::String(p.to_string()));
    }
    let payload = serde_json::Value::Object(payload);

    // 4. Defense-in-depth: payload validation. The boundary call in
    //    `routes/cards.rs` enforces this for direct create; composing
    //    inside the kernel means we re-run the check on the payload we
    //    just built.
    validate_card_payload("codex", &payload)?;

    // 5. Re-stamp the card with the real payload.
    let card = card_update_tx(
        tx,
        card.id.as_ref(),
        CardPatch {
            kind: None,
            sort: None,
            payload: Some(payload),
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

    Ok((card, term, mcp_token))
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

    // ---------------------------------------------------------------- waves
    async fn waves_by_cove(&self, cove_id: &str) -> Result<Vec<Wave>> {
        let rows = sqlx::query_as::<_, Wave>(
            r#"SELECT id, cove_id, title, sort, archived_at, created_at, updated_at
               FROM waves WHERE cove_id = ?1 ORDER BY sort ASC"#,
        )
        .bind(cove_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn wave_get(&self, id: &str) -> Result<Option<Wave>> {
        let row = sqlx::query_as::<_, Wave>(
            r#"SELECT id, cove_id, title, sort, archived_at, created_at, updated_at
               FROM waves WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn wave_detail(&self, id: &str) -> Result<Option<WaveDetail>> {
        let mut tx = self.pool.begin().await?;
        let wave = sqlx::query_as::<_, Wave>(
            r#"SELECT id, cove_id, title, sort, archived_at, created_at, updated_at
               FROM waves WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(wave) = wave else {
            return Ok(None);
        };

        let cards = sqlx::query_as::<_, Card>(
            r#"SELECT id, wave_id, kind, sort, payload, created_at, updated_at
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
            r#"SELECT id, wave_id, kind, sort, payload, created_at, updated_at
               FROM cards WHERE wave_id = ?1 ORDER BY sort ASC"#,
        )
        .bind(wave_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn card_get(&self, id: &str) -> Result<Option<Card>> {
        let row = sqlx::query_as::<_, Card>(
            r#"SELECT id, wave_id, kind, sort, payload, created_at, updated_at
               FROM cards WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
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
            r#"SELECT id, card_id, program, cwd, env, daemon_handle, pid, created_at
               FROM terminals WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn terminal_get_by_card(&self, card_id: &str) -> Result<Option<Terminal>> {
        let row = sqlx::query_as::<_, Terminal>(
            r#"SELECT id, card_id, program, cwd, env, daemon_handle, pid, created_at
               FROM terminals WHERE card_id = ?1"#,
        )
        .bind(card_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    async fn terminals_orphaned(&self, grace_seconds: i64) -> Result<Vec<Terminal>> {
        // Orphan: no card.payload.terminal_id references this terminal row,
        // AND the row was created more than `grace_seconds` ago (absorbs the
        // 3-step terminal-card create race in `eventBridge.tsx:60-70`).
        //
        // `created_at` is unix ms; the grace bound is `now_ms - grace_seconds * 1000`.
        let cutoff = now_ms() - grace_seconds.saturating_mul(1000);
        let rows = sqlx::query_as::<_, Terminal>(
            r#"SELECT t.id, t.card_id, t.program, t.cwd, t.env,
                      t.daemon_handle, t.pid, t.created_at
               FROM terminals t
               WHERE NOT EXISTS (
                   SELECT 1 FROM cards c
                   WHERE json_extract(c.payload, '$.terminal_id') = t.id
               )
               AND t.created_at < ?1"#,
        )
        .bind(cutoff)
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
        cove_delete_tx(&mut tx, id).await?;
        tx.commit().await?;
        Ok(())
    }

    // ---------------------------------------------------------------- waves
    async fn wave_create(&self, p: NewWave) -> Result<Wave> {
        let mut tx = self.pool.begin().await?;
        let out = wave_create_tx(&mut tx, p).await?;
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
        wave_delete_tx(&mut tx, id).await?;
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
        sqlx::query(
            r#"INSERT INTO terminals
                   (id, card_id, program, cwd, env, daemon_handle, pid, created_at)
               VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL, ?6)"#,
        )
        .bind(&id)
        .bind(&p.card_id)
        .bind(&p.program)
        .bind(&p.cwd)
        .bind(&env_text)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(Terminal {
            id,
            card_id: p.card_id,
            program: p.program,
            cwd: p.cwd,
            env: p.env,
            daemon_handle: None,
            pid: None,
            created_at: now,
        })
    }

    async fn terminal_set_handle(&self, id: &str, handle: Option<&str>) -> Result<()> {
        let res = sqlx::query("UPDATE terminals SET daemon_handle = ?1 WHERE id = ?2")
            .bind(handle)
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(CalmError::NotFound(format!("terminal {id}")));
        }
        Ok(())
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
}

// ---------------------------------------------------------------------------
// RepoEventWrite — the eventized write path. Every public write that the
// sync engine cares about lands here: `write_with_event` (atomic entity-
// write + event-log), `log_pure_event` (entity-less event log), and the
// `events_*` cursor queries used by replay.
// ---------------------------------------------------------------------------

#[async_trait]
impl RepoEventWrite for SqlxRepo {
    async fn write_with_event(
        &self,
        actor: ActorId,
        scope: EventScope,
        correlation: Option<&str>,
        bus: &EventBus,
        card_role_cache: &CardRoleCache,
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
        if let Err(violation) =
            crate::role_gate::enforce_role(&actor, &event, &scope, card_role_cache)
        {
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
        card_role_cache: &CardRoleCache,
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
            if let Err(violation) =
                crate::role_gate::enforce_role(&actor, event, scope, card_role_cache)
            {
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
            crate::role_gate::enforce_role(&actor, &event, &scope, card_role_cache)
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

    async fn events_earliest_id(&self) -> Result<Option<i64>> {
        // `MIN(id)` over an empty table returns a single `NULL` row. Reading
        // the column as `Option<i64>` surfaces that as `None`; non-empty
        // tables return `Some(min)`.
        let row: (Option<i64>,) = sqlx::query_as("SELECT MIN(id) FROM events")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0)
    }
}
