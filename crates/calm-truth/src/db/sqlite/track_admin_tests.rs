use sqlx::sqlite::SqlitePoolOptions;
use std::borrow::Cow;

async fn before_admin() -> sqlx::SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let migrator = sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|migration| migration.version < 165)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    migrator.run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO areas(id,name,color,sort,created_at,updated_at,kind) \
         VALUES('a','a','#000',0,0,0,'user'),('s','s','#000',0,0,0,'system')",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

async fn seed(pool: &sqlx::SqlitePool, id: &str) {
    sqlx::query("INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) VALUES(?1,'a',?1,0,100,100)")
        .bind(id).execute(pool).await.unwrap();
}

#[tokio::test]
async fn admin_backfill_grandfathers_eligible_creators_only() {
    let pool = before_admin().await;
    for id in [
        "eligible", "managed", "system", "child", "chat", "bound", "added", "unused", "parent",
    ] {
        seed(&pool, id).await;
        if id != "unused" {
            let add = format!("{id}-add");
            seed(&pool, &add).await;
            sqlx::query(
                "UPDATE tracks SET creator_track_id=?1,creator_key='key',created_at=17 WHERE id=?2",
            )
            .bind(id)
            .bind(add)
            .execute(&pool)
            .await
            .unwrap();
        }
    }
    seed(&pool, "second").await;
    sqlx::query("UPDATE tracks SET creator_track_id='eligible',creator_key='two',created_at=29 WHERE id='second'").execute(&pool).await.unwrap();
    for statement in [
        "UPDATE tracks SET area_id='s' WHERE id='system'",
        "UPDATE tracks SET parent_track_id='parent' WHERE id='child'",
        "UPDATE tracks SET purpose='area-chat' WHERE id='chat'",
        "UPDATE tracks SET plugin_scope='bound-plugin' WHERE id='bound'",
        "UPDATE tracks SET creator_track_id='parent',creator_key='added' WHERE id='added'",
        "INSERT INTO managed_track_identities VALUES('owner','identity','managed','area','UTC','standard',0)",
    ] {
        sqlx::query(statement).execute(&pool).await.unwrap();
    }
    crate::MIGRATOR.run(&pool).await.unwrap();
    let granted: Vec<(String, i64)> = sqlx::query_as(
        "SELECT id,admin_granted_at FROM tracks WHERE admin_granted_at IS NOT NULL ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        granted,
        vec![("eligible".into(), 17), ("parent".into(), 17)]
    );
}

#[tokio::test]
async fn the_database_refuses_an_admin_child_or_added_track() {
    let pool = before_admin().await;
    seed(&pool, "parent").await;
    seed(&pool, "target").await;
    crate::MIGRATOR.run(&pool).await.unwrap();
    for shape in [
        "parent_track_id='parent'",
        "creator_track_id='parent',creator_key='key'",
        "purpose='area-chat'",
    ] {
        let error = sqlx::query(&format!(
            "UPDATE tracks SET admin_granted_at=1,{shape} WHERE id='target'"
        ))
        .execute(&pool)
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("track_admin_is_top_level"),
            "{error}"
        );
    }
}
