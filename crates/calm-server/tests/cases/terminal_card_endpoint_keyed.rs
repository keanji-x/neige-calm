//! #2068: the keyed-write answers of `POST /api/tracks/{id}/terminal-cards` — an over-long key,
//! the same key with a different body, and a duplicate that passes the operation dedup check and
//! reaches the `operations` UNIQUE backstop.

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use calm_server::test_seams::{OPERATION_DEDUP_MISSED, PausePoint, install_pause_for_test};
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::{boot_happy, post_with_idempotency};

fn body(program: &str) -> Value {
    json!({ "program": program, "cwd": "", "env": {}, "sort": 1.0, "theme": {"fg": [216,219,226], "bg": [15,20,24]} })
}

async fn operations_under(boot: &super::Boot, key: &str) -> i64 {
    let pool = boot.repo.sqlite_db().expect("sqlite repo");
    sqlx::query_scalar("SELECT COUNT(*) FROM operations WHERE idempotency_key = ?1")
        .bind(key)
        .fetch_one(pool.read_pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn an_over_long_key_is_invalid_and_submits_nothing() {
    let boot = boot_happy().await;
    let uri = format!("/api/tracks/{}/terminal-cards", boot.track_id);
    let key = "k".repeat(129);
    let (status, answer) =
        post_with_idempotency(boot.app.clone(), uri, body("/bin/sh"), Some(&key)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
    assert_eq!(answer["code"], "idempotency_key_invalid", "{answer}");
    assert_eq!(operations_under(&boot, &key).await, 0);
}

#[tokio::test]
async fn the_same_key_with_another_body_is_reused_and_final() {
    let boot = boot_happy().await;
    let uri = format!("/api/tracks/{}/terminal-cards", boot.track_id);
    let (status, answer) =
        post_with_idempotency(boot.app.clone(), uri.clone(), body("/bin/sh"), Some("k-1")).await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    let (status, answer) =
        post_with_idempotency(boot.app.clone(), uri, body("/bin/bash"), Some("k-1")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert_eq!(answer["code"], "idempotency_key_reused", "{answer}");
    assert_eq!(operations_under(&boot, "k-1").await, 1);
}

/// The first request is paused inside the operation insert, after its dedup read found nothing; the
/// second commits the operation under the same key meanwhile. The first's INSERT then hits the
/// `(kind, idempotency_key)` UNIQUE index, and the backstop joins the stored operation.
#[tokio::test]
async fn a_duplicate_past_the_dedup_check_joins_the_stored_card() {
    let boot = boot_happy().await;
    let uri = format!("/api/tracks/{}/terminal-cards", boot.track_id);
    let paused = PausePoint {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    install_pause_for_test(OPERATION_DEDUP_MISSED, "k-race", paused.clone());
    let (first, second) = tokio::join!(
        post_with_idempotency(
            boot.app.clone(),
            uri.clone(),
            body("/bin/sh"),
            Some("k-race")
        ),
        async {
            tokio::time::timeout(Duration::from_secs(10), paused.entered.notified())
                .await
                .expect("the first request passed its dedup read; without it the case is vacuous");
            let second = post_with_idempotency(
                boot.app.clone(),
                uri.clone(),
                body("/bin/sh"),
                Some("k-race"),
            )
            .await;
            paused.release.notify_one();
            second
        }
    );
    assert_eq!(second.0, StatusCode::CREATED, "{}", second.1);
    assert_eq!(first.0, StatusCode::CREATED, "{}", first.1);
    assert_eq!(first.1["id"], second.1["id"]);
    assert_eq!(
        operations_under(&boot, "k-race").await,
        1,
        "one operation: the first request's INSERT was refused by the index"
    );
}

/// A boot whose every terminal spawn fails after the card's transaction committed, so the saga
/// compensates and the operation ends `Failed`.
async fn boot_failing_spawn() -> super::Boot {
    let super::Boot {
        state,
        track_id,
        events,
        repo,
        _tmp,
        ..
    } = super::boot().await;
    let hook: super::TestSpawnHook = Arc::new(|_terminal_id, _program, _cwd, _env| {
        Box::pin(async {
            Err(calm_server::error::CalmError::Internal(
                "injected spawn failure".into(),
            ))
        })
    });
    let state = super::install_spawn_runtime_with_hook(state, repo.clone(), events.clone(), hook);
    let app = calm_server::routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state.clone());
    super::Boot {
        app,
        state,
        track_id,
        events,
        repo,
        _tmp,
    }
}

/// #2131 S4: a create that failed after its commit is final under its key. The first answer and
/// every replay are 500 `operation_failed`, which the client reads as final and so releases the key.
#[tokio::test]
async fn a_create_that_failed_after_its_commit_is_final_under_its_key() {
    let boot = boot_failing_spawn().await;
    let uri = format!("/api/tracks/{}/terminal-cards", boot.track_id);
    for attempt in ["first", "replay"] {
        let (status, answer) = post_with_idempotency(
            boot.app.clone(),
            uri.clone(),
            body("/bin/sh"),
            Some("k-failed"),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{attempt}: {answer}"
        );
        assert_eq!(answer["code"], "operation_failed", "{attempt}: {answer}");
    }
    assert_eq!(operations_under(&boot, "k-failed").await, 1);
    let cards = boot.repo.cards_by_track(&boot.track_id).await.unwrap();
    assert!(
        cards.is_empty(),
        "the compensation removed the card: {cards:?}"
    );
}

/// Writes `stuck` onto the one operation a keyed create left under `key`, as the driver's stuck
/// sink does, recording `from_phase` as the phase it stopped at.
async fn stamp_stuck(boot: &super::Boot, key: &str, from_phase: &str) {
    let pool = boot.repo.sqlite_db().expect("sqlite repo");
    let updated = sqlx::query(
        "UPDATE operations \
         SET phase = 'stuck', \
             last_error = 'operation drive failed: injected', \
             phase_detail_json = ?1, \
             lease_owner = NULL, \
             lease_until_ms = NULL \
         WHERE idempotency_key = ?2",
    )
    .bind(
        json!({
            "reason": "operation drive failed: injected",
            "since": 1,
            "from_phase": from_phase,
        })
        .to_string(),
    )
    .bind(key)
    .execute(pool.writer_pool())
    .await
    .unwrap()
    .rows_affected();
    assert_eq!(updated, 1, "premise: the create's own operation went stuck");
}

/// #2175: how a replay classifies a key whose operation is stuck at `pending`. The premise is
/// stamped onto a create that succeeded (its card exists), so this pins the replay's answer, not
/// that a create stopped at `pending` wrote nothing; that rests on the pending arm committing its
/// effects with the lease cleared. A stuck operation is never driven again, so every replay answers
/// the same `operation_failed` (the answer the client mints a new key after) and makes no card.
#[tokio::test]
async fn a_key_stuck_at_pending_replays_as_failed_and_drives_nothing() {
    let boot = boot_happy().await;
    let uri = format!("/api/tracks/{}/terminal-cards", boot.track_id);
    let (status, answer) = post_with_idempotency(
        boot.app.clone(),
        uri.clone(),
        body("/bin/sh"),
        Some("k-stuck-pending"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    stamp_stuck(&boot, "k-stuck-pending", "pending").await;
    let before = boot
        .repo
        .cards_by_track(&boot.track_id)
        .await
        .unwrap()
        .len();
    for attempt in ["replay", "second replay"] {
        let (status, answer) = post_with_idempotency(
            boot.app.clone(),
            uri.clone(),
            body("/bin/sh"),
            Some("k-stuck-pending"),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{attempt}: {answer}"
        );
        assert_eq!(answer["code"], "operation_failed", "{attempt}: {answer}");
    }
    assert_eq!(operations_under(&boot, "k-stuck-pending").await, 1);
    let after = boot
        .repo
        .cards_by_track(&boot.track_id)
        .await
        .unwrap()
        .len();
    assert_eq!(after, before, "a replay under a stuck key makes no card");
}

/// #2175: stuck past `pending`, what the create made may exist, so the replay says so with its own
/// code, `operation_stuck`, which the client reads as final for the key and asks the reader to look.
#[tokio::test]
async fn a_create_stuck_after_its_commit_replays_as_stuck_and_makes_no_card() {
    let boot = boot_happy().await;
    let uri = format!("/api/tracks/{}/terminal-cards", boot.track_id);
    let (status, answer) = post_with_idempotency(
        boot.app.clone(),
        uri.clone(),
        body("/bin/sh"),
        Some("k-stuck-spawn"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");
    stamp_stuck(&boot, "k-stuck-spawn", "spawn_started").await;
    let before = boot
        .repo
        .cards_by_track(&boot.track_id)
        .await
        .unwrap()
        .len();
    for attempt in ["replay", "second replay"] {
        let (status, answer) = post_with_idempotency(
            boot.app.clone(),
            uri.clone(),
            body("/bin/sh"),
            Some("k-stuck-spawn"),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{attempt}: {answer}"
        );
        assert_eq!(answer["code"], "operation_stuck", "{attempt}: {answer}");
    }
    assert_eq!(operations_under(&boot, "k-stuck-spawn").await, 1);
    let after = boot
        .repo
        .cards_by_track(&boot.track_id)
        .await
        .unwrap()
        .len();
    assert_eq!(after, before, "a replay of a stuck create makes no card");
}

async fn placement(boot: &super::Boot, card_id: &str, body: Value) -> (StatusCode, Value) {
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let reply = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/cards/{card_id}/terminal-workbench"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = reply.status();
    let bytes = reply.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn terminal_presentation_retry_refreshes_live_membership_for_atomic_and_generic_receipts() {
    for generic in [false, true] {
        let boot = boot_happy().await;
        let uri = format!(
            "/api/tracks/{}/{}",
            boot.track_id,
            if generic { "cards" } else { "terminal-cards" }
        );
        let request = if generic {
            json!({"kind":"terminal","payload":null})
        } else {
            body("/bin/sh")
        };
        let key = if generic {
            "placement-generic"
        } else {
            "placement-atomic"
        };
        let (status, first) =
            post_with_idempotency(boot.app.clone(), uri.clone(), request.clone(), Some(key)).await;
        assert_eq!(status, StatusCode::CREATED, "{first}");
        assert_eq!(
            first["payload"]["terminal_presentation"],
            json!({"source":"manual","in_workbench":true})
        );
        let id = first["id"].as_str().unwrap();
        let pool = boot.repo.sqlite_db().unwrap();
        let original: String =
            sqlx::query_scalar("SELECT tx_output_json FROM operations WHERE idempotency_key=?1")
                .bind(key)
                .fetch_one(pool.read_pool())
                .await
                .unwrap();
        let mut old: Value = serde_json::from_str(&original).unwrap();
        old["result"]["payload"] = Value::Null;
        let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE operations SET tx_output_json=?1 WHERE idempotency_key=?2")
            .bind(old.to_string())
            .bind(key)
            .execute(&mut **tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let before:(String,String,String)=sqlx::query_as("SELECT payload_json,payload_hash,tx_output_json FROM operations WHERE idempotency_key=?1").bind(key).fetch_one(pool.read_pool()).await.unwrap();
        let (status, moved) = placement(&boot, id, json!({"in_workbench":false})).await;
        assert_eq!(status, StatusCode::OK, "{moved}");
        let (status, replayed) =
            post_with_idempotency(boot.app.clone(), uri, request, Some(key)).await;
        assert_eq!(status, StatusCode::CREATED, "{replayed}");
        assert_eq!(replayed["id"], first["id"]);
        assert_eq!(
            replayed["payload"]["terminal_presentation"],
            json!({"source":"manual","in_workbench":false})
        );
        assert_eq!(operations_under(&boot, key).await, 1);
        let after:(String,String,String)=sqlx::query_as("SELECT payload_json,payload_hash,tx_output_json FROM operations WHERE idempotency_key=?1").bind(key).fetch_one(pool.read_pool()).await.unwrap();
        assert_eq!(
            after, before,
            "live response refresh must not rewrite the immutable receipt or request"
        );
        let (status, _) = placement(&boot, id, json!({"in_workbench":true,"source":"agent"})).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let current = boot.repo.card_get(id).await.unwrap().unwrap();
        assert_eq!(
            current.payload["terminal_presentation"],
            json!({"source":"manual","in_workbench":false})
        );
    }
}

#[tokio::test]
async fn terminal_presentation_replay_refuses_a_deleted_terminal_without_recreating_it() {
    let boot = boot_happy().await;
    let uri = format!("/api/tracks/{}/cards", boot.track_id);
    let request = json!({"kind":"terminal","payload":null});
    let (status, card) = post_with_idempotency(
        boot.app.clone(),
        uri.clone(),
        request.clone(),
        Some("deleted-placement"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{card}");
    boot.repo
        .card_delete(card["id"].as_str().unwrap())
        .await
        .unwrap();
    let (status, replay) =
        post_with_idempotency(boot.app.clone(), uri, request, Some("deleted-placement")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{replay}");
    assert_eq!(operations_under(&boot, "deleted-placement").await, 1);
    assert!(
        boot.repo
            .cards_by_track(&boot.track_id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn terminal_presentation_generic_create_rejects_client_provenance() {
    let boot = boot_happy().await;
    let uri = format!("/api/tracks/{}/cards", boot.track_id);
    let (status, answer) = post_with_idempotency(boot.app.clone(), uri,
        json!({"kind":"terminal","payload":{"terminal_presentation":{"source":"manual","in_workbench":true}}}), Some("spoofed-placement")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
    assert_eq!(operations_under(&boot, "spoofed-placement").await, 0);
}

#[tokio::test]
async fn terminal_presentation_replay_refuses_a_card_retargeted_out_of_terminal() {
    for generic in [false, true] {
        let boot = boot_happy().await;
        let uri = format!(
            "/api/tracks/{}/{}",
            boot.track_id,
            if generic { "cards" } else { "terminal-cards" }
        );
        let request = if generic {
            json!({"kind":"terminal","payload":null})
        } else {
            body("/bin/sh")
        };
        let (status, card) = post_with_idempotency(
            boot.app.clone(),
            uri.clone(),
            request.clone(),
            Some("retargeted-placement"),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{card}");
        let pool = boot.repo.sqlite_db().unwrap();
        for payload in [
            json!({"terminal_presentation":{"source":"manual","in_workbench":true}}),
            json!({}),
        ] {
            // Both post-upgrade sticky metadata and an old retargeted card with no new metadata.
            let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
                .await
                .unwrap();
            sqlx::query("UPDATE cards SET kind='codex',payload=?1 WHERE id=?2")
                .bind(payload.to_string())
                .bind(card["id"].as_str().unwrap())
                .execute(&mut **tx)
                .await
                .unwrap();
            tx.commit().await.unwrap();
            let (status, replay) = post_with_idempotency(
                boot.app.clone(),
                uri.clone(),
                request.clone(),
                Some("retargeted-placement"),
            )
            .await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{replay}");
        }
        assert_eq!(operations_under(&boot, "retargeted-placement").await, 1);
    }
}
