//! A Claude rewind's pure halves (#1923): the anchor the translator records, how it is read back,
//! and the argv of the cut.

use std::path::Path;

use serde_json::{Value, json};
use uuid::Uuid;

use super::protocol::decode;
use super::rewind::{anchor, truncation};
use super::spawn::{ResumeTruncation, SessionStart, argv, truncation_check_argv};
use super::translate::{ToolNames, TurnContext, TurnOutcome, TurnTranslator};
use crate::db::TranscriptRow;
use crate::harness::planner_event::PlannerEventKind;
use crate::planner_model::TurnModelSelection;

const CLIENT: &str = "0123456789abcdef0123456789abcdef";
const THREAD: &str = "5d0b2694-9ccc-4543-ab84-611aa4287dbe";

fn uuid(n: u8) -> Uuid {
    Uuid::from_bytes([n; 16])
}

fn completed_turn(translator: &TurnTranslator) -> Value {
    match translator.turn_completed(&TurnOutcome::Completed, 0).kind {
        PlannerEventKind::TurnCompleted { turn } => turn,
        other => panic!("not a completion: {other:?}"),
    }
}

fn translator() -> TurnTranslator {
    TurnTranslator::new(
        TurnContext {
            thread_id: "thread-1".into(),
            turn_id: "turn-1".into(),
            client_id: CLIENT.into(),
            input: Vec::new(),
            cwd: "/ws".into(),
            prior_total_tokens: 0,
        },
        ToolNames::new(crate::mcp_server::wiring::MCP_SERVER_KEY, Vec::new()),
    )
    .unwrap()
}

/// Feed stdout lines through the translator's line entry point, as the driver does.
fn feed(translator: &mut TurnTranslator, lines: &[String]) {
    for (n, line) in lines.iter().enumerate() {
        let record = decode(line).unwrap_or_else(|e| panic!("line {}: {e}", n + 1));
        translator.translate_line(line, &record, n as i64);
    }
}

/// The last chain record wins even when it makes no item; non-chain lines with a uuid of their
/// own (system, stream frames, the result) do not move it.
#[test]
fn last_record_uuid_is_the_last_chain_record_including_one_that_makes_no_item() {
    let mut translator = translator();
    assert!(
        completed_turn(&translator).get("lastRecordUuid").is_none(),
        "no chain record seen, nothing to name"
    );
    let line = |value: Value| value.to_string();
    let silent = line(json!({"type": "assistant", "uuid": uuid(2).to_string(),
        "message": {"id": "msg_1", "content": [{"type": "server_tool_use"}]}}));
    assert!(
        translator
            .translate(&decode(&silent).unwrap(), 0)
            .is_empty(),
        "premise: this record produces no item"
    );
    feed(
        &mut translator,
        &[
            line(json!({"type": "assistant", "uuid": uuid(1).to_string(),
                "message": {"id": "msg_1", "content": [{"type": "text", "text": "hi"}]}})),
            silent,
            line(json!({"type": "system", "subtype": "status", "uuid": uuid(7).to_string()})),
        ],
    );
    assert_eq!(
        completed_turn(&translator)["lastRecordUuid"],
        json!(uuid(2).to_string())
    );
    feed(
        &mut translator,
        &[line(json!({"type": "user", "uuid": uuid(3).to_string(),
            "message": {"role": "user", "content": "[Request interrupted by user]"}}))],
    );
    assert_eq!(
        completed_turn(&translator)["lastRecordUuid"],
        json!(uuid(3).to_string())
    );
}

/// A real `--include-partial-messages` capture (claude 2.1.280, #1998's fixture): every
/// `stream_event` carries a uuid, and the last lines are stream frames and the result. The anchor
/// is the last `assistant` record, the last line the CLI wrote to its session chain.
#[test]
fn a_streamed_turn_anchors_at_its_last_chain_record_not_its_last_stream_frame() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../provider/tests/fixtures/claude_planner_stream/p1923_stream.ndjson"
    ))
    .unwrap();
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    let last: Value = serde_json::from_str(lines.last().unwrap()).unwrap();
    assert_eq!(
        last["type"], "result",
        "premise: the capture ends past its chain"
    );
    let mut translator = translator();
    feed(&mut translator, &lines);
    assert_eq!(
        completed_turn(&translator)["lastRecordUuid"],
        json!("06a874eb-9630-47d5-81a9-ea33dab877db")
    );
}

fn row(
    id: i64,
    item_uuid: Option<&str>,
    item_type: Option<&str>,
    method: &str,
    params: Value,
) -> TranscriptRow {
    TranscriptRow {
        id,
        turn_id: Some("turn-a".into()),
        item_uuid: item_uuid.map(str::to_string),
        item_type: item_type.map(str::to_string),
        method: method.into(),
        params: params.to_string(),
        input_segments: None,
    }
}

#[test]
fn the_anchor_is_the_outcome_record_and_falls_back_to_the_last_item_record() {
    let message = |id: i64, record: Uuid| {
        row(
            id,
            Some(&format!("{record}:0")),
            Some("agentMessage"),
            "item/completed",
            json!({}),
        )
    };
    let items = vec![
        message(1, uuid(1)),
        row(
            2,
            Some("toolu_1"),
            Some("commandExecution"),
            "item/completed",
            json!({}),
        ),
        message(3, uuid(3)),
    ];
    assert_eq!(
        anchor(&items),
        Some(uuid(3)),
        "no outcome anchor: the last message record"
    );
    let mut with_outcome = items.clone();
    with_outcome.push(row(
        4,
        None,
        None,
        "turn/completed",
        json!({"id": "turn-a", "status": "completed", "lastRecordUuid": uuid(9).to_string()}),
    ));
    assert_eq!(anchor(&with_outcome), Some(uuid(9)));
    assert_eq!(anchor(&items[1..2]), None, "a tool id is not a record uuid");
    let streamed = [row(
        5,
        Some("msg_01AbC:0"),
        Some("agentMessage"),
        "item/completed",
        json!({}),
    )];
    assert_eq!(
        anchor(&streamed),
        None,
        "an API message id is no chain entry"
    );
    assert!(truncation(Some(&items[1..2]), Some(CLIENT)).is_err());
    assert!(
        truncation(None, Some(CLIENT)).is_err(),
        "a first turn has no anchor"
    );
    assert_eq!(
        truncation(Some(&with_outcome), Some(CLIENT)).unwrap(),
        ResumeTruncation {
            at: uuid(9),
            drops_turn: Uuid::parse_str(CLIENT).unwrap(),
        }
    );
}

fn strings(args: Vec<std::ffi::OsString>) -> Vec<String> {
    args.into_iter()
        .map(|arg| arg.into_string().expect("utf-8"))
        .collect()
}

#[test]
fn the_dry_run_and_the_next_spawn_carry_the_cut() {
    let thread = Uuid::parse_str(THREAD).unwrap();
    let cut = ResumeTruncation {
        at: uuid(1),
        drops_turn: uuid(2),
    };
    let at = format!("--resume-session-at={}", uuid(1));
    let drops = format!("--resume-drops-turn={}", uuid(2));
    assert_eq!(
        strings(truncation_check_argv(thread, &cut)),
        [
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--resume",
            THREAD,
            &at,
            &drops,
            "--setting-sources",
            "project",
            "--disable-slash-commands",
            "--strict-mcp-config",
        ]
    );
    let spawn = |start, cut: Option<&ResumeTruncation>| {
        argv(
            thread,
            start,
            cut,
            &TurnModelSelection::inherit(),
            crate::planner_permission_mode::PlannerPermissionMode::Never,
            Path::new("/ws"),
            Path::new("/opt/shim"),
            Path::new("/data/x.md"),
        )
        .map(strings)
    };
    let mut expected = spawn(SessionStart::Resume, None).unwrap();
    let after_thread = expected.iter().position(|arg| arg == THREAD).unwrap() + 1;
    assert_eq!(expected[after_thread - 2], "--resume");
    expected.splice(after_thread..after_thread, [at, drops]);
    assert_eq!(
        spawn(SessionStart::Resume, Some(&cut)).unwrap(),
        expected,
        "right after `--resume <thread>`, nothing else changes"
    );
    assert!(
        spawn(SessionStart::New, Some(&cut)).is_err(),
        "a session that does not exist yet has nothing to cut"
    );
}
