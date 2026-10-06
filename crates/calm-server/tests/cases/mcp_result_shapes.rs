//! #2087 B3 (§5): kernel tool results are snake_case objects, never bare arrays. Driven through
//! the production MCP entry point (the kernel socket's `tools/call`) and, for the listings, the
//! `neige --json` path, which must print the same JSON.

#![cfg(unix)]

use crate::support;

use calm_server::mcp_server::build_default_registry;
use calm_server::model::{CardRole, NewCard};
use calm_server::track_report::TrackReportPayload;
use calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR;
use serde_json::{Value, json};
use support::mcp::{
    CardBoot, boot_with_role, call_tool_via_socket, cli_output, neige_cli_via_socket,
};

/// `structuredContent` of one successful `tools/call`, or the JSON-RPC error.
async fn call(boot: &CardBoot, name: &str, args: Value) -> Result<Value, Value> {
    let resp = call_tool_via_socket(
        &boot.socket_path,
        &boot.raw_token,
        &boot.thread_id,
        7,
        name,
        args,
    )
    .await;
    match resp.get("error") {
        Some(error) => Err(error.clone()),
        None => Ok(resp["result"]["structuredContent"].clone()),
    }
}

async fn ok(boot: &CardBoot, name: &str, args: Value) -> Value {
    call(boot, name, args)
        .await
        .unwrap_or_else(|error| panic!("{name} refused: {error}"))
}

/// The track's report card, as production mints it with the track.
async fn add_report(boot: &CardBoot) {
    boot.repo
        .card_create(NewCard {
            track_id: boot.track_id.clone(),
            title: None,
            kind: "track-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(TrackReportPayload::initial()).unwrap(),
        })
        .await
        .unwrap();
}

/// Every object key under `value` that is not `[a-z0-9_]+`, as a path. A block's `payload` is
/// opaque (§4) and is not entered.
fn non_snake_keys(value: &Value, at: &str, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let path = format!("{at}.{key}");
                if !key
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                {
                    out.push(path.clone());
                }
                if key != "payload" {
                    non_snake_keys(child, &path, out);
                }
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                non_snake_keys(child, &format!("{at}[{index}]"), out);
            }
        }
        _ => {}
    }
}

fn assert_snake_case(tool: &str, value: &Value) {
    assert!(value.is_object(), "{tool} must return an object: {value}");
    let mut bad = Vec::new();
    non_snake_keys(value, "", &mut bad);
    assert!(
        bad.is_empty(),
        "{tool} returned non-snake_case keys {bad:?}: {value}"
    );
}

fn task_payload(key: &str) -> Value {
    json!({
        "key": key, "kind": "codex", "goal": "build it",
        "acceptance": format!("accept {key}"), "context": {"key": key},
        "cwd": format!("/{key}"), "depends_on": ["missing"], "priority": 3,
        "gate": {"steps": [{"name": "accept", "cmd": "true"}]},
        "declared_by": PLANNER_DECLARATION_AUTHOR, "ready": true
    })
}

#[tokio::test]
async fn report_tool_results_are_snake_case() {
    let boot = boot_with_role(CardRole::Planner).await;
    add_report(&boot).await;

    let read = ok(&boot, "neige_report_read", json!({})).await;
    assert_snake_case("neige_report_read", &read);
    let written = ok(
        &boot,
        "neige_report_write",
        json!({"body": "# Plan\n\nFirst draft.\n", "message": "draft"}),
    )
    .await;
    assert_snake_case("neige_report_write", &written);
    assert!(written["doc_rev"].is_u64(), "{written}");

    let committed = ok(
        &boot,
        "neige_report_commit",
        json!({"message": "declare a task", "ops": [
            {"op": "upsert", "kind": "task", "payload": task_payload("build")}
        ]}),
    )
    .await;
    assert_snake_case("neige_report_commit", &committed);
    assert!(committed["doc_rev"].is_u64(), "{committed}");

    let read = ok(&boot, "neige_report_read", json!({})).await;
    assert_snake_case("neige_report_read", &read);
    for key in ["schema_version", "doc_rev", "updated_at", "summary", "text"] {
        assert!(read.get(key).is_some(), "read lacks {key}: {read}");
    }
    let verdict = &read["task_diagnostics"][0];
    assert!(
        verdict["block_id"].is_string() && verdict["diagnostics"].is_array(),
        "the Planner's task verdicts are respelled too, not skipped: {read}"
    );

    let found = ok(&boot, "neige_report_find", json!({"path": "area/reports/"})).await;
    assert_snake_case("neige_report_find", &found);
    let row = &found["reports"][0];
    assert_eq!(row["track_id"], json!(boot.track_id.as_str()), "{found}");
    assert!(row["updated_at"].is_string(), "{found}");

    let listed = ok(&boot, "neige_track_ls", json!({"path": "area/reports/"})).await;
    assert_snake_case("neige_track_ls", &listed);
    assert_eq!(listed, found, "ls and find list the same rows");

    // `neige --json` prints the tool result as is.
    for (argv, mcp) in [
        (&["--json", "track", "ls", "area/reports/"][..], &listed),
        (&["--json", "report", "find", "area/reports/"][..], &found),
    ] {
        let (stdout, stderr, exit) =
            cli_output(&neige_cli_via_socket(&boot.socket_path, &boot.raw_token, argv).await);
        assert_eq!((exit, stderr.as_str()), (0, ""), "{argv:?}");
        let printed: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(&printed, mcp, "{argv:?}");
    }
}

/// Every read-only kernel tool, called with no arguments, answers an object or refuses; the
/// listings that need arguments are called with them. (`plugin_calendar_ls` needs the running
/// Calendar component; its `{entries}` shape is pinned by the calendar verb tests.)
#[tokio::test]
async fn no_kernel_tool_returns_a_top_level_array() {
    let boot = boot_with_role(CardRole::Planner).await;
    add_report(&boot).await;
    let mut answered = Vec::new();
    let mut descriptors = build_default_registry().descriptors();
    descriptors.sort_by(|a, b| a.name.cmp(&b.name));
    for descriptor in descriptors {
        let read_only = descriptor
            .annotations
            .as_ref()
            .and_then(|annotations| annotations["readOnlyHint"].as_bool())
            == Some(true);
        if !read_only {
            continue;
        }
        if let Ok(value) = call(&boot, &descriptor.name, json!({})).await {
            assert!(value.is_object(), "{}: {value}", descriptor.name);
            answered.push(descriptor.name);
        }
    }
    for tool in ["neige_track_ls", "neige_report_read", "neige_task_ls"] {
        assert!(
            answered.iter().any(|name| name == tool),
            "{tool} did not answer: {answered:?}"
        );
    }
    for (tool, args) in [
        ("neige_report_find", json!({"path": "area/reports/"})),
        ("neige_track_ls", json!({"path": "area/"})),
        ("neige_track_ls", json!({"path": "guide/"})),
    ] {
        let value = ok(&boot, tool, args.clone()).await;
        assert!(value.is_object(), "{tool} {args}: {value}");
    }
}

/// Every key under `value` ending in `_at`, as `(path, value)`, and every key ending in `_at_ms`
/// (the retired spelling) into `retired`. A block's `payload` is opaque (§4) and `published_at` is
/// the caller's own text (§9), so neither is entered.
fn time_keys(value: &Value, at: &str, out: &mut Vec<(String, Value)>, retired: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let path = format!("{at}.{key}");
                if key.ends_with("_at_ms") {
                    retired.push(path.clone());
                }
                if key == "payload" || key == "published_at" {
                    continue;
                }
                if key.ends_with("_at") {
                    out.push((path.clone(), child.clone()));
                }
                time_keys(child, &path, out, retired);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                time_keys(child, &format!("{at}[{index}]"), out, retired);
            }
        }
        _ => {}
    }
}

/// `YYYY-MM-DDTHH:MM:SS.mmm` and an explicit offset, as `time_format::at` writes it.
fn is_rfc3339_ms_with_offset(text: &str) -> bool {
    let millis = text
        .get(19..23)
        .is_some_and(|ms| ms.starts_with('.') && ms[1..].bytes().all(|b| b.is_ascii_digit()));
    let offset = text.get(23..).is_some_and(|offset| {
        offset.len() == 6 && (offset.starts_with('+') || offset.starts_with('-'))
    });
    millis && offset && chrono::DateTime::parse_from_rfc3339(text).is_ok()
}

/// #2087 C2 (§4): every time in a real tool result is an RFC 3339 string with an explicit offset
/// at millisecond precision, keyed `<event>_at`; an absent time is `null`.
#[tokio::test]
async fn tool_result_times_are_rfc3339_with_an_offset() {
    let boot = boot_with_role(CardRole::Planner).await;
    add_report(&boot).await;
    let track = boot.track_id.as_str();
    let body = format!("# Plan\n\nSee [this plan](neige://wave/{track}).\n");
    let mut results = Vec::new();
    for (tool, args) in [
        ("neige_report_read", json!({})),
        (
            "neige_report_write",
            json!({"body": body, "message": "draft"}),
        ),
        (
            "neige_report_commit",
            json!({"message": "declare a task", "ops": [
                {"op": "upsert", "kind": "task", "payload": {
                    "key": "build", "kind": "terminal", "command": "true", "depends_on": [],
                    "declared_by": PLANNER_DECLARATION_AUTHOR, "ready": true
                }}
            ]}),
        ),
        ("neige_report_read", json!({})),
        ("neige_report_find", json!({"path": "area/reports/"})),
        ("neige_link_ls", json!({})),
        ("neige_task_ls", json!({})),
        (
            "neige_source_capture",
            json!({"manual": {"text": "quoted text"}, "provenance": "manual",
                   "title": "Note", "published_at": "2026-10-01"}),
        ),
        ("neige_source_ls", json!({})),
        ("neige_track_log", json!({})),
        ("neige_track_ls", json!({"path": "/"})),
        ("neige_track_ls", json!({"path": "cards/"})),
        ("neige_track_ls", json!({"path": "runs/"})),
        ("neige_track_status", json!({})),
        ("neige_track_close", json!({"message": "done"})),
        ("neige_area_ls", json!({})),
        ("neige_track_status", json!({})),
    ] {
        results.push((tool, ok(&boot, tool, args).await));
    }

    let (mut seen, mut present) = (Vec::new(), Vec::new());
    for (tool, value) in &results {
        let (mut times, mut retired) = (Vec::new(), Vec::new());
        time_keys(value, "", &mut times, &mut retired);
        assert!(
            retired.is_empty(),
            "{tool} keeps `_at_ms` keys {retired:?}: {value}"
        );
        for (path, time) in times {
            present.push(format!("{tool} {}", path.rsplit('.').next().unwrap()));
            match &time {
                Value::Null => {}
                Value::String(text) if is_rfc3339_ms_with_offset(text) => {
                    seen.push(format!("{tool} {}", path.rsplit('.').next().unwrap()));
                }
                other => panic!("{tool}{path} is {other}, not an RFC 3339 time: {value}"),
            }
        }
    }
    for expected in [
        "neige_report_read updated_at",
        "neige_report_write updated_at",
        "neige_report_commit updated_at",
        "neige_report_find updated_at",
        "neige_link_ls updated_at",
        "neige_task_ls created_at",
        "neige_source_ls captured_at",
        "neige_track_log created_at",
        "neige_track_ls updated_at",
        "neige_track_status created_at",
        "neige_track_status updated_at",
        "neige_track_close closed_at",
        "neige_area_ls closed_at",
        "neige_track_status closed_at",
    ] {
        assert!(
            seen.iter().any(|key| key == expected),
            "anti-vacuity: no {expected} time in {seen:?}"
        );
    }
    // An absent time is present as `null` (§4): an open track's close, an unfinished task, a
    // runtime that never completed a turn, a directory entry.
    for expected in [
        "neige_task_ls finished_at",
        "neige_track_ls updated_at",
        "neige_track_status pinned_at",
        "neige_track_status last_turn_completed_at",
    ] {
        assert!(
            present.iter().any(|key| key == expected),
            "anti-vacuity: no {expected} key in {present:?}"
        );
    }
    let runtime = &results[13].1["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["id"] == boot.card_id.as_str())
        .expect("the caller's card")["runtime"];
    assert!(
        runtime["updated_at"].is_string()
            && runtime.get("last_turn_completed_at") == Some(&Value::Null),
        "a runtime that never completed a turn: {runtime}"
    );
    let source = &results[8].1["sources"][0];
    assert_eq!(
        source["published_at"], "2026-10-01",
        "given text is returned as given"
    );
}
