use super::*;
use crate::db::RepoRead;
use crate::operation::execution_manager::tests::{fixture as manager_fixture, owner};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio_tungstenite::WebSocketStream;

struct Fixture {
    repo: Arc<crate::db::sqlite::SqlxRepo>,
    _cwd: tempfile::TempDir,
    shared: Arc<super::super::SharedCodexAppServer>,
    scope: Scope,
}
async fn fixture() -> Fixture {
    let (repo, cwd, track) = manager_fixture().await;
    let owner = owner(&repo, cwd.path(), &track, "ingress-thread", false).await;
    let terminal = crate::model::new_id();
    let execution = crate::model::new_id();
    sqlx::query(
        "INSERT INTO terminals(id,card_id,program,cwd,theme_fg,theme_bg,created_at) \
         VALUES(?1,?2,'codex',?3,'255,255,255','0,0,0',0)",
    )
    .bind(&terminal)
    .bind(&owner.card)
    .bind(cwd.path().to_str().unwrap())
    .execute(repo.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO operations(id,operation_key,kind,idempotency_key,payload_hash,target_type, \
         target_json,payload_json,phase,tx_output_json,created_at_ms,updated_at_ms) \
         VALUES(?1,?1,'codex-create',?1,'hash','card','{}','{}','pending',?2,0,0)",
    )
    .bind(crate::model::new_id())
    .bind(json!({"data":{"terminal_id":terminal,"card_id":owner.card}}).to_string())
    .execute(repo.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO workspace_leases(lease_id,card_id,track_id,path,state,lease_owner,access_mode, \
         write_root_id,holder_kind,holder_id,holder_phase,created_at_ms,updated_at_ms) \
         VALUES(?1,?2,?3,?4,'held',?1,'read_write',?1,'terminal',?5,'issuing',0,0)")
        .bind(&execution).bind(&owner.card).bind(&track).bind(cwd.path().to_str().unwrap()).bind(&terminal).execute(repo.pool()).await.unwrap();
    let runtime = crate::model::new_id();
    let mut tx = repo.pool().begin().await.unwrap();
    crate::db::sqlite::session_start_runtime_tx(
        &mut tx,
        crate::session_projection_repo::WorkerSessionInit {
            id: runtime,
            card_id: owner.card.clone(),
            kind: crate::session_projection_repo::WorkerSessionKind::CodexCard,
            agent_provider: Some(crate::session_projection_repo::AgentProvider::Codex),
            status: calm_types::worker::WorkerSessionState::Running,
            terminal_run_id: Some(terminal.clone()),
            thread_id: Some("ingress-thread".into()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: 0,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let repo = Arc::new(repo);
    let pending = Arc::new(
        crate::pending_codex_threads::PendingThreadStartRegistry::new(
            repo.clone(),
            crate::event::EventBus::new(),
        ),
    );
    let shared =
        super::super::SharedCodexAppServer::new_stub_with_pending(repo.clone(), Some(pending));
    let record = super::super::super::storage::load(repo.pool(), &execution)
        .await
        .unwrap()
        .unwrap();
    let (provider, directory) = shared.ingress_endpoints();
    let scope = scope::prepare(repo.pool(), &record, &directory.join("test.sock"), provider)
        .await
        .unwrap();
    Fixture {
        repo,
        _cwd: cwd,
        shared,
        scope,
    }
}
struct Provider {
    task: tokio::task::JoinHandle<()>,
    requests: Arc<std::sync::Mutex<Vec<Value>>>,
    stop_allowed: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    children: Arc<std::sync::Mutex<std::collections::BTreeMap<String, Value>>>,
    child_stop_allowed: Arc<AtomicBool>,
    spawn_grandchild: Arc<AtomicBool>,
    paged: Arc<AtomicBool>,
}
impl Drop for Provider {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn provider(f: &Fixture) -> Provider {
    tokio::fs::create_dir_all(f.scope.provider.parent().unwrap())
        .await
        .unwrap();
    let listener = UnixListener::bind(&f.scope.provider).unwrap();
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let stop_allowed = Arc::new(AtomicBool::new(false));
    let stopped = Arc::new(AtomicBool::new(false));
    let children = Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::<
        String,
        Value,
    >::new()));
    let child_stop_allowed = Arc::new(AtomicBool::new(false));
    let spawn_grandchild = Arc::new(AtomicBool::new(false));
    let paged = Arc::new(AtomicBool::new(false));
    let task = {
        let (pool, cwd, execution, requests, stop_allowed, stopped) = (
            f.repo.pool().clone(),
            f.scope.cwd.clone(),
            f.scope.execution.clone(),
            requests.clone(),
            stop_allowed.clone(),
            stopped.clone(),
        );
        let (children, child_stop_allowed, spawn_grandchild, paged) = (
            children.clone(),
            child_stop_allowed.clone(),
            spawn_grandchild.clone(),
            paged.clone(),
        );
        tokio::spawn(async move {
            let nonce = Arc::new(std::sync::Mutex::new(String::new()));
            let next_thread = Arc::new(std::sync::atomic::AtomicU64::new(1));
            let mut clients = JoinSet::new();
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let next_thread = next_thread.clone();
                let (children, child_stop_allowed, spawn_grandchild, paged) = (
                    children.clone(),
                    child_stop_allowed.clone(),
                    spawn_grandchild.clone(),
                    paged.clone(),
                );
                let (pool, cwd, execution, requests, nonce, stop_allowed, stopped) = (
                    pool.clone(),
                    cwd.clone(),
                    execution.clone(),
                    requests.clone(),
                    nonce.clone(),
                    stop_allowed.clone(),
                    stopped.clone(),
                );
                clients.spawn(async move {
                    let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                    while let Some(Ok(Message::Text(text))) = ws.next().await {
                        let frame: Value = serde_json::from_str(&text).unwrap();
                        requests.lock().unwrap().push(frame.clone());
                        if frame.get("id").is_none() { continue; }
                        let method = frame["method"].as_str().unwrap();
                        let result = match method {
                            "initialize" => json!({"userAgent":"fake","future":{"capabilities":"preserved"}}),
                            "thread/read" if children.lock().unwrap().contains_key(frame["params"]["threadId"].as_str().unwrap()) => {
                                json!({"thread":children.lock().unwrap()[frame["params"]["threadId"].as_str().unwrap()].clone()})
                            }
                            "thread/list" => {
                                assert_eq!(frame["params"]["sourceKinds"],json!(["subAgentThreadSpawn"]));
                                if frame["params"]["archived"]==true { json!({"data":[],"nextCursor":null}) }
                                else {
                                let data: Vec<Value> = children.lock().unwrap().values().cloned().collect();
                                if paged.load(Ordering::SeqCst) && data.len()>1 {
                                    if frame["params"]["cursor"] == "next-page" { json!({"data":data[1..],"nextCursor":null}) }
                                    else { json!({"data":data[..1],"nextCursor":"next-page"}) }
                                } else { json!({"data":data,"nextCursor":null}) }
                                }
                            }
                            "thread/loaded/list" => {
                                let mut data = vec!["ingress-thread".to_owned()];
                                data.extend(children.lock().unwrap().keys().cloned());
                                json!({"data":data,"nextCursor":null})
                            }
                            "thread/read" => {
                                let nonce = nonce.lock().unwrap().clone();
                                let stopped = stopped.load(Ordering::SeqCst);
                                json!({"thread":{"id":"ingress-thread","cwd":cwd,"source":"appServer","status":if stopped || nonce.is_empty() {json!({"type":"idle"})} else {json!({"type":"active","activeFlags":[]})},"turns":if nonce.is_empty() {json!([])} else {json!([{"id":"ingress-turn","status":if stopped {"interrupted"} else {"inProgress"},"items":[{"type":"userMessage","clientId":nonce}]}])}}})
                            }
                            "thread/start" => {
                                let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM native_session_thread_requests \
                                    WHERE session_execution_id=?1 AND reply_json IS NULL")
                                    .bind(&execution).fetch_one(&pool).await.unwrap();
                                assert_eq!(pending, 1, "thread request is durable before forward");
                                if frame["params"]["model"] == "unknown-creation" { json!({"future":"missing-thread-id"}) }
                                else { json!({"thread":{"id":format!("created-thread-{}", next_thread.fetch_add(1, Ordering::SeqCst)),"cwd":cwd},"future":"created"}) }
                            }
                            "thread/resume" => json!({"thread":{"id":"ingress-thread","cwd":cwd},"future":"resume"}),
                            "turn/start" => {
                                if frame["params"]["model"] == "rejected-model" {
                                    ws.send(Message::Text(json!({"jsonrpc":"2.0","id":frame["id"],"error":{"code":-32602,"message":"unknown model","data":{"field":"model","future":42}}}).to_string())).await.unwrap();
                                    continue;
                                }
                                let durable: (String, String, String) = sqlx::query_as(
                                    "SELECT native.native_client_id,native.holder_phase,relation.session_execution_id \
                                     FROM workspace_leases native JOIN native_session_executions relation \
                                     ON relation.execution_id=native.lease_id WHERE native.holder_id='ingress-thread' AND native.state='held'"
                                ).fetch_one(&pool).await.unwrap();
                                assert_eq!(durable.0, frame["params"]["clientUserMessageId"]);
                                assert_eq!(durable.1, "issuing");
                                assert_eq!(durable.2, execution);
                                *nonce.lock().unwrap() = durable.0;
                                json!({"turn":{"id":"ingress-turn","items":[],"status":"inProgress"},"future":{"reply":true}})
                            }
                            "turn/interrupt" if children.lock().unwrap().contains_key(frame["params"]["threadId"].as_str().unwrap()) => {
                                if !child_stop_allowed.load(Ordering::SeqCst) {
                                    ws.send(Message::Text(json!({"jsonrpc":"2.0","id":frame["id"],"error":{"code":-32000,"message":"child stop unconfirmed"}}).to_string())).await.unwrap();
                                    continue;
                                }
                                let id = frame["params"]["threadId"].as_str().unwrap();
                                let mut children = children.lock().unwrap();
                                let child = children.get_mut(id).unwrap();
                                child["status"] = json!({"type":"idle"});
                                for turn in child["turns"].as_array_mut().unwrap() { turn["status"] = json!("interrupted"); }
                                if spawn_grandchild.swap(false, Ordering::SeqCst) {
                                    let mut grandchild = child_facts("grandchild",id,&cwd,"inProgress");
                                    grandchild["source"]["subAgent"]["thread_spawn"]["depth"] = json!(2);
                                    children.insert("grandchild".into(), grandchild);
                                }
                                json!({})
                            }
                            "turn/interrupt" if stop_allowed.load(Ordering::SeqCst) => { stopped.store(true, Ordering::SeqCst); json!({}) }
                            "turn/interrupt" => {
                                ws.send(Message::Text(json!({"jsonrpc":"2.0","id":frame["id"],"error":{"code":-32000,"message":"stop unconfirmed"}}).to_string())).await.unwrap();
                                continue;
                            }
                            "thread/backgroundTerminals/clean" => json!({}),
                            "thread/backgroundTerminals/list" => json!({"data":[],"nextCursor":null}),
                            _ => panic!("unexpected forwarded method {method}"),
                        };
                        if ws.send(Message::Text(json!({"jsonrpc":"2.0","id":frame["id"],"result":result}).to_string())).await.is_err() { break; }
                    }
                });
            }
        })
    };
    Provider {
        task,
        requests,
        stop_allowed,
        stopped,
        children,
        child_stop_allowed,
        spawn_grandchild,
        paged,
    }
}
struct FakeClientBackend;
#[async_trait::async_trait]
impl super::super::super::backend::Backend for FakeClientBackend {
    type Request = ();
    type Output = ();
    fn kind(&self) -> super::super::super::BackendKind {
        super::super::super::BackendKind::NativeSession
    }
    async fn launch(
        &self,
        permit: super::super::super::LaunchPermit,
        _: (),
    ) -> super::super::super::backend::LaunchOutcome<()> {
        assert!(matches!(
            permit,
            super::super::super::LaunchPermit::Write(_)
        ));
        super::super::super::backend::LaunchOutcome::Started {
            identity: permit.record().holder.clone(),
            output: (),
        }
    }
    async fn recover(&self, _: &Record) -> Result<super::super::super::backend::Observation> {
        unreachable!()
    }
    async fn stop(&self, _: &Record) -> Result<super::super::super::backend::Observation> {
        unreachable!()
    }
}
async fn client(f: &Fixture) -> WebSocketStream<UnixStream> {
    ExecutionManager::new(f.repo.pool().clone())
        .launch_reserved(&FakeClientBackend, &f.scope.execution, ())
        .await
        .unwrap();
    bind(f.repo.pool().clone(), f.scope.clone(), f.shared.clone())
        .await
        .unwrap();
    let stream = UnixStream::connect(&f.scope.socket).await.unwrap();
    let (client, _) = tokio_tungstenite::client_async("ws://localhost/", stream)
        .await
        .unwrap();
    client
}
async fn rpc(
    client: &mut WebSocketStream<UnixStream>,
    id: Value,
    method: &str,
    params: Value,
) -> Value {
    client
        .send(Message::Text(
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string(),
        ))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let frame = client.next().await.unwrap().unwrap();
            if let Message::Text(text) = frame {
                let value: Value = serde_json::from_str(&text).unwrap();
                if value.get("id") == Some(&id) {
                    return value;
                }
            }
        }
    })
    .await
    .unwrap()
}
async fn initialize(client: &mut WebSocketStream<UnixStream>) {
    let reply = rpc(client, json!("init"), "initialize", json!({"clientInfo":{"name":"fake-tui","version":"test"},"capabilities":{"experimentalApi":true}})).await;
    assert_eq!(reply["result"]["future"]["capabilities"], "preserved");
}

#[tokio::test]
async fn native_ingress_socket_denies_unknown_and_foreign_owner_without_forwarding() {
    let f = fixture().await;
    let provider = provider(&f).await;
    let mut client = client(&f).await;
    initialize(&mut client).await;
    for method in [
        "command/exec",
        "thread/fork",
        "config/value/write",
        "new/futureMutation",
    ] {
        assert!(
            rpc(&mut client, json!(method), method, json!({}))
                .await
                .get("error")
                .is_some()
        );
    }
    let foreign = owner(
        &f.repo,
        f._cwd.path(),
        &f.scope.track,
        "foreign-thread",
        false,
    )
    .await;
    assert_ne!(foreign.card, f.scope.card);
    let reply = rpc(
        &mut client,
        json!("foreign"),
        "thread/resume",
        json!({"threadId":"foreign-thread"}),
    )
    .await;
    assert!(
        reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains(&foreign.card)
    );
    let methods: Vec<String> = provider
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter_map(|frame| frame["method"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(methods, vec!["initialize"]);
    quiesce(&f.scope.socket).await.unwrap();
}

#[tokio::test]
async fn native_ingress_socket_persists_turn_before_forward_and_preserves_full_reply() {
    let f = fixture().await;
    let provider = provider(&f).await;
    let mut client = client(&f).await;
    initialize(&mut client).await;
    let input = json!([{"type":"mention","name":"asset","path":"asset"}]);
    let reply = rpc(
        &mut client,
        json!("start"),
        "turn/start",
        json!({"threadId":"ingress-thread","input":input,"clientUserMessageId":"caller-nonce"}),
    )
    .await;
    assert_eq!(reply["result"]["future"]["reply"], true);
    let (phase, observed): (String, String) = sqlx::query_as("SELECT holder_phase,native_observed_turn_id FROM workspace_leases WHERE holder_kind='native' AND state='held'")
        .fetch_one(f.repo.pool()).await.unwrap();
    assert_eq!(
        (phase.as_str(), observed.as_str()),
        ("running", "ingress-turn")
    );
    let frames = provider.requests.lock().unwrap();
    let start = frames
        .iter()
        .find(|frame| frame["method"] == "turn/start")
        .unwrap();
    assert_eq!(start["params"]["input"], input);
    assert_ne!(start["params"]["clientUserMessageId"], "caller-nonce");
    drop(frames);
    quiesce(&f.scope.socket).await.unwrap();
}

#[tokio::test]
async fn native_ingress_command_and_recovery_keep_the_frozen_socket() {
    let f = fixture().await;
    let manager = ExecutionManager::new(f.repo.pool().clone());
    let terminal = f
        .repo
        .terminal_get(&f.scope.terminal)
        .await
        .unwrap()
        .unwrap();
    // Use a fresh production checkpoint rather than the fixture's selected socket.
    sqlx::query("DELETE FROM native_session_threads WHERE session_execution_id=?1")
        .bind(&f.scope.execution)
        .execute(f.repo.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM native_session_ingresses WHERE session_execution_id=?1")
        .bind(&f.scope.execution)
        .execute(f.repo.pool())
        .await
        .unwrap();
    let (_, command) = manager
        .prepare_native_session_command(&terminal, &f.shared, Some("ingress-thread"))
        .await
        .unwrap();
    let scope = scope::load(f.repo.pool(), &f.scope.execution)
        .await
        .unwrap();
    assert!(command.contains(scope.socket.to_str().unwrap()));
    assert!(!command.contains(f.scope.provider.to_str().unwrap()));
    quiesce(&scope.socket).await.unwrap();
    let restored = restore(f.repo.pool(), &scope.execution, f.shared.clone())
        .await
        .unwrap();
    assert_eq!(restored, format!("unix://{}", scope.socket.display()));
    assert!(scope.socket.exists());
    quiesce(&scope.socket).await.unwrap();
}

#[tokio::test]
async fn native_ingress_client_stop_cannot_release_live_remote_execution() {
    use calm_session::control::{ControlMsg, ControlReply};
    use calm_session::{read_frame, write_frame};
    let f = fixture().await;
    let provider = provider(&f).await;
    let sockets = calm_test_sockets::socket_dir("native-ingress-stop");
    let supervisor = sockets.path().join("control.sock");
    let listener = UnixListener::bind(&supervisor).unwrap();
    let control = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let reply = match read_frame::<ControlMsg, _>(&mut stream).await.unwrap() {
                ControlMsg::Probe(_) => ControlReply::ProbeOk {
                    supervisor_version: calm_session::SUPERVISOR_CONTROL_VERSION,
                    proc_running: false,
                },
                ControlMsg::StopAndConfirm { .. } => ControlReply::Stopped,
                other => panic!("unexpected supervisor request {other:?}"),
            };
            write_frame(&mut stream, &reply).await.unwrap();
        }
    });
    let mut client = client(&f).await;
    initialize(&mut client).await;
    let launched = rpc(
        &mut client,
        json!("start"),
        "turn/start",
        json!({"threadId":"ingress-thread","input":[{"type":"text","text":"hello"}]}),
    )
    .await;
    assert!(launched.get("result").is_some(), "{launched}");
    let first =
        super::super::stop_managed_native_session(f.repo.as_ref(), &supervisor, &f.scope.terminal)
            .await;
    assert!(first.is_err(), "remote provider did not confirm stop");
    let rows: Vec<(String,String)> = sqlx::query_as("SELECT state,holder_phase FROM workspace_leases WHERE holder_kind IN ('terminal','native') ORDER BY holder_kind")
        .fetch_all(f.repo.pool()).await.unwrap();
    assert_eq!(
        rows,
        vec![
            ("held".into(), "stopping".into()),
            ("held".into(), "stopping".into())
        ]
    );
    assert!(!provider.stopped.load(Ordering::SeqCst));
    provider.stop_allowed.store(true, Ordering::SeqCst);
    assert_eq!(
        super::super::stop_managed_native_session(f.repo.as_ref(), &supervisor, &f.scope.terminal)
            .await
            .unwrap(),
        Some(true)
    );
    let rows: Vec<(String,String)> = sqlx::query_as("SELECT state,holder_phase FROM workspace_leases WHERE holder_kind IN ('terminal','native') ORDER BY holder_kind")
        .fetch_all(f.repo.pool()).await.unwrap();
    assert_eq!(
        rows,
        vec![
            ("released".into(), "stopped".into()),
            ("released".into(), "stopped".into())
        ]
    );
    assert!(
        restore(f.repo.pool(), &f.scope.execution, f.shared.clone())
            .await
            .is_err()
    );
    assert_eq!(
        super::super::stop_managed_native_session(f.repo.as_ref(), &supervisor, &f.scope.terminal)
            .await
            .unwrap(),
        Some(true)
    );
    control.abort();
}

#[tokio::test]
async fn native_ingress_new_threads_keep_exact_runtime_owner_and_unknown_creation_is_not_retried() {
    let f = fixture().await;
    let provider = provider(&f).await;
    let mut client = client(&f).await;
    initialize(&mut client).await;
    for n in [1, 2] {
        let reply = rpc(
            &mut client,
            json!(n),
            "thread/start",
            json!({"cwd":f.scope.cwd}),
        )
        .await;
        assert_eq!(
            reply["result"]["thread"]["id"],
            format!("created-thread-{n}"),
            "{reply}"
        );
        let (card, cwd): (String, String) = sqlx::query_as(
            "SELECT card_id,cwd FROM workspace_execution_bindings \
            WHERE provider='codex' AND holder_id=?1",
        )
        .bind(format!("created-thread-{n}"))
        .fetch_one(f.repo.pool())
        .await
        .unwrap();
        assert_eq!(
            (card.as_str(), cwd.as_str()),
            (f.scope.card.as_str(), f.scope.cwd.as_str())
        );
    }
    let runtime_thread: String =
        sqlx::query_scalar("SELECT thread_id FROM worker_sessions WHERE card_id=?1")
            .bind(&f.scope.card)
            .fetch_one(f.repo.pool())
            .await
            .unwrap();
    assert_eq!(runtime_thread, "created-thread-2");
    let unknown = rpc(
        &mut client,
        json!(3),
        "thread/start",
        json!({"model":"unknown-creation"}),
    )
    .await;
    assert!(unknown.get("error").is_some());
    let retry = rpc(&mut client, json!(4), "thread/start", json!({})).await;
    assert!(
        retry["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unconfirmed")
    );
    assert_eq!(
        provider
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|frame| frame["method"] == "thread/start")
            .count(),
        3
    );
    quiesce(&f.scope.socket).await.unwrap();
}

#[tokio::test]
async fn native_ingress_provider_refusal_preserves_error_data_after_durable_rejection() {
    let f = fixture().await;
    let _provider = provider(&f).await;
    let mut client = client(&f).await;
    initialize(&mut client).await;
    let reply = rpc(
        &mut client,
        json!("rejected"),
        "turn/start",
        json!({"threadId":"ingress-thread","input":[],"model":"rejected-model"}),
    )
    .await;
    assert_eq!(
        reply["error"],
        json!({"code":-32602,"message":"unknown model","data":{"field":"model","future":42}})
    );
    let (state, phase): (String, String) = sqlx::query_as(
        "SELECT state,holder_phase FROM workspace_leases WHERE holder_kind='native'",
    )
    .fetch_one(f.repo.pool())
    .await
    .unwrap();
    assert_eq!((state.as_str(), phase.as_str()), ("released", "stopped"));
    quiesce(&f.scope.socket).await.unwrap();
}

fn child_facts(id: &str, parent: &str, cwd: &str, status: &str) -> Value {
    json!({"id":id,"cwd":cwd,"source":{"subAgent":{"thread_spawn":{"parent_thread_id":parent,"depth":1}}},
        "status":{"type":"active","activeFlags":[]},"turns":[{"id":format!("{id}-turn"),"status":status,"items":[]}]})
}
#[path = "protocol_tests.rs"]
mod protocol_tests;
