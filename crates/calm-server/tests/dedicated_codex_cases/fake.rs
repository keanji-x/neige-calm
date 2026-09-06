use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::io::Write;
use std::sync::Arc;
use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;

#[derive(Default)]
struct FakeState {
    thread: bool,
    turns: Vec<Value>,
    thread_reply_lost: bool,
    turn_reply_lost: bool,
}

// This ignored test is executed ONLY as the runtime-owned provider process.
// No production fake-provider binary or second client implementation is added.
#[test]
#[ignore = "runtime fixture process entry, launched by other tests"]
fn fake_provider_process() {
    assert_eq!(std::env::var("CODEX_HOME").unwrap(), "/provider/home");
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let text = std::fs::read_to_string("/provider/home/config.toml").unwrap();
        let config: toml_edit::DocumentMut = text.parse().unwrap();
        let scenario = config["model"].as_str().unwrap().to_owned();
        let listener = UnixListener::bind("/provider/control/app-server.sock").unwrap();
        let state = Arc::new(Mutex::new(FakeState::default()));
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let state = state.clone(); let scenario = scenario.clone();
            tokio::spawn(async move {
                let mut peer = tokio_tungstenite::accept_async(stream).await.unwrap();
                while let Some(Ok(frame)) = peer.next().await {
                    let Message::Text(text) = frame else { continue; };
                    let request: Value = serde_json::from_str(&text).unwrap();
                    if scenario == "backpressure" && request["method"] == "turn/start" {
                        std::fs::write("/provider/home/business-received", b"received").unwrap();
                    }
                    let mut log = std::fs::OpenOptions::new().create(true).append(true).open("/provider/home/fake-calls.jsonl").unwrap();
                    writeln!(log, "{text}").unwrap(); drop(log);
                    let mut state = state.lock().await;
                    let result = match request["method"].as_str().unwrap() {
                        "initialize" => json!({"userAgent":"fake-codex","codexHome":"/provider/home","platformFamily":"unix","platformOs":"linux"}),
                        "permissionProfile/list" => {
                            if scenario == "missing-allowed" { json!({"data":[{"id":"neige-delivery-v1"}],"nextCursor":null}) }
                            else if scenario == "missing-profile" { json!({"data":[],"nextCursor":null}) }
                            else { json!({"data":[{"id":"neige-delivery-v1","allowed":scenario != "denied-profile","description":null}],"nextCursor":null}) }
                        }
                        "thread/start" => {
                            assert_eq!(request["params"]["permissions"], "neige-delivery-v1");
                            assert!(request["params"].get("sandbox").is_none());
                            assert!(!state.thread, "controller duplicated thread/start");
                            state.thread = true;
                            if scenario == "lose-thread-reply" && !state.thread_reply_lost {
                                state.thread_reply_lost = true; let _ = peer.close(None).await; break;
                            }
                            json!({"thread":{"id":"owned-thread","cwd":if scenario == "wrong-context" {"/other"} else {"/workspace"},"turns":[]}})
                        }
                        "thread/loaded/list" => json!({"data":if state.thread {vec!["owned-thread"]} else {vec![]},"nextCursor":null}),
                        "thread/read" | "thread/resume" => json!({"thread":{"id":"owned-thread","cwd":"/workspace","turns":state.turns}}),
                        "turn/start" => {
                            assert!(state.thread); assert!(state.turns.is_empty(), "controller duplicated business turn");
                            state.turns.push(json!({"id":"owned-turn","status":"inProgress","items":[]}));
                            if scenario == "lose-turn-reply" && !state.turn_reply_lost {
                                state.turn_reply_lost = true; let _ = peer.close(None).await; break;
                            }
                            json!({"turn":{"id":"owned-turn"}})
                        }
                        method => panic!("unexpected client method {method}"),
                    };
                    let pause = scenario == "backpressure" && state.thread && state.turns.is_empty() && request["method"] == "permissionProfile/list";
                    drop(state);
                    let response = json!({"jsonrpc":"2.0","id":request["id"],"result":result});
                    if peer.send(Message::Text(response.to_string())).await.is_err() { break; }
                    if pause {
                        use std::os::fd::AsRawFd;
                        peer.get_ref().readable().await.unwrap();
                        let mut bytes: libc::c_int = 0;
                        assert_eq!(unsafe { libc::ioctl(peer.get_ref().as_raw_fd(), libc::FIONREAD, &mut bytes) }, 0);
                        std::fs::write("/provider/home/partial-bytes", bytes.to_string()).unwrap();
                        while !std::path::Path::new("/provider/home/release-reader").exists() {
                            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        }
                        // This can cause the real client read half to flush a retained frame.
                        let _ = peer.send(Message::Ping(vec![])).await;
                    }
                    if request["method"] == "turn/start" {
                        let _ = peer.send(Message::Text(json!({"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":"owned-thread","delta":"one provider event"}}).to_string())).await;
                    }
                }
                if scenario == "backpressure" {
                    std::fs::write("/provider/home/connection-ended", b"closed").unwrap();
                }
            });
        }
    });
}
