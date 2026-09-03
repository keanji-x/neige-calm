use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, card_create_with_id_tx};
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::model::{Card, CardRole, NewCard, NewCove, NewWave, new_id};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::wave_cove_cache::WaveCoveCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    state: AppState,
    repo: Arc<SqlxRepo>,
    spec_card: Card,
}

async fn boot() -> Boot {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "reset-clears-items".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            template_input: None,
            cove_id: cove.id.clone(),
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
    let wave_cove_cache = WaveCoveCache::new();
    wave_cove_cache.insert(wave.id.clone(), cove.id);
    let mut tx = repo.pool().begin().await.unwrap();
    let spec_card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            wave_id: wave.id,
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "spec_harness": true}),
        },
        CardRole::Spec,
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
            calm_server::state::WriteContext::new(role_cache.clone(), wave_cove_cache.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(role_cache),
        Some(wave_cove_cache),
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
        spec_card,
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
async fn reset_spec_card_clears_persisted_harness_items() {
    let boot = boot().await;
    // #1252 S0 R1/F4 — backdate the spec card so `card_age_ms_at_clear` has a
    // value only `created_at` can produce. Without this the card is
    // milliseconds old and the age assertion passes just as well when the
    // production expression reads `updated_at` — which the reset transaction
    // itself rewrites, so in production the field would read ~0 on every
    // reset forever while the test stayed green.
    const BACKDATE_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
    let backdated_created_at = boot.spec_card.created_at - BACKDATE_MS;
    sqlx::query("UPDATE cards SET created_at = ?1 WHERE id = ?2")
        .bind(backdated_created_at)
        .bind(boot.spec_card.id.as_str())
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
                boot.spec_card.id.as_str(),
                boot.spec_card.wave_id.as_str(),
                "thread-before-reset",
                Some("turn-before-reset"),
                Some(&item_uuid),
                Some("agent_message"),
                "item/completed",
                &params,
            )
            .await
            .unwrap();
    }
    assert_eq!(
        boot.repo
            .harness_item_list_by_card(boot.spec_card.id.as_str(), 0, 100, false)
            .await
            .unwrap()
            .len(),
        3
    );

    let (status, body) = post_empty(
        boot.app.clone(),
        format!("/api/cards/{}/spec/reset", boot.spec_card.id),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["card_id"], json!(boot.spec_card.id.as_str()));
    assert!(
        boot.repo
            .harness_item_list_by_card(boot.spec_card.id.as_str(), 0, 100, false)
            .await
            .unwrap()
            .is_empty()
    );

    let active = boot
        .repo
        .session_projection_active_for_card(&boot.spec_card.id.to_string())
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
                    EventScope::Card { card, wave, .. },
                    Event::HarnessTranscriptCleared {
                        runtime_id,
                        card_id,
                        wave_id,
                        cleared_item_count,
                        cleared_params_bytes,
                        card_age_ms_at_clear: cleared_age,
                    },
                ) if runtime_id == &active.id
                    && card_id == &boot.spec_card.id
                    && wave_id == &boot.spec_card.wave_id
                    && card == &boot.spec_card.id
                    && wave == &boot.spec_card.wave_id
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
        boot.spec_card.id
    );
    if let Some(handle) = boot.state.harness.remove(&active.id) {
        handle.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn reset_plain_chat_card_on_cove_chat_wave_succeeds_without_spec_goal() {
    let boot = boot().await;
    sqlx::query("UPDATE waves SET purpose = 'cove-chat' WHERE id = ?1")
        .bind(boot.spec_card.wave_id.as_str())
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let chat = boot
        .repo
        .card_create(NewCard {
            wave_id: boot.spec_card.wave_id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "harness_profile": "plain_chat"}),
        })
        .await
        .unwrap();
    boot.state
        .card_role_cache
        .insert(chat.id.clone(), CardRole::Worker, chat.wave_id.clone());

    let (status, body) = post_empty(
        boot.app.clone(),
        format!("/api/cards/{}/spec/reset", chat.id),
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
        "plain chat must not inherit the wave title as a goal"
    );
    if let Some(handle) = boot.state.harness.remove(&active.id) {
        handle.shutdown().await.unwrap();
    }
}
