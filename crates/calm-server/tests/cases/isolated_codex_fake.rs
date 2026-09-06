//! Test executable used as the actual runtime-owned provider; no production fake binary.
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::io::{BufRead, Write};

pub fn run() {
    assert_eq!(std::env::var("CODEX_HOME").unwrap(), "/provider/home");
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let config:toml_edit::DocumentMut=std::fs::read_to_string("/provider/home/config.toml").unwrap().parse().unwrap();
        let scenario=config["model"].as_str().unwrap().to_string();
        let listener=tokio::net::UnixListener::bind("/provider/control/app-server.sock").unwrap();
        let thread_id=uuid::Uuid::new_v4().to_string();let turn_id=uuid::Uuid::new_v4().to_string();
        let mut started=false;let mut turn=false;let mut prompt=String::new();
        loop {
            let (stream,_)=listener.accept().await.unwrap();
            let mut peer=tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(frame))=peer.next().await {
                let tokio_tungstenite::tungstenite::Message::Text(text)=frame else {continue;};
                let request:Value=serde_json::from_str(&text).unwrap();
                let method=request["method"].as_str().unwrap();
                let mut log=std::fs::OpenOptions::new().create(true).append(true).open("/provider/home/fake-calls.jsonl").unwrap();
                writeln!(log,"{text}").unwrap();drop(log);
                let result=match method {
                    "initialize"=>json!({"userAgent":"isolated-fake","codexHome":"/provider/home","platformFamily":"unix","platformOs":"linux"}),
                    "permissionProfile/list"=>json!({"data":[{"id":"neige-delivery-v1","allowed":true,"description":null}],"nextCursor":null}),
                    "thread/start"=>{assert!(!started,"duplicate thread/start");started=true;
                        prompt=request["params"]["developerInstructions"].as_str().unwrap().to_string();
                        json!({"thread":{"id":thread_id,"cwd":"/workspace","turns":[]}})},
                    "thread/loaded/list"=>json!({"data":if started {vec![thread_id.clone()]}else{vec![]},"nextCursor":null}),
                    "thread/read"|"thread/resume"=>json!({"thread":{"id":thread_id,"cwd":"/workspace","turns":if turn {vec![json!({"id":turn_id,"status":"inProgress","items":[]})]}else{vec![]}}}),
                    "turn/start"=>{assert!(!turn,"duplicate turn/start");turn=true;json!({"turn":{"id":turn_id}})},
                    other=>panic!("unexpected fake provider method {other}"),
                };
                if method=="turn/start" && scenario=="lose-ack" {let _=peer.close(None).await;break;}
                peer.send(tokio_tungstenite::tungstenite::Message::Text(json!({"jsonrpc":"2.0","id":request["id"],"result":result}).to_string())).await.unwrap();
                if method=="turn/start" {
                    assert!(!std::path::Path::new("/workspace/result.txt").exists(), "each execution starts empty");
                    std::fs::write("/workspace/result.txt",b"42\n").unwrap();
                    let directory="/provider/home/sessions/2026/09/06";std::fs::create_dir_all(directory).unwrap();
                    let mut file=std::fs::File::create(format!("{directory}/rollout-2026-09-06T00-00-00-{thread_id}.jsonl")).unwrap();
                    for record in [json!({"timestamp":"2026-09-06T00:00:00Z","type":"session_meta","payload":{"id":thread_id,"cwd":"/workspace","originator":"fake","cli_version":"fake"}}),
                        json!({"timestamp":"2026-09-06T00:00:01Z","type":"response_item","payload":{"type":"message","id":"answer-42","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"Created result.txt containing 42."}]}})] {
                        writeln!(file,"{record}").unwrap();
                    }
                    file.sync_all().unwrap();
                    // Make the running phase observable; completion still uses the actual native shim/server.
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    if scenario=="wait" {continue;}
                    if scenario.starts_with("crash-") {
                        while !std::path::Path::new("/workspace/report-now").exists() {
                            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        }
                    }
                    if scenario=="controlled" {
                        while !std::path::Path::new("/workspace/report-success").exists()
                            && !std::path::Path::new("/workspace/report-failure").exists() {
                            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        }
                    }
                    if scenario!="no-report" {
                        let task=prompt.split("Task completion idempotency_key: ").nth(1).unwrap().lines().next().unwrap().to_string();
                        let env=config["mcp_servers"]["calm"]["env"].as_table_like().unwrap().iter()
                            .map(|(k,v)|(k.to_string(),v.as_str().unwrap().to_string())).collect::<Vec<_>>();
                        let success=scenario!="fail" && !std::path::Path::new("/workspace/report-failure").exists();
                        tokio::task::spawn_blocking(move||report(&env,&task,success)).await.unwrap();
                    }
                    let _=peer.send(tokio_tungstenite::tungstenite::Message::Text(json!({"jsonrpc":"2.0","method":"turn/completed",
                        "params":{"threadId":thread_id,"turn":{"id":turn_id,"status":"completed","items":[]}}}).to_string())).await;
                }
            }
        }
    });
}
fn report(env: &[(String, String)], task: &str, success: bool) {
    use std::process::{Command, Stdio};
    let mut child = Command::new("/mcp-shim")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .envs(env.iter().cloned())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
    let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"isolated-fake","version":"1"}}});
    writeln!(input, "{init}").unwrap();
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    let response: Value = serde_json::from_str(&line).unwrap();
    assert!(
        response.get("error").is_none(),
        "native initialize failed: {response}"
    );
    let args = if success {
        json!({"idempotency_key":task,"result":{"answer":42},"artifacts":[]})
    } else {
        json!({"idempotency_key":task,"reason":"fixture requested failure"})
    };
    writeln!(input,"{}",json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":if success{"calm.task.complete"}else{"calm.task.fail"},"arguments":args}})).unwrap();
    line.clear();
    output.read_line(&mut line).unwrap();
    std::fs::write("/provider/home/fake-report-response.json", &line).unwrap();
    let response: Value = serde_json::from_str(&line).unwrap();
    assert!(
        response.get("error").is_none(),
        "native task report rejected: {response}"
    );
    drop(input);
    let _ = child.wait();
}
