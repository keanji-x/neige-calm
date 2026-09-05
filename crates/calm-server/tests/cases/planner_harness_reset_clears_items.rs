use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, card_create_with_id_tx};
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::model::{Card, CardRole, NewArea, NewCard, NewTrack, new_id};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::track_area_cache::TrackAreaCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    state: AppState,
    repo: Arc<SqlxRepo>,
    planner_card: Card,
}

async fn boot() -> Boot {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "reset-clears-items".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "reset clears items".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let role_cache = CardRoleCache::new();
    let track_area_cache = TrackAreaCache::new();
    track_area_cache.insert(track.id.clone(), area.id);
    let mut tx = repo.pool().begin().await.unwrap();
    let planner_card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            track_id: track.id,
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "planner_harness": true}),
        },
        CardRole::Planner,
        false,
        &role_cache,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let events = EventBus::new();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join(format!("calm-plugins-data-reset-clears-items-{}", new_id())),
            Vec::new(),
            events,
            calm_server::state::WriteContext::new(role_cache.clone(), track_area_cache.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(role_cache),
        Some(track_area_cache),
    )
    .with_shared_codex_appserver(SharedCodexAppServer::new_fake_running_with_pending(
        repo.clone(),
        None,
    ));
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state.clone());

    Boot {
        app,
        state,
        repo,
        planner_card,
    }
}

async fn post_empty(app: axum::Router, uri: String) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn reset_planner_card_clears_persisted_harness_items() {
    let boot = boot().await;
    // #1252 S0 R1/F4 — backdate the planner card so `card_age_ms_at_clear` has a
    // value only `created_at` can produce. Without this the card is
    // milliseconds old and the age assertion passes just as well when the
    // production expression reads `updated_at` — which the reset transaction
    // itself rewrites, so in production the field would read ~0 on every
    // reset forever while the test stayed green.
    const BACKDATE_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
    let backdated_created_at = boot.planner_card.created_at - BACKDATE_MS;
    sqlx::query("UPDATE cards SET created_at = ?1 WHERE id = ?2")
        .bind(backdated_created_at)
        .bind(boot.planner_card.id.as_str())
        .execute(boot.repo.pool())
        .await
        .unwrap();
    // #1252 S0-2: the reset hard-deletes these rows, so the emitted event is
    // the only surviving record of what was destroyed. Keep the seeded
    // payloads so the expected byte total is computed from the same strings
    // the production measurement sees.
    let mut seeded_params: Vec<String> = Vec::new();
    for index in 1..=3 {
        let item_uuid = format!("item-before-reset-{index}");
        let params = json!({
            "item": {
                "id": item_uuid.clone(),
                "type": "agent_message",
                "text": format!("old item {index}")
            }
        })
        .to_string();
        seeded_params.push(params.clone());
        boot.repo
            .harness_item_insert(
                "runtime-before-reset",
                boot.planner_card.id.as_str(),
                boot.planner_card.track_id.as_str(),
                "thread-before-reset",
                Some("turn-before-reset"),
                Some(&item_uuid),
                Some("agent_message"),
                "item/completed",
                &params,
                None,
            )
            .await
            .unwrap();
    }
    assert_eq!(
        boot.repo
            .harness_item_list_by_card(boot.planner_card.id.as_str(), 0, 100, false)
            .await
            .unwrap()
            .len(),
        3
    );

    let (status, body) = post_empty(
        boot.app.clone(),
        format!("/api/cards/{}/planner/reset", boot.planner_card.id),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["card_id"], json!(boot.planner_card.id.as_str()));
    assert!(
        boot.repo
            .harness_item_list_by_card(boot.planner_card.id.as_str(), 0, 100, false)
            .await
            .unwrap()
            .is_empty()
    );

    let active = boot
        .repo
        .session_projection_active_for_card(&boot.planner_card.id.to_string())
        .await
        .unwrap()
        .expect("new active runtime");
    let events = boot.repo.events_since(0, i64::MAX).await.unwrap();
    let expected_item_count = seeded_params.len() as i64;
    let expected_params_bytes: i64 = seeded_params.iter().map(|p| p.len() as i64).sum();
    assert!(
        expected_params_bytes > 0,
        "fixture must seed non-empty params, else the size assertion is vacuous"
    );
    assert!(
        events.iter().any(|(_id, _version, scope, event)| {
            matches!(
                (scope, event),
                (
                    EventScope::Card { card, track, .. },
                    Event::HarnessTranscriptCleared {
                        worker_session_id: runtime_id,
                        card_id,
                        track_id,
                        cleared_item_count,
                        cleared_params_bytes,
                        card_age_ms_at_clear: cleared_age,
                    },
                ) if runtime_id == &active.id
                    && card_id == &boot.planner_card.id
                    && track_id == &boot.planner_card.track_id
                    && card == &boot.planner_card.id
                    && track == &boot.planner_card.track_id
                    && *cleared_item_count == Some(expected_item_count)
                    && *cleared_params_bytes == Some(expected_params_bytes)
                    // The card was backdated a week; the age must reflect
                    // that, not the age of a row the reset tx just touched.
                    && cleared_age.is_some_and(|age| {
                        (BACKDATE_MS..BACKDATE_MS + 600_000).contains(&age)
                    })
            )
        }),
        "reset must emit durable harness.transcript.cleared carrying \
         item_count={expected_item_count} params_bytes={expected_params_bytes} \
         age_ms in [{BACKDATE_MS}, {}) for {}: {events:?}",
        BACKDATE_MS + 600_000,
        boot.planner_card.id
    );
    if let Some(handle) = boot.state.harness.remove(&active.id) {
        handle.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn reset_plain_chat_card_on_area_chat_track_succeeds_without_planner_goal() {
    let boot = boot().await;
    sqlx::query("UPDATE tracks SET purpose = 'area-chat' WHERE id = ?1")
        .bind(boot.planner_card.track_id.as_str())
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let chat = boot
        .repo
        .card_create(NewCard {
            track_id: boot.planner_card.track_id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "harness_profile": "plain_chat"}),
        })
        .await
        .unwrap();
    boot.state
        .card_role_cache
        .insert(chat.id.clone(), CardRole::Worker, chat.track_id.clone());

    let (status, body) = post_empty(
        boot.app.clone(),
        format!("/api/cards/{}/planner/reset", chat.id),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let active = boot
        .repo
        .session_projection_active_for_card(&chat.id.to_string())
        .await
        .unwrap()
        .expect("plain-chat reset runtime");
    assert_eq!(
        active.kind,
        calm_server::session_projection_repo::WorkerSessionKind::CodexCard
    );
    let snapshot: calm_server::harness::HarnessSnapshot =
        serde_json::from_value(active.handle_state_json.unwrap()).unwrap();
    assert!(
        snapshot.pending_queue.is_empty(),
        "plain chat must not inherit the track title as a goal"
    );
    if let Some(handle) = boot.state.harness.remove(&active.id) {
        handle.shutdown().await.unwrap();
    }
}
