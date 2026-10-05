//! #2068: an over-long `Idempotency-Key` on `POST /api/tracks` is refused by the shared parser on
//! both create shapes, before anything is minted. The other keyed answers (a different create under
//! a used key, the cross-instance primary-key race) are pinned in the parent file.

use axum::http::StatusCode;

use super::boot;

#[tokio::test]
async fn an_over_long_key_is_invalid_on_both_create_shapes_and_mints_nothing() {
    let b = boot().await;
    let key = "k".repeat(129);
    for message in [Some("a first message"), None] {
        let (status, body) = b.create_track(Some(&key), message).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "message={message:?} body={body}"
        );
        assert_eq!(
            body["code"], "idempotency_key_invalid",
            "message={message:?} body={body}"
        );
    }
    assert_eq!(b.track_count().await, 0);
    assert_eq!(b.binding_count().await, 0);
}
