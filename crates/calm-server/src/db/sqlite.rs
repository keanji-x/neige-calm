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

use super::{RepoEventWrite, RepoOutOfDomain, RepoRead, RepoSyncDomainRaw, WriteWithEventFn};
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, EventBus};
use crate::model::*;

pub struct SqlxRepo {
    pool: SqlitePool,
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

        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(|e| CalmError::Internal(format!("migrate: {e}")))?;

        Ok(Self { pool })
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

    /// **Private.** The raw events-table insert. Lives off the trait per
    /// design doc §1.4: only `Repo::write_with_event` and
    /// `Repo::log_pure_event` may reach this path, so the commit-then-emit
    /// invariant is unbypassable from the route / plugin host layers.
    ///
    /// Returns the auto-incremented row id, which is then stamped onto
    /// the `BroadcastEnvelope` the wrapper emits on the bus.
    async fn event_append_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        actor: &str,
        correlation: Option<&str>,
        event: &Event,
    ) -> Result<i64> {
        let kind = event.kind_tag();
        let payload = event.payload_value();
        let payload_text = serde_json::to_string(&payload)?;
        let at = now_ms();
        let row = sqlx::query(
            r#"INSERT INTO events (kind, payload, actor, at, correlation)
               VALUES (?1, ?2, ?3, ?4, ?5)
               RETURNING id"#,
        )
        .bind(kind)
        .bind(&payload_text)
        .bind(actor)
        .bind(at)
        .bind(correlation)
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
        actor: &str,
        correlation: Option<&str>,
        event: &Event,
    ) -> Result<i64> {
        let mut tx = self.pool.begin().await?;
        let id = Self::event_append_in_tx(&mut tx, actor, correlation, event).await?;
        tx.commit().await?;
        Ok(id)
    }
}

// ---- helpers -----------------------------------------------------------------

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
    sqlx::query(
        r#"INSERT INTO coves (id, name, color, sort, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6)"#,
    )
    .bind(&id)
    .bind(&p.name)
    .bind(&p.color)
    .bind(sort)
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(Cove {
        id,
        name: p.name,
        color: p.color,
        sort,
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
        r#"SELECT id, name, color, sort, created_at, updated_at
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
        None => next_sort_scoped_in_tx(tx, "waves", "WHERE cove_id = ?1", Some(&p.cove_id)).await?,
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
        id,
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

pub async fn card_create_tx(tx: &mut Transaction<'_, Sqlite>, p: NewCard) -> Result<Card> {
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM waves WHERE id = ?1")
        .bind(&p.wave_id)
        .fetch_optional(&mut **tx)
        .await?;
    if exists.is_none() {
        return Err(CalmError::NotFound(format!("wave {}", p.wave_id)));
    }

    let sort = match p.sort {
        Some(s) => s,
        None => next_sort_scoped_in_tx(tx, "cards", "WHERE wave_id = ?1", Some(&p.wave_id)).await?,
    };
    let now = now_ms();
    let id = new_id();
    let payload_text = serde_json::to_string(&p.payload)?;
    sqlx::query(
        r#"INSERT INTO cards (id, wave_id, kind, sort, payload, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"#,
    )
    .bind(&id)
    .bind(&p.wave_id)
    .bind(&p.kind)
    .bind(sort)
    .bind(&payload_text)
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(Card {
        id,
        wave_id: p.wave_id,
        kind: p.kind,
        sort,
        payload: p.payload,
        created_at: now,
        updated_at: now,
    })
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

pub async fn card_delete_tx(tx: &mut Transaction<'_, Sqlite>, id: &str) -> Result<()> {
    let res = sqlx::query("DELETE FROM cards WHERE id = ?1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    if res.rows_affected() == 0 {
        return Err(CalmError::NotFound(format!("card {id}")));
    }
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
            r#"SELECT id, name, color, sort, created_at, updated_at
               FROM coves ORDER BY sort ASC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn cove_get(&self, id: &str) -> Result<Option<Cove>> {
        let row = sqlx::query_as::<_, Cove>(
            r#"SELECT id, name, color, sort, created_at, updated_at
               FROM coves WHERE id = ?1"#,
        )
        .bind(id)
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
        let out = card_create_tx(&mut tx, p).await?;
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
        card_delete_tx(&mut tx, id).await?;
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
        actor: &str,
        correlation: Option<&str>,
        bus: &EventBus,
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
        // Persist the event in the same txn.
        let event_id = match Self::event_append_in_tx(&mut tx, actor, correlation, &event).await {
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
            actor: actor.to_string(),
            event,
        });
        Ok(event_id)
    }

    async fn log_pure_event(
        &self,
        actor: &str,
        correlation: Option<&str>,
        bus: &EventBus,
        event: Event,
    ) -> Result<i64> {
        let mut tx = self.pool.begin().await?;
        let event_id = match Self::event_append_in_tx(&mut tx, actor, correlation, &event).await {
            Ok(id) => id,
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        tx.commit().await?;
        bus.emit_envelope(BroadcastEnvelope {
            id: event_id,
            actor: actor.to_string(),
            event,
        });
        Ok(event_id)
    }

    async fn events_since(&self, since_id: i64, limit: Option<i64>) -> Result<Vec<(i64, Event)>> {
        // `LIMIT -1` is sqlite's "no limit" sentinel; using `?` binding lets
        // us keep one SQL string regardless of caller intent. Callers that
        // pass `None` want every row > since_id.
        let cap = limit.unwrap_or(-1);
        let rows: Vec<(i64, String, String)> = sqlx::query_as(
            r#"SELECT id, kind, payload
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
        for (id, kind, payload_text) in rows {
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
            match Event::from_kind_and_payload(&kind, payload) {
                Ok(ev) => out.push((id, ev)),
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
