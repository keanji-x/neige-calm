//! Real upgrade from 0186: no retrospective cleanup inference or historical release.

#[tokio::test]
async fn acp_cleanup_migration_preserves_0186_receipts_and_historical_fences() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("upgrade.db").display()
    );
    let pool = super::SqliteDb::connect(&url).await.unwrap();
    let old = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|m| m.version <= 186)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    old.run(pool.writer_pool()).await.unwrap();
    let repo = super::SqlxRepo {
        db: pool,
        card_role_cache: super::CardRoleCache::new(),
        track_area_cache: super::TrackAreaCache::new(),
        database_id: std::sync::Arc::new("acp-upgrade".into()),
    };
    for state in ["sending", "unknown", "completed", "released"] {
        let mut tx = super::begin_immediate_tx(repo.db()).await.unwrap();
        let card = super::runtime_read_flip_support::create_card_in_tx(&repo, &mut tx, state, "codex", serde_json::json!({"schemaVersion":1,"planner_harness":true,"planner_provider":"opencode"})).await;
        let mut init = crate::session_projection_repo::WorkerSessionInit::shared_planner(
            state.into(),
            card,
            calm_types::runtime::AgentProvider::OpenCode,
            calm_types::worker::WorkerSessionState::Idle,
            Some("thread".into()),
            serde_json::json!({}),
            7,
        );
        init.session_id = Some(format!("native-{state}"));
        super::session_start_runtime_tx(&mut tx, init)
            .await
            .unwrap();
        sqlx::query("INSERT INTO acp_managed_sessions VALUES(?1,'historical-registration')")
            .bind(state)
            .execute(&mut **tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    let mut db = repo.db().writer_pool().acquire().await.unwrap();
    assert!(
        sqlx::query_scalar::<_, bool>("PRAGMA foreign_keys")
            .fetch_one(&mut *db)
            .await
            .unwrap()
    );
    for state in ["sending", "unknown", "completed"] {
        sqlx::query("INSERT INTO acp_submissions VALUES(?1,?1,?1,'thread','native-'||?1,'{\"input\":[],\"prompt\":[],\"queue\":[]}',?1,CASE WHEN ?1='completed' THEN '{ \"result\": [] }' ELSE NULL END,7)")
            .bind(state).execute(&mut *db).await.unwrap();
    }
    let select = concat!(
        "SELECT ",
        "worker_session_id,client_id,turn_id,thread_id,native_session_id,input_json,state,outcome_json,c",
        "reated_at_ms FROM acp_submissions ORDER BY client_id",
    );
    type Row = (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        i64,
    );
    let before: Vec<Row> = sqlx::query_as(select).fetch_all(&mut *db).await.unwrap();
    let constraints_before = constraint_inventory(&mut db).await;
    drop(db);
    repo.db().writer_pool().close().await;
    repo.db().read_pool().close().await;
    drop(repo);
    let repo = super::SqlxRepo::open(&url).await.unwrap();
    let mut db = repo.db().writer_pool().acquire().await.unwrap();
    let after: Vec<Row> = sqlx::query_as(select).fetch_all(&mut *db).await.unwrap();
    assert_eq!(before, after);
    assert_eq!(constraints_before, constraint_inventory(&mut db).await);
    let schema: String =
        sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE name='acp_submissions'")
            .fetch_one(&mut *db)
            .await
            .unwrap();
    assert!(schema.contains("WITHOUT ROWID"));
    // Exercise every prior CHECK and NOT NULL after the rebuild.
    for assignment in [
        "input_json='not json'",
        "state='invalid'",
        "outcome_json='not json'",
        "state='completed'",
        "worker_session_id=NULL",
        "client_id=NULL",
        "turn_id=NULL",
        "thread_id=NULL",
        "native_session_id=NULL",
        "input_json=NULL",
        "state=NULL",
        "created_at_ms=NULL",
        "worker_session_id='absent-parent'",
    ] {
        let sql = format!("UPDATE acp_submissions SET {assignment} WHERE client_id='sending'");
        assert!(sqlx::query(&sql).execute(&mut *db).await.is_err(), "{sql}");
    }
    let after_constraints: Vec<Row> = sqlx::query_as(select).fetch_all(&mut *db).await.unwrap();
    assert_eq!(before, after_constraints);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM acp_submissions WHERE kept_cleanup_json IS NOT NULL",
    )
    .fetch_one(&mut *db)
    .await
    .unwrap();
    assert_eq!(count, 0);
    for state in ["sending", "unknown"] {
        assert!(sqlx::query(concat!(
"INSERT INTO ",
"acp_submissions(worker_session_id,client_id,turn_id,thread_id,native_session_id,input_json,stat",
"e,created_at_ms) VALUES(?1,'successor',?1||'-next','thread','native','{}','sending',8)",
))
            .bind(state).execute(&mut *db).await.is_err());
        for evidence in ["{}", "null", "not json", "{\"version\":\"v1\"}"] {
            assert!(
                sqlx::query(
                    "UPDATE acp_submissions SET kept_cleanup_json=?1 WHERE worker_session_id=?2"
                )
                .bind(evidence)
                .bind(state)
                .execute(&mut *db)
                .await
                .is_err()
            );
        }
    }
    // A newly observed canonical cleanup must release admission on the upgraded
    // index; historical Sending/Unknown rows above must continue fencing.
    sqlx::raw_sql(r#"
        INSERT INTO acp_submissions(worker_session_id,client_id,turn_id,thread_id,native_session_id,input_json,state,created_at_ms)
        VALUES('released','released','released','thread','native-released','{"version":"canonical_v1","association":{}}','unknown',8);
        UPDATE acp_submissions SET kept_cleanup_json=json_object(
          'version','v1','worker_session_id',worker_session_id,'client_id',client_id,
          'turn_id',turn_id,'thread_id',thread_id,'native_session_id',native_session_id,
          'association',json('{}'),'registration_digest','historical-registration',
          'keep_key','keep','keep_principal','owner','keep_fingerprint','fingerprint',
          'observation','stopped_revoked_normally_joined')
        WHERE worker_session_id='released';
        INSERT INTO acp_submissions(worker_session_id,client_id,turn_id,thread_id,native_session_id,input_json,state,created_at_ms)
        VALUES('released','successor','released-next','thread','native-released','{}','sending',9);
    "#).execute(&mut *db).await.unwrap();
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&mut *db)
            .await
            .unwrap()
            .is_empty()
    );
}

// Compare the prior column/FK/unique-key contracts independently of index names
// and the intentionally changed unresolved-index predicate.
type ConstraintInventory = (
    Vec<(String, String, i64, i64)>,
    Vec<(String, String, String, String)>,
    Vec<(String, i64, String)>,
);

async fn constraint_inventory(db: &mut sqlx::SqliteConnection) -> ConstraintInventory {
    let columns = sqlx::query_as("SELECT name,type,\"notnull\",pk FROM pragma_table_info('acp_submissions') WHERE name!='kept_cleanup_json' ORDER BY cid")
        .fetch_all(&mut *db).await.unwrap();
    let foreign_keys = sqlx::query_as("SELECT \"table\",\"from\",\"to\",on_delete FROM pragma_foreign_key_list('acp_submissions') ORDER BY id,seq")
        .fetch_all(&mut *db).await.unwrap();
    let indexes = sqlx::query_as(concat!(
        "SELECT origin,partial,(SELECT group_concat(name,',') FROM (SELECT name FROM ",
        "pragma_index_info(il.name) ORDER BY seqno)) FROM pragma_index_list('acp_submissions') ",
        "il WHERE \"unique\"=1 ORDER BY origin"
    ))
    .fetch_all(&mut *db)
    .await
    .unwrap();
    (columns, foreign_keys, indexes)
}
