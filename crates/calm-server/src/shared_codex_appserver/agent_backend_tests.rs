use super::native_owner_tests::{connect_with_reader, notify_thread, request, send};
use super::*;
use crate::agent_backend_pool::{codex::CodexBackends, identity::BackendIsolation};
use clap::Parser;
use serde_json::json;

#[tokio::test]
async fn agent_backend_nondefault_forwarder_observes_default_deletion_seal() {
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
    let repo: Arc<dyn Repo> = Arc::new(
        crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap(),
    );
    let isolation = BackendIsolation::prepare(&cfg, &repo).await.unwrap();
    let pending = Arc::new(PendingThreadStartRegistry::new(
        repo.clone(),
        crate::event::EventBus::new(),
    ));
    let pool = CodexBackends::build(&cfg, repo, pending, isolation);
    let other = &pool.instances()[1];
    let (_client, mut peer, _forwarder) = connect_with_reader(other).await;
    pool.default_only()
        .thread_seals()
        .seal_for_deletion("late-thread");
    notify_thread(
        other,
        &mut peer,
        "late-thread",
        "turn/started",
        json!({"id":"late-turn"}),
    )
    .await;
    let frame = request(&mut peer).await;
    assert_eq!(frame["method"], "turn/interrupt");
    assert_eq!(frame["params"]["threadId"], "late-thread");
    assert_eq!(frame["params"]["turnId"], "late-turn");
    send(&mut peer, json!({"id":frame["id"], "result":{}})).await;
    assert!(
        pool.default_only()
            .active_turn_id_for_thread("late-thread")
            .is_none()
    );
}
