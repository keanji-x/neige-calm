//! #1917: [`checkout_occupancy`] counts each in-flight in-tree task and each active lease with its
//! own access. #2139 R2: terminal tasks are in-tree tasks too.

use super::workspace_lease_lookup_tests::seed_track;
use super::{CheckoutOccupancy, SqlxRepo, checkout_occupancy};

async fn task(repo: &SqlxRepo, track_id: &str, key: &str, kind: &str, access: &str) -> String {
    let id = format!("{track_id}:{key}");
    (repo.db())
        .writer()
        .execute(
            sqlx::query(
                "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,access,\
         created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,'g','{}','running',?5,1,1)",
            )
            .bind(&id)
            .bind(track_id)
            .bind(key)
            .bind(kind)
            .bind(access),
        )
        .await
        .expect("insert task");
    id
}

async fn lease(repo: &SqlxRepo, track_id: &str, id: &str, access: &str) {
    (repo.db()).writer().execute(sqlx::query(
        "INSERT INTO workspace_leases(lease_id,card_id,track_id,path,state,lease_owner,\
         created_at_ms,updated_at_ms,access_mode) VALUES(?1,?1,?2,'/checkout','held','no-op',1,1,?3)",
    )
    .bind(id)
    .bind(track_id)
    .bind(access))
    .await
    .expect("insert lease");
}

async fn occupancy(repo: &SqlxRepo, track_id: &str, except: &str) -> CheckoutOccupancy {
    checkout_occupancy(&mut repo.pool().acquire().await.unwrap(), track_id, except)
        .await
        .expect("occupancy")
}

#[tokio::test]
async fn in_flight_tasks_count_with_their_own_access() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open repo");
    let track_id = seed_track(&repo).await;
    assert_eq!(
        occupancy(&repo, &track_id, "").await,
        CheckoutOccupancy::Free
    );
    task(&repo, &track_id, "review", "claude", "read_only").await;
    assert_eq!(
        occupancy(&repo, &track_id, "").await,
        CheckoutOccupancy::Readers
    );
    let writer = task(&repo, &track_id, "build", "codex", "read_write").await;
    assert_eq!(
        occupancy(&repo, &track_id, "").await,
        CheckoutOccupancy::Busy
    );
    assert_eq!(
        occupancy(&repo, &track_id, &writer).await,
        CheckoutOccupancy::Readers,
        "the asking attempt is not counted"
    );
}

#[tokio::test]
async fn reclaim_recovery_dispatched_pre_effect_blocker_releases_checkout() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open repo");
    let track_id = seed_track(&repo).await;
    let blocked = task(&repo, &track_id, "blocked", "terminal", "read_write").await;
    repo.db()
        .writer()
        .execute(sqlx::query("UPDATE tasks SET status='dispatched' WHERE id=?1").bind(&blocked))
        .await
        .unwrap();
    repo.db().writer().execute(sqlx::query(
        "INSERT INTO operations \
         (id,operation_key,kind,idempotency_key,payload_hash,target_type,target_json,payload_json,phase,phase_detail_json,created_at_ms,updated_at_ms) \
         VALUES ('blocked-op','blocked-op','terminal-worker',?1,'hash','task','{}',?3,'pending',?2,1,1)",
    )
    .bind(&blocked)
    .bind(
        serde_json::json!({
            "version":"v1",
            "class":"workspace_removal",
            "owner_scope":"workspace-location",
            "resource":"/removing",
            "retry_when":"settled"
        })
        .to_string(),
    )
    .bind(serde_json::json!({"track_id":track_id,"idempotency_key":blocked}).to_string()))
    .await
    .unwrap();
    assert_eq!(
        occupancy(&repo, &track_id, "").await,
        CheckoutOccupancy::Free
    );
    repo.db()
        .writer()
        .execute(sqlx::query(
            "UPDATE operations SET phase_detail_json=NULL WHERE id='blocked-op'",
        ))
        .await
        .unwrap();
    assert_eq!(
        occupancy(&repo, &track_id, "").await,
        CheckoutOccupancy::Busy,
        "missing or non-task release evidence stays fail-closed"
    );
}

#[tokio::test]
async fn reclaim_recovery_checkout_release_requires_complete_bound_blocker() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open repo");
    let track_id = seed_track(&repo).await;
    let blocked = task(&repo, &track_id, "blocked", "terminal", "read_write").await;
    repo.db()
        .writer()
        .execute(sqlx::query("UPDATE tasks SET status='dispatched' WHERE id=?1").bind(&blocked))
        .await
        .unwrap();
    let detail = serde_json::json!({
        "version":"v1","class":"workspace_removal",
        "owner_scope":"workspace-location","resource":"/removing",
        "retry_when":"settled"
    });
    repo.db().writer().execute(sqlx::query(
        "INSERT INTO operations \
         (id,operation_key,kind,idempotency_key,payload_hash,target_type,target_json,payload_json,phase,phase_detail_json,created_at_ms,updated_at_ms) \
         VALUES ('blocked-op','blocked-op','terminal-worker',?1,'hash','task','{}',?2,'pending',?3,1,1)",
    )
    .bind(&blocked)
    .bind(serde_json::json!({"track_id":track_id,"idempotency_key":blocked}).to_string())
    .bind(detail.to_string()))
    .await
    .unwrap();
    assert_eq!(
        occupancy(&repo, &track_id, "").await,
        CheckoutOccupancy::Free
    );

    for malformed in [
        serde_json::json!({"version":"v1","releases_checkout":true}),
        serde_json::json!({
            "version":"v1","class":"not-a-class","owner_scope":"workspace-location",
            "resource":"/removing","retry_when":"settled","releases_checkout":true
        }),
    ] {
        repo.db()
            .writer()
            .execute(
                sqlx::query("UPDATE operations SET phase_detail_json=?1 WHERE id='blocked-op'")
                    .bind(malformed.to_string()),
            )
            .await
            .unwrap();
        assert_eq!(
            occupancy(&repo, &track_id, "").await,
            CheckoutOccupancy::Busy,
            "partial or unknown blocker cannot release the checkout"
        );
    }
    repo.db().writer().execute(sqlx::query(
        "UPDATE operations SET phase_detail_json=?1,kind='forge-action' WHERE id='blocked-op'",
    )
    .bind(detail.to_string()))
    .await
    .unwrap();
    assert_eq!(
        occupancy(&repo, &track_id, "").await,
        CheckoutOccupancy::Busy
    );

    repo.db().writer().execute(sqlx::query(
        "UPDATE operations SET kind='terminal-worker',payload_json=?1 WHERE id='blocked-op'",
    )
    .bind(serde_json::json!({"track_id":"wrong-track","idempotency_key":blocked}).to_string()))
    .await
    .unwrap();
    assert_eq!(
        occupancy(&repo, &track_id, "").await,
        CheckoutOccupancy::Busy
    );
    repo.db().writer().execute(sqlx::query("UPDATE operations SET payload_json=?1,lease_owner='active',lease_until_ms=?2 WHERE id='blocked-op'")
        .bind(serde_json::json!({"track_id":track_id,"idempotency_key":blocked}).to_string())
        .bind(crate::model::now_ms()+60_000)).await.unwrap();
    assert_eq!(
        occupancy(&repo, &track_id, "").await,
        CheckoutOccupancy::Busy
    );
    repo.db()
        .writer()
        .execute(sqlx::query(
            "UPDATE operations SET lease_owner=NULL WHERE id='blocked-op'",
        ))
        .await
        .unwrap();
    assert_eq!(
        occupancy(&repo, &track_id, "").await,
        CheckoutOccupancy::Free
    );
    repo.db()
        .writer()
        .execute(sqlx::query(
            "UPDATE operations SET tx_output_json='{}' WHERE id='blocked-op'",
        ))
        .await
        .unwrap();
    assert_eq!(
        occupancy(&repo, &track_id, "").await,
        CheckoutOccupancy::Busy
    );
}

#[tokio::test]
async fn active_leases_count_with_their_own_access() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open repo");
    let track_id = seed_track(&repo).await;
    lease(&repo, &track_id, "reader-1", "read_only").await;
    lease(&repo, &track_id, "reader-2", "read_only").await;
    assert_eq!(
        occupancy(&repo, &track_id, "").await,
        CheckoutOccupancy::Readers,
        "read-only leases of one path are held side by side"
    );
    lease(&repo, &track_id, "writer", "read_write").await;
    assert_eq!(
        occupancy(&repo, &track_id, "").await,
        CheckoutOccupancy::Busy
    );
}

/// #2139 R2: a terminal task runs in the track's checkout, so an in-flight one occupies it with
/// its access like a codex task (a codex task then waits for it); on the child-track route it
/// does not.
#[tokio::test]
async fn an_in_flight_terminal_task_occupies_the_checkout() {
    let repo = SqlxRepo::open("sqlite::memory:").await.expect("open repo");
    let track_id = seed_track(&repo).await;
    let child = task(&repo, &track_id, "child", "terminal", "read_write").await;
    (repo.db())
        .writer()
        .execute(
            sqlx::query("UPDATE tasks SET spawn = ?1 WHERE id = ?2")
                .bind(calm_types::task_recovery::TASK_CHILD_TRACK_ROUTE)
                .bind(&child),
        )
        .await
        .expect("route the task to a child track");
    assert_eq!(
        occupancy(&repo, &track_id, "").await,
        CheckoutOccupancy::Free,
        "a child-track terminal task runs in its child's workspace"
    );
    let reader = task(&repo, &track_id, "look", "terminal", "read_only").await;
    assert_eq!(
        occupancy(&repo, &track_id, "").await,
        CheckoutOccupancy::Readers,
        "a read-only terminal task occupies the checkout as a reader"
    );
    let shell = task(&repo, &track_id, "shell", "terminal", "read_write").await;
    assert_eq!(
        occupancy(&repo, &track_id, "").await,
        CheckoutOccupancy::Busy,
        "a terminal task that changes the checkout holds it alone"
    );
    assert_eq!(
        occupancy(&repo, &track_id, &shell).await,
        CheckoutOccupancy::Readers,
        "the asking attempt is not counted"
    );
    assert_eq!(
        occupancy(&repo, &track_id, &reader).await,
        CheckoutOccupancy::Busy
    );
}
