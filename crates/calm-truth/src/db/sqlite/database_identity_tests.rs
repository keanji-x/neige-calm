//! `database_identity` is one row that every open of the same file reads back.

use std::str::FromStr;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use super::SqlxRepo;
use super::database_identity::ensure_database_identity;

fn file_url(dir: &tempfile::TempDir) -> String {
    format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("identity.db").display()
    )
}

async fn raw_pool(url: &str) -> SqlitePool {
    let opts = SqliteConnectOptions::from_str(url)
        .unwrap()
        .create_if_missing(true);
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap()
}

async fn identity_rows(pool: &SqlitePool) -> Vec<(i64, String)> {
    sqlx::query("SELECT singleton, id FROM database_identity ORDER BY singleton")
        .fetch_all(pool)
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.get::<i64, _>("singleton"), row.get::<String, _>("id")))
        .collect()
}

/// Two opens of one file are two boots: the second reads the first's id.
#[tokio::test]
async fn database_id_survives_reopen_of_the_same_file() {
    let dir = tempfile::tempdir().unwrap();
    let url = file_url(&dir);

    let first = SqlxRepo::open(&url).await.unwrap();
    let first_id = first.database_id.clone();
    uuid::Uuid::parse_str(&first_id).expect("the identity is a uuid");
    assert_eq!(
        identity_rows(first.pool()).await,
        vec![(1, first_id.to_string())]
    );
    drop(first);

    let second = SqlxRepo::open(&url).await.unwrap();
    assert_eq!(
        second.database_id, first_id,
        "a reboot must read the minted id back, not mint a new one"
    );
    assert_eq!(
        identity_rows(second.pool()).await,
        vec![(1, first_id.to_string())],
        "still one row after the second open"
    );

    // Two separate databases are two identities.
    let other = SqlxRepo::open("sqlite::memory:").await.unwrap();
    assert_ne!(other.database_id, first_id);
}

/// `INSERT OR IGNORE` on the fixed key serializes on the write lock and every loser reads the winner's row.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn database_id_concurrent_boot_reads_one_id() {
    let dir = tempfile::tempdir().unwrap();
    let url = file_url(&dir);

    // Migrate without minting: the race under test is the mint itself.
    let setup = raw_pool(&url).await;
    crate::MIGRATOR.run(&setup).await.unwrap();
    assert!(identity_rows(&setup).await.is_empty());
    setup.close().await;

    let mut pools = Vec::new();
    for _ in 0..8 {
        pools.push(raw_pool(&url).await);
    }
    let mut tasks = Vec::new();
    for pool in &pools {
        let pool = pool.clone();
        tasks.push(tokio::spawn(async move {
            ensure_database_identity(&pool).await
        }));
    }
    let mut ids = Vec::new();
    for task in tasks {
        ids.push(task.await.unwrap().unwrap());
    }
    assert!(
        ids.iter().all(|id| id == &ids[0]),
        "every concurrent boot must read the same id: {ids:?}"
    );
    assert_eq!(
        identity_rows(&pools[0]).await,
        vec![(1, ids[0].clone())],
        "one row, the winner's"
    );
}

/// The two tests above stay green without the CHECK (they only ever insert key 1).
#[tokio::test]
async fn database_identity_rejects_second_row() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let before = identity_rows(repo.pool()).await;
    assert_eq!(before.len(), 1);

    let err =
        sqlx::query("INSERT INTO database_identity(singleton, id, minted_at_ms) VALUES (2, ?1, 0)")
            .bind("second-identity")
            .execute(repo.pool())
            .await
            .expect_err("a second key must be refused");
    let message = err.to_string();
    assert!(
        message.contains("CHECK constraint failed"),
        "expected a CHECK violation, got: {message}"
    );
    assert_eq!(
        identity_rows(repo.pool()).await,
        before,
        "still the one row"
    );
}
