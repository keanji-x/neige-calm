//! Scripted app-server connections; production transport, replay and native owner.
use super::*;
use futures_util::{SinkExt, StreamExt};
use provider::codex::RequestCrossing;
use serde_json::{Value, json};
use std::sync::Mutex as StdMutex;
use std::sync::atomic::AtomicUsize;
use tokio::net::UnixListener;

type Seen = Arc<StdMutex<Vec<(usize, String, String)>>>;
struct Fixture {
    daemon: Arc<SharedCodexAppServer>,
    seen: Seen,
    closed: Arc<StdMutex<HashSet<String>>>,
    listener: JoinHandle<()>,
    _root: tempfile::TempDir,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.listener.abort();
    }
}
async fn fixture() -> Fixture {
    use clap::Parser;
    let root = tempfile::tempdir().unwrap();
    let repo = Arc::new(
        crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap(),
    );
    let cfg = Config::parse_from(["calm-server", "--data-dir", root.path().to_str().unwrap()]);
    let home = Arc::new(SharedCodexHome::new(
        root.path().join("codex-home"),
        root.path().join("codex-homes"),
    ));
    let daemon = SharedCodexAppServer::new(&cfg, home, repo);
    std::fs::create_dir_all(daemon.sock.parent().unwrap()).unwrap();
    let socket = UnixListener::bind(&daemon.sock).unwrap();
    let seen = Seen::default();
    let closed = Arc::new(StdMutex::new(HashSet::new()));
    let count = Arc::new(AtomicUsize::new(0));
    let listener = tokio::spawn({
        let seen = seen.clone();
        let closed = closed.clone();
        async move {
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                let (stream, _) = socket.accept().await.unwrap();
                let connection = count.fetch_add(1, Ordering::SeqCst);
                let seen = seen.clone();
                let closed = closed.clone();
                tasks.spawn(async move {
                    let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                    while let Some(Ok(message)) = ws.next().await {
                        let Ok(raw) = message.to_text() else { continue; };
                        let Ok(request) = serde_json::from_str::<Value>(raw) else { continue; };
                        let method = request["method"].as_str().unwrap_or_default().to_owned();
                        let thread = request["params"]["threadId"].as_str().unwrap_or_default().to_owned();
                        seen.lock().unwrap().push((connection, method.clone(), thread.clone()));
                        if closed.lock().unwrap().contains(&method) || (method == "thread/resume" && closed.lock().unwrap().contains(&thread)) {
                            let _ = ws.close(None).await; break;
                        }
                        let Some(id) = request.get("id") else { continue; };
                        let result = match method.as_str() {
                            "initialize" => json!({"userAgent":"fixture"}),
                            "model/list" => json!({"data":[],"nextCursor":null}),
                            "thread/resume" | "thread/start" => json!({"thread":{"id":if thread.is_empty() {"worker"} else {&thread}}}),
                            "thread/unsubscribe" => json!({"status":"unsubscribed"}),
                            "turn/start" => json!({"turn":{"id":"fresh-turn"}}),
                            _ => json!({}),
                        };
                        if ws.send(tokio_tungstenite::tungstenite::Message::Text(json!({"id":id,"result":result}).to_string())).await.is_err() {break;}
                    }
                });
            }
        }
    });
    let (client, notifications) = connect_initialized(&daemon.sock, &daemon.approval_routes)
        .await
        .unwrap();
    let client = Arc::new(client);
    drop(
        daemon
            .install_client(client.clone(), notifications)
            .await
            .unwrap(),
    );
    native_owner_tests::publish(&daemon, client).await;
    Fixture {
        daemon,
        seen,
        closed,
        listener,
        _root: root,
    }
}

#[tokio::test]
async fn next_request_reconnects_without_resending_unknown_turn() {
    let f = fixture().await;
    f.closed.lock().unwrap().insert("turn/start".into());
    let original = f.daemon.connected_client().await.unwrap();
    let failure = original
        .turn_start_attempt_gated(
            "thread",
            vec![InputItem::text("one input")],
            &TurnModelSelection::inherit(),
            TurnApprovals::Explicit(crate::planner_permission_mode::PlannerPermissionMode::Never),
            None,
            &provider::send_boundary::PreSendGate::new(|| Box::pin(async { Ok(()) })),
        )
        .await
        .unwrap_err();
    assert_eq!(failure.crossing(), RequestCrossing::AfterSend);
    native_owner_tests::reader_finished(&original).await;
    f.closed.lock().unwrap().clear();
    // Concurrent callers must share one newly initialized connection.
    let (a, b) = tokio::join!(f.daemon.connected_client(), f.daemon.connected_client());
    let (a, b) = (a.unwrap(), b.unwrap());
    assert!(
        !Arc::ptr_eq(&a, &original),
        "a dead client must be replaced"
    );
    assert!(Arc::ptr_eq(&a, &b));
    f.daemon
        .model_list(tokio::time::Instant::now() + Duration::from_secs(2))
        .await
        .unwrap();
    let seen = f.seen.lock().unwrap();
    assert_eq!(
        seen.iter().filter(|(_, m, _)| m == "turn/start").count(),
        1,
        "unknown input must never be resent"
    );
    assert_eq!(seen.iter().filter(|(_, m, _)| m == "initialize").count(), 2);
    assert_eq!(
        f.daemon.status_snapshot().restart_count,
        0,
        "reconnect must preserve the process"
    );
}

#[tokio::test]
async fn failed_startup_resume_does_not_block_other_threads_or_new_work() {
    let f = fixture().await;
    for thread in ["before", "broken", "after"] {
        f.daemon
            .thread_cache
            .insert(thread.into(), format!("card-{thread}"));
    }
    f.closed.lock().unwrap().insert("broken".into());
    f.daemon
        .resume_cached_threads(ResumeMode::HotTakeover)
        .await;
    f.daemon
        .model_list(tokio::time::Instant::now() + Duration::from_secs(2))
        .await
        .expect("catalog remains usable after one broken startup history");
    let client = f.daemon.connected_client().await.unwrap();
    client
        .thread_start(None)
        .await
        .expect("worker mint remains usable");
    // The deletion cleanup uses the recovered connection and drops only its mapping.
    f.daemon
        .forget_threads_for_deleted_cards(&HashSet::from(["card-broken".into()]))
        .await;
    assert!(!f.daemon.thread_cache.contains_key("broken"));
    let seen = f.seen.lock().unwrap();
    let final_connection = seen.last().unwrap().0;
    for thread in ["before", "after"] {
        assert!(
            seen.iter()
                .any(|(c, m, t)| *c == final_connection && m == "thread/resume" && t == thread),
            "all healthy subscriptions must be restored on the final connection: {seen:?}"
        );
    }
    assert_eq!(
        seen.iter()
            .filter(|(_, m, t)| m == "thread/resume" && t == "broken")
            .count(),
        1,
        "automatic recovery must not loop on a broken history"
    );
}

#[tokio::test]
async fn cancelled_subscription_recovery_is_finished_by_the_next_caller() {
    let f = fixture().await;
    let original = f.daemon.connected_client().await.unwrap();
    f.closed.lock().unwrap().insert("model/list".into());
    assert!(
        original
            .model_list(None, tokio::time::Instant::now() + Duration::from_secs(2))
            .await
            .is_err()
    );
    native_owner_tests::reader_finished(&original).await;
    f.closed.lock().unwrap().clear();
    f.daemon
        .thread_cache
        .insert("restore".into(), "card-restore".into());
    let fence = f.daemon.resume_replay_serial.lock().await;
    let daemon = f.daemon.clone();
    let recovering = tokio::spawn(async move { daemon.connected_client().await });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let current = f.daemon.running_client().await;
            if current
                .as_ref()
                .is_some_and(|client| !Arc::ptr_eq(client, &original))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    recovering.abort();
    assert!(matches!(recovering.await, Err(error) if error.is_cancelled()));
    let daemon = f.daemon.clone();
    let resumed = tokio::spawn(async move { daemon.connected_client().await });
    drop(fence);
    let recovered = tokio::time::timeout(Duration::from_secs(5), resumed)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!Arc::ptr_eq(&recovered, &original));
    assert!(
        f.seen
            .lock()
            .unwrap()
            .iter()
            .any(|(_, method, thread)| method == "thread/resume" && thread == "restore"),
        "a cancelled reconnect must not strand subscriptions"
    );
}
