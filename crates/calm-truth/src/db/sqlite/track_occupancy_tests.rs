//! #1917: [`checkout_occupancy`] counts each in-flight in-tree task and each active lease with its
//! own access.

use super::workspace_lease_lookup_tests::seed_track;
use super::{CheckoutOccupancy, SqlxRepo, checkout_occupancy};

async fn task(repo: &SqlxRepo, track_id: &str, key: &str, kind: &str, access: &str) -> String {
    let id = format!("{track_id}:{key}");
    sqlx::query(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,access,\
         created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,'g','{}','running',?5,1,1)",
    )
    .bind(&id)
    .bind(track_id)
    .bind(key)
    .bind(kind)
    .bind(access)
    .execute(repo.pool())
    .await
    .expect("insert task");
    id
}

async fn lease(repo: &SqlxRepo, track_id: &str, id: &str, access: &str) {
    sqlx::query(
        "INSERT INTO workspace_leases(lease_id,card_id,track_id,path,state,lease_owner,\
         created_at_ms,updated_at_ms,access_mode) VALUES(?1,?1,?2,'/checkout','held','no-op',1,1,?3)",
    )
    .bind(id)
    .bind(track_id)
    .bind(access)
    .execute(repo.pool())
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
    task(&repo, &track_id, "shell", "terminal", "read_write").await;
    assert_eq!(
        occupancy(&repo, &track_id, "").await,
        CheckoutOccupancy::Free,
        "a terminal task does not use the checkout"
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
