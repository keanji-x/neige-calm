//! Fixtures: `tests/fixtures/claude_planner_stream/*.ndjson` are the stdout of the #1791 probes (claude 2.1.280, `claude-haiku-4-5`; ids in the design's evidence companion §E3: `pB_*` P-B,
//! `pB_emptycfg` P-D, `pE` P-E, `pF_sigint` P-F1, `pF_kill`/`pF_resume_sid_kill` P-F2, `pF_ctlint`
//! P-F3, `pG` P-G, `pH` P-H, `pI` P-I, `pJ` P-J, `pK` P-K, `pL*` P-L, `pS_strict` P-S1, `pS_nofail`
//! P-S2), every line except the `stream_event` ones: the probes passed `--include-partial-messages`,
//! which production never does (design §5.2), and those deltas split paths into fragments no
//! line-level redaction can see whole. The rest is redacted line by line, still one JSON value per
//! line: the probe directory became `/probe`, the home directory `/redacted-home`, the user name
//! `user`, and the account email, organization, plan, thinking signatures and API request/message ids
//! became placeholders; [`no_fixture_names_the_recording_machine`] pins it.

use serde_json::{Value, json};
use uuid::Uuid;

use super::protocol::{
    Base64Image, ControlRequestKind, ControlRequestOut, ControlResponseBody, ControlResponseOut,
    ControlResponseOutBody, ErrorSubtype, ProtocolError, Record, UserLine, UserLineContent,
    client_line_uuid, decode,
};
use super::translate::{CalmToolNames, TurnContext, TurnOutcome, TurnTranslator};
use crate::codex_appserver::{InputItem, Notification};

const FIXTURE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/claude_planner_stream"
);

fn fixture_lines(name: &str) -> Vec<String> {
    let path = format!("{FIXTURE_DIR}/{name}");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    text.lines().map(str::to_string).collect()
}

fn decode_fixture(name: &str) -> Vec<Record> {
    fixture_lines(name)
        .iter()
        .enumerate()
        .map(|(n, line)| decode(line).unwrap_or_else(|e| panic!("{name}:{}: {e}", n + 1)))
        .collect()
}

/// The client id whose line the fixture's first replay echoes, as the harness would have minted it.
fn first_replay_client_id(records: &[Record]) -> String {
    records
        .iter()
        .find_map(|record| match record {
            Record::UserReplay { uuid, .. } => Some(uuid.simple().to_string()),
            _ => None,
        })
        .expect("fixture has a replay")
}

fn context(client_id: String, input: Vec<InputItem>) -> TurnContext {
    TurnContext {
        thread_id: "thread-1".into(),
        turn_id: "turn-1".into(),
        client_id,
        input,
        cwd: "/probe/ws".into(),
        prior_total_tokens: 1_000,
    }
}

fn visible_tools() -> CalmToolNames {
    CalmToolNames::new(
        [
            "calm.report.write",
            "calm.report.read",
            "calm.user.notify",
            "plugin.dev-neige-market_market_quote",
        ]
        .map(String::from),
    )
}

/// Every record of the fixture through one translator, one millisecond apart from 1000.
fn translate_fixture(name: &str, input: Vec<InputItem>) -> Vec<Notification> {
    translator_after(name, input).1
}

fn translator_after(name: &str, input: Vec<InputItem>) -> (TurnTranslator, Vec<Notification>) {
    let records = decode_fixture(name);
    let ctx = context(first_replay_client_id(&records), input);
    let mut translator = TurnTranslator::new(ctx, visible_tools()).unwrap();
    let notifications = (0_i64..)
        .zip(records.iter())
        .flat_map(|(ms, record)| translator.translate(record, 1_000 + ms))
        .collect();
    (translator, notifications)
}

fn items<'a>(notifications: &'a [Notification], method: &str) -> Vec<&'a Value> {
    notifications
        .iter()
        .filter_map(|n| match n {
            Notification::Item { method: m, params } if m == method => Some(&params["item"]),
            _ => None,
        })
        .collect()
}

fn usage_frames(notifications: &[Notification]) -> Vec<&Value> {
    notifications
        .iter()
        .filter_map(|n| match n {
            Notification::Other { method, params } if method == "thread/tokenUsage/updated" => {
                Some(params)
            }
            _ => None,
        })
        .collect()
}

fn of_type<'a>(items: &[&'a Value], item_type: &str) -> Vec<&'a Value> {
    items
        .iter()
        .copied()
        .filter(|item| item["type"] == item_type)
        .collect()
}

#[test]
fn every_line_of_every_recorded_fixture_decodes() {
    let mut names: Vec<String> = std::fs::read_dir(FIXTURE_DIR)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".ndjson"))
        .collect();
    names.sort();
    assert_eq!(names.len(), 17, "fixture set changed: {names:?}");
    let mut lines = 0;
    let mut failures = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for name in &names {
        for (n, line) in fixture_lines(name).iter().enumerate() {
            lines += 1;
            match decode(line) {
                Ok(record) => {
                    seen.insert(variant(&record));
                }
                Err(e) => failures.push(format!("{name}:{}: {e}", n + 1)),
            }
        }
    }
    assert!(failures.is_empty(), "undecodable lines: {failures:#?}");
    assert_eq!(lines, 322);
    let all = [
        "Assistant",
        "ControlRequestIn",
        "ControlResponseIn",
        "Ignored",
        "ResultError",
        "ResultSuccess",
        "SystemInit",
        "UserReplay",
        "UserText",
        "UserToolResults",
    ];
    assert_eq!(seen.into_iter().collect::<Vec<_>>(), all);
}

fn variant(record: &Record) -> &'static str {
    match record {
        Record::SystemInit(_) => "SystemInit",
        Record::UserReplay { .. } => "UserReplay",
        Record::UserText { .. } => "UserText",
        Record::UserToolResults { .. } => "UserToolResults",
        Record::Assistant { .. } => "Assistant",
        Record::ResultSuccess(_) => "ResultSuccess",
        Record::ResultError(_) => "ResultError",
        Record::ControlResponseIn { .. } => "ControlResponseIn",
        Record::ControlRequestIn { .. } => "ControlRequestIn",
        Record::Ignored { .. } => "Ignored",
    }
}

#[test]
fn a_user_text_record_is_not_a_protocol_error_and_renders_nothing() {
    for (name, expected) in [
        ("pF_ctlint.ndjson", "[Request interrupted by user]"),
        (
            "pF_sigint.ndjson",
            "[Request interrupted by user for tool use]",
        ),
    ] {
        let lines = fixture_lines(name);
        let line = lines
            .iter()
            .find(|line| line.contains(expected) && line.contains(r#""type":"user""#))
            .unwrap_or_else(|| panic!("{name} records the interrupt text"));
        let record = decode(line).unwrap();
        let Record::UserText { text, .. } = &record else {
            panic!("{name}: {record:?}");
        };
        assert_eq!(text, expected);
        let ctx = context("0".repeat(32), Vec::new());
        let mut translator = TurnTranslator::new(ctx, visible_tools()).unwrap();
        assert!(translator.translate(&record, 1).is_empty());
    }
}

#[test]
fn a_calm_tool_keeps_its_dotted_name_from_the_visible_tool_list() {
    let notifications = translate_fixture("pB_baseline.ndjson", Vec::new());
    for method in ["item/started", "item/completed"] {
        let calls = of_type(&items(&notifications, method), "mcpToolCall");
        let tools: Vec<&Value> = calls.iter().map(|item| &item["tool"]).collect();
        assert_eq!(
            tools,
            [
                &json!("calm.report.write"),
                &json!("plugin.dev-neige-market_market_quote")
            ],
            "{method}"
        );
        assert!(calls.iter().all(|item| item["server"] == "calm"));
    }
    let completed = of_type(&items(&notifications, "item/completed"), "mcpToolCall");
    assert_eq!(completed[0]["arguments"], json!({ "text": "hi" }));
    assert_eq!(completed[0]["status"], "completed");
    assert_eq!(
        completed[0]["result"],
        json!({ "content": [{ "type": "text", "text": "ok" }] })
    );
}

#[test]
fn an_unknown_or_ambiguous_calm_tool_keeps_the_claude_name() {
    let records = decode_fixture("pB_baseline.ndjson");
    let client_id = first_replay_client_id(&records);
    // `calm.report.write` and `calm_report.write` both spell `calm_report_write`.
    let ambiguous =
        CalmToolNames::new(["calm.report.write", "calm_report.write"].map(String::from));
    for tools in [CalmToolNames::new(Vec::new()), ambiguous] {
        let mut translator =
            TurnTranslator::new(context(client_id.clone(), Vec::new()), tools).unwrap();
        let notifications: Vec<_> = records
            .iter()
            .flat_map(|r| translator.translate(r, 1))
            .collect();
        let calls = of_type(&items(&notifications, "item/started"), "mcpToolCall");
        assert_eq!(calls[0]["tool"], "mcp__calm__calm_report_write");
    }
}

#[test]
fn text_and_thinking_blocks_of_one_record_get_one_id_each() {
    let uuid = "6ca4287e-8ed8-4b41-a543-25ddaf55de4e";
    let line = json!({
        "type": "assistant", "uuid": uuid, "session_id": "s",
        "message": { "content": [
            { "type": "thinking", "thinking": "", "signature": "x" },
            { "type": "text", "text": "hello" },
        ] },
    })
    .to_string();
    let record = decode(&line).unwrap();
    let mut translator =
        TurnTranslator::new(context("0".repeat(32), Vec::new()), visible_tools()).unwrap();
    let notifications = translator.translate(&record, 7);
    let completed = items(&notifications, "item/completed");
    assert_eq!(
        completed,
        [
            &json!({ "id": format!("{uuid}:0"), "type": "reasoning", "content": [], "summary": [] }),
            &json!({ "id": format!("{uuid}:1"), "type": "agentMessage", "text": "hello" }),
        ]
    );
    let started: Vec<&Value> = items(&notifications, "item/started")
        .iter()
        .map(|i| &i["id"])
        .collect();
    assert_eq!(
        started,
        [&json!(format!("{uuid}:0")), &json!(format!("{uuid}:1"))]
    );
}

#[test]
fn every_item_a_recorded_turn_completes_has_its_own_id() {
    for name in [
        "pB_baseline.ndjson",
        "pE.ndjson",
        "pH.ndjson",
        "pS_nofail.ndjson",
    ] {
        let notifications = translate_fixture(name, Vec::new());
        let completed = items(&notifications, "item/completed");
        let ids: std::collections::BTreeSet<String> =
            completed.iter().map(|i| i["id"].to_string()).collect();
        assert_eq!(ids.len(), completed.len(), "{name}: {completed:#?}");
    }
}

#[test]
fn the_replayed_image_base64_is_never_stored() {
    let lines = fixture_lines("pI.ndjson");
    let replay: Value = lines
        .iter()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|v| v["isReplay"] == true)
        .unwrap();
    let data = replay["message"]["content"][1]["source"]["data"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        data.len() > 64,
        "the fixture's replay carries the base64 image"
    );

    let input = vec![
        InputItem::Text {
            text: "What single color fills this image? Answer with one word.".into(),
        },
        InputItem::LocalImage {
            path: "/data/attachments/red.png".into(),
        },
    ];
    let notifications = translate_fixture("pI.ndjson", input);
    let users = of_type(&items(&notifications, "item/completed"), "userMessage");
    assert_eq!(
        users,
        [&json!({
            "id": "cccccccc-0000-4000-8000-000000000000",
            "type": "userMessage",
            "clientId": "cccccccc000040008000000000000000",
            "content": [
                { "type": "text", "text": "What single color fills this image? Answer with one word." },
                { "type": "localImage", "path": "/data/attachments/red.png" },
            ],
        })]
    );
    let everything = format!("{notifications:?}");
    assert!(
        !everything.contains(&data[..64]),
        "base64 reached a notification"
    );
}

#[test]
fn a_success_without_iterations_emits_no_usage_frame() {
    // P-D: not logged in, `is_error: true`, `iterations: []`, `modelUsage: {}`.
    let records = decode_fixture("pB_emptycfg.ndjson");
    let Some(Record::ResultSuccess(success)) = records.last() else {
        panic!("P-D ends with a success result");
    };
    assert!(success.is_error && success.usage.iterations.is_empty());
    assert!(usage_frames(&translate_fixture("pB_emptycfg.ndjson", Vec::new())).is_empty());
}

#[test]
fn a_success_with_iterations_emits_one_usage_frame() {
    let notifications = translate_fixture("pB_baseline.ndjson", Vec::new());
    let frames = usage_frames(&notifications);
    // last iteration 8 + 22914 + 269 + 197; the turn 28 + 59812 + 8479 + 870 on top of the seeded 1000.
    assert_eq!(
        frames,
        [&json!({
            "threadId": "thread-1",
            "turnId": "turn-1",
            "tokenUsage": {
                "last": { "totalTokens": 23_388 },
                "total": { "totalTokens": 70_189 },
                "modelContextWindow": 200_000,
            },
        })]
    );
    // P-F3: the interrupted turn's error result carries none; the queued line's success carries one.
    assert_eq!(
        usage_frames(&translate_fixture("pF_ctlint.ndjson", Vec::new())).len(),
        1
    );
}

#[test]
fn claude_native_tools_map_to_the_rendered_codex_items() {
    let notifications = translate_fixture("pE.ndjson", Vec::new());
    let completed = items(&notifications, "item/completed");

    let commands = of_type(&completed, "commandExecution");
    let summary: Vec<(Value, Value, Value)> = commands
        .iter()
        .map(|c| {
            (
                c["command"].clone(),
                c["status"].clone(),
                c["exitCode"].clone(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (json!("echo one; exit 3"), json!("failed"), json!(3)),
            (json!("echo two"), json!("completed"), json!(0)),
            (json!("echo three"), json!("completed"), json!(0)),
        ]
    );
    assert_eq!(commands[0]["aggregatedOutput"], "Exit code 3\none");
    assert_eq!(commands[0]["cwd"], "/probe/ws");

    let changes: Vec<&Value> = of_type(&completed, "fileChange")
        .iter()
        .map(|f| &f["changes"])
        .collect();
    assert_eq!(
        changes,
        [
            &json!([{ "path": "/probe/ws/notes.txt", "kind": { "type": "add" }, "diff": "+alpha\n" }]),
            &json!([{
                "path": "/probe/ws/notes.txt",
                "kind": { "type": "update" },
                "diff": "@@ -1,1 +1,1 @@\n-alpha\n\\ No newline at end of file\n+beta\n\\ No newline at end of file\n",
            }]),
        ]
    );

    let dynamic = of_type(&completed, "dynamicToolCall");
    assert_eq!(dynamic.len(), 1);
    assert_eq!(dynamic[0]["tool"], "Read");
    assert_eq!(dynamic[0]["status"], "completed");
    for tool in ["commandExecution", "fileChange", "dynamicToolCall"] {
        assert!(
            of_type(&completed, tool)
                .iter()
                .all(|item| item["durationMs"].is_i64())
        );
    }
}

#[test]
fn tool_search_and_foreign_replays_render_nothing() {
    let notifications = translate_fixture("pB_baseline.ndjson", Vec::new());
    let started = items(&notifications, "item/started");
    assert!(
        started.iter().all(|item| item["tool"] != "ToolSearch"),
        "{started:#?}"
    );
    // The `<local-command-stdout>` replay of `set_model` is not the line we wrote.
    let users = of_type(&items(&notifications, "item/completed"), "userMessage");
    assert_eq!(users.len(), 1);
    assert_eq!(users[0]["id"], "11111111-2222-4333-8444-555555555555");
}

#[test]
fn results_and_control_responses_decode_to_their_recorded_shapes() {
    let strict = decode_fixture("pS_strict.ndjson");
    let [Record::ResultError(error)] = strict.as_slice() else {
        panic!("{strict:?}");
    };
    assert_eq!(error.subtype, ErrorSubtype::ErrorDuringExecution);
    assert_eq!(error.terminal_reason, None);
    assert!(error.errors[0].starts_with("Sandbox required but unavailable"));

    let interrupted = decode_fixture("pF_ctlint.ndjson");
    let aborted = interrupted.iter().find_map(|r| match r {
        Record::ResultError(e) => e.terminal_reason.as_deref(),
        _ => None,
    });
    assert_eq!(aborted, Some("aborted_streaming"));
    let still_queued = interrupted.iter().find_map(|r| match r {
        Record::ControlResponseIn {
            response:
                ControlResponseBody::Success {
                    request_id,
                    response,
                },
        } if request_id == "int-1" => response.clone(),
        _ => None,
    });
    assert!(still_queued.unwrap()["still_queued"].is_array());

    let set_model = decode_fixture("pB_baseline.ndjson")
        .into_iter()
        .find_map(|r| match r {
            Record::ControlResponseIn {
                response:
                    ControlResponseBody::Success {
                        request_id,
                        response,
                    },
            } if request_id == "model-1" => Some(response),
            _ => None,
        });
    assert_eq!(set_model, Some(None));
}

#[test]
fn a_line_without_a_type_or_with_a_broken_known_shape_is_a_protocol_error() {
    assert!(matches!(decode("not json"), Err(ProtocolError::NotJson(_))));
    assert!(matches!(
        decode(r#"{"subtype":"init"}"#),
        Err(ProtocolError::NoType)
    ));
    assert!(matches!(
        decode(r#"{"type":"assistant","uuid":"6ca4287e-8ed8-4b41-a543-25ddaf55de4e"}"#),
        Err(ProtocolError::Shape { .. })
    ));
    assert!(matches!(
        decode(r#"{"type":"something_new","x":1}"#),
        Ok(Record::Ignored { kind }) if kind == "something_new"
    ));
}

#[test]
fn client_ids_go_out_as_the_dashed_uuid_of_the_same_bits() {
    let uuid = client_line_uuid("cccccccc000040008000000000000000").unwrap();
    assert_eq!(uuid.to_string(), "cccccccc-0000-4000-8000-000000000000");
    for bad in ["cccccccc-0000-4000-8000-000000000000", "xyz", ""] {
        assert!(
            matches!(client_line_uuid(bad), Err(ProtocolError::ClientId(_))),
            "{bad}"
        );
    }
    assert!(TurnTranslator::new(context("bad".into(), Vec::new()), visible_tools()).is_err());
}

#[test]
fn stdin_lines_serialize_to_the_probed_shapes() {
    let session = Uuid::parse_str("3ce1da0a-1e9c-4fa7-834d-0df281ece0f4").unwrap();
    let uuid = Uuid::parse_str("cccccccc-0000-4000-8000-000000000000").unwrap();
    let line = UserLine::new(
        session,
        uuid,
        vec![
            UserLineContent::Text {
                text: "What color?".into(),
            },
            UserLineContent::Image {
                source: Base64Image::new("image/png", "iVBO"),
            },
        ],
    );
    assert_eq!(
        serde_json::to_value(&line).unwrap(),
        json!({
            "type": "user",
            "message": { "role": "user", "content": [
                { "type": "text", "text": "What color?" },
                { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "iVBO" } },
            ] },
            "parent_tool_use_id": null,
            "session_id": "3ce1da0a-1e9c-4fa7-834d-0df281ece0f4",
            "uuid": "cccccccc-0000-4000-8000-000000000000",
        })
    );
    assert_eq!(
        serde_json::to_value(ControlRequestOut::new(
            "int-1",
            ControlRequestKind::Interrupt
        ))
        .unwrap(),
        json!({ "type": "control_request", "request_id": "int-1", "request": { "subtype": "interrupt" } })
    );
    let deny = ControlResponseOut::new(ControlResponseOutBody::Success {
        request_id: "r1".into(),
        response: json!({ "behavior": "deny", "message": "no" }),
    });
    assert_eq!(
        serde_json::to_value(deny).unwrap(),
        json!({ "type": "control_response", "response": {
            "subtype": "success", "request_id": "r1", "response": { "behavior": "deny", "message": "no" },
        } })
    );
    let refuse = ControlResponseOut::new(ControlResponseOutBody::Error {
        request_id: "r2".into(),
        error: "unsupported".into(),
    });
    assert_eq!(
        serde_json::to_value(refuse).unwrap(),
        json!({ "type": "control_response", "response": {
            "subtype": "error", "request_id": "r2", "error": "unsupported",
        } })
    );
}

#[test]
fn turn_frames_carry_the_issued_turn_id() {
    let translator =
        TurnTranslator::new(context("0".repeat(32), Vec::new()), visible_tools()).unwrap();
    let Notification::TurnStarted { thread_id, turn } = translator.turn_started() else {
        panic!("turn_started");
    };
    assert_eq!(
        (thread_id.as_str(), &turn["id"]),
        ("thread-1", &json!("turn-1"))
    );
    let failed = TurnOutcome::Failed {
        message: "Not logged in".into(),
    };
    let cases = [
        (
            TurnOutcome::Completed,
            json!({ "id": "turn-1", "status": "completed", "error": null }),
        ),
        (
            TurnOutcome::Interrupted,
            json!({ "id": "turn-1", "status": "interrupted", "error": null }),
        ),
        (
            failed,
            json!({ "id": "turn-1", "status": "failed", "error": { "message": "Not logged in" } }),
        ),
    ];
    for (outcome, expected) in cases {
        let Notification::TurnCompleted { turn, .. } = translator.turn_completed(&outcome) else {
            panic!("turn_completed");
        };
        assert_eq!(turn, expected);
    }
}

#[test]
fn no_fixture_names_the_recording_machine() {
    let needles = ["kenji", "data2", "/tmp/claude-", "852c3533", "pivot"];
    let mut hits = Vec::new();
    for entry in std::fs::read_dir(FIXTURE_DIR).unwrap() {
        let path = entry.unwrap().path();
        let bytes = std::fs::read(&path).unwrap();
        let text = String::from_utf8_lossy(&bytes).to_lowercase();
        for needle in needles {
            if text.contains(needle) {
                hits.push(format!("{}: {needle}", path.display()));
            }
        }
    }
    assert!(hits.is_empty(), "{hits:#?}");
}

#[test]
fn an_error_result_with_iterations_emits_a_usage_frame() {
    // P-F1: SIGINT mid-tool ends with `error_during_execution` / `aborted_tools` and one iteration.
    let frames = translate_fixture("pF_sigint.ndjson", Vec::new());
    assert_eq!(
        usage_frames(&frames),
        [&json!({
            "threadId": "thread-1",
            "turnId": "turn-1",
            "tokenUsage": {
                "last": { "totalTokens": 12_657 },
                "total": { "totalTokens": 13_657 },
                "modelContextWindow": 200_000,
            },
        })]
    );
    // P-S1: the sandbox refusal has no iteration and no frame.
    let mut translator =
        TurnTranslator::new(context("0".repeat(32), Vec::new()), visible_tools()).unwrap();
    let strict: Vec<Notification> = decode_fixture("pS_strict.ndjson")
        .iter()
        .flat_map(|record| translator.translate(record, 1))
        .collect();
    assert!(usage_frames(&strict).is_empty());
}

#[test]
fn a_known_type_without_its_subtype_is_a_protocol_error() {
    for line in [
        r#"{"type":"result","is_error":true}"#,
        r#"{"type":"system","session_id":"s"}"#,
        r#"{"type":"control_response","response":{"request_id":"r1"}}"#,
    ] {
        assert!(
            matches!(decode(line), Err(ProtocolError::NoSubtype { .. })),
            "{line}: {:?}",
            decode(line)
        );
    }
}

#[test]
fn closing_a_turn_fails_every_tool_item_left_without_a_result() {
    for (name, command) in [
        ("pF_kill.ndjson", "sleep 20; echo finished-sleep"),
        ("pK.ndjson", "sleep 300; echo done"),
    ] {
        let (mut translator, notifications) = translator_after(name, Vec::new());
        let started = of_type(&items(&notifications, "item/started"), "commandExecution");
        assert_eq!(started.len(), 1, "{name}");
        let started_at = notifications
            .iter()
            .find_map(|n| match n {
                Notification::Item { method, params }
                    if method == "item/started" && params["item"]["type"] == "commandExecution" =>
                {
                    params["startedAtMs"].as_i64()
                }
                _ => None,
            })
            .unwrap();
        let closed = translator.close_open(90_000);
        let completed = items(&closed, "item/completed");
        assert_eq!(completed.len(), 1, "{name}: {closed:?}");
        assert_eq!(completed[0]["id"], started[0]["id"]);
        assert_eq!(completed[0]["command"], command);
        assert_eq!(completed[0]["status"], "failed");
        assert_eq!(completed[0]["durationMs"], 90_000 - started_at);
        assert!(
            translator.close_open(90_001).is_empty(),
            "{name}: closed twice"
        );
    }
    // Settled tools and `ToolSearch` leave nothing open.
    let (mut settled, _) = translator_after("pB_baseline.ndjson", Vec::new());
    assert!(settled.close_open(90_000).is_empty());
}
