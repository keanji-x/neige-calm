use super::*;

#[tokio::test]
async fn acp_workspace_resident_inventory_preserves_siblings_and_drains_legacy() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query("CREATE TABLE workspace_executions (execution_id TEXT, desired TEXT, secrets_json TEXT, alias TEXT, intent_json TEXT)")
        .execute(&pool).await.unwrap();
    for (id, backend, track) in [
        ("legacy", "opencode", None),
        ("selected", "opencode", Some("track-a")),
        ("rebound", "opencode", Some("track-a")),
        ("sibling", "opencode", Some("track-b")),
        ("other-provider", "antigravity", Some("track-a")),
    ] {
        let scope = track.map_or_else(
            || serde_json::json!({"backend": backend}),
            |track| serde_json::json!(track),
        );
        let intent = serde_json::json!({"consumer":CONSUMER, "track":scope,
            "owner":format!("{CONSUMER}:{backend}:workspace:{id}")});
        sqlx::query("INSERT INTO workspace_executions VALUES (?1,'run','{}',?2,?3)")
            .bind(id)
            .bind(format!("{CONSUMER}:{backend}:workspace:{id}:generation"))
            .bind(intent.to_string())
            .execute(&pool)
            .await
            .unwrap();
    }
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(
        sql::resident_inventory_tx(
            &mut tx,
            CONSUMER,
            Some("opencode"),
            Some("acp-resident:opencode:workspace:selected")
        )
        .await
        .unwrap(),
        ["legacy", "selected"]
    );
    assert_eq!(
        sql::resident_inventory_tx(&mut tx, CONSUMER, Some("opencode"), None)
            .await
            .unwrap(),
        ["legacy", "rebound", "selected", "sibling"]
    );
    assert_eq!(
        sql::resident_inventory_tx(&mut tx, CONSUMER, None, None)
            .await
            .unwrap()
            .len(),
        5
    );
    // A damaged alias does not hide the selected Track's committed Run: its
    // independently stored ownership still names the same consumer and scope.
    sqlx::query("UPDATE workspace_executions SET alias='damaged' WHERE execution_id='selected'")
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(
        sql::resident_inventory_tx(
            &mut tx,
            CONSUMER,
            Some("opencode"),
            Some("acp-resident:opencode:workspace:selected")
        )
        .await
        .unwrap(),
        ["legacy", "selected"]
    );
    assert_eq!(
        sql::resident_inventory_tx(&mut tx, CONSUMER, Some("opencode"), None)
            .await
            .unwrap(),
        ["legacy", "rebound", "selected", "sibling"]
    );
}

#[tokio::test]
async fn acp_workspace_repoint_has_an_independent_resident_launch_boundary() {
    let directory = tempfile::tempdir().unwrap();
    let old = directory.path().join("old");
    let new = directory.path().join("new");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::create_dir_all(&new).unwrap();
    let host = AcpPlannerHost::unconfigured_scratch().unwrap();
    let original = host
        .resident_slot(&AgentProvider::OpenCode, "same-track", &old)
        .unwrap();
    let _launch = original.lock().await;
    let rebound = host
        .resident_slot(&AgentProvider::OpenCode, "same-track", &new)
        .unwrap();
    assert!(
        rebound.try_lock().is_ok(),
        "a repointed workspace must not wait on the old resident launch"
    );
    let same = host
        .resident_slot(&AgentProvider::OpenCode, "same-track", &old)
        .unwrap();
    assert!(
        same.try_lock().is_err(),
        "one workspace binding serializes its resident startup"
    );
}
