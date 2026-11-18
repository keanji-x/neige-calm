//! Upgrade populated released schemas, including receipts that insert guards cannot recreate.
use calm_truth::MIGRATOR;
use sqlx::{
    Connection, Row, SqliteConnection, ValueRef, migrate::Migrate, sqlite::SqliteConnectOptions,
};

async fn exec(db: &mut SqliteConnection, sql: &str) {
    sqlx::raw_sql(sql).execute(db).await.unwrap();
}

async fn dump(db: &mut SqliteConnection, table: &str) -> Vec<Vec<Option<String>>> {
    sqlx::query(&format!("SELECT * FROM {table} ORDER BY 1,2"))
        .fetch_all(db)
        .await
        .unwrap()
        .iter()
        .map(|row| {
            (0..row.len())
                .map(|i| {
                    if row.try_get_raw(i).unwrap().is_null() {
                        None
                    } else {
                        Some(row.try_get::<String, _>(i).unwrap_or_else(|_| {
                            row.try_get::<i64, _>(i)
                                .map(|v| v.to_string())
                                .unwrap_or_else(|_| row.get::<f64, _>(i).to_string())
                        }))
                    }
                })
                .collect()
        })
        .collect()
}

#[tokio::test]
async fn agent_worker_migration_preserves_populated_graph_and_guards() {
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
        .filter(|m| m.version < 222 && !m.migration_type.is_down_migration())
    {
        db.apply(migration).await.unwrap();
    }
    exec(&mut db, r#"
        INSERT INTO areas(id,name,color,sort,created_at,updated_at) VALUES('area','Area','blue',0,1,1);
        INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) VALUES('track','area','Track',0,1,1);
        INSERT INTO cards(id,track_id,kind,role,sort,created_at,updated_at) VALUES('card','track','codex','planner',0,1,1);
        INSERT INTO worker_sessions(id,track_id,provider,mode,contract,state,card_id,created_at_ms,updated_at_ms)
          VALUES('session','track','opencode','resumable','planner','failed','card',1,2);
        UPDATE worker_sessions SET failure_epoch=7 WHERE id='session';
        INSERT INTO worker_sessions(id,track_id,provider,mode,contract,state,parent_session_id,requester_session_id,created_at_ms,updated_at_ms)
          VALUES('child','track','codex','resumable','executor','exited','session','session',2,3);
        INSERT INTO agent_backend_registry VALUES('opencode','test','acp',X'01',X'02');
        INSERT INTO worker_session_backends VALUES('session','opencode','test');
        UPDATE worker_sessions SET thread_id='thread' WHERE id='session';
        UPDATE cards SET session_id='session' WHERE id='card';
        UPDATE tracks SET root_session_id='session' WHERE id='track';
        INSERT INTO worker_flow_items(card_id,worker_session_id,kind,payload,created_at_ms) VALUES('card','session','text','{}',2);
        INSERT INTO acp_managed_sessions VALUES('session','digest');
        INSERT INTO acp_submissions(worker_session_id,client_id,turn_id,thread_id,native_session_id,input_json,state,created_at_ms)
          VALUES('session','kept','turn-kept','thread','native','{"version":"canonical_v1","association":{}}','unknown',2);
        UPDATE acp_submissions SET kept_cleanup_json='{"version":"v1","worker_session_id":"session","client_id":"kept","turn_id":"turn-kept","thread_id":"thread","native_session_id":"native","association":{},"registration_digest":"digest","keep_key":"key","keep_principal":"principal","keep_fingerprint":"fingerprint","observation":"stopped_revoked_normally_joined"}' WHERE client_id='kept';
        INSERT INTO acp_submissions(worker_session_id,client_id,turn_id,thread_id,native_session_id,input_json,state,created_at_ms,shared_restart_owner_json)
          VALUES('session','released','turn-released','thread','native','{"version":"canonical_v1"}','unknown',3,'{"owner":"boot"}');
        UPDATE acp_submissions SET restart_released_at_ms=4 WHERE client_id='released';
        INSERT INTO acp_normal_joins VALUES('session','kept','{"joined":true}');
        INSERT INTO report_read_ledger VALUES('session','card',9,1,'["section"]');
        INSERT INTO report_read_ledger_blocks VALUES('session','card','b_1234',8,'{"history":"saved"}');
        INSERT INTO conversation_abandonments VALUES('card','principal','key','{}','session','child',5);
        INSERT INTO claude_session_launches VALUES('child','card','track','native-child','execution',5);
        INSERT INTO tasks(id,track_id,key,kind,goal,context_json,created_at_ms,updated_at_ms) VALUES('task','track','task','codex','Goal','{}',1,1);
        INSERT INTO task_ref_index VALUES('task','track','b_1234');
    "#).await;
    let tables = [
        "worker_sessions",
        "cards",
        "tracks",
        "worker_flow_items",
        "acp_managed_sessions",
        "acp_submissions",
        "acp_normal_joins",
        "worker_session_backends",
        "report_read_ledger",
        "report_read_ledger_blocks",
        "conversation_abandonments",
        "claude_session_launches",
        "task_ref_index",
        "task_attempt_allocations",
    ];
    let mut before = Vec::new();
    for table in tables {
        let rows = dump(&mut db, table).await;
        assert!(!rows.is_empty(), "{table}");
        before.push(rows);
    }
    let schema_sql = "SELECT type,name,sql FROM sqlite_master WHERE type IN ('index','trigger','view') AND sql IS NOT NULL ORDER BY type,name";
    let schema_before: Vec<(String, String, String)> =
        sqlx::query_as(schema_sql).fetch_all(&mut db).await.unwrap();
    let task_before = dump(&mut db, "tasks").await;
    for migration in MIGRATOR.iter().filter(|m| m.version == 222) {
        db.apply(migration).await.unwrap();
    }
    for (table, expected) in tables.into_iter().zip(before) {
        assert_eq!(dump(&mut db, table).await, expected, "{table}");
    }
    let schema_after: Vec<(String, String, String)> =
        sqlx::query_as(schema_sql).fetch_all(&mut db).await.unwrap();
    let normalize = |rows: Vec<(String, String, String)>| {
        rows.into_iter()
            .map(|(kind, name, sql)| {
                (
                    kind,
                    name,
                    sql.chars()
                        .filter(|c| !c.is_whitespace() && *c != '"')
                        .collect::<String>(),
                )
            })
            .collect::<Vec<_>>()
    };
    let before_objects = normalize(schema_before);
    let after_objects = normalize(schema_after);
    assert_eq!(before_objects.len(), after_objects.len());
    for (before, after) in before_objects.iter().zip(&after_objects) {
        assert_eq!(before, after, "schema object {}", before.1);
    }
    let task_after = dump(&mut db, "current_tasks").await;
    assert_eq!(&task_after[0][..task_before[0].len()], task_before[0]);
    assert_eq!(&task_after[0][task_before[0].len()..], &[None, None, None]);
    assert!(
        sqlx::query("SELECT rowid FROM acp_submissions")
            .fetch_all(&mut db)
            .await
            .is_err()
    );
    // New contracts are admitted; ephemeral OpenCode executors remain forbidden.
    exec(&mut db,"INSERT INTO tasks(id,track_id,key,kind,goal,context_json,provider,model,reasoning_effort,created_at_ms,updated_at_ms) VALUES('agent','track','agent','agent','Goal','{}','opencode','model','high',2,2); INSERT INTO worker_sessions(id,track_id,provider,mode,contract,state,created_at_ms,updated_at_ms) VALUES('worker','track','opencode','resumable','executor','starting',2,2);").await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT generation FROM task_attempt_allocations WHERE attempt_id='agent'"
        )
        .fetch_one(&mut db)
        .await
        .unwrap(),
        1
    );
    for sql in [
        "UPDATE worker_sessions SET mode='ephemeral' WHERE id='worker'",
        "INSERT INTO worker_session_backends VALUES('child','opencode','test')",
        "UPDATE worker_session_backends SET backend_id='other' WHERE worker_session_id='session'",
        "INSERT INTO acp_submissions SELECT * FROM acp_submissions WHERE client_id='kept'",
        "INSERT INTO acp_submissions SELECT * FROM acp_submissions WHERE client_id='released'",
        "UPDATE acp_submissions SET created_at_ms=10 WHERE client_id='kept'",
        "UPDATE acp_submissions SET restart_released_at_ms=10 WHERE client_id='released'",
        "INSERT INTO task_attempt_allocations(attempt_id,track_id,key,generation,origin_json,created_at_ms) VALUES('bad','track','task',2,'{\"kind\":\"recovery\",\"previous_attempt_id\":\"task\"}',3)",
    ] {
        assert!(sqlx::query(sql).execute(&mut db).await.is_err(), "{sql}");
    }
    exec(&mut db,"UPDATE worker_sessions SET state='running' WHERE id='session'; UPDATE worker_sessions SET state='failed' WHERE id='session'; UPDATE tasks SET status='done' WHERE id='task';").await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT failure_epoch FROM worker_sessions WHERE id='session'"
        )
        .fetch_one(&mut db)
        .await
        .unwrap(),
        8
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM task_ref_index")
            .fetch_one(&mut db)
            .await
            .unwrap(),
        0
    );
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&mut db)
            .await
            .unwrap()
            .is_empty()
    );
}
