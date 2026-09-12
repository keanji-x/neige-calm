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
                        if (scenario=="candidate-review-preturn" && prompt.contains("Read the exact sealed files"))
                            || (scenario=="candidate-repair-preturn" && prompt.contains("Repair the exact C1 files")) {
                            std::fs::write("/workspace/await-preturn",b"").unwrap();
                            while !std::path::Path::new("/workspace/resume-preturn").exists() {
                                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                            }
                        }
                        if scenario=="candidate-corrupt" && std::path::Path::new("/workspace/inputs/source/project.py").exists() {
                            std::fs::write("/workspace/inputs/source/project.py",b"corrupt before turn").unwrap();
                        }
                        if scenario=="delivery-corrupt" && std::path::Path::new("/workspace/inputs/source/result.json").exists() {
                            std::fs::write("/workspace/inputs/source/result.json",b"99").unwrap();
                        }
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
                    if scenario=="files" { create_files(); }
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
                    if scenario=="controlled" || scenario=="files" || (scenario.starts_with("delivery-") || scenario.starts_with("candidate-")) {
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
                        let artifacts = if scenario=="files" {
                            serde_json::from_str(&std::fs::read_to_string("/workspace/reported-files.json").unwrap()).unwrap()
                        } else {Vec::new()};
                        let plugin_proxy = scenario=="plugin-proxy";
                        tokio::task::spawn_blocking(move||report(&env,&task,success,artifacts,plugin_proxy)).await.unwrap();
                    }
                    let _=peer.send(tokio_tungstenite::tungstenite::Message::Text(json!({"jsonrpc":"2.0","method":"turn/completed",
                        "params":{"threadId":thread_id,"turn":{"id":turn_id,"status":"completed","items":[]}}}).to_string())).await;
                }
            }
        }
    });
}
fn report(
    env: &[(String, String)],
    task: &str,
    success: bool,
    artifacts: Vec<String>,
    plugin_proxy: bool,
) {
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
    if plugin_proxy {
        probe_plugin_proxy(&mut input, &mut output);
    }
    let args = if success {
        {
            let result = if std::path::Path::new("/workspace/report-result.json").exists() {
                serde_json::from_str::<Value>(
                    &std::fs::read_to_string("/workspace/report-result.json").unwrap(),
                )
                .unwrap()
            } else {
                json!({"answer":42})
            };
            json!({"idempotency_key":task,"result":result,"artifacts":artifacts})
        }
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

fn create_files() {
    std::fs::create_dir("/workspace/nested").unwrap();
    std::fs::write(
        "/workspace/nested/报告 🧊.txt",
        "<script>hello</script> 雪\n",
    )
    .unwrap();
    std::fs::write("/workspace/blob.bin", [0, 255, 128, 1]).unwrap();
    std::fs::write("/workspace/empty.txt", []).unwrap();
    for (name, contents) in [
        (" result.txt", "leading-space"),
        ("result.txt ", "trailing-space"),
        ("\u{2003}result.txt", "leading-unicode-space"),
        ("result.txt\u{2003}", "trailing-unicode-space"),
    ] {
        std::fs::write(format!("/workspace/{name}"), contents).unwrap();
    }
    for (name, size) in [
        ("limit.bin", 8 * 1024 * 1024),
        ("large.bin", 8 * 1024 * 1024 + 1),
    ] {
        std::fs::File::create(format!("/workspace/{name}"))
            .unwrap()
            .set_len(size)
            .unwrap();
    }
}

fn probe_plugin_proxy(input: &mut impl Write, output: &mut impl BufRead) {
    let text = std::fs::read_to_string("/provider/home/config.toml").unwrap();
    assert!(
        !text.contains("fixture-only-secret"),
        "external plugin secret must stay on platform"
    );
    let config: toml_edit::DocumentMut = text.parse().unwrap();
    let tools: Vec<_> = config["mcp_servers"]["calm"]["enabled_tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(tools.len(), 6);
    assert!(tools.contains(&"plugin.research_search") && tools.contains(&"plugin.research_detail"));
    assert!(!tools.contains(&"plugin.research_ungranted"));
    let mut rpc = |id: u64, method: &str, params: Value| {
        writeln!(
            input,
            "{}",
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
        )
        .unwrap();
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()
    };
    let listed = rpc(10, "tools/list", json!({}));
    let names: Vec<_> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"plugin.research_search") && names.contains(&"plugin.research_detail"),
        "{listed}"
    );
    assert!(!names.contains(&"plugin.research_ungranted"), "{listed}");
    let denied = rpc(
        11,
        "tools/call",
        json!({"name":"plugin.research_ungranted","arguments":{}}),
    );
    assert_eq!(denied["error"]["code"], -32601, "{denied}");
    let search = rpc(
        12,
        "tools/call",
        json!({"name":"plugin.research_search","arguments":{"query":"research"}}),
    );
    let found: Value =
        serde_json::from_str(search["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let detail = rpc(
        13,
        "tools/call",
        json!({"name":"plugin.research_detail","arguments":{"id":found["id"]}}),
    );
    let data: Value =
        serde_json::from_str(detail["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(data["answer"], 42);
    // Provider transport needs network. This fixture checks the actual command
    // policy, not the provider's own socket access or a copied sandbox.
    assert_eq!(
        config["permissions"]["neige-delivery-v1"]["network"]["enabled"].as_bool(),
        Some(false)
    );
    assert_eq!(config["web_search"].as_str(), Some("disabled"));
    std::fs::write(
        "/workspace/report-result.json",
        json!({"source":found["id"],"answer":data["answer"],"command_network_policy":false})
            .to_string(),
    )
    .unwrap();
}
