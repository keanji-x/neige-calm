//! #1434 — forward-only request-fingerprint migration for track-create keys.

use std::borrow::Cow;

use sqlx::sqlite::SqlitePoolOptions;

fn migrator_through(version: i64) -> sqlx::migrate::Migrator {
    sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|migration| migration.version <= version)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    }
}

#[tokio::test]
async fn migration_0089_preserves_old_bindings_as_explicitly_unknown() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open migration fixture");
    migrator_through(88)
        .run(&pool)
        .await
        .expect("apply migrations through 0088");
    sqlx::query(
        "INSERT INTO track_create_idempotency \
         (area_id, idempotency_key, track_id, planner_card_id, report_card_id, created_at_ms) \
         VALUES ('area', 'key', 'track', 'planner', 'report', 1)",
    )
    .execute(&pool)
    .await
    .expect("seed an 0088 binding");

    migrator_through(89)
        .run(&pool)
        .await
        .expect("apply migration 0089");

    let row: (i64, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT request_fingerprint_version, create_request_sha256, first_message_sha256 \
         FROM track_create_idempotency WHERE area_id = 'area' AND idempotency_key = 'key'",
    )
    .fetch_one(&pool)
    .await
    .expect("read the upgraded binding");
    assert_eq!(row, (0, None, None));
}

#[tokio::test]
async fn migration_0089_enforces_the_versioned_fingerprint_shape() {
    const CREATE_HASH: &str = concat!(
        "aaaaaaaa", "aaaaaaaa", "aaaaaaaa", "aaaaaaaa", "aaaaaaaa", "aaaaaaaa", "aaaaaaaa",
        "aaaaaaaa"
    );
    const MESSAGE_HASH: &str = concat!(
        "bbbbbbbb", "bbbbbbbb", "bbbbbbbb", "bbbbbbbb", "bbbbbbbb", "bbbbbbbb", "bbbbbbbb",
        "bbbbbbbb"
    );
    const NON_HEX_CREATE_HASH: &str = concat!(
        "aaaaaaaa", "aaaaaaaa", "aaaaaaaa", "aaaaaaaa", "aaaaaaaa", "aaaaaaaa", "aaaaaaaa",
        "aaaaaaag"
    );
    const NON_HEX_MESSAGE_HASH: &str = concat!(
        "bbbbbbbb", "bbbbbbbb", "bbbbbbbb", "bbbbbbbb", "bbbbbbbb", "bbbbbbbb", "bbbbbbbb",
        "bbbbbbbg"
    );
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open migration fixture");
    migrator_through(89)
        .run(&pool)
        .await
        .expect("apply migrations through 0089");

    for (key, version, create_hash, message_hash) in [
        ("known-without-message", 1, Some(CREATE_HASH), None),
        ("known-without-create", 1, None, Some(MESSAGE_HASH)),
        ("legacy-with-create", 0, Some(CREATE_HASH), None),
        ("legacy-with-message", 0, None, Some(MESSAGE_HASH)),
        ("known-short-create", 1, Some("aaaa"), Some(MESSAGE_HASH)),
        ("known-short-message", 1, Some(CREATE_HASH), Some("bbbb")),
        (
            "known-non-hex-create",
            1,
            Some(NON_HEX_CREATE_HASH),
            Some(MESSAGE_HASH),
        ),
        (
            "known-non-hex-message",
            1,
            Some(CREATE_HASH),
            Some(NON_HEX_MESSAGE_HASH),
        ),
        ("unknown-version", 2, Some(CREATE_HASH), Some(MESSAGE_HASH)),
    ] {
        let error = sqlx::query(
            "INSERT INTO track_create_idempotency \
             (area_id, idempotency_key, track_id, planner_card_id, report_card_id, created_at_ms, \
              request_fingerprint_version, create_request_sha256, first_message_sha256) \
             VALUES ('area', ?1, 'track', 'planner', 'report', 1, ?2, ?3, ?4)",
        )
        .bind(key)
        .bind(version)
        .bind(create_hash)
        .bind(message_hash)
        .execute(&pool)
        .await
        .expect_err("the fingerprint state must be whole");
        assert!(
            error.to_string().contains("CHECK constraint failed"),
            "key={key}: {error}"
        );
    }

    sqlx::query(
        "INSERT INTO track_create_idempotency \
         (area_id, idempotency_key, track_id, planner_card_id, report_card_id, created_at_ms, \
          request_fingerprint_version, create_request_sha256, first_message_sha256) \
         VALUES ('area', 'known-complete', 'track', 'planner', 'report', 1, 1, ?1, ?2)",
    )
    .bind(CREATE_HASH)
    .bind(MESSAGE_HASH)
    .execute(&pool)
    .await
    .expect("a complete pair of valid V1 fingerprints must remain admissible");
}
