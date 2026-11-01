use super::*;

async fn rejects_mutation(db: &SqliteDb) {
    let error = sqlx::query("INSERT INTO read_fence VALUES('forbidden')")
        .execute(db.read_pool())
        .await
        .unwrap_err();
    assert!(
        error
            .as_database_error()
            .is_some_and(|error| error.code().as_deref() == Some("8")),
        "read capacity must reject mutation with SQLITE_READONLY: {error:?}"
    );
}

#[tokio::test]
async fn published_read_capacity_rejects_writes_after_connection_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let file = format!("sqlite://{}", dir.path().join("read-fence.db").display());
    for url in ["sqlite::memory:", file.as_str()] {
        let db = SqliteDb::connect(url).await.unwrap();
        db.writer()
            .execute(sqlx::query("CREATE TABLE read_fence(value TEXT NOT NULL)"))
            .await
            .unwrap();
        rejects_mutation(&db).await;
        let connection = db.read_pool().acquire().await.unwrap();
        connection.close().await.unwrap();
        rejects_mutation(&db).await;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM read_fence")
                .fetch_one(db.read_pool())
                .await
                .unwrap(),
            0
        );
        db.writer()
            .execute(sqlx::query("INSERT INTO read_fence VALUES('admitted')"))
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM read_fence")
                .fetch_one(db.read_pool())
                .await
                .unwrap(),
            1
        );
        db.close().await;
    }
}
