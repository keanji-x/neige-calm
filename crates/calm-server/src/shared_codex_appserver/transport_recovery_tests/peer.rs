//! Reconnectable protocol endpoint shared by owner and public Close regressions.
use super::*;
use std::collections::HashMap;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::AtomicUsize;
use tokio::net::UnixListener;

type Seen = Arc<StdMutex<Vec<(usize, String, String)>>>;
struct ReadHold {
    thread: String,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
type HeldRead = Arc<StdMutex<Option<ReadHold>>>;
pub(crate) struct Fixture {
    pub(super) repo: Arc<crate::db::sqlite::SqlxRepo>,
    pub(super) daemon: Arc<SharedCodexAppServer>,
    pub(super) seen: Seen,
    pub(crate) closed: Arc<StdMutex<HashSet<String>>>,
    listener: JoinHandle<()>,
    pub(crate) requests: Arc<StdMutex<Vec<Value>>>,
    pub(super) pause_resume: Arc<AtomicBool>,
    pub(crate) answers: Arc<StdMutex<HashMap<String, Value>>>,
    held_read: HeldRead,
    notices: tokio::sync::broadcast::Sender<Value>,
    pub(super) resume_release: Arc<tokio::sync::Notify>,
    pub(super) _root: tempfile::TempDir,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.listener.abort();
    }
}
impl Fixture {
    pub(crate) fn notify(&self, notice: Value) {
        self.notices.send(notice).unwrap();
    }

    pub(crate) fn hold_read(
        &self,
        thread: &str,
    ) -> (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        *self.held_read.lock().unwrap() = Some(ReadHold {
            thread: thread.into(),
            entered: entered.clone(),
            release: release.clone(),
        });
        (entered, release)
    }

    pub(crate) async fn reconnect(&self) {
        self.daemon.connected_client().await.unwrap();
    }

    pub(crate) async fn connect_new_owner(&self, daemon: &Arc<SharedCodexAppServer>) {
        let (client, notifications) = CodexAppServer::connect(&self.daemon.paths.socket)
            .await
            .unwrap();
        client
            .initialize(ClientInfo {
                name: "replacement-test-owner".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            })
            .await
            .unwrap();
        drop(
            daemon
                .install_protocol_peer_for_test(Arc::new(client), notifications)
                .await
                .unwrap(),
        );
    }

    pub(crate) async fn keep_peer_process_alive(&self) {
        // This test process owns the protocol listener even after one client
        // disconnects. Record its actual incarnation, not the ordinary fixture's
        // zero PID/placeholder boot identity used by absence controls.
        let pid = i32::try_from(std::process::id()).unwrap();
        let mut core = self.daemon.core.lock().await;
        let SupervisorState::Running { runtime, .. } = &mut core.state else {
            panic!("the peer must be installed");
        };
        runtime.pid = pid;
        runtime.process_start_time = crate::proc_identity::read_proc_start_time(pid).unwrap();
        runtime.boot_id = crate::proc_identity::read_boot_id().unwrap();
    }

    pub(crate) async fn disconnect(&self) {
        let client = self.daemon.connected_client().await.unwrap();
        self.closed.lock().unwrap().insert("model/list".into());
        assert!(
            self.daemon
                .model_list(tokio::time::Instant::now() + Duration::from_secs(2))
                .await
                .is_err()
        );
        native_owner_tests::reader_finished(&client).await;
        self.closed.lock().unwrap().clear();
    }
}
pub(super) async fn fixture() -> Fixture {
    use clap::Parser;
    let root = tempfile::tempdir().unwrap();
    let repo = Arc::new(
        crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap(),
    );
    let cfg = Config::parse_from(["calm-server", "--data-dir", root.path().to_str().unwrap()]);
    let home = Arc::new(SharedCodexHome::new(root.path().join("codex-home")));
    let daemon = SharedCodexAppServer::new(&cfg, home, repo.clone());
    peer_for(daemon, repo, root).await
}

pub(crate) async fn peer_for(
    daemon: Arc<SharedCodexAppServer>,
    repo: Arc<crate::db::sqlite::SqlxRepo>,
    root: tempfile::TempDir,
) -> Fixture {
    std::fs::create_dir_all(daemon.paths.socket.parent().unwrap()).unwrap();
    let socket = UnixListener::bind(&daemon.paths.socket).unwrap();
    let seen = Seen::default();
    let closed = Arc::new(StdMutex::new(HashSet::new()));
    let count = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(StdMutex::new(Vec::new()));
    let answers = Arc::new(StdMutex::new(HashMap::<String, Value>::new()));
    let pause_resume = Arc::new(AtomicBool::new(false));
    let resume_release = Arc::new(tokio::sync::Notify::new());
    let held_read = HeldRead::default();
    let (notices, _) = tokio::sync::broadcast::channel::<Value>(16);
    let listener = tokio::spawn({
        let answers = answers.clone();
        let seen = seen.clone();
        let closed = closed.clone();
        let requests = requests.clone();
        let pause_resume = pause_resume.clone();
        let resume_release = resume_release.clone();
        let held_read = held_read.clone();
        let notices = notices.clone();
        async move {
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                let (stream, _) = socket.accept().await.unwrap();
                let connection = count.fetch_add(1, Ordering::SeqCst);
                let answers = answers.clone();
                let seen = seen.clone();
                let closed = closed.clone();
                let requests = requests.clone();
                let pause_resume = pause_resume.clone();
                let resume_release = resume_release.clone();
                let held_read = held_read.clone();
                let mut notices = notices.subscribe();
                tasks.spawn(async move {
                    let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                    let (sink, mut stream) = ws.split();
                    let sink = Arc::new(tokio::sync::Mutex::new(sink));
                    let mut replies = tokio::task::JoinSet::new();
                    loop {
                        let message = tokio::select! {
                        message = stream.next() => message,
                        notice = notices.recv() => {
                            if let Ok(notice) = notice
                                && sink.lock().await.send(tokio_tungstenite::tungstenite::Message::Text(notice.to_string())).await.is_err() { break; }
                            continue;
                        }
                        };
                        let Some(Ok(message)) = message else { break; };
                        let Ok(raw) = message.to_text() else { continue; };
                        let Ok(request) = serde_json::from_str::<Value>(raw) else { continue; };
                        let method = request["method"].as_str().unwrap_or_default().to_owned();
                        let thread = request["params"]["threadId"].as_str().unwrap_or_default().to_owned();
                        seen.lock().unwrap().push((connection, method.clone(), thread.clone()));
                        requests.lock().unwrap().push(request.clone());
                        if method == "thread/resume" && pause_resume.load(Ordering::SeqCst) {
                            resume_release.notified().await;
                        }
                        if closed.lock().unwrap().contains(&method) || (method == "thread/resume" && closed.lock().unwrap().contains(&thread)) {
                            let _ = sink.lock().await.close().await; break;
                        }
                        let Some(id) = request.get("id") else { continue; };
                        let answer = answers.lock().unwrap().get(&method).cloned();
                        if let Some(mut answer) = answer {
                            answer["id"] = id.clone();
                            let hold = if method == "thread/read" {
                                let mut held = held_read.lock().unwrap();
                                if held.as_ref().is_some_and(|hold| hold.thread == thread) { held.take() } else { None }
                            } else { None };
                            if let Some(ReadHold { entered, release, .. }) = hold {
                                let sink = sink.clone();
                                replies.spawn(async move {
                                    entered.notify_one();
                                    release.notified().await;
                                    let _ = sink.lock().await.send(tokio_tungstenite::tungstenite::Message::Text(answer.to_string())).await;
                                });
                            } else if sink.lock().await.send(tokio_tungstenite::tungstenite::Message::Text(answer.to_string())).await.is_err() {break;}
                            continue;
                        }
                        let result = match method.as_str() {
                            "initialize" => json!({"userAgent":"fixture"}),
                            "model/list" => json!({"data":[],"nextCursor":null}),
                            "thread/loaded/list" => json!({"data":[]}),
                            "config/read" => json!({"config":{}}),
                            "thread/read" => json!({"thread":{"id":thread,"status":{"type":"idle"},"turns":[]}}),
                            "thread/resume" | "thread/start" => json!({"thread":{"id":if thread.is_empty() {"worker"} else {&thread}}}),
                            "thread/unsubscribe" => json!({"status":"unsubscribed"}),
                            "turn/start" => json!({"turn":{"id":"fresh-turn"}}),
                            _ => json!({}),
                        };
                        if sink.lock().await.send(tokio_tungstenite::tungstenite::Message::Text(json!({"id":id,"result":result}).to_string())).await.is_err() {break;}
                    }
                });
            }
        }
    });
    connect_original(&daemon).await;
    Fixture {
        repo,
        daemon,
        seen,
        closed,
        listener,
        requests,
        pause_resume,
        answers,
        held_read,
        notices,
        resume_release,
        _root: root,
    }
}

pub(crate) async fn connect_original(daemon: &Arc<SharedCodexAppServer>) {
    let (client, notifications) = connect_initialized(
        &daemon.verify_backend_isolation().await.unwrap(),
        &daemon.approval_routes,
    )
    .await
    .unwrap();
    let client = Arc::new(client);
    drop(
        daemon
            .install_client(client.clone(), notifications)
            .await
            .unwrap(),
    );
    native_owner_tests::publish(daemon, client).await;
}
