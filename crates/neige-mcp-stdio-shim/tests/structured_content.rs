//! Model-visible results through the real stdio/UDS bridge.
#![cfg(unix)]
mod common;

use common::*;
use serde_json::{Value, json};
use tokio::io::BufReader;

#[tokio::test]
async fn structured_text_exposes_report_and_terminal_state_without_changing_other_replies() {
    let (_root, path) = socket();
    let listener = listen(&path);
    let mut child = spawn_shim_with_args(&path, &["--structured-content-as-text"]);
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut stub = accept(&listener, "result test").await;
    write_stdin(&mut stdin, &initialize_line(1)).await;
    assert_replayed_initialize(&stub.read_frame("initialize").await, 1);
    stub.reply_ok(&json!(1)).await;
    assert_eq!(read_stdout(&mut stdout, "initialize result").await["id"], 1);
    for (id, method, state) in [
        (
            2,
            "tools/call",
            json!({"doc_rev":7,"blocks":[{"id":"paragraph-1","text":"\u{5b8c}\u{6574}\u{62a5}\u{544a}"}]}),
        ),
        (
            3,
            "tools/call",
            json!({"terminal":"term-1","screen":"\u{5b8c}\u{6574}\u{7ec8}\u{7aef}\u{5185}\u{5bb9}"}),
        ),
        (4, "other/method", json!({"must":"remain unchanged"})),
    ] {
        let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":{}});
        write_stdin(&mut stdin, &format!("{request}\n")).await;
        assert_eq!(stub.read_frame("request").await, request);
        let response = json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":"summary only"},{"type":"image","data":"AA==","mimeType":"image/png"}],"structuredContent":state,"isError":false}});
        stub.write_frame(&response).await;
        let actual = read_stdout(&mut stdout, "result").await;
        if method == "tools/call" {
            assert_eq!(
                serde_json::from_str::<Value>(
                    actual["result"]["content"][0]["text"].as_str().unwrap()
                )
                .expect("the model must receive complete JSON, not the summary"),
                state
            );
            assert_eq!(actual["result"]["structuredContent"], state);
            assert_eq!(
                &actual["result"]["content"].as_array().unwrap()[1..],
                response["result"]["content"].as_array().unwrap()
            );
            assert_eq!(actual["id"], response["id"]);
            assert_eq!(actual["result"]["isError"], false);
        } else {
            assert_eq!(actual, response);
        }
    }
    drop(stdin);
    drop(stub);
    assert_eq!(wait_exit_code(&mut child, "completed result test").await, 0);
}

#[tokio::test]
async fn planner_diagnostics_regression_result_projection_modes() {
    for args in [&[][..], &["--structured-content-as-text"][..]] {
        let (_root, path) = socket();
        let listener = listen(&path);
        let mut child = spawn_shim_with_args(&path, args);
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut stub = accept(&listener, "unchanged results").await;
        for (id, result) in [
            (
                2,
                json!({"content":[{"type":"text","text":"summary"}],"structuredContent":{"blocks":[1]}}),
            ),
            (
                3,
                json!({"content":[{"type":"text","text":"failure detail"}],"structuredContent":{"error":"detail"},"isError":true}),
            ),
            (
                4,
                json!({"content":[{"type":"text","text":"ordinary text"}]}),
            ),
            (
                5,
                json!({"content":[{"type":"text","text":"{\"n\":1}"}],"structuredContent":{"n":1}}),
            ),
        ] {
            write_stdin(&mut stdin, &tools_call_line(id)).await;
            assert_eq!(stub.read_frame("tools/call").await["id"], id);
            let response = json!({"jsonrpc":"2.0","id":id,"result":result});
            stub.write_frame(&response).await;
            let actual = read_stdout(&mut stdout, "tool reply").await;
            if args.is_empty() || id != 2 {
                assert_eq!(actual, response);
            } else {
                assert_eq!(actual["result"]["content"][0]["text"], "{\"blocks\":[1]}");
            }
        }
        write_stdin(&mut stdin, &tools_call_line(6)).await;
        stub.read_frame("error request").await;
        let error = json!({"jsonrpc":"2.0","id":6,"error":{"code":-32602,"message":"invalid"}});
        stub.write_frame(&error).await;
        let actual = read_stdout(&mut stdout, "RPC error").await;
        assert_eq!(actual["result"]["isError"], true);
        assert_eq!(actual["result"]["structuredContent"], error["error"]);
        let unsolicited = json!({"jsonrpc":"2.0","id":99,"result":{"content":[{"type":"text","text":"unchanged"}],"structuredContent":{"n":1}}});
        stub.write_frame(&unsolicited).await;
        assert_eq!(
            read_stdout(&mut stdout, "unsolicited reply").await,
            unsolicited
        );
        drop(stdin);
        drop(stub);
        assert_eq!(wait_exit_code(&mut child, "unchanged results").await, 0);
    }
}

#[tokio::test]
async fn structured_text_projection_survives_the_kernel_reconnect_handshake() {
    for flag in [
        "--structured-content-as-text",
        "--deduplicate-equivalent-content",
    ] {
        let (_root, path) = socket();
        let listener = listen(&path);
        let mut child = spawn_shim_with_args(&path, &[flag]);
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut stderr = BufReader::new(child.stderr.take().unwrap());
        let mut stub = accept(&listener, "first connection").await;
        write_stdin(&mut stdin, &initialize_line(1)).await;
        assert_replayed_initialize(&stub.read_frame("initialize").await, 1);
        stub.reply_ok(&json!(1)).await;
        read_stdout(&mut stdout, "initialize reply").await;
        drop(stub);
        assert!(
            read_stderr_line(&mut stderr)
                .await
                .contains("connection to kernel lost")
        );
        write_stdin(&mut stdin, &tools_call_line(2)).await;
        let mut stub = accept(&listener, "reconnect").await;
        assert_replayed_initialize(&stub.read_frame("replayed initialize").await, 1);
        stub.reply_ok(&json!(1)).await;
        assert_eq!(stub.read_frame("tools/call after handshake").await["id"], 2);
        let text = if flag == "--structured-content-as-text" {
            "summary"
        } else {
            "{\"blocks\":[\"restored\"]}"
        };
        stub.write_frame(&json!({"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":text}],"structuredContent":{"blocks":["restored"]}}})).await;
        let actual = read_stdout(&mut stdout, "projected reply").await;
        assert_eq!(
            actual["result"].get("structuredContent").is_some(),
            flag == "--structured-content-as-text"
        );
        assert_eq!(
            actual["result"]["content"][0]["text"],
            "{\"blocks\":[\"restored\"]}"
        );
        drop(stdin);
        drop(stub);
        assert_eq!(wait_exit_code(&mut child, "reconnected result").await, 0);
    }
}

#[tokio::test]
async fn deduplicate_equivalent_content_preserves_information_and_correlation() {
    let (_root, path) = socket();
    let listener = listen(&path);
    let mut child = spawn_shim_with_args(&path, &["--deduplicate-equivalent-content"]);
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut stub = accept(&listener, "deduplicate results").await;
    let content = json!([
        {"type":"text","text":"summary"},
        {"type":"text","text":" {\"z\": [1, true], \"a\": \"\\u5b8c成\"} "},
        {"type":"image","data":"AA==","mimeType":"image/png"},
        {"type":"resource","resource":{"uri":"test:///report","text":"details"}}
    ]);
    let equivalent = json!({"content":content,"structuredContent":{"a":"完成","z":[1,true]},"isError":false,"_meta":{"kept":true}});
    let mut failure = equivalent.clone();
    failure["isError"] = json!(true);
    for (id, method, result, deduplicated) in [
        (1, "tools/call", equivalent.clone(), true),
        (2, "other/method", equivalent.clone(), false),
        (3, "tools/call", failure, false),
        (
            4,
            "tools/call",
            json!({"content":[{"type":"text","text":"summary"}],"structuredContent":{"a":1}}),
            false,
        ),
        (
            5,
            "tools/call",
            json!({"content":[{"type":"text","text":"{invalid"}],"structuredContent":{"a":1}}),
            false,
        ),
        (
            6,
            "tools/call",
            json!({"content":[{"type":"text","text":"{\"a\":2}"}],"structuredContent":{"a":1}}),
            false,
        ),
        (7, "tools/call", json!({"structuredContent":{"a":1}}), false),
        (8, "tools/call", json!({"content":content}), false),
        (
            9,
            "tools/call",
            json!({"content":[{"type":"text","text":"[1]"}],"structuredContent":[1]}),
            false,
        ),
    ] {
        write_stdin(&mut stdin, &request_line(id, method)).await;
        assert_eq!(stub.read_frame("request").await["id"], id);
        let mut expected = json!({"jsonrpc":"2.0","id":id,"result":result});
        stub.write_frame(&expected).await;
        if deduplicated {
            expected["result"]
                .as_object_mut()
                .unwrap()
                .remove("structuredContent");
        }
        assert_eq!(
            read_stdout(&mut stdout, "reply").await,
            expected,
            "case {id}"
        );
    }
    let unsolicited = json!({"jsonrpc":"2.0","id":99,"result":equivalent});
    stub.write_frame(&unsolicited).await;
    assert_eq!(read_stdout(&mut stdout, "unsolicited").await, unsolicited);
    write_stdin(&mut stdin, &tools_call_line(10)).await;
    stub.read_frame("RPC error request").await;
    let error = json!({"jsonrpc":"2.0","id":10,"error":{"code":-32603,"message":"failure"}});
    stub.write_frame(&error).await;
    let actual = read_stdout(&mut stdout, "RPC error").await;
    assert_eq!(actual["jsonrpc"], "2.0");
    assert_eq!(actual["id"], 10);
    assert!(actual.get("error").is_none());
    assert_eq!(actual["result"]["isError"], true);
    assert_eq!(actual["result"]["structuredContent"], error["error"]);
    assert_eq!(actual["result"]["content"][0]["type"], "text");
    let text = actual["result"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(serde_json::from_str::<Value>(text).unwrap(), error["error"]);
    drop(stdin);
    drop(stub);
    assert_eq!(wait_exit_code(&mut child, "deduplicate results").await, 0);
}

#[tokio::test]
async fn planner_diagnostics_regression_tool_error_reaches_model_with_code_and_data() {
    for args in [&[][..], &["--structured-content-as-text"][..]] {
        let (_root, path) = socket();
        let listener = listen(&path);
        let mut child = spawn_shim_with_args(&path, args);
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut stub = accept(&listener, "planner error").await;
        for (id, method) in [(2, "tools/call"), (3, "other/method")] {
            let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":{}});
            write_stdin(&mut stdin, &format!("{request}\n")).await;
            assert_eq!(stub.read_frame("request").await, request);
            let error = json!({"code":-32602,"message":"neige_report_commit: invalid task", "data":{"task_diagnostics":[{"code":"invalid_payload","reason":"goal: 2049 chars","block_id":"b_test"}],"unicode":"中文"}});
            let response = json!({"jsonrpc":"2.0","id":id,"error":error});
            stub.write_frame(&response).await;
            let actual = read_stdout(&mut stdout, "error reply").await;
            if method != "tools/call" {
                assert_eq!(actual, response);
            } else {
                assert_eq!(actual["id"], id);
                assert_eq!(actual["result"]["isError"], true);
                assert_eq!(actual["result"]["content"][0]["type"], "text");
                let text = actual["result"]["content"][0]["text"]
                    .as_str()
                    .expect("model-visible error text");
                assert_eq!(serde_json::from_str::<Value>(text).unwrap(), error);
                assert_eq!(actual["result"]["structuredContent"], error);
                assert!(actual.get("error").is_none());
            }
        }
        drop(stdin);
        drop(stub);
        assert_eq!(wait_exit_code(&mut child, "planner error").await, 0);
    }
}
