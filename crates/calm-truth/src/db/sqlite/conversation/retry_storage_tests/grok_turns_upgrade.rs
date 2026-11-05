//! Populated provider upgrades retain the canonical graph and widen only its provider CHECK.
use super::*;

const TABLES: [&str; 5] = [
    "conversation_turns",
    "conversation_input_submissions",
    "conversation_submission_inputs",
    "conversation_presentations",
    "conversation_input_retry_relations",
];

/// The shipped migrations up to `last`, so later migrations cannot change what this test pins.
pub(super) fn through(last: i64) -> sqlx::migrate::Migrator {
    sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|m| m.version <= last)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    }
}

pub(super) fn repo_on(db: super::super::super::SqliteDb) -> SqlxRepo {
    SqlxRepo {
        db,
        card_role_cache: super::super::super::CardRoleCache::new(),
        track_area_cache: super::super::super::TrackAreaCache::new(),
        database_id: std::sync::Arc::new("grok-upgrade".into()),
    }
}

pub(super) async fn rows(pool: &sqlx::SqlitePool, table: &str) -> String {
    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
            .bind(table)
            .fetch_all(pool)
            .await
            .unwrap();
    let columns = columns
        .iter()
        .map(|column| format!("\"{column}\""))
        .collect::<Vec<_>>()
        .join(",");
    sqlx::query_scalar(&format!(
        "SELECT json_group_array(json(row)) FROM (SELECT json_array({columns}) AS row FROM {table} ORDER BY rowid)"
    ))
    .fetch_one(pool)
    .await
    .unwrap()
}

type SchemaObject = (String, String, String, Option<String>);

async fn schema(pool: &sqlx::SqlitePool) -> Vec<SchemaObject> {
    sqlx::query_as("SELECT type,name,tbl_name,sql FROM sqlite_master ORDER BY type,name")
        .fetch_all(pool)
        .await
        .unwrap()
}

/// A raw Turn insert in a rolled-back transaction: only the provider CHECK can refuse it,
/// because the deferred Initial edge is never committed.
async fn turn_admits(repo: &SqlxRepo, card: &str, provider: &str) -> bool {
    let write_db = (repo.db()).clone();
    write_db
        .writer()
        .with_writer(|write_session| {
            Box::pin(async move {
                            let mut tx = write_session.begin_immediate().await.unwrap();
                            let admitted = sqlx::query(
                    "INSERT INTO conversation_turns(id,card_id,initiating_runtime_id,initial_submission_id,
         execution_kind,deployment_owner_user_id,provider,profile,created_at_ms)
         VALUES('ffffffffffffffffffffffffffffffff',?1,'carrier','ffffffffffffffffffffffffffffffff',
         'legacy_single_user_deployment',?2,?3,'spec',1)",
                )
                .bind(card)
                .bind(LEGACY_OWNER_USER_ID)
                .bind(provider)
                .execute(&mut **tx)
                .await
                .is_ok();
                            tx.rollback().await.unwrap();
                            admitted
            })
        })
        .await
        .expect("independent write operation")
}

/// 0188's Assistant receipt trigger reads conversation_presentations, which 0190 drops and
/// re-creates. A refused raw insert proves the trigger still exists, its body still resolves every
/// referenced table (a missing one fails at prepare) and its predicate still runs.
async fn assistant_creation_refusal(repo: &SqlxRepo, card: &str) -> String {
    let write_db = (repo.db()).clone();
    write_db
        .writer()
        .with_writer(|write_session| {
            Box::pin(async move {
                let mut tx = write_session.begin_immediate().await.unwrap();
                let error = sqlx::query(
                    "INSERT INTO conversation_assistant_creations
         VALUES(?1,'track',?2,?3,'carrier','message',1,'{}')",
                )
                .bind(card)
                .bind("a".repeat(64))
                .bind(LEGACY_OWNER_USER_ID)
                .execute(&mut **tx)
                .await
                .unwrap_err()
                .to_string();
                tx.rollback().await.unwrap();
                error
            })
        })
        .await
        .expect("independent write operation")
}

const ASSISTANT_REFUSAL: &str =
    "Assistant creation must bind its dormant carrier and original acceptance";

async fn assistant_trigger_sql(repo: &SqlxRepo) -> String {
    sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type='trigger' AND name='assistant_creation_fact_scope'
         AND tbl_name='conversation_assistant_creations'",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn canonical_grok_turns_migration_preserves_populated_graph() {
    provider_upgrade(
        189,
        190,
        "grok",
        "('codex','claude','opencode')",
        "('codex','claude','opencode','grok')",
    )
    .await;
}

#[tokio::test]
async fn canonical_antigravity_turns_migration_preserves_populated_graph() {
    provider_upgrade(
        212,
        213,
        "antigravity",
        "('codex','claude','opencode','grok')",
        "('codex','claude','opencode','grok','antigravity')",
    )
    .await;
}

async fn provider_upgrade(
    from: i64,
    to: i64,
    added: &str,
    old_providers: &str,
    new_providers: &str,
) {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/upgrade.db?mode=rwc", dir.path().display());
    let db = super::super::super::SqliteDb::connect(&url).await.unwrap();
    let old = through(from);
    {
        let migrator = &(old);
        (db).writer()
            .with_writer(|write_session| {
                Box::pin(async move {
                    write_session
                        .autocommit(|connection| {
                            Box::pin(async move {
                                Ok::<_, sqlx::Error>(migrator.run_direct(&mut **connection).await)
                            })
                        })
                        .await
                })
            })
            .await
            .unwrap()
            .unwrap()
    }
    .unwrap();
    let repo = repo_on(db);
    let (_, card) = crate::db::sqlite::historical_fixtures::conversation_card(&repo).await;
    // The real claim writer, an attempt, a retry relation, a Human retirement and a reference
    // successor: every member and deferred edge of the rebuilt graph holds a row.
    let s = seed_repo(repo, card, false).await;
    let write_db = (s.repo.db()).clone();
    write_db.writer().with_writer(|write_session| Box::pin(async move {
        let mut tx = write_session.begin_immediate().await.unwrap();
        successor(&mut tx, &s).await;
        tx.commit().await.unwrap();
        assert!(turn_admits(&s.repo, &s.card, "codex").await);
        assert!(!turn_admits(&s.repo, &s.card, added).await);
        assert!(
            assistant_creation_refusal(&s.repo, &s.card)
                .await
                .contains(ASSISTANT_REFUSAL)
        );
        let assistant_trigger = assistant_trigger_sql(&s.repo).await;
        (s.repo.db()).writer().execute(sqlx::query("UPDATE conversation_presentations SET submission_id=?1")
            .bind(s.root.as_str()))
            .await
            .unwrap();
        (s.repo.db()).writer().execute(sqlx::query("UPDATE sqlite_sequence SET seq=1000 WHERE name='conversation_presentations'"))
            .await
            .unwrap();
        let mut before = Vec::new();
        for table in TABLES {
            let stored = rows(s.repo.pool(), table).await;
            assert_ne!(stored, "[]", "{table} must hold a row");
            before.push(stored);
        }
        let sequences: Vec<(String, i64)> =
            sqlx::query_as("SELECT name,seq FROM sqlite_sequence ORDER BY name")
                .fetch_all(s.repo.pool())
                .await
                .unwrap();
        let objects = schema(s.repo.pool()).await;
        let Seed { repo, card, .. } = s;
        drop(repo);

        // Apply exactly this forward migration through the shipped migrator, not a later HEAD.
        let db = super::super::super::SqliteDb::connect(&url).await.unwrap();
        let up_to_target = through(to);
        super::super::super::infra::check_no_unknown_future_migrations(db.read_pool(), &up_to_target)
            .await
            .unwrap();
        { let migrator = &(up_to_target);
    (db).writer().with_writer(|write_session| Box::pin(async move {
        write_session.autocommit(|connection| Box::pin(async move {
            Ok::<_, sqlx::Error>(migrator.run_direct(&mut **connection).await)
        })).await
    })).await.unwrap().unwrap()
}.unwrap();
        let upgraded = repo_on(db);
        assert!(
            sqlx::query("PRAGMA foreign_key_check")
                .fetch_all(upgraded.pool())
                .await
                .unwrap()
                .is_empty()
        );
        for (table, expected) in TABLES.into_iter().zip(before) {
            assert_eq!(
                rows(upgraded.pool(), table).await,
                expected,
                "{table} lost or changed rows"
            );
        }
        assert_eq!(
            sqlx::query_as::<_, (String, i64)>("SELECT name,seq FROM sqlite_sequence ORDER BY name")
                .fetch_all(upgraded.pool())
                .await
                .unwrap(),
            sequences
        );
        // Every table, index, trigger and view is exactly as before, except the one widened list.
        let expected: Vec<SchemaObject> = objects
            .into_iter()
            .map(|(kind, name, table, sql)| {
                let sql = if (kind.as_str(), name.as_str()) == ("table", "conversation_turns") {
                    sql.map(|sql| {
                        sql.replace(
                            &format!("CHECK(provider IN {old_providers})"),
                            &format!("CHECK(provider IN {new_providers})"),
                        )
                    })
                } else {
                    sql
                };
                (kind, name, table, sql)
            })
            .collect();
        assert_eq!(schema(upgraded.pool()).await, expected);
        assert_eq!(assistant_trigger_sql(&upgraded).await, assistant_trigger);
        let refusal = assistant_creation_refusal(&upgraded, &card).await;
        assert!(refusal.contains(ASSISTANT_REFUSAL), "{refusal}");
        assert!(turn_admits(&upgraded, &card, added).await);
        assert!(
            !turn_admits(&upgraded, &card, "terminal").await,
            "the CHECK still bounds providers"
        );
        assert!(
            super::super::super::infra::check_no_unknown_future_migrations(upgraded.pool(), &old)
                .await
                .is_err(),
            "old binary must refuse upgraded database"
        );

    })).await.expect("independent write operation")
}
