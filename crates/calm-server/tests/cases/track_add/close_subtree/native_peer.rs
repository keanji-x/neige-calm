//! An external native protocol peer, never a checkpoint/FSM or input-claim fixture.
use super::*;
use futures_util::{SinkExt, StreamExt};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::Message;

struct StopHold {
    threads: Vec<String>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[derive(Default)]
pub(super) struct Stats {
    pub turns: AtomicU64,
    pub starts: Mutex<Vec<(String, Value)>>,
    pub threads: AtomicU64,
    interrupted: Mutex<Vec<(String, String)>>,
    fail_stop: AtomicBool,
    hold: Mutex<Option<StopHold>>,
    hold_end: Mutex<Option<(Arc<Notify>, Arc<Notify>)>>,
}

impl Stats {
    pub fn interrupted(&self) -> Vec<(String, String)> {
        self.interrupted.lock().unwrap().clone()
    }
    pub fn fail_stop(&self, fail: bool) {
        self.fail_stop.store(fail, Ordering::SeqCst);
    }
    pub fn hold_stop(&self) -> (Arc<Notify>, Arc<Notify>) {
        self.hold_stops_for(&[])
    }
    pub fn hold_stops_for(&self, threads: &[&str]) -> (Arc<Notify>, Arc<Notify>) {
        let hold = (Arc::new(Notify::new()), Arc::new(Notify::new()));
        *self.hold.lock().unwrap() = Some(StopHold {
            threads: threads.iter().map(|s| (*s).to_owned()).collect(),
            entered: hold.0.clone(),
            release: hold.1.clone(),
        });
        hold
    }
    pub fn hold_end(&self) -> (Arc<Notify>, Arc<Notify>) {
        let hold = (Arc::new(Notify::new()), Arc::new(Notify::new()));
        *self.hold_end.lock().unwrap() = Some(hold.clone());
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
    let mut pending_end: Option<(Value, Arc<Notify>)> = None;
    let mut pending_stop: Option<(Value, Option<Value>, Arc<Notify>)> = None;
    loop {
        // A delayed response belongs to one RPC. Keep receiving unrelated Tracks' RPCs.
        let stop_release = pending_stop.as_ref().map(|(_, _, release)| release.clone());
        let end_release = pending_end.as_ref().map(|(_, release)| release.clone());
        let frame = tokio::select! {
            _ = async {
                match stop_release { Some(release) => release.notified().await, None => std::future::pending().await }
            } => {
                let (response, terminal, _) = pending_stop.take().unwrap();
                peer.send(Message::Text(response.to_string())).await.unwrap();
                if let Some(terminal) = terminal {
                    let hold = stats.hold_end.lock().unwrap().take();
                    if let Some((entered, release)) = hold {
                        assert!(pending_end.replace((terminal, release)).is_none());
                        entered.notify_one();
                    } else {
                        peer.send(Message::Text(terminal.to_string())).await.unwrap();
                    }
                }
                continue;
            }
            _ = async {
                match end_release { Some(release) => release.notified().await, None => std::future::pending().await }
            } => {
                let (end, _) = pending_end.take().unwrap();
                peer.send(Message::Text(end.to_string())).await.unwrap();
                continue;
            }
            frame = peer.next() => frame,
        };
        let Some(frame) = frame else { break };
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
                stats.starts.lock().unwrap().push((
                    call["params"]["threadId"].as_str().unwrap().to_owned(),
                    call["params"]["input"].clone(),
                ));
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
                stats
                    .interrupted
                    .lock()
                    .unwrap()
                    .push((thread.clone(), turn));
                let hold = {
                    let mut hold = stats.hold.lock().unwrap();
                    if hold.as_ref().is_some_and(|hold| {
                        hold.threads.is_empty() || hold.threads.contains(&thread)
                    }) {
                        hold.take()
                    } else {
                        None
                    }
                };
                if let Some(StopHold {
                    entered, release, ..
                }) = hold
                {
                    let response = if failed {
                        json!({"id":call["id"],"error":{"code":-32000,"message":"Stop unconfirmed"}})
                    } else {
                        json!({"id":call["id"],"result":{}})
                    };
                    let end = (!failed).then(|| {
                        json!({"method":"turn/completed","params":{
                        "threadId":call["params"]["threadId"],
                        "turn":{"id":call["params"]["turnId"],"status":"interrupted"}}})
                    });
                    assert!(pending_stop.replace((response, end, release)).is_none());
                    entered.notify_one();
                    continue;
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
        if let Some(notification) = started {
            peer.send(Message::Text(notification.to_string()))
                .await
                .unwrap();
        }
        if let Some(terminal) = terminal {
            let hold = stats.hold_end.lock().unwrap().take();
            if let Some((entered, release)) = hold {
                assert!(pending_end.replace((terminal, release)).is_none());
                entered.notify_one();
            } else {
                peer.send(Message::Text(terminal.to_string()))
                    .await
                    .unwrap();
            }
        }
    }
}
