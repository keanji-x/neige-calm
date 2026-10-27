use super::*;
use crate::agent_backend_pool::{codex::CodexBackends, identity::BackendIsolation};
use crate::card_role_cache::CardRoleCache;
use crate::config::Config;
use crate::db::{prelude::*, sqlite::SqlxRepo};
use crate::shared_codex_appserver::native_owner_tests::{connect_with_reader, send};
use clap::Parser;
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn agent_backend_additional_notifications_cannot_write_default_liveness() {
    let root = tempfile::tempdir().unwrap();
    let mut cfg = Config::parse_from(["calm-server"]);
    cfg.data_dir = Some(root.path().to_owned());
    let config = root.path().join("backends.json");
    std::fs::write(&config, json!({
        "cooldown_after_concurrency_limit_secs":60,"cooldown_default_secs":300,"cooldown_max_secs":3600,
        "backends": (["default", "other"].map(|id| json!({
            "provider":"codex", "id":id, "weight":1, "accepts_new_sessions":true,
            "credentials":{"kind":"codex_home", "path":root.path().join(if id == "default" { "codex-home" } else { id })}
        })))
    }).to_string()).unwrap();
    cfg.agent_backend_config = Some(config);
    let sql = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let repo: Arc<dyn Repo> = sql.clone();
    let area = repo
        .area_create(
            calm_types::organization::LEGACY_OWNER_USER_ID,
            crate::model::NewArea {
                name: "a".into(),
                color: "#abc".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let track = repo
        .track_create(crate::model::NewTrack {
            area_id: area.id,
            title: "t".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    sqlx::query("INSERT INTO worker_sessions
        (id, track_id, provider, mode, contract, state, thread_id, created_at_ms, updated_at_ms,
         last_activity_ms, last_thread_status, last_turn_completed_ms)
        VALUES ('worker', ?, 'codex', 'resumable', 'executor', 'running', 't', 0, 0, 11, 'notLoaded', 7)")
        .bind(track.id.as_str()).execute(sql.pool()).await.unwrap();
    let isolation = BackendIsolation::prepare(&cfg, &repo).await.unwrap();
    let events = EventBus::new();
    let pending = Arc::new(
        crate::pending_codex_threads::PendingThreadStartRegistry::new(repo.clone(), events.clone()),
    );
    let pool = CodexBackends::build(&cfg, repo.clone(), pending, isolation);
    let default = pool.default_only().clone();
    let write = WriteContext::new(
        CardRoleCache::new(),
        crate::track_area_cache::TrackAreaCache::new(),
    );
    let dispatcher = Dispatcher::spawn(
        repo.clone(),
        events,
        write,
        Arc::new(CodexClient::new_stub()),
        Arc::new(DaemonClient {
            data_dir: root.path().join("daemon"),
            proc_supervisor_sock: None,
        }),
        None,
        default.clone(),
        root.path().to_owned(),
        1,
    );
    dispatcher.subscribe_additional_codex_backends(&pool);
    let other = &pool.instances()[1];
    let (client, mut peer, forwarder) = connect_with_reader(other).await;
    let mut published = other.subscribe_notifications();
    send(&mut peer, json!({"method":"thread/status/changed", "params":{"threadId":"t", "status":{"type":"active","activeFlags":[]}}})).await;
    send(&mut peer, json!({"method":"turn/completed", "params":{"threadId":"t", "turn":{"id":"foreign-turn","status":"completed"}}})).await;
    // Keep the source current until both native notifications have been published.
    // Closing first can retire the source before its forwarder consumes the frames.
    let barrier = format!("fixture/barrier/{}", uuid::Uuid::new_v4());
    send(&mut peer, json!({"method":barrier,"params":{}})).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        let (mut status, mut completed) = (false, false);
        loop {
            match published.recv().await.unwrap() {
                crate::codex_appserver::Notification::ThreadStatusChanged { .. } => status = true,
                crate::codex_appserver::Notification::TurnCompleted { .. } => completed = true,
                crate::codex_appserver::Notification::Other { method, .. } if method == barrier => {
                    assert!(
                        status && completed,
                        "both collision notifications must reach the forwarder output"
                    );
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    drop(published);
    // Close and drain the real forwarder and any subscribed feeders. This proves
    // absence of writes without a timed negative assertion.
    peer.close(None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), forwarder)
        .await
        .unwrap()
        .unwrap();
    drop(client);
    drop(peer);
    drop(pool);
    let handles = std::mem::take(&mut *dispatcher.additional_liveness_feeders.lock().unwrap());
    for handle in handles {
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .unwrap()
            .unwrap();
    }
    let snapshot = || async {
        sqlx::query_as::<_, (Option<i64>, Option<String>, Option<i64>)>(
            "SELECT last_activity_ms, last_thread_status, last_turn_completed_ms FROM worker_sessions WHERE id = 'worker'")
            .fetch_one(sql.pool()).await.unwrap()
    };
    assert_eq!(
        snapshot().await,
        (Some(11), Some("notLoaded".into()), Some(7))
    );

    let (_client, mut peer, _forwarder) = connect_with_reader(&default).await;
    send(&mut peer, json!({"method":"thread/status/changed", "params":{"threadId":"t", "status":{"type":"active","activeFlags":[]}}})).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (activity, status, completed) = snapshot().await;
            if status.as_deref() == Some("active") {
                assert!(activity.unwrap() > 11);
                assert_eq!(completed, Some(7));
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    send(&mut peer, json!({"method":"turn/completed", "params":{"threadId":"t", "turn":{"id":"own-turn","status":"completed"}}})).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (activity, status, completed) = snapshot().await;
            if completed.unwrap() > 7 {
                assert_eq!(status.as_deref(), Some("idle"));
                assert_eq!(activity, completed);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
