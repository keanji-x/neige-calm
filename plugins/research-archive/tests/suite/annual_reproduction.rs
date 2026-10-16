use research_archive::{Catalog, Store};

#[test]
fn annual_endpoint_is_available() {
    assert!(Catalog::compiled().get("consensus_annual").is_some());
}

#[tokio::test]
async fn annual_new_archive_uses_v2() {
    let dir = tempfile::tempdir().unwrap();
    let path = Store::path(dir.path());
    Store::open(&path).await.unwrap().close().await;
    let mut conn = crate::support::raw(&path, false).await;
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(version, 2);
}
