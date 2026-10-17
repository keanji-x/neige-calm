//! The original binding is the future sole resource/fingerprint source for ACKs.
//! Exercise the real acceptance transaction before attacking that persisted fact.
use super::*;

async fn mutation_is_refused(sql: &str) {
    let fx = Fixture::new().await;
    let key = "immutable-binding";
    let accepted = fx.accept(key, FIRST).await.unwrap().ack;
    let mut connection = fx.repo.pool().acquire().await.unwrap();
    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    assert_eq!(foreign_keys, 1);
    // REPLACE must not rely on SQLite firing recursive DELETE triggers.
    sqlx::query("PRAGMA recursive_triggers=OFF")
        .execute(&mut *connection)
        .await
        .unwrap();
    let changed = sqlx::query(sql)
        .bind(&fx.area)
        .bind(key)
        .execute(&mut *connection)
        .await;
    drop(connection);
    assert!(
        changed.is_err(),
        "a referenced creation binding changed: {sql}"
    );
    assert_replayed(fx.accept(key, FIRST).await.unwrap(), &accepted);
}

#[tokio::test]
async fn canonical_ack_prevents_original_binding_update() {
    mutation_is_refused(
        "UPDATE track_create_idempotency SET track_id='replacement-track' \
         WHERE area_id=?1 AND idempotency_key=?2",
    )
    .await;
}

#[tokio::test]
async fn canonical_ack_prevents_original_binding_delete() {
    mutation_is_refused(
        "DELETE FROM track_create_idempotency WHERE area_id=?1 AND idempotency_key=?2",
    )
    .await;
}

#[tokio::test]
async fn canonical_ack_prevents_original_binding_insert_or_replace() {
    mutation_is_refused(
        "INSERT OR REPLACE INTO track_create_idempotency \
         (area_id,idempotency_key,track_id,planner_card_id,report_card_id,created_at_ms, \
          request_fingerprint_version,create_request_sha256,first_message_sha256) \
         SELECT area_id,idempotency_key,'replacement-track',planner_card_id,report_card_id, \
                created_at_ms,request_fingerprint_version,create_request_sha256,first_message_sha256 \
         FROM track_create_idempotency WHERE area_id=?1 AND idempotency_key=?2",
    )
    .await;
}

#[tokio::test]
async fn canonical_ack_prevents_another_binding_update_or_replace_over_its_key() {
    let fx = Fixture::new().await;
    let key = "protected-key";
    let accepted = fx.accept(key, FIRST).await.unwrap().ack;
    sqlx::query(
        "INSERT INTO track_create_idempotency \
         (area_id,idempotency_key,track_id,planner_card_id,report_card_id,created_at_ms, \
          request_fingerprint_version,create_request_sha256,first_message_sha256) \
         SELECT area_id,'unreferenced-key','other-track','other-planner','other-report', \
                created_at_ms,request_fingerprint_version,create_request_sha256,first_message_sha256 \
         FROM track_create_idempotency WHERE area_id=?1 AND idempotency_key=?2",
    )
    .bind(&fx.area)
    .bind(key)
    .execute(fx.repo.pool())
    .await
    .unwrap();
    let changed = sqlx::query(
        "UPDATE OR REPLACE track_create_idempotency SET idempotency_key=?2 \
         WHERE area_id=?1 AND idempotency_key='unreferenced-key'",
    )
    .bind(&fx.area)
    .bind(key)
    .execute(fx.repo.pool())
    .await;
    assert!(
        changed.is_err(),
        "an unrelated binding replaced the original ACK binding"
    );
    assert_replayed(fx.accept(key, FIRST).await.unwrap(), &accepted);
}

#[tokio::test]
async fn canonical_extension_cannot_be_replaced_without_recursive_delete_triggers() {
    mutation_is_refused(
        "INSERT OR REPLACE INTO conversation_human_track_creations \
         (area_id,idempotency_key,source_user_id,ack_version,origin_runtime_id, \
          message_id,message_revision,presentation_sequence) \
         SELECT area_id,idempotency_key,source_user_id,ack_version,origin_runtime_id, \
                message_id,message_revision,presentation_sequence \
         FROM conversation_human_track_creations WHERE area_id=?1 AND idempotency_key=?2",
    )
    .await;
}

#[tokio::test]
async fn historical_noncanonical_fingerprints_do_not_become_human_receipts() {
    for version in [0_i64, 2_i64] {
        let fx = Fixture::new().await;
        let key = "historical-noncanonical";
        let create_sha =
            create_request_digest(&CreateRequestShape::from_request(&fx.request(FIRST))).unwrap();
        sqlx::query(
            "INSERT INTO track_create_idempotency \
             (area_id,idempotency_key,track_id,planner_card_id,report_card_id,created_at_ms, \
              request_fingerprint_version,create_request_sha256,first_message_sha256) \
             VALUES(?1,?2,'historic-track','historic-planner','historic-report',0,?3,?4,NULL)",
        )
        .bind(&fx.area)
        .bind(key)
        .bind(version)
        .bind((version == 2).then_some(create_sha))
        .execute(fx.repo.pool())
        .await
        .unwrap();
        assert!(matches!(
            fx.accept(key, FIRST).await,
            Err(CalmError::IdempotencyKeyReused(_))
        ));
        assert_eq!(fx.counts().await, (0, 0, 0, 0, 0, 0));
    }
}
