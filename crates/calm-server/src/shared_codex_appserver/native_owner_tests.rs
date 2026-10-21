#![cfg(test)]
//! Real WebSocket receipts and notifications through the shared daemon owner.
use super::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::UnixListener;
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};

pub(crate) type Peer = WebSocketStream<UnixStream>;

pub(crate) async fn connect_unpublished(
    daemon: &Arc<SharedCodexAppServer>,
) -> (Arc<CodexAppServer>, Peer, JoinHandle<()>) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("peer.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = async {
        let (stream, _) = listener.accept().await.unwrap();
        tokio_tungstenite::accept_async(stream).await.unwrap()
    };
    let (client, peer) = tokio::join!(CodexAppServer::connect(&socket), server);
    let (client, notifications) = client.unwrap();
    let client = Arc::new(client);
    let forwarder = daemon
        .install_client(client.clone(), notifications)
        .await
        .unwrap();
    (client, peer, forwarder)
}

pub(crate) async fn publish(daemon: &Arc<SharedCodexAppServer>, client: Arc<CodexAppServer>) {
    let launched = client.clone();
    daemon
        .start_new_process_typestate(|_| async move {
            Ok(LaunchedSharedDaemon {
                child: None,
                client: launched,
                runtime: SharedDaemonRuntime {
                    pid: 0,
                    pgid: 0,
                    boot_id: "fixture".into(),
                    process_start_time: 0,
                    started_at: 0,
                },
                watcher: SupervisorWatcher {
                    kind: WatcherKind::TakenOverPid,
                    handle: tokio::spawn(async {}),
                },
            })
        })
        .await
        .unwrap();
}

pub(crate) async fn connect_with_reader(
    daemon: &Arc<SharedCodexAppServer>,
) -> (Arc<CodexAppServer>, Peer, JoinHandle<()>) {
    let (client, peer, forwarder) = connect_unpublished(daemon).await;
    publish(daemon, client.clone()).await;
    (client, peer, forwarder)
}

pub(crate) async fn connect(daemon: &Arc<SharedCodexAppServer>) -> (Arc<CodexAppServer>, Peer) {
    let (client, peer, _forwarder) = connect_with_reader(daemon).await;
    (client, peer)
}

/// Exercise the actual automatic recovery owner with no request source. Cache
/// attribution is populated by the real native receipt, never this helper.
pub(crate) async fn resume_cached_without_source(daemon: &SharedCodexAppServer) {
    daemon.resume_cached_threads(ResumeMode::HotTakeover).await;
    daemon.resume_cached_threads(ResumeMode::ColdRespawn).await;
}

pub(crate) async fn hold_attribution(daemon: &SharedCodexAppServer) -> impl Drop + '_ {
    daemon.kernel_initiated_threads.lock().await
}

pub(crate) async fn hold_connected_client_gate(daemon: &SharedCodexAppServer) -> impl Drop + '_ {
    daemon.core.lock().await
}

pub(crate) async fn attribution_blocked(daemon: &SharedCodexAppServer) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            // This test holds only the inner attribution gate. Failure to take
            // the outer gate proves the real forwarder reached that inner wait.
            if daemon.kernel_thread_start_serial.try_lock().is_err() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

pub(crate) async fn reader_finished(client: &CodexAppServer) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !client.reader_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

async fn daemon() -> Arc<SharedCodexAppServer> {
    let repo = Arc::new(
        crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap(),
    );
    SharedCodexAppServer::new_stub(repo)
}

pub(crate) async fn request(peer: &mut Peer) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Message::Text(frame) = peer.next().await.unwrap().unwrap() {
                return serde_json::from_str(&frame).unwrap();
            }
        }
    })
    .await
    .unwrap()
}

pub(crate) async fn send(peer: &mut Peer, value: Value) {
    peer.send(Message::Text(value.to_string())).await.unwrap();
}

/// After the tested owner completes, flush the shared sink with a server-request
/// reply. Any RPC it sent is ordered before this reply, including buffered frames.
pub(crate) async fn assert_no_request(peer: &mut Peer) {
    let id = format!("fixture/no-request/{}", uuid::Uuid::new_v4());
    send(
        peer,
        json!({"id":id,"method":"fixture/unsupported","params":{}}),
    )
    .await;
    let frame = request(peer).await;
    assert_eq!(
        frame["id"], id,
        "unexpected native frame before the reply barrier: method={}",
        frame["method"]
    );
    assert_eq!(frame["error"]["code"], -32601);
}

async fn notify(daemon: &SharedCodexAppServer, peer: &mut Peer, method: &str, turn: Value) {
    notify_thread(daemon, peer, "thread", method, turn).await;
}

pub(crate) async fn notify_thread(
    daemon: &SharedCodexAppServer,
    peer: &mut Peer,
    thread: &str,
    method: &str,
    turn: Value,
) {
    let mut seen = daemon.subscribe_notifications();
    send(
        peer,
        json!({"method":method,"params":{"threadId":thread,"turn":turn}}),
    )
    .await;
    // A no-thread diagnostic follows the tested frame on the same stream. It
    // synchronizes even when the stale thread-bearing frame is correctly dropped.
    let barrier = format!("fixture/barrier/{}", uuid::Uuid::new_v4());
    send(peer, json!({"method":barrier,"params":{}})).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(seen.recv().await.unwrap(), Notification::Other {method, ..} if method == barrier) {break;}
        }
    }).await.unwrap();
}

fn start(daemon: &Arc<SharedCodexAppServer>) -> JoinHandle<CodexPromptReport> {
    let daemon = daemon.clone();
    tokio::spawn(async move {
        daemon
            .turn_start_report(
                "thread",
                vec![],
                &TurnModelSelection::inherit(),
                TurnApprovals::Explicit(
                    crate::planner_permission_mode::PlannerPermissionMode::Never,
                ),
                Some("client"),
                NativeInput::Legacy,
                &crate::harness::native_admission::NativePromptAdmission::new(
                    || Ok(()),
                    || Box::pin(async { Ok(()) }),
                ),
            )
            .await
    })
}

#[tokio::test]
async fn historical_remote_refusal_releases_native_start_for_corrected_model() {
    let daemon = daemon().await;
    let (_client, mut peer) = connect(&daemon).await;
    for (model, accepted) in [("unknown-model", false), ("corrected-model", true)] {
        let starting = daemon.clone();
        let mut task = tokio::spawn(async move {
            starting
                .turn_start_control(
                    "thread",
                    vec![InputItem::text("original input")],
                    &TurnModelSelection {
                        model: Some(model.into()),
                        effort: None,
                    },
                    TurnApprovals::Explicit(
                        crate::planner_permission_mode::PlannerPermissionMode::Never,
                    ),
                    Some("historical-input"),
                )
                .await
        });
        let call = tokio::select! {
            call = request(&mut peer) => call,
            result = &mut task => panic!("retry never reached the native peer: {result:?}"),
        };
        assert_eq!(call["method"], "turn/start");
        assert_eq!(call["params"]["model"], model);
        if accepted {
            send(
                &mut peer,
                json!({"id":call["id"],"result":{"turn":{"id":"retried"}}}),
            )
            .await;
            assert!(
                matches!(task.await.unwrap(), StartDelivery::NativeAck { turn, cleanup: None } if turn == "retried")
            );
            assert_eq!(
                daemon.active_turn_id_for_thread("thread").as_deref(),
                Some("retried")
            );
        } else {
            send(
                &mut peer,
                json!({"id":call["id"],"error":{"code":-32600,"message":"unknown model"}}),
            )
            .await;
            assert!(matches!(
                task.await.unwrap(),
                StartDelivery::Unknown {
                    error: control::StartError::Refused(_),
                    response_turn: None
                }
            ));
        }
    }
}

#[tokio::test]
async fn start_reply_without_native_identity_keeps_unknown_delivery_and_native_failure() {
    for reply in [json!({"turn": {}}), json!({"turn": {"id": ""}})] {
        let daemon = daemon().await;
        let (_client, mut peer) = connect(&daemon).await;
        let task = start(&daemon);
        let call = request(&mut peer).await;
        assert_eq!(call["method"], "turn/start");
        send(&mut peer, json!({"id": call["id"], "result": reply})).await;
        let report = task.await.unwrap();
        assert_eq!(report.delivery(), &CodexPromptDelivery::Unknown);
        match report.into_start_outcome().unwrap_err() {
            control::StartError::Unanswered(CalmError::CodexAppServer(message)) => {
                assert_eq!(message, "turn/start returned no turn.id");
            }
            other => panic!("native response lost its outcome classification: {other:?}"),
        }
        let retry = start(&daemon).await.unwrap();
        assert_eq!(retry.delivery(), &CodexPromptDelivery::NotDispatched);
        assert!(
            retry.rpc_failure_ref().is_none(),
            "Unknown must keep the native slot"
        );
        assert_no_request(&mut peer).await;
    }
}

#[tokio::test]
async fn completed_before_start_ack_does_not_resurrect_active_turn() {
    let daemon = daemon().await;
    let (_client, mut peer) = connect(&daemon).await;
    let task = start(&daemon);
    let call = request(&mut peer).await;
    assert_eq!(call["method"], "turn/start");
    notify(&daemon, &mut peer, "turn/started", json!({"id":"fast"})).await;
    notify(
        &daemon,
        &mut peer,
        "turn/completed",
        json!({"id":"fast","status":"completed"}),
    )
    .await;
    send(
        &mut peer,
        json!({"id":call["id"],"result":{"turn":{"id":"fast"}}}),
    )
    .await;
    let report = task.await.unwrap();
    assert!(
        matches!(report.delivery(), CodexPromptDelivery::Accepted { turn_id } if turn_id=="fast")
    );
    assert_eq!(daemon.active_turn_id_for_thread("thread"), None);
}

#[tokio::test]
async fn old_source_start_cannot_replace_current_native_turn() {
    let daemon = daemon().await;
    let (_old, mut old_peer) = connect(&daemon).await;
    let (_current, mut peer) = connect(&daemon).await;
    notify(&daemon, &mut peer, "turn/started", json!({"id":"current"})).await;
    notify(&daemon, &mut old_peer, "turn/started", json!({"id":"old"})).await;
    assert_eq!(
        daemon.active_turn_id_for_thread("thread").as_deref(),
        Some("current")
    );
}

#[tokio::test]
async fn old_start_ack_preserves_accepted_fact_without_overwriting_current_slot() {
    let daemon = daemon().await;
    let (_old, mut old_peer) = connect(&daemon).await;
    let task = start(&daemon);
    let call = request(&mut old_peer).await;
    let (_current, mut peer) = connect(&daemon).await;
    notify(&daemon, &mut peer, "turn/started", json!({"id":"current"})).await;
    send(
        &mut old_peer,
        json!({"id":call["id"],"result":{"turn":{"id":"old"}}}),
    )
    .await;
    let report = task.await.unwrap();
    assert!(
        matches!(report.delivery(), CodexPromptDelivery::Accepted { turn_id } if turn_id=="old")
    );
    assert_eq!(
        daemon.active_turn_id_for_thread("thread").as_deref(),
        Some("current")
    );
}

#[tokio::test]
async fn refused_new_start_preserves_known_active_native_turn() {
    let daemon = daemon().await;
    let (_client, mut peer) = connect(&daemon).await;
    notify(&daemon, &mut peer, "turn/started", json!({"id":"known"})).await;
    let task = start(&daemon);
    let call = request(&mut peer).await;
    notify(
        &daemon,
        &mut peer,
        "turn/started",
        json!({"id":"candidate"}),
    )
    .await;
    send(
        &mut peer,
        json!({"id":call["id"],"error":{"code":-32600,"message":"refused"}}),
    )
    .await;
    assert!(matches!(
        task.await.unwrap().delivery(),
        CodexPromptDelivery::Unknown
    ));
    assert_eq!(
        daemon.active_turn_id_for_thread("thread").as_deref(),
        Some("known")
    );
}

#[tokio::test]
async fn unknown_start_without_native_id_cannot_quiesce_as_idle() {
    let daemon = daemon().await;
    let (_client, mut peer) = connect(&daemon).await;
    let task = start(&daemon);
    let call = request(&mut peer).await;
    send(&mut peer, json!({"id":call["id"],"result":{}})).await;
    assert!(matches!(
        task.await.unwrap().delivery(),
        CodexPromptDelivery::Unknown
    ));
    assert!(matches!(
        daemon.interrupt_active_outcome("thread").await,
        provider::control::NativeInterruptOutcome::Unconfirmed { .. }
    ));
}

#[tokio::test]
async fn installed_client_is_not_current_before_running() {
    let daemon = daemon().await;
    let (client, mut peer, _forwarder) = connect_unpublished(&daemon).await;
    notify(
        &daemon,
        &mut peer,
        "turn/started",
        json!({"id":"too-early"}),
    )
    .await;
    assert_eq!(daemon.active_turn_id_for_thread("thread"), None);
    publish(&daemon, client).await;
    notify(&daemon, &mut peer, "turn/started", json!({"id":"current"})).await;
    assert_eq!(
        daemon.active_turn_id_for_thread("thread").as_deref(),
        Some("current")
    );
}

#[tokio::test]
async fn source_failure_invalidates_old_callbacks_without_a_generation_change() {
    let daemon = daemon().await;
    let (_retained_old_client, mut peer) = connect(&daemon).await;
    notify(&daemon, &mut peer, "turn/started", json!({"id":"current"})).await;
    let generation = daemon.core.lock().await.generation;
    let failed = daemon
        .start_new_process_typestate(|_| async {
            Err(CalmError::Conflict(
                "fixture: refused source replacement".into(),
            ))
        })
        .await;
    assert!(failed.is_err());
    assert_eq!(daemon.core.lock().await.generation, generation);
    notify(&daemon, &mut peer, "turn/started", json!({"id":"late"})).await;
    assert_eq!(
        daemon.active_turn_id_for_thread("thread").as_deref(),
        Some("current")
    );
}

pub(crate) async fn rebuild(daemon: &SharedCodexAppServer) {
    daemon.rebuild_thread_cache_from_db().await.unwrap();
}

#[tokio::test]
async fn old_source_malformed_item_cannot_enter_the_native_event_fanout() {
    let daemon = daemon().await;
    let (_old, mut old_peer) = connect(&daemon).await;
    let (_current, _peer) = connect(&daemon).await;
    let mut seen = daemon.subscribe_notifications();
    send(&mut old_peer,json!({"method":"item/completed","params":{"item":{"type":"userMessage","clientId":"old"}}})).await;
    send(
        &mut old_peer,
        json!({"method":"fixture/barrier","params":{}}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match seen.recv().await.unwrap() {
                Notification::Item { .. } => {
                    panic!("old receipt-bearing frame escaped source fence")
                }
                Notification::Other { method, .. } if method == "fixture/barrier" => break,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn interrupt_known_active_even_when_new_start_has_no_native_id() {
    let daemon = daemon().await;
    let (_client, mut peer) = connect(&daemon).await;
    notify(&daemon, &mut peer, "turn/started", json!({"id":"known"})).await;
    let task = start(&daemon);
    let call = request(&mut peer).await;
    send(&mut peer, json!({"id":call["id"],"result":{}})).await;
    let report = task.await.unwrap();
    assert!(matches!(report.delivery(), CodexPromptDelivery::Unknown));
    let stopping = daemon.clone();
    let stop = tokio::spawn(async move { stopping.interrupt_active_outcome("thread").await });
    let call = request(&mut peer).await;
    assert_eq!(call["method"], "turn/interrupt");
    assert_eq!(call["params"]["turnId"], "known");
    send(&mut peer, json!({"id":call["id"],"result":{}})).await;
    assert!(
        matches!(
            stop.await.unwrap(),
            provider::control::NativeInterruptOutcome::Unconfirmed { .. }
        ),
        "unknown candidate must still hold quiescence"
    );
    assert_eq!(daemon.active_turn_id_for_thread("thread"), None);
    assert!(matches!(
        daemon.interrupt_active_outcome("thread").await,
        provider::control::NativeInterruptOutcome::Unconfirmed { .. }
    ));
    assert!(matches!(report.delivery(), CodexPromptDelivery::Unknown));
}

#[tokio::test]
async fn bounded_interrupt_keeps_the_failed_candidate_and_retires_only_confirmed_control() {
    let daemon = daemon().await;
    let (_client, mut peer) = connect(&daemon).await;
    notify(&daemon, &mut peer, "turn/started", json!({"id":"known"})).await;
    let task = start(&daemon);
    let call = request(&mut peer).await;
    notify(
        &daemon,
        &mut peer,
        "turn/started",
        json!({"id":"candidate"}),
    )
    .await;
    send(
        &mut peer,
        json!({"id":call["id"],"error":{"code":-32600,"message":"refused"}}),
    )
    .await;
    assert!(matches!(
        task.await.unwrap().delivery(),
        CodexPromptDelivery::Unknown
    ));
    let third = tokio::time::timeout(Duration::from_secs(1), start(&daemon))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        third.delivery(),
        CodexPromptDelivery::NotDispatched
    ));
    let stopping = daemon.clone();
    let stop = tokio::spawn(async move { stopping.interrupt_active_outcome("thread").await });
    let first = request(&mut peer).await;
    assert_eq!(first["method"], "turn/interrupt");
    assert_eq!(first["params"]["turnId"], "known");
    send(&mut peer, json!({"id":first["id"],"result":{}})).await;
    let second = request(&mut peer).await;
    assert_eq!(second["method"], "turn/interrupt");
    assert_eq!(second["params"]["turnId"], "candidate");
    send(
        &mut peer,
        json!({"id":second["id"],"error":{"code":-32600,"message":"interrupt failed"}}),
    )
    .await;
    assert!(matches!(
        stop.await.unwrap(),
        provider::control::NativeInterruptOutcome::Unconfirmed { .. }
    ));
    assert_eq!(
        daemon.active_turn_id_for_thread("thread").as_deref(),
        Some("candidate")
    );
}

#[tokio::test]
async fn reader_eof_with_an_empty_slot_is_not_quiescent_even_while_core_says_running() {
    let daemon = daemon().await;
    let (_client, peer, forwarder) = connect_with_reader(&daemon).await;
    drop(peer);
    tokio::time::timeout(Duration::from_secs(5), forwarder)
        .await
        .unwrap()
        .unwrap();
    assert!(daemon.is_running());
    assert!(matches!(
        daemon.interrupt_active_outcome("existing-thread").await,
        provider::control::NativeInterruptOutcome::Unconfirmed { .. }
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn aborted_unpolled_forwarder_before_running_cannot_admit_a_native_start() {
    let daemon = daemon().await;
    // install_client contains no await after spawn; this current-thread caller
    // aborts the actual future before yielding to its first poll.
    let (client, mut peer, forwarder) = connect_unpublished(&daemon).await;
    forwarder.abort();
    assert!(forwarder.await.unwrap_err().is_cancelled());
    assert!(client.notification_stream_closed());
    assert!(!client.reader_finished());
    publish(&daemon, client).await;
    let mut task = start(&daemon);
    let report = tokio::select! {
        result=&mut task => result.unwrap(),
        call=request(&mut peer) => panic!("closed native owner issued a request: {}",call["method"]),
    };
    assert!(matches!(
        report.delivery(),
        CodexPromptDelivery::NotDispatched
    ));
    assert!(
        report.rpc_failure_ref().is_none(),
        "the owner must refuse before the transport core"
    );
    assert!(matches!(
        daemon.interrupt_active_outcome("thread").await,
        provider::control::NativeInterruptOutcome::Unconfirmed { .. }
    ));
}

#[tokio::test]
async fn reader_eof_before_running_cannot_be_published_as_a_live_native_source() {
    let daemon = daemon().await;
    let (client, peer, forwarder) = connect_unpublished(&daemon).await;
    drop(peer);
    forwarder.await.unwrap();
    reader_finished(&client).await;
    publish(&daemon, client).await;
    let report = start(&daemon).await.unwrap();
    assert!(matches!(
        report.delivery(),
        CodexPromptDelivery::NotDispatched
    ));
    assert!(report.rpc_failure_ref().is_none());
}

#[tokio::test]
async fn completion_blocked_after_observation_cannot_publish_from_a_retired_source() {
    let daemon = daemon().await;
    let (_old, mut old_peer) = connect(&daemon).await;
    notify(&daemon, &mut old_peer, "turn/started", json!({"id":"old"})).await;
    assert_eq!(
        daemon.active_turn_id_for_thread("thread").as_deref(),
        Some("old")
    );
    let mut markers = daemon.kernel_initiated_threads.lock().await;
    let mut seen = daemon.subscribe_notifications();
    send(&mut old_peer, json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"old","status":"completed"}}})).await;
    // The actual slot changes only when the forwarder observes this END. Its
    // next operation is blocked on the real bookkeeping mutex held above.
    tokio::time::timeout(Duration::from_secs(5), async {
        while daemon.active_turn_id_for_thread("thread").is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let (_current, mut peer) = connect(&daemon).await;
    notify(&daemon, &mut peer, "turn/started", json!({"id":"current"})).await;
    markers.insert("thread".into());
    drop(markers);
    let barrier = format!("fixture/retired-completion/{}", uuid::Uuid::new_v4());
    send(&mut old_peer, json!({"method":barrier,"params":{}})).await;
    let escaped = tokio::time::timeout(Duration::from_secs(5), async {
        let mut escaped = false;
        loop {
            match seen.recv().await.unwrap() {
                Notification::TurnCompleted { .. } => escaped = true,
                Notification::Other { method, .. } if method == barrier => break escaped,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    let marker_survived = daemon
        .kernel_initiated_threads
        .lock()
        .await
        .contains("thread");
    assert!(
        !escaped && marker_survived,
        "retired completion escaped={escaped}, replacement marker survived={marker_survived}"
    );
    assert_eq!(
        daemon.active_turn_id_for_thread("thread").as_deref(),
        Some("current")
    );
}
