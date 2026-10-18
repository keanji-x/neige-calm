use super::*;

#[tokio::test]
async fn archive_migration_upgrades_current_main_without_rewriting_released_migrations() {
    use sqlx::migrate::Migrator;
    use std::borrow::Cow;
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let head = sqlx::migrate!("../calm-truth/migrations");
    let main = Migrator {
        migrations: Cow::Owned(head.iter().filter(|m| m.version <= 168).cloned().collect()),
        ..Migrator::DEFAULT
    };
    main.run(&pool).await.unwrap();
    let absent: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE name='assistant_conversation_archives'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(absent, 0);
    head.run(&pool).await.unwrap();
    let version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, 169);
    let present: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE name='assistant_conversation_archives'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(present, 1);
}

#[tokio::test]
async fn archive_assignment_rolls_back_and_cascades_with_its_card() {
    use calm_server::db::sqlite::conversation_archive_set_tx;
    let b = boot().await;
    let track = b.create_track("Archive transaction").await;
    let card = assistant(&b, &track, "codex").await;
    let mut tx = b.repo.pool().begin().await.unwrap();
    assert!(
        conversation_archive_set_tx(&mut tx, &card, true)
            .await
            .unwrap()
    );
    tx.rollback().await.unwrap();
    assert!(
        b.state
            .repo
            .conversation_archived_at(&card)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        archive(&b, &track, &card, true).await.0,
        StatusCode::NO_CONTENT
    );
    let mut tx = b.repo.pool().begin().await.unwrap();
    sqlx::query("DELETE FROM cards WHERE id=?")
        .bind(&card)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(
        b.state
            .repo
            .conversation_archived_at(&card)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn archive_route_rolls_back_metadata_card_and_event_on_write_failure() {
    let b = boot().await;
    let track = b.create_track("Atomic archive event").await;
    let card = assistant(&b, &track, "codex").await;
    let original = b.repo.card_get(&card).await.unwrap().unwrap();
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER refuse_archive_update BEFORE UPDATE ON cards
         BEGIN SELECT RAISE(ABORT,'archive fixture write refusal'); END",
    )
    .execute(b.repo.pool())
    .await
    .unwrap();
    let (status, body) = archive(&b, &track, &card, true).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert!(
        b.state
            .repo
            .conversation_archived_at(&card)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        b.repo.card_get(&card).await.unwrap().unwrap().updated_at,
        original.updated_at
    );
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(b.repo.pool())
        .await
        .unwrap();
    assert_eq!(before, after);
}
