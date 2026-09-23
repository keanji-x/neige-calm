//! 0115 (#1777): `workspace_leases` is rebuilt so its base CHECK admits
//! `'upstream'`. Every row of every CHECK-accepted shape, and every row of the
//! three tables that hang off it by foreign key, survives byte for byte
//! (rowid included); the indexes, the `delivery_policy` CHECK and foreign-key
//! enforcement are what they were.

use calm_truth::MIGRATOR;
use sqlx::{Connection, Row, SqliteConnection, migrate::Migrate, sqlite::SqliteConnectOptions};

const TABLES: [&str; 4] = [
    "workspace_leases",
    "task_git_deliveries",
    "task_git_delivery_abandonments",
    "task_candidates",
];

async fn schema_before_0115() -> SqliteConnection {
    let mut db = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .in_memory(true)
            .foreign_keys(true),
    )
    .await
    .unwrap();
    db.ensure_migrations_table().await.unwrap();
    for migration in MIGRATOR
        .iter()
        .filter(|m| m.version <= 114 && !m.migration_type.is_down_migration())
    {
        db.apply(migration).await.unwrap();
    }
    db
}

/// `rowid` plus every column, from the schema the connection holds now.
async fn columns_of(db: &mut SqliteConnection, table: &str) -> String {
    assert!(TABLES.contains(&table));
    let columns = sqlx::query(&format!("PRAGMA table_info({table})"))
        .fetch_all(&mut *db)
        .await
        .unwrap();
    std::iter::once("rowid".to_string())
        .chain(columns.iter().map(|row| {
            let name: String = row.get("name");
            assert!(name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_'));
            format!("\"{name}\"")
        }))
        .collect::<Vec<_>>()
        .join(",")
}

/// Every row over exactly `columns` (read once, before the upgrade), with
/// each value's storage type beside it, so an INTEGER that came back as TEXT
/// is a difference too.
async fn all_rows(db: &mut SqliteConnection, table: &str, columns: &str) -> Vec<String> {
    assert!(TABLES.contains(&table));
    let typed = columns
        .split(',')
        .map(|column| format!("{column},typeof({column})"))
        .collect::<Vec<_>>()
        .join(",");
    sqlx::query_scalar(&format!(
        "SELECT json_array({typed}) FROM {table} ORDER BY rowid"
    ))
    .fetch_all(db)
    .await
    .unwrap()
}

async fn index_sql(db: &mut SqliteConnection) -> Vec<(String, Option<String>)> {
    sqlx::query_as(
        "SELECT name, sql FROM sqlite_master WHERE type = 'index' AND tbl_name = 'workspace_leases' ORDER BY name",
    )
    .fetch_all(db)
    .await
    .unwrap()
}

async fn exec(db: &mut SqliteConnection, sql: &str) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(sql).execute(db).await.map(|_| ())
}

fn lease(id: &str, path: &str, state: &str, base: &str, policy: &str) -> String {
    format!(
        "INSERT INTO workspace_leases(lease_id,card_id,track_id,path,
         state,lease_owner,lease_until_ms,boot_id,created_at_ms,updated_at_ms,released_at_ms,
            base_sha,base_source,base_attempt_id,canonical_path,git_common_dir,delivery_policy)
         VALUES('{id}','card-{id}','track','{path}','{state}','op-{id}',100,'boot',10,20,NULL,{base},{policy});"
    )
}

#[tokio::test]
async fn upstream_base_rebuild_preserves_rows_references_and_constraints() {
    let mut db = schema_before_0115().await;
    exec(
        &mut db,
        "INSERT INTO areas(id,name,color,sort,created_at,updated_at) VALUES('area','A','red',0,1,2);
         INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) VALUES('track','area','T',0,1,2);",
    )
    .await
    .unwrap();
    // Every shape the 0111 tuple CHECK × 0113 policy CHECK accepts. Rowids
    // are left with a gap (the deleted row) so a renumbering copy would show.
    let based = |source: &str, attempt: &str| {
        format!("'{source}-sha','{source}',{attempt},'/real/{source}','/repo/.git'")
    };
    let null_base = "NULL,NULL,NULL,NULL,NULL";
    for statement in [
        lease("legacy", "/w/legacy", "released", null_base, "NULL"),
        lease("gap", "/w/gap", "released", null_base, "NULL"),
        lease(
            "slice1",
            "/w/slice1",
            "released",
            &based("head", "NULL"),
            "NULL",
        ),
        lease(
            "kernel-head",
            "/w/head",
            "held",
            &based("head", "NULL"),
            "'kernel'",
        ),
        lease(
            "kernel-commit",
            "/w/commit",
            "releasing",
            &based("commit", "NULL"),
            "'kernel'",
        ),
        lease(
            "kernel-attempt",
            "/w/attempt",
            "held",
            &based("attempt", "'attempt-0'"),
            "'kernel'",
        ),
        lease(
            "attempt-legacy",
            "/w/attempt-legacy",
            "released",
            &based("attempt", "'attempt-1'"),
            "NULL",
        ),
        "DELETE FROM workspace_leases WHERE lease_id = 'gap';".to_string(),
    ] {
        exec(&mut db, &statement).await.unwrap();
    }
    // The children: a failed delivery (abandoned), its retry (predecessor,
    // settled as a candidate) and that candidate — all on kernel leases.
    exec(
        &mut db,
        "INSERT INTO task_git_deliveries(delivery_id,track_id,producer_attempt_id,card_id,
         lease_id,ordinal,operation_key,forge_idempotency_key,
            predecessor_delivery_id,request_idempotency_key,reason,created_at_ms,settlement,settled_event_id,failure_code,failure_reason,retry_allowed,wake_reason)
         VALUES('d-1','track','attempt-a','card-kernel-head','kernel-head',1,'op-1','forge-1',NULL,NULL,NULL,30,'failed',7,'commit_failed','boom',1,'failed'),
               ('d-2','track','attempt-a','card-kernel-head','kernel-head',2,'op-2','forge-2','d-1','retry-1','again',31,'candidate',8,NULL,NULL,NULL,'ungated_candidate'),
               ('d-3','track','attempt-b','card-kernel-attempt','kernel-attempt',1,'op-3','forge-3',NULL,NULL,NULL,32,NULL,NULL,NULL,NULL,NULL,NULL);
         INSERT INTO task_git_delivery_abandonments(delivery_id,track_id,producer_attempt_id,request_idempotency_key,reason,task_outcome,task_status,created_at_ms)
         VALUES('d-1','track','attempt-a','abandon-1','gave up','failed','failed',33);
         INSERT INTO task_candidates(candidate_id,track_id,producer_attempt_id,card_id,lease_id,repo_root,git_common_dir,branch,base_sha,commit_sha,base_is_ancestor,ref_name,created_at_ms)
         VALUES('d-2','track','attempt-a','card-kernel-head','kernel-head','/repo','/repo/.git','neige/x','head-sha','commit-sha',1,'refs/neige/candidates/track/card/d-2',34);",
    )
    .await
    .unwrap();
    // The released CHECK refuses the new value.
    assert!(
        exec(
            &mut db,
            &lease(
                "early",
                "/w/early",
                "held",
                &based("upstream", "NULL"),
                "'kernel'"
            )
        )
        .await
        .is_err()
    );
    let mut before = Vec::new();
    for table in TABLES {
        let columns = columns_of(&mut db, table).await;
        let rows = all_rows(&mut db, table, &columns).await;
        assert!(!rows.is_empty(), "{table} has fixture rows");
        before.push((table, columns, rows));
    }
    let indexes = index_sql(&mut db).await;
    assert_eq!(indexes.len(), 5, "{indexes:?}");

    MIGRATOR.run(&mut db).await.unwrap();

    for (table, columns, rows) in &before {
        assert_eq!(&all_rows(&mut db, table, columns).await, rows, "{table}");
    }
    assert_eq!(index_sql(&mut db).await, indexes);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("PRAGMA foreign_keys")
            .fetch_one(&mut db)
            .await
            .unwrap(),
        1
    );
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&mut db)
            .await
            .unwrap()
            .is_empty()
    );
    let leftovers: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE name LIKE '%0115%'")
            .fetch_one(&mut db)
            .await
            .unwrap();
    assert_eq!(leftovers, 0);

    // `upstream` joins the head/commit arm: no attempt id, every column set.
    exec(
        &mut db,
        &lease(
            "upstream",
            "/w/upstream",
            "held",
            &based("upstream", "NULL"),
            "'kernel'",
        ),
    )
    .await
    .unwrap();
    for refused in [
        lease(
            "up-attempt",
            "/w/u1",
            "held",
            &based("upstream", "'attempt-2'"),
            "'kernel'",
        ),
        lease(
            "up-partial",
            "/w/u2",
            "held",
            "'sha','upstream',NULL,NULL,'/repo/.git'",
            "'kernel'",
        ),
        lease(
            "bogus",
            "/w/u3",
            "held",
            &based("bogus", "NULL"),
            "'kernel'",
        ),
        lease("policy", "/w/u4", "held", null_base, "'kernel'"),
        // The partial unique index: one active lease per path.
        lease(
            "dup",
            "/w/upstream",
            "held",
            &based("upstream", "NULL"),
            "'kernel'",
        ),
    ] {
        assert!(exec(&mut db, &refused).await.is_err(), "{refused}");
    }
    // The children still reference the rebuilt table, enforced.
    assert!(
        exec(
            &mut db,
            "DELETE FROM workspace_leases WHERE lease_id = 'kernel-head'"
        )
        .await
        .is_err()
    );
    assert!(
        exec(
            &mut db,
            "INSERT INTO task_git_deliveries(delivery_id,track_id,producer_attempt_id,card_id,
             lease_id,ordinal,operation_key,forge_idempotency_key,created_at_ms)
             VALUES('d-9','track','attempt-z','card','no-such-lease',1,'op-9','forge-9',40)",
        )
        .await
        .is_err()
    );
    // The children's triggers survived the empty-and-refill.
    assert!(
        exec(&mut db, "UPDATE task_candidates SET branch = 'other'")
            .await
            .is_err()
    );
    // The Track delete still cascades through all four tables.
    exec(&mut db, "DELETE FROM tracks WHERE id = 'track'")
        .await
        .unwrap();
    for table in TABLES {
        let rows: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&mut db)
            .await
            .unwrap();
        assert_eq!(rows, 0, "{table}");
    }
}
