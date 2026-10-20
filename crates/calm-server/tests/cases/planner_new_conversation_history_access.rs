//! #2774 — `GET /api/cards/{id}/planner/conversations` reads under the Track's current Area
//! access, as the transcript read does: a person is admitted before the list is read, and access is
//! checked again before the list is returned.

use axum::http::StatusCode;
use calm_server::db::prelude::*;
use calm_server::model::new_id;
use calm_server::routes::tracks::{TrackDetailReadHook, install_track_detail_read_hook_for_test};
use calm_server::session_projection_repo::WorkerSessionProjection;
use serde_json::{Value, json};

use crate::planner_first_start::Boot;
use crate::planner_new_conversation::{person, record_a_reply, send};
use crate::planner_repoint_restart_lock::joined;
use crate::planner_restart::ordinary_track;

/// A Track whose Planner has one earlier conversation holding a row, and `alice`, a registered
/// person who is a member of the Track's Area only when `member` is set.
async fn with_history(member: bool) -> (Boot, WorkerSessionProjection, String) {
    use calm_server::db::sqlite::area_membership::application_user_register;
    let boot = ordinary_track().await;
    let old = boot.active().await;
    record_a_reply(&boot, &old).await;
    let track = boot
        .repo
        .card_get(&boot.planner_card_id)
        .await
        .unwrap()
        .unwrap()
        .track_id;
    let (status, body) = boot.new_conversation(&new_id()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let area = boot
        .repo
        .track_get(track.as_str())
        .await
        .unwrap()
        .unwrap()
        .area_id;
    let mut tx = boot.repo.pool().begin().await.unwrap();
    application_user_register(&mut tx, "alice", "Alice")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    if member {
        sqlx::query("INSERT INTO area_members VALUES(?1,'alice','member',0)")
            .bind(area.as_str())
            .execute(boot.repo.pool())
            .await
            .unwrap();
    }
    (boot, old, area.to_string())
}

async fn revoke(boot: &Boot, area: &str) {
    sqlx::query("DELETE FROM area_members WHERE area_id = ?1 AND user_id = 'alice'")
        .bind(area)
        .execute(boot.repo.pool())
        .await
        .unwrap();
}

async fn list(boot: &Boot) -> (StatusCode, Value) {
    let uri = boot.card_uri("planner/conversations");
    send(person(boot, "alice"), "GET", uri, None, None).await
}

fn assert_refused(status: StatusCode, body: &Value) {
    assert_eq!(status, StatusCode::FORBIDDEN, "body={body}");
    assert!(
        !body.to_string().contains("ordinal"),
        "no history leaks: {body}"
    );
}

#[tokio::test]
async fn planner_new_conversation_history_list_refuses_a_person_outside_the_area() {
    let (boot, old, _) = with_history(false).await;
    let (status, body) = list(&boot).await;
    assert_refused(status, &body);
    boot.shutdown_session(&old.id).await;
    boot.shutdown().await;
}

#[tokio::test]
async fn planner_new_conversation_history_list_reads_for_a_member_until_removed() {
    let (boot, old, area) = with_history(true).await;
    let (status, body) = list(&boot).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body.as_array().map(Vec::len), Some(1), "body={body}");
    assert_eq!(body[0]["ordinal"], json!(0));
    revoke(&boot, &area).await;
    let (status, body) = list(&boot).await;
    assert_refused(status, &body);
    boot.shutdown_session(&old.id).await;
    boot.shutdown().await;
}

/// A member removed after the list is read, and before it is returned, receives nothing.
#[tokio::test]
async fn planner_new_conversation_history_list_rechecks_access_before_answering() {
    let (boot, old, area) = with_history(true).await;
    let track = boot
        .repo
        .card_get(&boot.planner_card_id)
        .await
        .unwrap()
        .unwrap()
        .track_id;
    let hook = TrackDetailReadHook {
        entered: Default::default(),
        release: Default::default(),
    };
    install_track_detail_read_hook_for_test(track.as_str(), hook.clone());
    let request = {
        let (app, uri) = (
            person(&boot, "alice"),
            boot.card_uri("planner/conversations"),
        );
        tokio::spawn(async move { send(app, "GET", uri, None, None).await })
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), hook.entered.notified())
        .await
        .expect("the list was read after admission");
    revoke(&boot, &area).await;
    hook.release.notify_one();
    let (status, body) = joined(request, "the revoked list").await;
    assert_refused(status, &body);
    boot.shutdown_session(&old.id).await;
    boot.shutdown().await;
}
