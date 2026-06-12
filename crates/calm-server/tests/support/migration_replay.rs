//! Snapshot migration-replay harness — PR0-D of #679.
//!
//! Acceptance gate shared by the #679 sequence (PR2 migration-chain move,
//! PR7 `root_session_id` backfill, PR9b retirement, PR11 historical
//! replay): any DB staged at an arbitrary historical schema version and
//! replayed to head must end up **structurally identical** to a freshly
//! created DB, and representative seeded data must survive the trip.
//!
//! Building blocks (each usable on its own):
//!
//!   * [`stage_db_at`]      — apply migrations, in order, up to version N
//!     against a temp sqlite file. Uses the same `sqlx::migrate::Migrate`
//!     machinery production boot uses (`db/sqlite.rs` `migrator.run()`),
//!     so checksums recorded while staging are byte-identical to what
//!     [`replay_to_head`]'s `Migrator::run` later validates.
//!   * [`seed`]             — version-aware fixture seeding from JSON.
//!     Rows declare a column superset; columns that don't exist at the
//!     staged version are filtered out, rows for tables that don't exist
//!     (yet / anymore) are skipped, and `min_version` / `max_version`
//!     gates handle value-level constraints that changed over time
//!     (e.g. `cards.role = 'plain'` is trigger-rejected from 0037 on).
//!   * [`replay_to_head`]   — run the remaining migrations exactly like
//!     production boot does.
//!   * [`schema_fingerprint`] / [`assert_schema_matches`] — full
//!     structural diff (tables, columns, indexes, triggers, views, FKs)
//!     against a fresh `Migrator::run` DB, with a per-object diff report
//!     on mismatch.
//!
//! External prod snapshots: the `migration_replay_harness.rs` test reads
//! `NEIGE_SNAPSHOT_DB` (path to a — sanitized — production sqlite file),
//! copies it into a tempdir, replays it to head and runs the same schema
//! diff. When the env var is unset the test self-skips with an explicit
//! marker, following the codex-e2e self-skip pattern.

use sqlx::migrate::{Migrate, Migrator};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The production migration chain, embedded at compile time. Must stay
/// pointed at the same dir as `db/sqlite.rs`'s `sqlx::migrate!`.
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// Every applicable (up) migration version, ascending.
pub fn migration_versions() -> Vec<i64> {
    let mut versions: Vec<i64> = MIGRATOR
        .iter()
        .filter(|m| !m.migration_type.is_down_migration())
        .map(|m| m.version)
        .collect();
    versions.sort_unstable();
    versions
}

/// Open (creating if missing) a sqlite file with the same pragmas the
/// production pool uses (FK enforcement on, WAL).
pub async fn open_sqlite(path: &Path) -> SqlitePool {
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true);
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap_or_else(|e| panic!("open sqlite at {}: {e}", path.display()))
}

/// Apply migrations in version order up to and including `version`,
/// recording them in `_sqlx_migrations` exactly as `Migrator::run` would
/// (same checksum bookkeeping, same per-migration transaction semantics).
pub async fn stage_db_at(pool: &SqlitePool, version: i64) {
    let mut conn = pool.acquire().await.expect("acquire conn");
    conn.ensure_migrations_table()
        .await
        .expect("ensure _sqlx_migrations");
    let mut migrations: Vec<_> = MIGRATOR
        .iter()
        .filter(|m| !m.migration_type.is_down_migration())
        .collect();
    migrations.sort_by_key(|m| m.version);
    for migration in migrations {
        if migration.version > version {
            break;
        }
        conn.apply(migration)
            .await
            .unwrap_or_else(|e| panic!("staging migration {} failed: {e}", migration.version));
    }
}

/// Apply every remaining migration, exactly like production boot
/// (`db/sqlite.rs`): `Migrator::run` validates the checksums recorded
/// while staging, then applies what's missing.
pub async fn replay_to_head(pool: &SqlitePool) {
    MIGRATOR
        .run(pool)
        .await
        .expect("replay to head via Migrator::run");
}

// ---------------------------------------------------------------------------
// Fixture seeding
// ---------------------------------------------------------------------------

/// JSON fixture: `{ "tables": [ { "table": "...", "rows": [ ... ] } ] }`.
/// Tables are seeded in declaration order (FK parents first). Each row is
/// `{ "min_version"?: N, "max_version"?: N, "columns": { col: value } }`.
#[derive(serde::Deserialize)]
pub struct Fixture {
    pub tables: Vec<TableFixture>,
}

#[derive(serde::Deserialize)]
pub struct TableFixture {
    pub table: String,
    pub rows: Vec<RowFixture>,
}

#[derive(serde::Deserialize)]
pub struct RowFixture {
    /// Skip this row when staging strictly below this version (e.g. the
    /// value only became expressible / legal at that version).
    #[serde(default)]
    pub min_version: Option<i64>,
    /// Skip this row when staging strictly above this version (e.g. the
    /// value is rejected by a CHECK/trigger introduced later).
    #[serde(default)]
    pub max_version: Option<i64>,
    pub columns: serde_json::Map<String, serde_json::Value>,
}

impl Fixture {
    pub fn from_json(json: &str) -> Self {
        serde_json::from_str(json).expect("parse migration-replay fixture JSON")
    }
}

/// One successfully seeded row, addressable for post-replay survival
/// checks via its primary-key column.
pub struct SeededRow {
    pub table: String,
    pub pk_col: String,
    pub pk: serde_json::Value,
}

pub struct SeedReport {
    pub rows: Vec<SeededRow>,
}

async fn table_exists(pool: &SqlitePool, table: &str) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
    )
    .bind(table)
    .fetch_one(pool)
    .await
    .expect("query sqlite_master")
        > 0
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

struct ColumnInfo {
    name: String,
    pk: bool,
}

async fn table_columns(pool: &SqlitePool, table: &str) -> Vec<ColumnInfo> {
    sqlx::query(&format!("PRAGMA table_info({})", quote_ident(table)))
        .fetch_all(pool)
        .await
        .expect("PRAGMA table_info")
        .into_iter()
        .map(|row| ColumnInfo {
            name: row.try_get::<String, _>("name").unwrap(),
            pk: row.try_get::<i64, _>("pk").unwrap() > 0,
        })
        .collect()
}

fn bind_json_value<'q>(
    query: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    value: &'q serde_json::Value,
) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
    match value {
        serde_json::Value::Null => query.bind(None::<String>),
        serde_json::Value::Bool(b) => query.bind(i64::from(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                query.bind(i)
            } else {
                query.bind(n.as_f64().expect("numeric fixture value"))
            }
        }
        serde_json::Value::String(s) => query.bind(s.as_str()),
        // Nested JSON: store its serialization (TEXT JSON columns).
        other => query.bind(other.to_string()),
    }
}

/// Seed the fixture into a DB staged at `staged_version`. Tables that
/// don't exist at that version are skipped wholesale; per-row version
/// gates and missing columns are filtered as documented on [`Fixture`].
pub async fn seed(pool: &SqlitePool, fixture: &Fixture, staged_version: i64) -> SeedReport {
    let mut seeded = Vec::new();
    for table_fixture in &fixture.tables {
        let table = table_fixture.table.as_str();
        if !table_exists(pool, table).await {
            continue;
        }
        let columns = table_columns(pool, table).await;
        let column_names: BTreeSet<&str> = columns.iter().map(|c| c.name.as_str()).collect();
        let pk_col = columns
            .iter()
            .find(|c| c.pk)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| panic!("fixture table {table} has no primary key column"));

        for row in &table_fixture.rows {
            if row.min_version.is_some_and(|min| staged_version < min)
                || row.max_version.is_some_and(|max| staged_version > max)
            {
                continue;
            }
            let cols: Vec<(&String, &serde_json::Value)> = row
                .columns
                .iter()
                .filter(|(name, _)| column_names.contains(name.as_str()))
                .collect();
            assert!(
                !cols.is_empty(),
                "fixture row for {table} has no columns present at v{staged_version}"
            );
            let pk = row.columns.get(&pk_col).unwrap_or_else(|| {
                panic!("fixture row for {table} must provide pk column {pk_col}")
            });
            let col_list = cols
                .iter()
                .map(|(name, _)| quote_ident(name))
                .collect::<Vec<_>>()
                .join(", ");
            let placeholders = (1..=cols.len())
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "INSERT INTO {} ({col_list}) VALUES ({placeholders})",
                quote_ident(table)
            );
            let mut query = sqlx::query(&sql);
            for (_, value) in &cols {
                query = bind_json_value(query, value);
            }
            query.execute(pool).await.unwrap_or_else(|e| {
                panic!("seed {table} pk={pk} at v{staged_version} failed: {e}")
            });
            seeded.push(SeededRow {
                table: table.to_string(),
                pk_col: pk_col.clone(),
                pk: pk.clone(),
            });
        }
    }
    SeedReport { rows: seeded }
}

/// Assert every seeded row whose table still exists at head is still
/// present (by primary key). Tables retired by later migrations (e.g.
/// `spec_push_queue` at 0033, `card_codex_threads` at 0034) are skipped —
/// the schema diff already proves they were dropped.
pub async fn assert_rows_survive(pool: &SqlitePool, report: &SeedReport, context: &str) {
    let mut checked = 0usize;
    for row in &report.rows {
        if !table_exists(pool, &row.table).await {
            continue;
        }
        let sql = format!(
            "SELECT COUNT(*) FROM {} WHERE {} = ?1",
            quote_ident(&row.table),
            quote_ident(&row.pk_col)
        );
        let count: i64 = bind_json_value(sqlx::query(&sql), &row.pk)
            .fetch_one(pool)
            .await
            .map(|r| r.get::<i64, _>(0))
            .unwrap_or_else(|e| panic!("[{context}] survival query on {} failed: {e}", row.table));
        assert_eq!(
            count, 1,
            "[{context}] seeded row {}.{} = {} did not survive replay to head",
            row.table, row.pk_col, row.pk
        );
        checked += 1;
    }
    assert!(checked > 0, "[{context}] survival check covered no rows");
}

// ---------------------------------------------------------------------------
// Schema fingerprint + diff
// ---------------------------------------------------------------------------

fn normalize_sql(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Structural description of every schema object: `type:name` →
/// normalized DDL plus (for tables) the column / FK / index structure
/// from the pragma interface. Internal `sqlite_*` objects (autoindexes,
/// sqlite_sequence) are excluded.
pub async fn schema_fingerprint(pool: &SqlitePool) -> BTreeMap<String, String> {
    let mut fingerprint = BTreeMap::new();
    let objects = sqlx::query(
        "SELECT type, name, sql FROM sqlite_master \
         WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
    )
    .fetch_all(pool)
    .await
    .expect("read sqlite_master");

    for object in objects {
        let kind: String = object.try_get("type").unwrap();
        let name: String = object.try_get("name").unwrap();
        let sql: Option<String> = object.try_get("sql").unwrap();
        let mut description = match &sql {
            Some(sql) => format!("sql: {}", normalize_sql(sql)),
            None => "sql: <none>".to_string(),
        };

        if kind == "table" {
            let mut columns = Vec::new();
            for col in sqlx::query(&format!("PRAGMA table_info({})", quote_ident(&name)))
                .fetch_all(pool)
                .await
                .expect("PRAGMA table_info")
            {
                columns.push(format!(
                    "{} type={} notnull={} default={:?} pk={}",
                    col.try_get::<String, _>("name").unwrap(),
                    col.try_get::<String, _>("type").unwrap(),
                    col.try_get::<i64, _>("notnull").unwrap(),
                    col.try_get::<Option<String>, _>("dflt_value").unwrap(),
                    col.try_get::<i64, _>("pk").unwrap(),
                ));
            }
            let mut fks = Vec::new();
            for fk in sqlx::query(&format!("PRAGMA foreign_key_list({})", quote_ident(&name)))
                .fetch_all(pool)
                .await
                .expect("PRAGMA foreign_key_list")
            {
                fks.push(format!(
                    "{} -> {}({:?}) on_update={} on_delete={}",
                    fk.try_get::<String, _>("from").unwrap(),
                    fk.try_get::<String, _>("table").unwrap(),
                    fk.try_get::<Option<String>, _>("to").unwrap(),
                    fk.try_get::<String, _>("on_update").unwrap(),
                    fk.try_get::<String, _>("on_delete").unwrap(),
                ));
            }
            fks.sort();
            description.push_str(&format!("\ncolumns: [{}]", columns.join("; ")));
            description.push_str(&format!("\nforeign_keys: [{}]", fks.join("; ")));
        }

        fingerprint.insert(format!("{kind}:{name}"), description);
    }
    fingerprint
}

/// Build the reference fingerprint: a brand-new DB taken straight to head
/// by the production `Migrator::run` path.
pub async fn fresh_head_fingerprint() -> BTreeMap<String, String> {
    let dir = tempfile::tempdir().expect("tempdir for fresh head DB");
    let pool = open_sqlite(&dir.path().join("fresh.sqlite")).await;
    replay_to_head(&pool).await;
    let fingerprint = schema_fingerprint(&pool).await;
    pool.close().await;
    fingerprint
}

/// Structural diff. Panics with a per-object report (missing / extra /
/// differing) when the replayed schema deviates from the fresh one.
pub fn assert_schema_matches(
    replayed: &BTreeMap<String, String>,
    fresh: &BTreeMap<String, String>,
    context: &str,
) {
    let mut diffs = Vec::new();
    for (key, fresh_desc) in fresh {
        match replayed.get(key) {
            None => diffs.push(format!(
                "missing after replay: {key}\n  fresh: {fresh_desc}"
            )),
            Some(replayed_desc) if replayed_desc != fresh_desc => diffs.push(format!(
                "differs: {key}\n  fresh:    {fresh_desc}\n  replayed: {replayed_desc}"
            )),
            Some(_) => {}
        }
    }
    for (key, replayed_desc) in replayed {
        if !fresh.contains_key(key) {
            diffs.push(format!(
                "extra after replay: {key}\n  replayed: {replayed_desc}"
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "[{context}] replayed schema != fresh schema ({} difference(s)):\n{}",
        diffs.len(),
        diffs.join("\n")
    );
}

/// Convenience wrapper: fingerprint `pool` and diff it against a freshly
/// built head DB.
pub async fn assert_schema_matches_fresh(pool: &SqlitePool, context: &str) {
    let fresh = fresh_head_fingerprint().await;
    let replayed = schema_fingerprint(pool).await;
    assert_schema_matches(&replayed, &fresh, context);
}
