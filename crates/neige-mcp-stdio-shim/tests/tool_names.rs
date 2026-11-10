//! A model-facing alias round trip through the production stdio/UDS bridge.
#![cfg(unix)]
mod common;

use common::*;
use serde_json::json;
use tokio::io::BufReader;

#[tokio::test]
async fn constrained_names_keep_schema_and_dispatch_to_the_original_tool() {
    let (_root, path) = socket();
    let listener = listen(&path);
    let mut child = spawn_shim_with_args(
        &path,
        &[
            "--structured-content-as-text",
            "--max-tool-name-bytes",
            "41",
        ],
    );
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut stub = accept(&listener, "tool aliases").await;
    let within_limit = "n".repeat(41);
    let over_limit = "n".repeat(42);
    let schema = json!({
        "type":"object",
        "properties":{"id":{"type":"integer"},"category":{"enum":["company","ib"]}},
        "required":["id"],
        "additionalProperties":false
    });
    let list = json!({"jsonrpc":"2.0","id":1,"method":"tools/list"});
    write_stdin(&mut stdin, &format!("{list}\n")).await;
    assert_eq!(stub.read_frame("list request").await, list);
    stub.write_frame(&json!({"jsonrpc":"2.0","id":1,"result":{"tools":[
        {"name":within_limit,"inputSchema":{"type":"object"}},
        {"name":over_limit,"description":"Get report details","inputSchema":schema}
    ]}}))
    .await;
    let listed = read_stdout(&mut stdout, "listed aliases").await;
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert_eq!(tools[0]["name"], within_limit);
    let alias = tools[1]["name"].as_str().unwrap();
    assert_ne!(alias, over_limit);
    assert!(alias.len() <= 41);
    assert_eq!(tools[1]["description"], "Get report details");
    assert_eq!(tools[1]["inputSchema"], schema);

    let call = json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
        "params":{"name":alias,"arguments":{"id":123},"_meta":{"progressToken":"p"}}});
    write_stdin(&mut stdin, &format!("{call}\n")).await;
    let mut expected = call;
    expected["params"]["name"] = json!(over_limit);
    assert_eq!(stub.read_frame("canonical call").await, expected);
    stub.reply_ok(&json!(2)).await;
    assert_eq!(read_stdout(&mut stdout, "call result").await["id"], 2);
    drop(stdin);
    drop(stub);
    assert_eq!(wait_exit_code(&mut child, "alias round trip").await, 0);
}

#[tokio::test]
async fn ambiguous_discovery_cannot_retarget_an_advertised_alias() {
    let (_root, path) = socket();
    let listener = listen(&path);
    let mut child = spawn_shim_with_args(&path, &["--max-tool-name-bytes", "41"]);
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut stub = accept(&listener, "alias collision").await;
    let original = "plugin_connector_get_details_for_a_company_report";
    let other = "plugin_second_connector_get_details_for_a_company_report";
    let mut first = None;
    for (id, tools) in [(1, vec![original, other]), (2, vec![other, original])] {
        let request = json!({"jsonrpc":"2.0","id":id,"method":"tools/list"});
        write_stdin(&mut stdin, &format!("{request}\n")).await;
        stub.read_frame("list request").await;
        let tools: Vec<_> = tools
            .into_iter()
            .map(|name| json!({"name":name,"inputSchema":{"type":"object"}}))
            .collect();
        stub.write_frame(&json!({"jsonrpc":"2.0","id":id,"result":{"tools":tools}}))
            .await;
        let result = read_stdout(&mut stdout, "list reply").await;
        if id == 1 {
            let tools = &result["result"]["tools"];
            assert_ne!(tools[0]["name"], tools[1]["name"]);
            first = Some(result);
        } else {
            let first = first.as_ref().unwrap();
            assert_eq!(first["result"]["tools"][0], result["result"]["tools"][1]);
            assert_eq!(first["result"]["tools"][1], result["result"]["tools"][0]);
        }
    }
    let first = first.unwrap();
    let alias = first["result"]["tools"][0]["name"].as_str().unwrap();
    let request = json!({"jsonrpc":"2.0","id":3,"method":"tools/list"});
    write_stdin(&mut stdin, &format!("{request}\n")).await;
    stub.read_frame("colliding list request").await;
    stub.write_frame(&json!({"jsonrpc":"2.0","id":3,"result":{"tools":[
        {"name":original,"inputSchema":{"type":"object"}},
        {"name":alias,"inputSchema":{"type":"object"}}
    ]}}))
    .await;
    assert_eq!(
        read_stdout(&mut stdout, "ambiguous list refusal").await["error"]["code"],
        -32603
    );

    let call = json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":alias}});
    write_stdin(&mut stdin, &format!("{call}\n")).await;
    let mut expected = call;
    expected["params"]["name"] = json!(original);
    assert_eq!(
        stub.read_frame("previous alias still routes to its original")
            .await,
        expected
    );
    stub.reply_ok(&json!(4)).await;
    read_stdout(&mut stdout, "call result").await;
    drop(stdin);
    drop(stub);
    assert_eq!(wait_exit_code(&mut child, "alias collision").await, 0);
}
