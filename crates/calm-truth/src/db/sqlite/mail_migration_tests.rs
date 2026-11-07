//! Upgrade through production boot with foreign keys enabled, including a populated reply chain.

use sqlx::sqlite::SqlitePoolOptions;

#[tokio::test]
async fn mail_migration_preserves_all_fields_reply_chain_and_constraints() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("mail.db").display());
    let old_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    sqlx::query("PRAGMA foreign_keys=ON")
        .execute(&old_pool)
        .await
        .unwrap();
    let old = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|m| m.version < 214)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    old.run(&old_pool).await.unwrap();
    sqlx::raw_sql("INSERT INTO areas(id,name,color,sort,created_at,updated_at) VALUES('a','a','#000',0,1,1);
        INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) VALUES('t1','a','one',0,1,1),('t2','a','two',1,2,3),('t3','a','three',2,4,5);
        INSERT INTO mails VALUES('m1','t1','t2',NULL,'first résumé','body\n一',1,123,456),
        ('m2','t2','t1','m1','reply','second body',6,789,NULL),
        ('m3','t1','t3','m2','third','third body',3,900,901);")
        .execute(&old_pool).await.unwrap();
    let select = "SELECT id,from_track_id,to_track_id,reply_to,summary,text,hop,sent_at,read_at FROM mails ORDER BY id";
    type Mail = (
        String,
        String,
        String,
        Option<String>,
        String,
        String,
        i64,
        i64,
        Option<i64>,
    );
    let before: Vec<Mail> = sqlx::query_as(select).fetch_all(&old_pool).await.unwrap();
    let indexes_before: Vec<(String,String)> = sqlx::query_as("SELECT name,sql FROM sqlite_master WHERE tbl_name='mails' AND sql IS NOT NULL AND type='index' ORDER BY name").fetch_all(&old_pool).await.unwrap();
    old_pool.close().await;
    let repo = super::SqlxRepo::open(&url).await.unwrap();
    let pool = repo.pool();
    let after: Vec<Mail> = sqlx::query_as(select).fetch_all(pool).await.unwrap();
    assert_eq!(
        after, before,
        "every field and reply edge survives the rebuild"
    );
    let indexes_after: Vec<(String,String)> = sqlx::query_as("SELECT name,sql FROM sqlite_master WHERE tbl_name='mails' AND sql IS NOT NULL AND type='index' ORDER BY name").fetch_all(pool).await.unwrap();
    assert_eq!(indexes_after, indexes_before);
    assert!(
        sqlx::query_scalar::<_, bool>("PRAGMA foreign_keys")
            .fetch_one(pool)
            .await
            .unwrap()
    );
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(pool)
            .await
            .unwrap()
            .is_empty()
    );
    let writer = repo.db().writer();
    for hop in [7_i64, 21, i64::MAX] {
        writer
            .execute(sqlx::query("UPDATE mails SET hop=? WHERE id='m3'").bind(hop))
            .await
            .unwrap();
    }
    for assignment in [
        "hop=0",
        "hop='bad'",
        "hop=x'01'",
        "summary=''",
        "summary='x'||printf('%0200d',0)",
        "text=''",
        "text=printf('%08001d',0)",
        "from_track_id=to_track_id",
        "reply_to='missing'",
        "from_track_id='missing'",
        "to_track_id='missing'",
        "sent_at=NULL",
        "hop=NULL",
        "summary=NULL",
        "text=NULL",
    ] {
        assert!(
            writer
                .execute(sqlx::query(&format!(
                    "UPDATE mails SET {assignment} WHERE id='m3'"
                )))
                .await
                .is_err(),
            "{assignment}"
        );
    }
    writer
        .execute(sqlx::query("DELETE FROM tracks WHERE id='t2'"))
        .await
        .unwrap();
    let kept: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT id,reply_to FROM mails ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap();
    assert_eq!(
        kept,
        vec![("m3".into(), None)],
        "both track FKs cascade; a surviving reply is detached"
    );
}
