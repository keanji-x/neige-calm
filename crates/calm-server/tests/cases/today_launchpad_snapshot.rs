use super::SqlxRepo;

// Snapshot every persisted value, including raw history and exact JSON text.
pub(crate) async fn database(repo: &SqlxRepo) -> Vec<(String, Vec<String>)> {
    database_except(repo, &[]).await
}

pub(crate) async fn database_except(
    repo: &SqlxRepo,
    track_metadata: &[&str],
) -> Vec<(String, Vec<String>)> {
    let mut tx = repo.pool().begin().await.unwrap();
    let tables: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .fetch_all(&mut *tx)
            .await
            .unwrap();
    let mut result = vec![];
    for table in tables {
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
                .bind(&table)
                .fetch_all(&mut *tx)
                .await
                .unwrap();
        let expressions = columns
            .iter()
            .map(|c| {
                if table == "tracks" && track_metadata.contains(&c.as_str()) {
                    "NULL".into()
                } else {
                    format!("quote(\"{c}\")")
                }
            })
            .collect::<Vec<_>>()
            .join(",");
        let sql =
            format!("SELECT json_array({expressions}) FROM \"{table}\" ORDER BY {expressions}");
        let rows = sqlx::query_scalar(&sql).fetch_all(&mut *tx).await.unwrap();
        result.push((table, rows));
    }
    tx.rollback().await.unwrap();
    result
}
