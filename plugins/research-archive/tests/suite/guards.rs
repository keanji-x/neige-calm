//! I2: every table is append-only at the file level, for ordinary SQL on a
//! connection with SQLite's default trigger and check settings.
//!
//! Each test runs one statement against an existing row of one table on a
//! raw connection (foreign keys off), first with `recursive_triggers` OFF and
//! then ON, and asserts the guard's own error (or, for `blob`, a silent
//! no-op) and a byte-identical file.

use crate::support::{dump, populated, raw};

/// The result a statement must have.
enum Expect {
    /// Refused by the guard whose message contains this text.
    Abort(&'static str),
    /// Accepted and ignored by the `blob_dedup` guard.
    Ignored,
}

async fn assert_guarded(sql: &str, expect: Expect) {
    let (_dir, path) = populated().await;
    for recursive in [false, true] {
        let mut conn = raw(&path, recursive).await;
        let before = dump(&mut conn).await;
        let result = sqlx::query(sql).execute(&mut conn).await;
        match (&expect, result) {
            (Expect::Abort(text), Err(sqlx::Error::Database(e))) => assert!(
                e.message().contains(text),
                "recursive_triggers={recursive}: {sql}: {}",
                e.message()
            ),
            (Expect::Ignored, Ok(done)) => assert_eq!(done.rows_affected(), 0, "{sql}"),
            (_, other) => panic!("recursive_triggers={recursive}: {sql}: {other:?}"),
        }
        assert_eq!(
            dump(&mut conn).await,
            before,
            "recursive_triggers={recursive}: {sql} changed the file"
        );
    }
}

macro_rules! guarded {
    ($($name:ident: $sql:expr => $expect:expr;)*) => {$(
        #[tokio::test]
        async fn $name() {
            assert_guarded($sql, $expect).await;
        }
    )*};
}

const ENDPOINT_ROW: &str = "('consensus', 'GET', '/x', '[]', '[]', NULL, 'security')";
const BLOB_ROW: &str = "((SELECT MIN(sha256) FROM blob), X'00')";
const RUN_ROW: &str = "(1, 'longbridge', '2026-01-02', 0, 1, 0)";
const RUN_KEY_ROW: &str = "(1, 'AAPL.US', 'config')";
const MEMBERSHIP_ROW: &str = "(1, '.SPX.US', 'none', NULL, 0)";
const OBSERVATION_ROW: &str =
    "(1, 'AAPL.US', 'consensus', 1, 0, 'transient', NULL, 'transport', 'request_timeout')";
const RUN_END_ROW: &str = "(1, 0, 'partial', 'x')";

guarded! {
    i2_endpoint_update: "UPDATE endpoint SET path = '/x'"
        => Expect::Abort("endpoint is append-only");
    i2_endpoint_delete: "DELETE FROM endpoint"
        => Expect::Abort("endpoint is append-only");
    i2_endpoint_insert_or_replace: &format!("INSERT OR REPLACE INTO endpoint VALUES {ENDPOINT_ROW}")
        => Expect::Abort("endpoint row exists");
    i2_endpoint_on_conflict_do_update: &format!(
        "INSERT INTO endpoint VALUES {ENDPOINT_ROW}
         ON CONFLICT (endpoint_id) DO UPDATE SET path = excluded.path")
        => Expect::Abort("endpoint row exists");

    i2_blob_update: "UPDATE blob SET body = X'00'"
        => Expect::Abort("blob is append-only");
    i2_blob_delete: "DELETE FROM blob"
        => Expect::Abort("blob is append-only");
    i2_blob_insert_or_replace: &format!("INSERT OR REPLACE INTO blob VALUES {BLOB_ROW}")
        => Expect::Ignored;
    i2_blob_on_conflict_do_update: &format!(
        "INSERT INTO blob VALUES {BLOB_ROW}
         ON CONFLICT (sha256) DO UPDATE SET body = excluded.body")
        => Expect::Ignored;
    i2_blob_duplicate_insert_keeps_the_original: &format!("INSERT INTO blob VALUES {BLOB_ROW}")
        => Expect::Ignored;

    i2_run_update: "UPDATE run SET started_at = started_at + 1"
        => Expect::Abort("run is append-only");
    i2_run_delete: "DELETE FROM run"
        => Expect::Abort("run is append-only");
    i2_run_insert_or_replace: &format!("INSERT OR REPLACE INTO run VALUES {RUN_ROW}")
        => Expect::Abort("run row exists");
    i2_run_on_conflict_do_update: &format!(
        "INSERT INTO run VALUES {RUN_ROW}
         ON CONFLICT (run_id) DO UPDATE SET started_at = excluded.started_at")
        => Expect::Abort("run row exists");
    i2_run_replace_by_unique_day: "INSERT OR REPLACE INTO run
         VALUES (99, 'longbridge', '2026-01-02', 0, 1, 0)"
        => Expect::Abort("run row exists");

    i2_run_key_update: "UPDATE run_key SET key = key || 'x'"
        => Expect::Abort("run_key is append-only");
    i2_run_key_delete: "DELETE FROM run_key"
        => Expect::Abort("run_key is append-only");
    i2_run_key_insert_or_replace: &format!("INSERT OR REPLACE INTO run_key VALUES {RUN_KEY_ROW}")
        => Expect::Abort("run_key row exists");
    i2_run_key_on_conflict_do_update: &format!(
        "INSERT INTO run_key VALUES {RUN_KEY_ROW}
         ON CONFLICT (run_id, key, origin) DO UPDATE SET key = 'ZZZ.US'")
        => Expect::Abort("run_key row exists");

    i2_run_membership_update: "UPDATE run_membership SET frozen_at = frozen_at + 1"
        => Expect::Abort("run_membership is append-only");
    i2_run_membership_delete: "DELETE FROM run_membership"
        => Expect::Abort("run_membership is append-only");
    i2_run_membership_insert_or_replace: &format!(
        "INSERT OR REPLACE INTO run_membership VALUES {MEMBERSHIP_ROW}")
        => Expect::Abort("run_membership row exists");
    i2_run_membership_on_conflict_do_update: &format!(
        "INSERT INTO run_membership VALUES {MEMBERSHIP_ROW}
         ON CONFLICT (run_id, index_sym) DO UPDATE SET frozen_at = excluded.frozen_at")
        => Expect::Abort("run_membership row exists");

    i2_observation_update: "UPDATE observation SET fetched_at = fetched_at + 1"
        => Expect::Abort("observation is append-only");
    i2_observation_delete: "DELETE FROM observation"
        => Expect::Abort("observation is append-only");
    i2_observation_insert_or_replace: &format!(
        "INSERT OR REPLACE INTO observation VALUES {OBSERVATION_ROW}")
        => Expect::Abort("observation row exists");
    i2_observation_on_conflict_do_update: &format!(
        "INSERT INTO observation VALUES {OBSERVATION_ROW}
         ON CONFLICT (run_id, key, endpoint_id, attempt) DO UPDATE SET status = excluded.status")
        => Expect::Abort("observation row exists");

    i2_run_end_update: "UPDATE run_end SET finished_at = finished_at + 1"
        => Expect::Abort("run_end is append-only");
    i2_run_end_delete: "DELETE FROM run_end"
        => Expect::Abort("run_end is append-only");
    i2_run_end_insert_or_replace: &format!("INSERT OR REPLACE INTO run_end VALUES {RUN_END_ROW}")
        => Expect::Abort("run_end row exists");
    i2_run_end_on_conflict_do_update: &format!(
        "INSERT INTO run_end VALUES {RUN_END_ROW}
         ON CONFLICT (run_id) DO UPDATE SET outcome = excluded.outcome")
        => Expect::Abort("run_end row exists");

    // An explicit rowid that collides with a stored row, under a fresh
    // primary key: the guard checks the key, so a hidden rowid would let
    // REPLACE delete the old row. No table has one.
    i2_endpoint_rowid_collision: "INSERT OR REPLACE INTO endpoint
         (rowid, endpoint_id, method, path, query_json, headers_json, dc_restrict, key_kind)
         VALUES (1, 'fresh', 'GET', '/x', '[]', '[]', NULL, 'security')"
        => Expect::Abort("has no column named rowid");
    i2_blob_rowid_collision: "INSERT OR REPLACE INTO blob (rowid, sha256, body)
         VALUES (1, printf('%064d', 9), X'00')"
        => Expect::Abort("has no column named rowid");
    i2_run_rowid_collision: "INSERT OR REPLACE INTO run
         (rowid, source, as_of, window_start, window_end, started_at)
         VALUES (1, 'longbridge', '2030-01-01', 0, 1, 0)"
        => Expect::Abort("run row exists");
    i2_run_key_rowid_collision: "INSERT OR REPLACE INTO run_key (rowid, run_id, key, origin)
         VALUES (1, 1, 'FRESH.US', 'config')"
        => Expect::Abort("has no column named rowid");
    i2_run_membership_rowid_collision: "INSERT OR REPLACE INTO run_membership
         (rowid, run_id, index_sym, mode, source_run_id, frozen_at)
         VALUES (1, 1, '.FRESH.US', 'none', NULL, 0)"
        => Expect::Abort("has no column named rowid");
    i2_observation_rowid_collision: "INSERT OR REPLACE INTO observation
         (rowid, run_id, key, endpoint_id, attempt, fetched_at, status, sha256, error_class, error_code)
         VALUES (1, 1, 'AAPL.US', 'consensus', 99, 0, 'transient', NULL, 'transport', 'x')"
        => Expect::Abort("has no column named rowid");
    i2_run_end_rowid_collision: "INSERT OR REPLACE INTO run_end (rowid, finished_at, outcome, reason)
         VALUES (1, 0, 'partial', 'x')"
        => Expect::Abort("run_end row exists");
}

/// The guards stop the statements, not the absence of rows: a fresh row in
/// each table is still accepted on the same raw connection.
#[tokio::test]
async fn i2_new_rows_are_still_accepted() {
    let (_dir, path) = populated().await;
    let mut conn = raw(&path, true).await;
    for sql in [
        "INSERT INTO endpoint VALUES ('new', 'GET', '/n', '[]', '[]', NULL, 'index')",
        "INSERT INTO blob VALUES (printf('%064d', 7), X'')",
        "INSERT INTO run VALUES (2, 'longbridge', '2026-01-03', 0, 1, 0)",
        "INSERT INTO run_key VALUES (2, 'NEW.US', 'config')",
        "INSERT INTO run_membership VALUES (2, '.SPX.US', 'none', NULL, 0)",
        "INSERT INTO observation VALUES (2, 'NEW.US', 'consensus', 1, 0, 'permanent', NULL, 'http_status', '404')",
        "INSERT INTO run_end VALUES (2, 0, 'partial', NULL)",
    ] {
        let done = sqlx::query(sql).execute(&mut conn).await.unwrap();
        assert_eq!(done.rows_affected(), 1, "{sql}");
    }
}
