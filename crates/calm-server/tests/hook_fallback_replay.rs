use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use calm_server::actor::{actor_middleware, require_loopback_connect_info};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{Event, EventBus};
use calm_server::model::{NewCard, NewCove, NewWave};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};

async fn collect_replay_order(
    axum::extract::State(replayed): axum::extract::State<Arc<tokio::sync::Mutex<Vec<String>>>>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> axum::http::StatusCode {
    let sequence = body
        .get("sequence")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("<missing>")
        .to_owned();
    replayed.lock().await.push(sequence);
    axum::http::StatusCode::NO_CONTENT
}

#[tokio::test]
async fn hook_fallback_replay_posts_files_in_timestamp_order() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let fallback = tempfile::Builder::new()
        .prefix("calm-hook-fallback-order-")
        .tempdir_in("/tmp")
        .expect("tempdir in /tmp");
    let codex_dir = fallback.path().join("codex");
    std::fs::create_dir_all(&codex_dir).expect("fallback codex dir");

    let files = [
        ("0000000000001003-replay-third-3333333333333333.json", "t3"),
        ("0000000000001001-replay-first-1111111111111111.json", "t1"),
        ("0000000000001002-replay-second-2222222222222222.json", "t2"),
    ];
    for &(name, sequence) in &files {
        std::fs::write(
            codex_dir.join(name),
            serde_json::to_vec(&serde_json::json!({
                "card_id": "replay-order-card",
                "body": {
                    "sequence": sequence,
                }
            }))
            .unwrap(),
        )
        .expect("write fallback");
    }

    let replayed = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let app = axum::Router::new()
        .route(
            "/internal/codex/hook",
            axum::routing::post(collect_replay_order),
        )
        .with_state(replayed.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind replay collector");
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve replay collector");
    });

    let base_url = format!("http://{addr}");
    tokio::time::timeout(
        Duration::from_secs(5),
        calm_server::replay_hook_fallback_dir_once(fallback.path(), &base_url),
    )
    .await
    .expect("fallback replay timeout");

    let replayed = replayed.lock().await.clone();
    server.abort();
    assert_eq!(replayed, ["t1", "t2", "t3"]);
    for &(name, _) in &files {
        assert!(!codex_dir.join(name).exists(), "{name} should be deleted");
    }
}

#[tokio::test]
async fn fallback_replay_posts_file_and_deletes_on_success() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "c".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "w".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: serde_json::json!({}),
        })
        .await
        .unwrap();
    let cache = calm_server::card_role_cache::CardRoleCache::new();
    repo.seed_card_role_cache(&cache).await.unwrap();

    let fallback = tempfile::tempdir().expect("tempdir");
    let codex_dir = fallback.path().join("codex");
    std::fs::create_dir_all(&codex_dir).expect("fallback codex dir");
    let fallback_file = codex_dir.join("replay-key.json");
    std::fs::write(
        &fallback_file,
        serde_json::to_vec(&serde_json::json!({
            "card_id": card.id.as_str(),
            "body": {
                "hook_event_name": "Stop",
                "session_id": "replay-session",
                "transcript_path": "/tmp/replay.jsonl",
                "transcript_size_bytes": 99,
            }
        }))
        .unwrap(),
    )
    .expect("write fallback");

    let events = EventBus::new();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            events.clone(),
            calm_server::state::WriteContext::new(
                cache.clone(),
                calm_server::wave_cove_cache::WaveCoveCache::new(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(cache),
        Some(calm_server::wave_cove_cache::WaveCoveCache::new()),
    );
    let app = axum::Router::new()
        .merge(routes::internal_router())
        .layer(axum::middleware::from_fn(actor_middleware))
        .layer(axum::middleware::from_fn(require_loopback_connect_info))
        .with_state(state);
    let mut rx = events.subscribe();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind replay server");
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .expect("serve replay test");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    calm_server::replay_hook_fallback_dir_once(fallback.path(), &format!("http://{addr}")).await;

    let env = match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
        Ok(Ok(env)) => env,
        Ok(Err(e)) => panic!("event channel closed: {e}"),
        Err(e) => panic!(
            "event timeout: {e}; fallback_exists={}, failed_exists={}",
            fallback_file.exists(),
            fallback_file
                .with_file_name("replay-key.json.failed")
                .exists()
        ),
    };
    match env.event {
        Event::CodexHook {
            card_id,
            kind,
            hook_idempotency_key,
            payload,
        } => {
            assert_eq!(card_id.as_str(), card.id.as_str());
            assert_eq!(kind, "hook.codex.stop");
            assert!(!hook_idempotency_key.is_empty());
            assert_eq!(payload["session_id"], "replay-session");
        }
        other => panic!("expected CodexHook, got {other:?}"),
    }
    assert!(!fallback_file.exists(), "fallback file should be deleted");
    server.abort();
}
