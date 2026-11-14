//! Resource writes must consume actual credential capabilities, not actor labels.
use super::*;
use calm_truth::db::RepoRead;

#[tokio::test]
async fn guest_cannot_manually_write_report_or_any_block_port() {
    let mut results = vec![];
    for port in 0..5 {
        let fx = Fixture::new().await;
        fx.state
            .raw_repo()
            .card_create(calm_server::model::NewCard {
                track_id: fx.track.clone().into(),
                kind: "track-report".into(),
                sort: None,
                title: None,
                payload: serde_json::to_value(
                    calm_server::track_report::TrackReportPayload::initial(),
                )
                .unwrap(),
            })
            .await
            .unwrap();
        let report = format!("/api/tracks/{}/report", fx.track);
        let (status, mut block) = fx
            .request(
                "POST",
                &format!("{report}/blocks"),
                Some(&fx.cookie),
                json!({"kind":"prose","markdown":"Owner block","ifDocRev":0}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{block}");
        if port == 3 {
            let (status, second) = fx.request("POST", &format!("{report}/blocks"), Some(&fx.cookie),
                json!({"kind":"prose","markdown":"Owner second block","ifDocRev":block["docRev"]})).await;
            assert_eq!(status, StatusCode::OK, "{second}");
            block["docRev"] = second["docRev"].clone();
        }
        let block_id = block["id"].as_str().unwrap();
        let link = fx.create(true).await;
        let id = link["link"]["delegation"]["id"].as_str().unwrap();
        let cookie = super::tokens::unlock(&fx, &link).await;
        let (method, path, body) = [
            (
                "POST",
                report.clone(),
                json!({"summary":"Guest","body":"Guest","ifDocRev":block["docRev"]}),
            ),
            (
                "POST",
                format!("{report}/blocks"),
                json!({"kind":"prose","markdown":"Guest","ifDocRev":block["docRev"]}),
            ),
            (
                "PATCH",
                format!("{report}/blocks/{block_id}"),
                json!({"kind":"prose","markdown":"Guest","ifBlockRev":block["rev"]}),
            ),
            (
                "POST",
                format!("{report}/blocks/{block_id}/move"),
                json!({"toIndex":1,"ifDocRev":block["docRev"]}),
            ),
            (
                "DELETE",
                format!("{report}/blocks/{block_id}"),
                json!({"ifBlockRev":block["rev"]}),
            ),
        ]
        .into_iter()
        .nth(port)
        .unwrap();
        let before = fx
            .request("GET", &report, Some(&fx.cookie), Value::Null)
            .await;
        let result = fx
            .request(
                method,
                &format!("{path}?share_link={id}"),
                Some(&cookie),
                body.clone(),
            )
            .await;
        let after = fx
            .request("GET", &report, Some(&fx.cookie), Value::Null)
            .await;
        let owner = fx.request(method, &path, Some(&fx.cookie), body).await;
        results.push((method, path, result.0, before == after, owner));
    }
    assert!(
        results.iter().all(
            |(_, _, status, unchanged, owner)| *status == StatusCode::FORBIDDEN
                && *unchanged
                && owner.0 == StatusCode::OK
        ),
        "{results:?}"
    );
}

#[tokio::test]
async fn guest_cannot_control_historical_human_queue() {
    use crate::support::planner_queue_fixture::{
        Issuance, boot_with_issuance, get, idle_snapshot, post_input,
    };
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    assert_eq!(
        post_input(
            boot.app.clone(),
            boot.planner_card.id.as_str(),
            "Owner running"
        )
        .await
        .0,
        StatusCode::OK
    );
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let (_, run) = get(
                boot.app.clone(),
                format!("/api/cards/{}/planner/run", boot.planner_card.id),
            )
            .await;
            if run["phase"] == "turn_running" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    boot.harness.pause_issuance_for_dev();
    assert_eq!(boot.daemon.started_turns_for_test().len(), 1);
    let mut fx = Fixture::with_state(boot.state.clone()).await;
    fx.track = boot.planner_card.track_id.to_string();
    fx.area = boot
        .repo
        .track_get(&fx.track)
        .await
        .unwrap()
        .unwrap()
        .area_id
        .to_string();
    let link = fx.create(true).await;
    let id = link["link"]["delegation"]["id"].as_str().unwrap();
    let cookie = super::tokens::unlock(&fx, &link).await;
    let mut results = vec![];
    for method in ["PATCH", "DELETE", "POST"] {
        let (status, ack) = post_input(
            boot.app.clone(),
            boot.planner_card.id.as_str(),
            "Owner pending",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{ack}");
        let entry = ack["entry_id"].as_str().unwrap();
        let path = format!(
            "/api/cards/{}/planner/input/{entry}{}?share_link={id}",
            boot.planner_card.id,
            if method == "POST" { "/steer" } else { "" }
        );
        let result = fx
            .request(
                method,
                &path,
                Some(&cookie),
                if method == "PATCH" {
                    json!({"text":"Guest rewrite","if_entry_rev":0})
                } else {
                    json!({"if_entry_rev":0})
                },
            )
            .await;
        results.push((method, result.0));
    }
    let (_, run) = fx
        .request(
            "GET",
            &format!("/api/cards/{}/planner/run", boot.planner_card.id),
            Some(&fx.cookie),
            Value::Null,
        )
        .await;
    let pending = run["pending"].as_array().unwrap();
    let intact = pending.len() == 3
        && pending
            .iter()
            .all(|p| p["text"] == "Owner pending" && p["rev"] == 0);
    let no_provider = boot.daemon.started_turns_for_test().len() == 1
        && boot.daemon.steered_turns_for_test().is_empty();
    boot.harness.shutdown().await.unwrap();
    assert!(
        results.iter().all(|(_, s)| *s == StatusCode::FORBIDDEN) && intact && no_provider,
        "{results:?}, {run}, no_provider={no_provider}"
    );
}

async fn logout_before_decision(stage: &str, replay: bool) {
    let fx = Fixture::new().await;
    let (card, _) = super::fixtures::canonical(&fx).await;
    let link = fx.create(true).await;
    let id = link["link"]["delegation"]["id"].as_str().unwrap();
    let cookie = super::tokens::unlock(&fx, &link).await;
    let path = format!("/api/cards/{card}/planner/input?share_link={id}");
    if replay {
        let initial = fx
            .app
            .clone()
            .oneshot(
                Request::post(&path)
                    .header("cookie", &cookie)
                    .header("content-type", "application/json")
                    .header("Idempotency-Key", "logout-race")
                    .body(Body::from(json!({"text":"must not persist"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(initial.status(), StatusCode::OK);
    }
    let pause = calm_server::test_seams::PausePoint::default();
    calm_server::test_seams::install_pause_for_test(stage, &card, pause.clone());
    let app = fx.app.clone();
    let path = format!("/api/cards/{card}/planner/input?share_link={id}");
    let sending_cookie = cookie.clone();
    let sending = tokio::spawn(async move {
        app.oneshot(
            Request::post(path)
                .header("cookie", sending_cookie)
                .header("content-type", "application/json")
                .header("Idempotency-Key", "logout-race")
                .body(Body::from(json!({"text":"must not persist"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), pause.entered.notified())
        .await
        .unwrap();
    assert_eq!(
        fx.request(
            "POST",
            &format!("/api/auth/logout?share_link={id}"),
            Some(&cookie),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    pause.release.notify_one();
    assert_eq!(sending.await.unwrap().status(), StatusCode::UNAUTHORIZED);
    let db = fx.state.raw_repo().sqlite_db().unwrap();
    let pool = db.read_pool();
    let messages: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM conversation_messages WHERE author_share_link_id=?1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap();
    let receipts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM planner_input_idempotency WHERE card_id=?1")
            .bind(&card)
            .fetch_one(pool)
            .await
            .unwrap();
    let dispatches: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM events WHERE kind='harness.user_message.enqueued' AND scope_card=?1",
    )
    .bind(&card)
    .fetch_one(pool)
    .await
    .unwrap();
    let expected = i64::from(replay);
    assert_eq!(
        (messages, receipts, dispatches),
        (expected, expected, expected)
    );
}

#[tokio::test]
async fn guest_logout_before_acceptance_leaves_no_message_receipt_or_dispatch() {
    logout_before_decision("share-input-before-accept", false).await;
}
#[tokio::test]
async fn guest_logout_before_replay_rejects_access_and_preserves_receipt() {
    logout_before_decision("share-input-before-accept", true).await;
}
#[tokio::test]
async fn guest_logout_before_commit_rolls_back_message_receipt_and_dispatch() {
    logout_before_decision("share-input-before-commit", false).await;
}
#[tokio::test]
async fn guest_logout_before_replay_commit_rejects_access_and_preserves_receipt() {
    logout_before_decision("share-input-before-commit", true).await;
}

#[tokio::test]
async fn guest_image_declaration_and_upload_match_text_only_send() {
    use crate::support::planner_queue_fixture::{boot_with, idle_snapshot};
    let boot = boot_with(idle_snapshot(vec![])).await;
    let mut fx = Fixture::with_state(
        boot.state
            .clone()
            .with_shared_codex_appserver(fresh_state().await.shared_codex_appserver.clone()),
    )
    .await;
    fx.track = boot.planner_card.track_id.to_string();
    fx.area = boot
        .repo
        .track_get(&fx.track)
        .await
        .unwrap()
        .unwrap()
        .area_id
        .to_string();
    let (assistant, _) = super::fixtures::canonical(&fx).await;
    let historical = boot.planner_card.id.to_string();
    let (created_status, created) = fx.request("POST", "/api/tracks", Some(&fx.cookie),
        json!({"planner_provider":"codex","theme":{"fg":[255,255,255],"bg":[0,0,0]},"area_id":fx.area,"title":"Image declaration Planner"})).await;
    assert_eq!(created_status, StatusCode::CREATED, "{created}");
    let planner: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=?1 AND role='planner'")
            .bind(created["id"].as_str().unwrap())
            .fetch_one(boot.repo.pool())
            .await
            .unwrap();
    let link = fx.create(false).await;
    let id = link["link"]["delegation"]["id"].as_str().unwrap();
    let cookie = super::tokens::unlock(&fx, &link).await;
    let mut results = vec![];
    for card in [&historical, &assistant, &planner] {
        for contract in ["?contract=conversation_v1&", "?"] {
            let (status, run) = fx
                .request(
                    "GET",
                    &format!("/api/cards/{card}/planner/run{contract}share_link={id}"),
                    Some(&cookie),
                    Value::Null,
                )
                .await;
            if contract == "?" && card != &historical {
                assert_eq!(status, StatusCode::CONFLICT);
            } else {
                assert_eq!(status, StatusCode::OK, "{run}");
                results.push((
                    card.clone(),
                    "declaration",
                    run["attachments_supported"] == false,
                ));
                assert_eq!(run["manual_compact_supported"], false, "{run}");
            }
        }
        let upload = fx
            .app
            .clone()
            .oneshot(
                Request::post(format!(
                    "/api/cards/{card}/planner/attachments?share_link={id}"
                ))
                .header("cookie", &cookie)
                .body(Body::from(vec![137, 80, 78, 71, 13, 10, 26, 10, 0]))
                .unwrap(),
            )
            .await
            .unwrap();
        results.push((
            card.clone(),
            "upload",
            upload.status() == StatusCode::FORBIDDEN,
        ));
        let (_, owner) = fx
            .request(
                "GET",
                &format!("/api/cards/{card}/planner/run?contract=conversation_v1"),
                Some(&fx.cookie),
                Value::Null,
            )
            .await;
        assert_eq!(owner["attachments_supported"], true, "{owner}");
        let upload = fx
            .app
            .clone()
            .oneshot(
                Request::post(format!("/api/cards/{card}/planner/attachments"))
                    .header("cookie", &fx.cookie)
                    .body(Body::from(vec![137, 80, 78, 71, 13, 10, 26, 10]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(upload.status(), StatusCode::CREATED);
    }
    boot.harness.shutdown().await.unwrap();
    assert!(results.iter().all(|(_, _, ok)| *ok), "{results:?}");
}

#[tokio::test]
async fn historical_compact_keeps_request_source_through_effect_admission() {
    use crate::support::planner_queue_fixture::{SEED_THREAD_ID, boot_with, idle_snapshot};
    let mut snapshot = idle_snapshot(vec![]);
    snapshot.last_turn_id = Some("previous-turn".into());
    let boot = boot_with(snapshot).await;
    let mut fx = Fixture::with_state(boot.state.clone()).await;
    fx.track = boot.planner_card.track_id.to_string();
    let link = fx.create(true).await;
    let id = link["link"]["delegation"]["id"].as_str().unwrap();
    let cookie = super::tokens::unlock(&fx, &link).await;
    let path = format!("/api/cards/{}/planner/compact", boot.planner_card.id);
    let before = boot.harness.snapshot().await.unwrap();
    let guest = fx
        .request(
            "POST",
            &format!("{path}?share_link={id}"),
            Some(&cookie),
            json!({}),
        )
        .await;
    let unchanged = boot.harness.snapshot().await.unwrap() == before;
    let no_effect = boot.daemon.compacted_threads_for_test().is_empty();
    assert_eq!(
        guest.0,
        StatusCode::FORBIDDEN,
        "{guest:?}; unchanged={unchanged}; no_effect={no_effect}"
    );
    assert!(unchanged && no_effect);
    let owner = fx.request("POST", &path, Some(&fx.cookie), json!({})).await;
    assert_eq!(owner.0, StatusCode::OK, "{owner:?}");
    assert_eq!(boot.daemon.compacted_threads_for_test(), [SEED_THREAD_ID]);
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn historical_compact_rechecks_owner_source_at_provider_effect() {
    use crate::support::planner_queue_fixture::{SEED_THREAD_ID, boot_with, idle_snapshot};
    let mut snapshot = idle_snapshot(vec![]);
    snapshot.last_turn_id = Some("previous-turn".into());
    let boot = boot_with(snapshot).await;
    let fx = Fixture::with_state(boot.state.clone()).await;
    let pause = calm_server::test_seams::PausePoint::default();
    calm_server::test_seams::install_pause_for_test(
        "historical-compact-before-effect",
        SEED_THREAD_ID,
        pause.clone(),
    );
    let app = fx.app.clone();
    let card = boot.planner_card.id.clone();
    let cookie = fx.cookie.clone();
    let pending = tokio::spawn(async move {
        request(
            &app,
            "POST",
            &format!("/api/cards/{card}/planner/compact"),
            Some(&cookie),
            json!({}),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), pause.entered.notified())
        .await
        .unwrap();
    assert_eq!(
        fx.request("POST", "/api/auth/logout", Some(&fx.cookie), json!({}))
            .await
            .0,
        StatusCode::OK
    );
    pause.release.notify_one();
    assert_eq!(pending.await.unwrap().0, StatusCode::UNAUTHORIZED);
    assert!(boot.daemon.compacted_threads_for_test().is_empty());
    assert_eq!(
        boot.harness.snapshot().await.unwrap().phase,
        calm_server::harness::HarnessPhaseTag::Idle
    );
    boot.harness.shutdown().await.unwrap();
}
