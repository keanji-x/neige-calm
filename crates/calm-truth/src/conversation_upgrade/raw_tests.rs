use super::*;
use sqlx::{Connection, Row, SqliteConnection, sqlite::SqliteConnectOptions};
use tempfile::TempDir;

pub(super) struct Fixture {
    pub dir: TempDir,
    pub path: PathBuf,
}
impl Fixture {
    pub async fn new(sql: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("backup.db");
        let mut c = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
        sqlx::raw_sql(sql).execute(&mut c).await.unwrap();
        c.close().await.unwrap();
        Self { dir, path }
    }
    pub fn input(&self) -> FrozenBackup {
        FrozenBackup { path: self.path.clone(), operator: "fixture backup owner".into(),
            freeze_evidence: "writers stopped; consistent self-contained backup; namespace and bytes frozen through close".into() }
    }
    pub async fn audit(&self) -> CompleteBlockedAudit {
        OfflineSnapshot::open(self.input())
            .await
            .unwrap()
            .inventory()
            .await
            .unwrap()
    }
    pub async fn oracle(&self) -> SqliteConnection {
        SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(&self.path)
                .read_only(true)
                .immutable(true),
        )
        .await
        .unwrap()
    }
}
pub(super) fn relation<'a>(a: &'a CompleteBlockedAudit, name: &str) -> &'a RawRelation {
    a.inventory()
        .relations
        .iter()
        .find(|r| r.name == name)
        .unwrap()
}
fn t(s: &str) -> RawCell {
    RawCell::Text(s.as_bytes().to_vec())
}

#[tokio::test]
async fn raw_inventory_keeps_all_relations_and_columns() {
    // Independent historical fixture: no application repository or migration.
    // Only this test contains unknown_sentinel (the no-omission observer).
    let f = Fixture::new(r#"
        CREATE TABLE planner_recovery_threads(id TEXT, snapshot TEXT);
        CREATE TABLE planner_recovery_issuances(id TEXT, body TEXT);
        CREATE TABLE planner_recovery_turns(id TEXT, body TEXT);
        CREATE TABLE planner_recovery_calls(id TEXT, body TEXT);
        INSERT INTO planner_recovery_threads VALUES('old','{"v":1,"parallel":[1,null]}');
        INSERT INTO planner_recovery_issuances VALUES('issued','broken');
        INSERT INTO planner_recovery_turns VALUES('turn','{"x":1,"x":2}');
        INSERT INTO planner_recovery_calls VALUES('call','unknown');
        CREATE TABLE worker_sessions(id TEXT, state TEXT, card_id TEXT, handle_state_json TEXT, future_col BLOB);
        INSERT INTO worker_sessions VALUES
          ('retired','retired','missing','bad',X'80'),
          ('replaced','replaced','old','{}',NULL),
          ('closed','closed','closed','{}',NULL),
          ('archived','archived','archive','{}',NULL),
          ('orphan','unknown',NULL,'{"role":"future"}',NULL);
        CREATE TABLE cards(id TEXT, session_id TEXT);
        INSERT INTO cards VALUES('current',NULL);
        CREATE TABLE tracks(id TEXT, closed_at INTEGER);
        INSERT INTO tracks VALUES('closed',123);
        CREATE TABLE operations(operation_key TEXT, phase TEXT, tx_output TEXT);
        INSERT INTO operations VALUES('start','pending',NULL),('start#1','tx_committed','bad'),
          ('start#2','succeeded','{}'),('start#3','failed',NULL),('start#4','parked',NULL),
          ('start#5','compensating',NULL),('start#6','app_server_interact',NULL),('start#7','future','bad'),
          ('start#8','awaiting_retry',NULL),('start#9','spawn_started',NULL),
          ('start#10','spawn_succeeded',NULL),('start#11','stuck','bad');
        CREATE TABLE acp_managed_sessions(worker_session_id TEXT, config BLOB);
        INSERT INTO acp_managed_sessions VALUES('missing',X'00FF');
        CREATE TABLE acp_submissions(worker_session_id TEXT, client_id TEXT, ack TEXT);
        INSERT INTO acp_submissions VALUES('orphan','input',NULL);
        CREATE TABLE planner_input_idempotency(key TEXT, entry_id TEXT);
        INSERT INTO planner_input_idempotency VALUES('v1',NULL),('future','ack');
        CREATE TABLE harness_items(id INTEGER, method TEXT, params TEXT);
        INSERT INTO harness_items VALUES(1,'future','not json');
        CREATE TABLE conversation_messages(id TEXT, body TEXT);
        INSERT INTO conversation_messages VALUES('canonical','raw');
        CREATE TABLE config(root TEXT, canonical BLOB);
        INSERT INTO config VALUES('/explicit',X'FF');
        CREATE TABLE unknown_sentinel(id INTEGER, unknown_column TEXT, extra BLOB);
        INSERT INTO unknown_sentinel VALUES(7,'unclassified',X'80'),(7,'unclassified',X'80');
        CREATE TABLE sequence_owner(id INTEGER PRIMARY KEY AUTOINCREMENT);
        INSERT INTO sequence_owner DEFAULT VALUES;
        CREATE TABLE _sqlx_migrations(version INTEGER, success INTEGER, checksum BLOB);
        INSERT INTO _sqlx_migrations VALUES(100,0,X'00FF'),(999999,1,X'');
        CREATE TABLE database_identity(singleton INTEGER, id TEXT, minted_at_ms INTEGER);
        INSERT INTO database_identity VALUES(1,'bad identity',NULL),(1,NULL,-1);
    "#).await;
    let a = f.audit().await;
    assert_eq!(a.semantic_status(), "BlockedAudit/Unclassified");
    let expected = [
        ("planner_recovery_threads", 1),
        ("planner_recovery_issuances", 1),
        ("planner_recovery_turns", 1),
        ("planner_recovery_calls", 1),
        ("worker_sessions", 5),
        ("cards", 1),
        ("tracks", 1),
        ("operations", 12),
        ("acp_managed_sessions", 1),
        ("acp_submissions", 1),
        ("planner_input_idempotency", 2),
        ("harness_items", 1),
        ("conversation_messages", 1),
        ("config", 1),
        ("unknown_sentinel", 2),
        ("sequence_owner", 1),
        ("sqlite_sequence", 1),
        ("_sqlx_migrations", 2),
        ("database_identity", 2),
        ("sqlite_schema", 19),
    ];
    let mut oracle = f.oracle().await;
    let mut actual = a
        .inventory()
        .relations
        .iter()
        .map(|r| r.name.as_str())
        .collect::<Vec<_>>();
    actual.sort();
    let mut names = expected.iter().map(|(n, _)| *n).collect::<Vec<_>>();
    names.sort();
    assert_eq!(actual, names);
    for (name, count) in expected {
        let n: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM \"{name}\""))
            .fetch_one(&mut oracle)
            .await
            .unwrap();
        assert_eq!(n, count, "fixture oracle {name}");
        let r = relation(&a, name);
        assert_eq!(r.source_count, n, "{name}");
        assert_eq!(r.rows.len() as i64, n, "{name}");
        let metadata = sqlx::query(&format!("PRAGMA table_info(\"{name}\")"))
            .fetch_all(&mut oracle)
            .await
            .unwrap();
        assert_eq!(
            r.columns,
            metadata
                .iter()
                .map(|r| r.get::<String, _>("name"))
                .collect::<Vec<_>>()
        );
        let ids: Vec<i64> =
            sqlx::query_scalar(&format!("SELECT rowid FROM \"{name}\" ORDER BY rowid"))
                .fetch_all(&mut oracle)
                .await
                .unwrap();
        assert_eq!(
            r.rows.iter().map(|r| r.key.clone()).collect::<Vec<_>>(),
            ids.into_iter()
                .map(|i| vec![RawCell::Integer(i)])
                .collect::<Vec<_>>()
        );
    }
    let sentinel = relation(&a, "unknown_sentinel");
    let direct = sqlx::query("SELECT typeof(unknown_column), hex(unknown_column), hex(extra) FROM unknown_sentinel ORDER BY rowid")
        .fetch_all(&mut oracle).await.unwrap();
    for (r, o) in sentinel.rows.iter().zip(direct) {
        assert_eq!(o.get::<String, _>(0), "text");
        assert_eq!(
            r.cells,
            vec![
                RawCell::Integer(7),
                RawCell::Text(hex::decode(o.get::<String, _>(1)).unwrap()),
                RawCell::Blob(hex::decode(o.get::<String, _>(2)).unwrap())
            ]
        );
    }
    assert_ne!(sentinel.rows[0].key, sentinel.rows[1].key);
    assert_eq!(sentinel.rows[0].cells, sentinel.rows[1].cells);
    assert_eq!(relation(&a, "worker_sessions").rows[0].cells[3], t("bad"));
    assert_eq!(
        relation(&a, "acp_submissions").rows[0].cells[2],
        RawCell::Null
    );
    assert_eq!(
        relation(&a, "planner_input_idempotency").rows[0].cells[1],
        RawCell::Null
    );
    assert_eq!(relation(&a, "operations").rows[7].cells[0], t("start#7"));
    assert_eq!(
        relation(&a, "database_identity").rows[1].cells[1],
        RawCell::Null
    );
    assert_eq!(
        relation(&a, "_sqlx_migrations").rows[0].cells[2],
        RawCell::Blob(vec![0, 255])
    );
    oracle.close().await.unwrap();
}

#[tokio::test]
async fn raw_cells_preserve_storage_class_encoding_and_bytes() {
    for (encoding, raw) in [
        ("UTF-8", "80FF0061"),
        ("UTF-16le", "00D8E9000000"),
        ("UTF-16be", "D80000E90000"),
    ] {
        let sql = format!(
            r#"
            PRAGMA encoding='{encoding}';
            CREATE TABLE values_raw(v);
            INSERT INTO values_raw VALUES(NULL),(''),(X''),(CAST(X'{raw}' AS TEXT)),
              (X'{raw}'),(-9223372036854775808),(9223372036854775807),(1.5),(-0.125);
            CREATE TABLE "é雪"("x😀"); INSERT INTO "é雪" VALUES('é');
        "#
        );
        let f = Fixture::new(&sql).await;
        let a = f.audit().await;
        assert_eq!(a.inventory().encoding, encoding);
        assert_eq!(relation(&a, "é雪").columns, vec!["x😀"]);
        assert_eq!(
            relation(&a, "é雪").rows[0].cells[0],
            RawCell::Text(match encoding {
                "UTF-8" => vec![0xc3, 0xa9],
                "UTF-16le" => vec![0xe9, 0],
                _ => vec![0, 0xe9],
            })
        );
        let r = relation(&a, "values_raw");
        let expected = vec![
            RawCell::Null,
            t(""),
            RawCell::Blob(vec![]),
            RawCell::Text(hex::decode(raw).unwrap()),
            RawCell::Blob(hex::decode(raw).unwrap()),
            RawCell::Integer(i64::MIN),
            RawCell::Integer(i64::MAX),
            RawCell::RealBits(1.5f64.to_bits()),
            RawCell::RealBits((-0.125f64).to_bits()),
        ];
        assert_eq!(
            r.rows
                .iter()
                .map(|r| r.cells[0].clone())
                .collect::<Vec<_>>(),
            expected
        );
        let mut oracle = f.oracle().await;
        let rows = sqlx::query(
            "SELECT rowid,typeof(v),hex(CAST(v AS BLOB)),v FROM values_raw ORDER BY rowid",
        )
        .fetch_all(&mut oracle)
        .await
        .unwrap();
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(r.rows[i].key, vec![RawCell::Integer(row.get(0))]);
            let class: String = row.get(1);
            match &r.rows[i].cells[0] {
                RawCell::Text(b) | RawCell::Blob(b) => {
                    assert_eq!(hex::encode_upper(b), row.get::<String, _>(2));
                    assert_eq!(
                        class,
                        if matches!(r.rows[i].cells[0], RawCell::Text(_)) {
                            "text"
                        } else {
                            "blob"
                        }
                    );
                }
                RawCell::Integer(v) => {
                    assert_eq!(class, "integer");
                    assert_eq!(*v, row.get::<i64, _>(3));
                }
                RawCell::RealBits(v) => {
                    assert_eq!(class, "real");
                    assert_eq!(*v, row.get::<f64, _>(3).to_bits());
                }
                RawCell::Null => assert_eq!(class, "null"),
                RawCell::DerivedNotStored => panic!("ordinary stored value"),
            }
        }
        oracle.close().await.unwrap();
    }
}

#[tokio::test]
async fn raw_schema_distinguishes_storage_and_derived() {
    let f = Fixture::new(
        r#"
        CREATE TABLE "q'u""ote"("weird""column" TEXT);
        INSERT INTO "q'u""ote" VALUES('raw');
        CREATE TABLE keyed(a TEXT, b INTEGER, v BLOB, PRIMARY KEY(a,b)) WITHOUT ROWID;
        INSERT INTO keyed VALUES('z',2,X'00'),('a',3,X'FF');
        CREATE TABLE shadows(rowid TEXT, _rowid_ TEXT, oid TEXT, k TEXT NOT NULL PRIMARY KEY);
        INSERT INTO shadows VALUES('one','two','three','key');
        CREATE TABLE generated(a INTEGER, stored INTEGER GENERATED ALWAYS AS (a+1) STORED,
          derived INTEGER GENERATED ALWAYS AS (a+2) VIRTUAL);
        INSERT INTO generated(a) VALUES(4);
        CREATE INDEX quoted_index ON keyed(v);
        CREATE TRIGGER never_run AFTER UPDATE ON generated BEGIN SELECT 1; END;
        CREATE VIEW valid_view AS SELECT a FROM generated;
        CREATE VIEW broken_view AS SELECT not_here FROM missing;
        CREATE TABLE pragma_table_list(v); INSERT INTO pragma_table_list VALUES('ordinary');
        CREATE TABLE pragma_table_xinfo(v); INSERT INTO pragma_table_xinfo VALUES('ordinary');
    "#,
    )
    .await;
    let a = f.audit().await;
    assert_eq!(relation(&a, "q'u\"ote").columns, vec!["weird\"column"]);
    let k = relation(&a, "keyed");
    assert_eq!(k.key_columns, vec!["a", "b"]);
    assert_eq!(k.rows[0].key, vec![t("a"), RawCell::Integer(3)]);
    assert_eq!(relation(&a, "shadows").rows[0].key, vec![t("key")]);
    assert_eq!(
        relation(&a, "generated").rows[0].cells,
        vec![
            RawCell::Integer(4),
            RawCell::Integer(5),
            RawCell::DerivedNotStored
        ]
    );
    assert!(
        !a.inventory()
            .relations
            .iter()
            .any(|r| r.name.ends_with("view"))
    );
    let mut oracle = f.oracle().await;
    let ddl: Vec<Option<Vec<u8>>> =
        sqlx::query_scalar("SELECT CAST(sql AS BLOB) FROM sqlite_schema ORDER BY rowid")
            .fetch_all(&mut oracle)
            .await
            .unwrap();
    assert_eq!(
        a.inventory()
            .schema
            .iter()
            .map(|r| match &r[4] {
                RawCell::Text(b) => Some(b.clone()),
                RawCell::Null => None,
                _ => panic!(),
            })
            .collect::<Vec<_>>(),
        ddl
    );
    assert_eq!(a.inventory().schema.len(), 11);
    assert_eq!(
        relation(&a, "pragma_table_list").rows[0].cells,
        vec![t("ordinary")]
    );
    assert_eq!(
        relation(&a, "pragma_table_xinfo").rows[0].cells,
        vec![t("ordinary")]
    );
    oracle.close().await.unwrap();
}

#[tokio::test]
async fn raw_pre0127_recovery_tables_remain_intact() {
    let sql = format!(
        "CREATE TABLE tracks(id TEXT PRIMARY KEY); INSERT INTO tracks VALUES('t'); {} {}",
        include_str!("../../migrations/0101_planner_recovery_bindings.sql"),
        r#"
        INSERT INTO planner_recovery_threads VALUES('native','t','retired',1);
        INSERT INTO planner_recovery_issuances VALUES('i','t','missing','native','{}','[{}]',2);
        INSERT INTO planner_recovery_turns VALUES('native','turn','missing','t','i',3);
        INSERT INTO planner_recovery_calls VALUES('native','turn','call','missing','t','future',
          'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',4);
    "#
    );
    let f = Fixture::new(&sql).await;
    let a = f.audit().await;
    let mut oracle = f.oracle().await;
    for name in [
        "planner_recovery_threads",
        "planner_recovery_issuances",
        "planner_recovery_turns",
        "planner_recovery_calls",
    ] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {name}"))
            .fetch_one(&mut oracle)
            .await
            .unwrap();
        assert_eq!(count, 1);
        let r = relation(&a, name);
        assert_eq!(r.source_count, count);
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.rows[0].key, vec![RawCell::Integer(1)]);
        let column_count: i64 =
            sqlx::query_scalar(&format!("SELECT count(*) FROM pragma_table_info('{name}')"))
                .fetch_one(&mut oracle)
                .await
                .unwrap();
        assert_eq!(r.rows[0].cells.len() as i64, column_count);
    }
    assert_eq!(
        relation(&a, "planner_recovery_issuances").rows[0].cells[2],
        t("missing")
    );
    assert_eq!(a.semantic_status(), "BlockedAudit/Unclassified");
    assert!(
        !a.inventory()
            .relations
            .iter()
            .any(|r| r.name == "database_identity")
    );
    oracle.close().await.unwrap();
}

#[tokio::test]
async fn raw_wide_unknown_table_keeps_every_column() {
    let columns = (0..600).map(|i| format!("c{i}")).collect::<Vec<_>>();
    let values = (0..600).map(|i| i.to_string()).collect::<Vec<_>>();
    let f = Fixture::new(&format!(
        "CREATE TABLE wide({}); INSERT INTO wide VALUES({});",
        columns.join(","),
        values.join(",")
    ))
    .await;
    let a = f.audit().await;
    let r = relation(&a, "wide");
    assert_eq!(r.columns, columns);
    assert_eq!(
        r.rows[0].cells,
        (0..600).map(RawCell::Integer).collect::<Vec<_>>()
    );
    let mut oracle = f.oracle().await;
    let direct = sqlx::query("SELECT * FROM wide")
        .fetch_one(&mut oracle)
        .await
        .unwrap();
    for i in 0..600 {
        assert_eq!(r.rows[0].cells[i], RawCell::Integer(direct.get(i)));
    }
    oracle.close().await.unwrap();
}
