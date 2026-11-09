//! An external native protocol peer, never a checkpoint/FSM or input-claim fixture.
use super::*;
use futures_util::{SinkExt, StreamExt};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::Message;

#[derive(Default)]
pub(super) struct Stats {
    pub turns: AtomicU64,
    pub threads: AtomicU64,
    interrupted: Mutex<Vec<(String, String)>>,
    fail_stop: AtomicBool,
    hold: Mutex<Option<(Arc<Notify>, Arc<Notify>)>>,
}

impl Stats {
    pub fn interrupted(&self) -> Vec<(String, String)> {
        self.interrupted.lock().unwrap().clone()
    }
    pub fn fail_stop(&self, fail: bool) {
        self.fail_stop.store(fail, Ordering::SeqCst);
    }
    pub fn hold_stop(&self) -> (Arc<Notify>, Arc<Notify>) {
        let hold = (Arc::new(Notify::new()), Arc::new(Notify::new()));
        *self.hold.lock().unwrap() = Some(hold.clone());
        hold
    }
}

pub(super) struct NativePeer {
    pub stats: Arc<Stats>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for NativePeer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl NativePeer {
    pub async fn connect(daemon: &Arc<SharedCodexAppServer>) -> Self {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let socket = dir.path().join("native.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let serving = async {
            let (stream, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(stream).await.unwrap()
        };
        let (client, peer) = tokio::join!(
            calm_server::codex_appserver::CodexAppServer::connect(&socket),
            serving
        );
        let (client, notifications) = client.unwrap();
        let stats = Arc::new(Stats::default());
        let task = tokio::spawn({
            let stats = stats.clone();
            async move { drive(peer, stats).await }
        });
        drop(
            daemon
                .install_protocol_peer_for_test(Arc::new(client), notifications)
                .await
                .unwrap(),
        );
        Self { stats, task }
    }
}

async fn drive(
    mut peer: tokio_tungstenite::WebSocketStream<tokio::net::UnixStream>,
    stats: Arc<Stats>,
) {
    while let Some(frame) = peer.next().await {
        let Ok(Message::Text(frame)) = frame else {
            continue;
        };
        let call: Value = serde_json::from_str(&frame).unwrap();
        if call.get("id").is_none() {
            continue;
        }
        let mut terminal = None;
        let mut started = None;
        let response = match call["method"].as_str().unwrap() {
            "initialize" | "config/mcpServer/reload" => json!({}),
            "config/read" => json!({"config":{}}),
            "account/read" => json!({"account":null}),
            "thread/start" => {
                let n = stats.threads.fetch_add(1, Ordering::SeqCst);
                json!({"thread":{"id":format!("close-native-thread-{n}")}})
            }
            "thread/read" | "thread/resume" => {
                json!({"thread":{"id":call["params"]["threadId"],"status":{"type":"idle"},"turns":[],"path":null}})
            }
            "thread/loaded/list" => json!({"data":[]}),
            "thread/unsubscribe" => json!({"status":"unsubscribed"}),
            "turn/start" => {
                let n = stats.turns.fetch_add(1, Ordering::SeqCst);
                let turn = format!("close-native-turn-{n}");
                started = Some(
                    json!({"method":"turn/started","params":{"threadId":call["params"]["threadId"],"turn":{"id":turn,"status":"inProgress"}}}),
                );
                json!({"turn":{"id":turn}})
            }
            "turn/interrupt" => {
                let thread = call["params"]["threadId"].as_str().unwrap().to_owned();
                let turn = call["params"]["turnId"].as_str().unwrap().to_owned();
                let failed = stats.fail_stop.load(Ordering::SeqCst);
                stats.interrupted.lock().unwrap().push((thread, turn));
                let hold = stats.hold.lock().unwrap().clone();
                if let Some((entered, release)) = hold {
                    entered.notify_one();
                    release.notified().await;
                    *stats.hold.lock().unwrap() = None;
                }
                if failed {
                    peer.send(Message::Text(json!({"id":call["id"],"error":{"code":-32000,"message":"Stop unconfirmed"}}).to_string())).await.unwrap();
                    continue;
                }
                terminal = Some(
                    json!({"method":"turn/completed","params":{"threadId":call["params"]["threadId"],"turn":{"id":call["params"]["turnId"],"status":"interrupted"}}}),
                );
                json!({})
            }
            method => panic!("unexpected close native peer method {method}: {call}"),
        };
        peer.send(Message::Text(
            json!({"id":call["id"],"result":response}).to_string(),
        ))
        .await
        .unwrap();
        for notification in [started, terminal].into_iter().flatten() {
            peer.send(Message::Text(notification.to_string()))
                .await
                .unwrap();
        }
    }
}
