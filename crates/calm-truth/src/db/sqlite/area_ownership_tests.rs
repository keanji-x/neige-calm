//! Ownership and creator identity use the real migration and persistent writers.
use std::borrow::Cow;

use super::area_membership::{
    application_user_register, area_require_access, areas_list_for_member,
};
use super::{
    SqlxRepo, area_create_bind_tx, area_create_replay_tx, area_create_system_tx, area_create_tx,
    area_delete_tx,
};
use crate::error::TruthError;
use crate::model::NewArea;
use calm_types::organization::{AreaAccess, LEGACY_OWNER_USER_ID};

fn new_area() -> NewArea {
    NewArea {
        name: "Owned".into(),
        color: "#123".into(),
        sort: None,
    }
}

#[tokio::test]
async fn area_ownership_migration_adopts_live_and_deleted_keys_once_and_preserves_events() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("legacy.db").display()
    );
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    sqlx::query("PRAGMA foreign_keys=ON")
        .execute(&pool)
        .await
        .unwrap();
    let old = sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|m| m.version <= 165)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    old.run(&pool).await.unwrap();
    sqlx::query("INSERT INTO areas(id,name,color,sort,kind,created_at,updated_at) VALUES ('live','Old','#000',1,'user',10,11),('system','system','#000',-1,'system',5,5)").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO area_create_idempotency VALUES ('raw-live-key','v1:original\\hash','live'),('raw-deleted-key','v1:tombstone','deleted')").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO events(kind,payload,actor,at,scope_kind) VALUES('area.updated','{\"unmodified\":true}','user',42,'system')").execute(&pool).await.unwrap();
    let event: (i64, String, String, String, i64, String) =
        sqlx::query_as("SELECT id,kind,payload,actor,at,scope_kind FROM events")
            .fetch_one(&pool)
            .await
            .unwrap();
    pool.close().await;
    let repo = SqlxRepo::open(&url).await.unwrap();
    let members: Vec<(String, String, String, i64)> =
        sqlx::query_as("SELECT area_id,user_id,role,joined_at FROM area_members")
            .fetch_all(repo.pool())
            .await
            .unwrap();
    assert_eq!(
        members,
        vec![(
            "live".into(),
            LEGACY_OWNER_USER_ID.into(),
            "owner".into(),
            10
        )]
    );
    let keys: Vec<(String,String,String,String)> = sqlx::query_as("SELECT creator_user_id,idempotency_key,request_fingerprint,area_id FROM area_create_idempotency ORDER BY area_id").fetch_all(repo.pool()).await.unwrap();
    assert_eq!(
        keys,
        vec![
            (
                LEGACY_OWNER_USER_ID.into(),
                "raw-deleted-key".into(),
                "v1:tombstone".into(),
                "deleted".into()
            ),
            (
                LEGACY_OWNER_USER_ID.into(),
                "raw-live-key".into(),
                "v1:original\\hash".into(),
                "live".into()
            )
        ]
    );
    assert_eq!(
        sqlx::query_as::<_, (i64, String, String, String, i64, String)>(
            "SELECT id,kind,payload,actor,at,scope_kind FROM events"
        )
        .fetch_one(repo.pool())
        .await
        .unwrap(),
        event
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM pragma_foreign_key_check")
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        0
    );
    let mut replay_tx = repo.pool().begin().await.unwrap();
    assert_eq!(
        area_create_replay_tx(
            &mut replay_tx,
            LEGACY_OWNER_USER_ID,
            "raw-live-key",
            "v1:original\\hash"
        )
        .await
        .unwrap()
        .unwrap()
        .id
        .as_str(),
        "live"
    );
    assert!(matches!(
        area_create_replay_tx(
            &mut replay_tx,
            LEGACY_OWNER_USER_ID,
            "raw-deleted-key",
            "v1:tombstone"
        )
        .await,
        Err(TruthError::IdempotencyKeyExhausted(_))
    ));
    replay_tx.rollback().await.unwrap();
    // Reopening must not recreate a removed relation or adopt later corrupt rows.
    sqlx::query("DELETE FROM area_members WHERE area_id='live'")
        .execute(repo.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO areas(id,name,color,sort,kind,created_at,updated_at) VALUES('unowned','Corrupt','#000',2,'user',20,20)").execute(repo.pool()).await.unwrap();
    repo.pool().close().await;
    let reopened = SqlxRepo::open(&url).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM area_members")
            .fetch_one(reopened.pool())
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn area_ownership_requires_registered_creator_and_commits_exactly_one_owner() {
    use crate::db::RepoSyncDomainRaw;
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    assert!(matches!(
        repo.area_create("unknown", new_area()).await,
        Err(TruthError::Forbidden(_))
    ));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM areas")
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        0
    );
    let mut tx = repo.pool().begin().await.unwrap();
    application_user_register(&mut tx, "alice", "Alice")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let area = repo.area_create("alice", new_area()).await.unwrap();
    let owners: Vec<(String, String)> =
        sqlx::query_as("SELECT user_id,role FROM area_members WHERE area_id=?1")
            .bind(area.id.as_str())
            .fetch_all(repo.pool())
            .await
            .unwrap();
    assert_eq!(owners, vec![("alice".into(), "owner".into())]);
    assert!(
        sqlx::query("INSERT INTO area_members VALUES(?1,?2,'owner',0)")
            .bind(area.id.as_str())
            .bind(LEGACY_OWNER_USER_ID)
            .execute(repo.pool())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn area_ownership_members_collaborate_owner_deletes_and_system_has_no_members() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let mut tx = repo.pool().begin().await.unwrap();
    application_user_register(&mut tx, "alice", "Alice")
        .await
        .unwrap();
    application_user_register(&mut tx, "outsider", "Outsider")
        .await
        .unwrap();
    let area = area_create_tx(&mut tx, LEGACY_OWNER_USER_ID, new_area())
        .await
        .unwrap();
    sqlx::query("INSERT INTO area_members VALUES(?1,'alice','member',0)")
        .bind(area.id.as_str())
        .execute(&mut *tx)
        .await
        .unwrap();
    area_require_access(&mut tx, area.id.as_str(), "alice", AreaAccess::Collaborate)
        .await
        .unwrap();
    assert!(matches!(
        area_require_access(&mut tx, area.id.as_str(), "alice", AreaAccess::Delete).await,
        Err(TruthError::Forbidden(_))
    ));
    assert!(matches!(
        area_require_access(
            &mut tx,
            area.id.as_str(),
            "outsider",
            AreaAccess::Collaborate
        )
        .await,
        Err(TruthError::Forbidden(_))
    ));
    assert_eq!(
        areas_list_for_member(&mut tx, "alice").await.unwrap().len(),
        1
    );
    assert!(
        areas_list_for_member(&mut tx, "outsider")
            .await
            .unwrap()
            .is_empty()
    );
    let system = area_create_system_tx(&mut tx).await.unwrap();
    assert!(
        sqlx::query("INSERT INTO area_members VALUES(?1,'alice','member',0)")
            .bind(system.id.as_str())
            .execute(&mut *tx)
            .await
            .is_err()
    );
    assert!(matches!(
        area_require_access(
            &mut tx,
            system.id.as_str(),
            LEGACY_OWNER_USER_ID,
            AreaAccess::Collaborate
        )
        .await,
        Err(TruthError::Forbidden(_))
    ));
    area_require_access(
        &mut tx,
        area.id.as_str(),
        LEGACY_OWNER_USER_ID,
        AreaAccess::Delete,
    )
    .await
    .unwrap();
    area_delete_tx(&mut tx, area.id.as_str()).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM area_members")
            .fetch_one(&mut *tx)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM application_users")
            .fetch_one(&mut *tx)
            .await
            .unwrap(),
        3
    );
}

#[tokio::test]
async fn area_ownership_creator_keys_are_independent_permanent_and_replay_checks_current_membership()
 {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let mut tx = repo.pool().begin().await.unwrap();
    application_user_register(&mut tx, "alice", "Alice")
        .await
        .unwrap();
    let first = area_create_tx(&mut tx, LEGACY_OWNER_USER_ID, new_area())
        .await
        .unwrap();
    area_create_bind_tx(
        &mut tx,
        LEGACY_OWNER_USER_ID,
        "same",
        "v1:owner",
        first.id.as_str(),
    )
    .await
    .unwrap();
    assert!(
        area_create_replay_tx(&mut tx, "alice", "same", "v1:alice")
            .await
            .unwrap()
            .is_none()
    );
    let second = area_create_tx(&mut tx, "alice", new_area()).await.unwrap();
    area_create_bind_tx(&mut tx, "alice", "same", "v1:alice", second.id.as_str())
        .await
        .unwrap();
    assert_ne!(first.id, second.id);
    assert_eq!(
        area_create_replay_tx(&mut tx, "alice", "same", "v1:alice")
            .await
            .unwrap()
            .unwrap()
            .id,
        second.id
    );
    assert!(matches!(
        area_create_replay_tx(&mut tx, "alice", "same", "different").await,
        Err(TruthError::IdempotencyKeyReused(_))
    ));
    sqlx::query("DELETE FROM area_members WHERE area_id=?1")
        .bind(second.id.as_str())
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(matches!(
        area_create_replay_tx(&mut tx, "alice", "same", "v1:alice").await,
        Err(TruthError::Forbidden(_))
    ));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM area_members WHERE area_id=?1")
            .bind(second.id.as_str())
            .fetch_one(&mut *tx)
            .await
            .unwrap(),
        0
    );
    area_delete_tx(&mut tx, first.id.as_str()).await.unwrap();
    assert!(matches!(
        area_create_replay_tx(&mut tx, LEGACY_OWNER_USER_ID, "same", "v1:owner").await,
        Err(TruthError::IdempotencyKeyExhausted(_))
    ));
    assert!(
        sqlx::query("DELETE FROM area_create_idempotency")
            .execute(&mut *tx)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE area_create_idempotency SET request_fingerprint='changed'")
            .execute(&mut *tx)
            .await
            .is_err()
    );
}
