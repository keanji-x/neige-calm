//! `calm.source.capture` / `calm.source.list` through the real tool registry. The transient ring
//! is seeded directly here (`ctx.plugin_results`); the transport recording point is covered by
//! `mcp_source_capture_e2e.rs`.

#![cfg(unix)]

use calm_server::mcp_server::tools::source::{TOOL_SOURCE_CAPTURE, TOOL_SOURCE_LIST};
use calm_server::mcp_server::tools::track_state::TOOL_TRACK_STATE;
use calm_server::plugin_host::mcp::{CallToolResult, ContentBlock, RpcError};
use calm_server::plugin_host::{Manifest, PluginHost, PluginRegistry};
use calm_server::plugin_results::{MAX_ARGS_BYTES, MAX_TEXT_BYTES, sha256_hex};
use calm_server::report_sources::{MAX_BODY_BYTES, MAX_QUOTES_PER_SOURCE, MAX_SOURCES_PER_TRACK};
use serde_json::{Value, json};
use std::sync::Arc;

use crate::mcp_track_report::{
    Boot, assistant_identity, boot, call_tool, planner_identity, worker_identity,
};

const PLUGIN_ID: &str = "dev.echo";
const TOOL_NAME: &str = "do.thing";
const REGISTRY_NAME: &str = "plugin.dev.echo_do.thing";
const SANITIZED_NAME: &str = "plugin_dev_echo_do_thing";
/// The name the model's tool list actually shows for [`REGISTRY_NAME`].
const QUALIFIED_NAME: &str = "mcp__calm__plugin_dev_echo_do_thing";
const COLLIDING_PLUGIN_ID: &str = "dev";
const COLLIDING_TOOL_NAME: &str = "echo.do.thing";

fn text_block(text: &str) -> ContentBlock {
    ContentBlock {
        kind: "text".into(),
        text: Some(text.into()),
        extra: Default::default(),
    }
}

fn ok_result(parts: &[&str]) -> CallToolResult {
    CallToolResult {
        content: parts.iter().map(|p| text_block(p)).collect(),
        is_error: None,
        meta: None,
        structured_content: None,
    }
}

fn record(boot: &Boot, plugin_id: &str, tool: &str, args: &Value, result: &CallToolResult) {
    boot.ctx
        .plugin_results
        .record(boot.track_id.as_str(), plugin_id, tool, args, result);
}

/// A plugin host whose registry exposes [`REGISTRY_NAME`], a second `dev.echo` tool and the
/// colliding plugin, running nothing: the track-visible universe the refusal consults.
const OTHER_TOOL_NAME: &str = "other.thing";
const OTHER_REGISTRY_NAME: &str = "plugin.dev.echo_other.thing";

fn install_registry(boot: &Boot) {
    let manifest = |id: &str, tools: &[&str]| {
        let tools: Vec<Value> = tools.iter().map(|t| json!({ "name": t })).collect();
        Manifest::parse(
            &json!({
                "manifest_version": 1, "id": id, "version": "0.1.0", "min_kernel_version": "0.0.1",
                "display_name": id, "entrypoint": { "command": "bin/stub" },
                "exposes_tools": tools, "permissions": {}
            })
            .to_string(),
        )
        .expect("fixture manifest parses")
    };
    let registry = PluginRegistry::from_manifests([
        (manifest(PLUGIN_ID, &[TOOL_NAME, OTHER_TOOL_NAME]), None),
        (manifest(COLLIDING_PLUGIN_ID, &[COLLIDING_TOOL_NAME]), None),
    ]);
    let plugins = std::env::temp_dir().join("neige-test-source-capture-plugins");
    let host = PluginHost::new_full(
        Arc::new(registry),
        boot.repo.clone(),
        plugins.clone(),
        plugins.join("data"),
        Vec::new(),
        boot.ctx.events.clone(),
        boot.ctx.write.clone(),
    );
    assert!(
        boot.ctx.plugin_host.set(Arc::new(host)).is_ok(),
        "plugin host installed once"
    );
}

async fn capture(boot: &Boot, args: Value) -> Result<Value, RpcError> {
    call_tool(boot, TOOL_SOURCE_CAPTURE, planner_identity(boot), args).await
}

async fn list(boot: &Boot) -> Vec<Value> {
    call_tool(boot, TOOL_SOURCE_LIST, planner_identity(boot), json!({}))
        .await
        .expect("list")["sources"]
        .as_array()
        .expect("sources array")
        .clone()
}

fn assert_invalid_params(err: &RpcError, needle: &str) {
    assert_eq!(err.code, -32602, "{err}");
    assert!(err.message.contains(needle), "{err}");
}

#[tokio::test]
async fn capture_call_stores_the_joined_text_blocks_and_anchors() {
    let boot = boot().await;
    let args = json!({ "id": 752972 });
    record(
        &boot,
        PLUGIN_ID,
        TOOL_NAME,
        &args,
        &ok_result(&["first block", "second block with a quote inside"]),
    );
    let receipt = capture(
        &boot,
        json!({
            "call": { "tool": REGISTRY_NAME, "args": { "id": 752972 } },
            "provenance": "full_text",
            "title": "An article",
            "published_at": "2026-09-14",
            "content_id": "752972",
            "quotes": ["a quote inside", "first"],
        }),
    )
    .await
    .expect("capture");
    let source_id = receipt["source_id"].as_str().expect("source_id");
    assert!(
        calm_types::report_source_links::is_source_id(source_id),
        "{receipt}"
    );
    let body = "first block\nsecond block with a quote inside";
    assert_eq!(receipt["provenance"], "full_text");
    assert_eq!(receipt["body_bytes"], body.len());
    assert_eq!(receipt["body_sha256"], sha256_hex(body.as_bytes()));
    assert_eq!(
        receipt["quotes"],
        json!([
            { "id": "q1", "text": "a quote inside" },
            { "id": "q2", "text": "first" },
        ])
    );
    assert_eq!(receipt["matched_call"]["tool"], REGISTRY_NAME);
    assert_eq!(receipt["matched_call"]["args"], json!({ "id": 752972 }));
    assert!(
        receipt["matched_call"]["completed_at"]
            .as_str()
            .is_some_and(|t| t.ends_with('Z')),
        "{receipt}"
    );

    let sources = list(&boot).await;
    assert_eq!(sources.len(), 1);
    let entry = &sources[0];
    assert_eq!(entry["source_id"], source_id);
    assert_eq!(entry["title"], "An article");
    assert_eq!(entry["published_at"], "2026-09-14");
    assert_eq!(entry["content_id"], "752972");
    assert!(entry.get("url").is_none(), "{entry}");
    assert!(
        entry.get("body").is_none(),
        "list never carries bodies: {entry}"
    );
    assert_eq!(entry["body_bytes"], body.len());
    assert_eq!(entry["quotes"][0]["id"], "q1");
    assert_eq!(entry["quotes"][0]["text"], "a quote inside");
}

#[tokio::test]
async fn capture_resolves_the_sanitized_spelling_and_defaults_to_the_latest_call() {
    let boot = boot().await;
    record(
        &boot,
        PLUGIN_ID,
        TOOL_NAME,
        &json!({ "id": 1 }),
        &ok_result(&["one"]),
    );
    record(
        &boot,
        PLUGIN_ID,
        TOOL_NAME,
        &json!({ "id": 2 }),
        &ok_result(&["two"]),
    );
    let receipt = capture(
        &boot,
        json!({
            "call": { "tool": SANITIZED_NAME },
            "provenance": "summary",
            "title": "Latest",
        }),
    )
    .await
    .expect("capture");
    assert_eq!(receipt["matched_call"]["tool"], REGISTRY_NAME);
    assert_eq!(receipt["matched_call"]["args"], json!({ "id": 2 }));
    assert_eq!(receipt["body_sha256"], sha256_hex(b"two"));
    // `args: null` is the omitted form, not a call with null arguments.
    let receipt = capture(
        &boot,
        json!({
            "call": { "tool": SANITIZED_NAME, "args": null },
            "provenance": "summary",
            "title": "Latest again",
        }),
    )
    .await
    .expect("capture");
    assert_eq!(receipt["matched_call"]["args"], json!({ "id": 2 }));
    // Explicit args pick the exact entry, whatever is newest.
    let receipt = capture(
        &boot,
        json!({
            "call": { "tool": SANITIZED_NAME, "args": { "id": 1 } },
            "provenance": "summary",
            "title": "Older",
        }),
    )
    .await
    .expect("capture");
    assert_eq!(receipt["body_sha256"], sha256_hex(b"one"));
    // Args hash exactly: `1` and `1.0` are different calls.
    let err = capture(
        &boot,
        json!({
            "call": { "tool": REGISTRY_NAME, "args": { "id": 1.0 } },
            "provenance": "summary",
            "title": "Float",
        }),
    )
    .await
    .unwrap_err();
    assert_invalid_params(&err, "no recorded result for this call in this track");
}

/// A matched tool whose entry for these args is missing gets the "no recorded result" wording,
/// not the unknown-name one.
#[tokio::test]
async fn capture_resolves_the_codex_qualified_spelling() {
    let boot = boot().await;
    record(
        &boot,
        PLUGIN_ID,
        TOOL_NAME,
        &json!({ "id": 1 }),
        &ok_result(&["one"]),
    );
    let receipt = capture(
        &boot,
        json!({
            "call": { "tool": QUALIFIED_NAME, "args": { "id": 1 } },
            "provenance": "summary",
            "title": "Qualified",
        }),
    )
    .await
    .expect("capture");
    assert_eq!(receipt["matched_call"]["tool"], REGISTRY_NAME);
    assert_eq!(receipt["body_sha256"], sha256_hex(b"one"));
    let err = capture(
        &boot,
        json!({
            "call": { "tool": QUALIFIED_NAME, "args": { "id": 2 } },
            "provenance": "summary",
            "title": "Missing args",
        }),
    )
    .await
    .unwrap_err();
    assert_invalid_params(&err, "no recorded result for this call in this track");
    assert!(!err.message.contains("unknown tool name"), "{err}");
}

/// A name no visible plugin exposes, in any spelling, is refused as unknown, listing the tools
/// that do have a record; a `mcp__` prefix without a delimited server segment is not stripped.
#[tokio::test]
async fn capture_refuses_an_unknown_tool_name_listing_the_recorded_tools() {
    let boot = boot().await;
    install_registry(&boot);
    record(&boot, PLUGIN_ID, TOOL_NAME, &json!({}), &ok_result(&["a"]));
    for probe in [
        "mcp__plugin_dev_echo_do_thing",
        "mcp____plugin_dev_echo_do_thing",
        "plugin.dev.echo_other",
        "mcp__calm__plugin_dev_echo_other",
    ] {
        let err = capture(
            &boot,
            json!({ "call": { "tool": probe }, "provenance": "summary", "title": "x" }),
        )
        .await
        .unwrap_err();
        assert_invalid_params(&err, "unknown tool name");
        assert!(err.message.contains(&format!("{probe:?}")), "{err}");
        assert!(
            err.message.contains(&format!(
                "tools with a recorded result in this track: [{REGISTRY_NAME}]"
            )),
            "{err}"
        );
        assert!(!err.message.contains("no recorded result"), "{err}");
    }
}

/// A tool a visible plugin exposes but nobody in this track called keeps the "no recorded
/// result" sentence, in every spelling.
#[tokio::test]
async fn capture_names_a_known_tool_without_a_record_as_no_record() {
    let boot = boot().await;
    install_registry(&boot);
    record(
        &boot,
        PLUGIN_ID,
        OTHER_TOOL_NAME,
        &json!({}),
        &ok_result(&["other"]),
    );
    for probe in [REGISTRY_NAME, SANITIZED_NAME, QUALIFIED_NAME] {
        let err = capture(
            &boot,
            json!({ "call": { "tool": probe }, "provenance": "summary", "title": "x" }),
        )
        .await
        .unwrap_err();
        assert_invalid_params(&err, "never made, or made by a worker");
        assert!(
            err.message.contains(&format!(
                "tools with a recorded result in this track: [{OTHER_REGISTRY_NAME}]"
            )),
            "{err}"
        );
        assert!(!err.message.contains("unknown tool name"), "{err}");
    }
}

/// Two calls complete in the same millisecond (frozen clock); an explicit capture of the older
/// one is an LRU touch, and the args-omitted capture must still take the later completion.
#[tokio::test]
async fn omitted_args_take_the_latest_completion_not_the_last_lookup() {
    let boot = boot().await;
    let frozen = Arc::new(calm_server::plugin_results::PluginResults::with_clock(
        Arc::new(|| 1_700_000_000_000),
    ));
    let mut ctx = (*boot.ctx).clone();
    ctx.plugin_results = frozen.clone();
    let ctx = Arc::new(ctx);
    let track = boot.track_id.as_str();
    frozen.record(
        track,
        PLUGIN_ID,
        TOOL_NAME,
        &json!({ "id": "a" }),
        &ok_result(&["A"]),
    );
    frozen.record(
        track,
        PLUGIN_ID,
        TOOL_NAME,
        &json!({ "id": "b" }),
        &ok_result(&["B"]),
    );
    let handler = boot
        .registry
        .lookup(TOOL_SOURCE_CAPTURE)
        .expect("registered");
    let capture = |args: Value| {
        let handler = handler.clone();
        let ctx = ctx.clone();
        let identity = planner_identity(&boot);
        async move {
            handler(ctx, identity, args)
                .await
                .map(calm_server::mcp_server::result::ToolResult::into_structured)
        }
    };
    let explicit = capture(json!({
        "call": { "tool": REGISTRY_NAME, "args": { "id": "a" } },
        "provenance": "summary",
        "title": "A",
    }))
    .await
    .expect("explicit capture of A");
    assert_eq!(explicit["body_sha256"], sha256_hex(b"A"));
    let latest = capture(json!({
        "call": { "tool": REGISTRY_NAME },
        "provenance": "summary",
        "title": "latest",
    }))
    .await
    .expect("args-omitted capture");
    assert_eq!(latest["matched_call"]["args"], json!({ "id": "b" }));
    assert_eq!(latest["body_sha256"], sha256_hex(b"B"));
}

#[tokio::test]
async fn capture_refuses_an_ambiguous_sanitized_spelling_and_lists_candidates() {
    let boot = boot().await;
    record(&boot, PLUGIN_ID, TOOL_NAME, &json!({}), &ok_result(&["a"]));
    record(
        &boot,
        COLLIDING_PLUGIN_ID,
        COLLIDING_TOOL_NAME,
        &json!({}),
        &ok_result(&["b"]),
    );
    let err = capture(
        &boot,
        json!({
            "call": { "tool": SANITIZED_NAME },
            "provenance": "summary",
            "title": "x",
        }),
    )
    .await
    .unwrap_err();
    assert_invalid_params(&err, "ambiguous");
    assert!(err.message.contains(REGISTRY_NAME), "{err}");
    assert!(err.message.contains("plugin.dev_echo.do.thing"), "{err}");
    // The exact registry name still resolves.
    let receipt = capture(
        &boot,
        json!({
            "call": { "tool": "plugin.dev_echo.do.thing" },
            "provenance": "summary",
            "title": "x",
        }),
    )
    .await
    .expect("exact name");
    assert_eq!(receipt["body_sha256"], sha256_hex(b"b"));
}

#[tokio::test]
async fn capture_refuses_error_no_text_and_too_large_records() {
    let boot = boot().await;
    let mut error = ok_result(&["failed"]);
    error.is_error = Some(true);
    record(&boot, PLUGIN_ID, "err", &json!({}), &error);
    record(&boot, PLUGIN_ID, "empty", &json!({}), &ok_result(&[]));
    let huge = "x".repeat(MAX_TEXT_BYTES + 1);
    record(&boot, PLUGIN_ID, "huge", &json!({}), &ok_result(&[&huge]));
    let big_args = json!({ "blob": "y".repeat(MAX_ARGS_BYTES) });
    record(
        &boot,
        PLUGIN_ID,
        "bigargs",
        &big_args,
        &ok_result(&["fine"]),
    );
    // Over the source body cap but under the ring cap: refused, not truncated.
    let over_body = "z".repeat(MAX_BODY_BYTES + 1);
    record(
        &boot,
        PLUGIN_ID,
        "overbody",
        &json!({}),
        &ok_result(&[&over_body]),
    );

    for (tool, needle) in [
        ("plugin.dev.echo_err", "isError"),
        ("plugin.dev.echo_empty", "no text block"),
        ("plugin.dev.echo_huge", "size limit"),
        ("plugin.dev.echo_bigargs", "size limit"),
        ("plugin.dev.echo_overbody", "at most"),
    ] {
        let err = capture(
            &boot,
            json!({ "call": { "tool": tool }, "provenance": "summary", "title": "x" }),
        )
        .await
        .unwrap_err();
        assert_invalid_params(&err, needle);
    }
    assert!(list(&boot).await.is_empty(), "nothing was stored");
}

#[tokio::test]
async fn capture_does_not_see_another_tracks_records() {
    let boot = boot().await;
    install_registry(&boot);
    boot.ctx.plugin_results.record(
        "some-other-track",
        PLUGIN_ID,
        TOOL_NAME,
        &json!({ "id": 1 }),
        &ok_result(&["elsewhere"]),
    );
    let err = capture(
        &boot,
        json!({
            "call": { "tool": REGISTRY_NAME, "args": { "id": 1 } },
            "provenance": "full_text",
            "title": "x",
        }),
    )
    .await
    .unwrap_err();
    assert_invalid_params(&err, "never made, or made by a worker");
    assert!(
        err.message
            .contains("tools with a recorded result in this track: []"),
        "{err}"
    );
}

#[tokio::test]
async fn capture_refuses_a_quote_that_is_not_a_byte_exact_substring() {
    let boot = boot().await;
    record(
        &boot,
        PLUGIN_ID,
        TOOL_NAME,
        &json!({}),
        &ok_result(&["**bold** text here"]),
    );
    let err = capture(
        &boot,
        json!({
            "call": { "tool": REGISTRY_NAME },
            "provenance": "full_text",
            "title": "x",
            "quotes": ["bold text"],
        }),
    )
    .await
    .unwrap_err();
    assert_invalid_params(&err, "quotes[0] is not a byte-exact substring");
    assert!(list(&boot).await.is_empty(), "a bad quote stores nothing");
}

#[tokio::test]
async fn append_branch_merges_duplicates_keeps_ids_and_caps_at_thirty_two() {
    let boot = boot().await;
    let body: String = (0..40).map(|i| format!("w{i} ")).collect();
    record(
        &boot,
        PLUGIN_ID,
        TOOL_NAME,
        &json!({}),
        &ok_result(&[&body]),
    );
    let receipt = capture(
        &boot,
        json!({
            "call": { "tool": REGISTRY_NAME },
            "provenance": "full_text",
            "title": "x",
            "quotes": ["w1 ", "w2 ", "w1 "],
        }),
    )
    .await
    .expect("capture");
    let source_id = receipt["source_id"].as_str().unwrap().to_string();
    assert_eq!(
        receipt["quotes"],
        json!([
            { "id": "q1", "text": "w1 " },
            { "id": "q2", "text": "w2 " },
            { "id": "q1", "text": "w1 " },
        ])
    );
    // Append: an existing text keeps its id, a new one gets the next.
    let receipt = capture(
        &boot,
        json!({ "source_id": source_id, "quotes": ["w2 ", "w3 "] }),
    )
    .await
    .expect("append");
    assert_eq!(receipt["source_id"], source_id);
    assert_eq!(receipt["provenance"], "full_text");
    assert_eq!(
        receipt["quotes"],
        json!([{ "id": "q2", "text": "w2 " }, { "id": "q3", "text": "w3 " }])
    );
    // Fill to the cap, then one more new text is refused.
    let more: Vec<String> = (4..=MAX_QUOTES_PER_SOURCE)
        .map(|i| format!("w{i} "))
        .collect();
    capture(&boot, json!({ "source_id": source_id, "quotes": more }))
        .await
        .expect("fill to cap");
    let err = capture(&boot, json!({ "source_id": source_id, "quotes": ["w39 "] }))
        .await
        .unwrap_err();
    assert_invalid_params(&err, "at most 32 quotes");
    let sources = list(&boot).await;
    assert_eq!(
        sources[0]["quotes"].as_array().unwrap().len(),
        MAX_QUOTES_PER_SOURCE
    );
    // Unknown source id, and a malformed one.
    let err = capture(
        &boot,
        json!({ "source_id": "src_00000000", "quotes": ["w1 "] }),
    )
    .await
    .unwrap_err();
    assert_invalid_params(&err, "not found in this track");
    let err = capture(&boot, json!({ "source_id": "nope", "quotes": ["w1 "] }))
        .await
        .unwrap_err();
    assert_invalid_params(&err, "not a source id");
}

#[tokio::test]
async fn capture_manual_stores_the_planners_bytes_with_url() {
    let boot = boot().await;
    let receipt = capture(
        &boot,
        json!({
            "manual": { "text": "web page text", "url": "https://example.com/a" },
            "provenance": "manual",
            "title": "A page",
            "quotes": ["page text"],
        }),
    )
    .await
    .expect("manual capture");
    assert_eq!(receipt["provenance"], "manual");
    assert!(receipt.get("matched_call").is_none(), "{receipt}");
    assert_eq!(receipt["body_sha256"], sha256_hex(b"web page text"));
    let sources = list(&boot).await;
    assert_eq!(sources[0]["url"], "https://example.com/a");
    assert_eq!(sources[0]["provenance"], "manual");
}

#[tokio::test]
async fn capture_field_matrix_refusals() {
    let boot = boot().await;
    record(
        &boot,
        PLUGIN_ID,
        TOOL_NAME,
        &json!({}),
        &ok_result(&["body"]),
    );
    let cases: Vec<(Value, &str)> = vec![
        (
            json!({ "manual": { "text": "t" }, "provenance": "full_text", "title": "x" }),
            "requires provenance `manual`",
        ),
        (
            json!({ "call": { "tool": REGISTRY_NAME }, "provenance": "manual", "title": "x" }),
            "requires `manual`, not `call`",
        ),
        (
            json!({
                "call": { "tool": REGISTRY_NAME },
                "manual": { "text": "t" },
                "provenance": "manual",
                "title": "x",
            }),
            "mutually exclusive",
        ),
        (
            json!({ "source_id": "src_00000001", "call": { "tool": REGISTRY_NAME }, "quotes": ["b"] }),
            "mutually exclusive",
        ),
        // A key given as `null` is still given: two branches named at once.
        (
            json!({ "call": { "tool": REGISTRY_NAME }, "manual": null, "provenance": "full_text", "title": "x" }),
            "mutually exclusive",
        ),
        (
            json!({ "source_id": "src_00000001", "quotes": ["b"], "title": null }),
            "`title` is not accepted with `source_id`",
        ),
        (
            json!({ "source_id": "src_00000001", "quotes": ["b"], "published_at": null }),
            "`published_at` is not accepted with `source_id`",
        ),
        (
            json!({ "call": null, "provenance": "full_text", "title": "x" }),
            "`call` must be an object",
        ),
        (
            json!({ "provenance": "manual", "title": "x" }),
            "one of `call`, `manual`, `source_id`",
        ),
        (
            json!({ "source_id": "src_00000001", "quotes": ["b"], "title": "x" }),
            "`title` is not accepted with `source_id`",
        ),
        (
            json!({ "source_id": "src_00000001", "quotes": ["b"], "provenance": "manual" }),
            "`provenance` is not accepted with `source_id`",
        ),
        (
            json!({ "source_id": "src_00000001" }),
            "`quotes` must be a non-empty array",
        ),
        (
            json!({ "call": { "tool": REGISTRY_NAME }, "provenance": "full_text" }),
            "missing `title`",
        ),
        (
            json!({ "call": { "tool": REGISTRY_NAME }, "title": "x" }),
            "missing `provenance`",
        ),
        (
            json!({ "call": { "tool": REGISTRY_NAME }, "provenance": "digest", "title": "x" }),
            "`provenance` must be one of",
        ),
        (
            json!({ "call": { "tool": REGISTRY_NAME }, "provenance": "full_text", "title": "x", "bogus": 1 }),
            "unknown key `bogus`",
        ),
        (
            json!({ "call": { "tool": REGISTRY_NAME, "extra": 1 }, "provenance": "full_text", "title": "x" }),
            "unknown key `extra`",
        ),
        (
            json!({ "call": { "tool": REGISTRY_NAME }, "provenance": "full_text", "title": "x", "published_at": "yesterday" }),
            "`published_at` must be",
        ),
        (
            json!({ "call": { "tool": REGISTRY_NAME }, "provenance": "full_text", "title": "x", "quotes": [1] }),
            "quotes[0] must be a string",
        ),
        (
            json!({ "call": { "tool": REGISTRY_NAME }, "provenance": "full_text", "title": "   " }),
            "`title` is empty",
        ),
        (
            json!({ "manual": { "text": "" }, "provenance": "manual", "title": "x" }),
            "`manual.text` is empty",
        ),
        (
            json!({ "manual": { "text": "x".repeat(MAX_BODY_BYTES + 1) }, "provenance": "manual", "title": "x" }),
            "at most",
        ),
        (json!([]), "arguments must be an object"),
    ];
    for (args, needle) in cases {
        let err = capture(&boot, args.clone()).await.unwrap_err();
        assert_invalid_params(&err, needle);
    }
    assert!(list(&boot).await.is_empty(), "refusals store nothing");
}

#[tokio::test]
async fn capture_over_the_per_track_quota_is_forbidden() {
    let boot = boot().await;
    let pool = boot.repo.sqlite_pool().expect("pool");
    for i in 0..MAX_SOURCES_PER_TRACK {
        sqlx::query(concat!(
            "INSERT INTO report_sources ",
            "(track_id, source_id, provenance, origin, title, published_at, ",
            " body, body_sha256, captured_at, quotes) ",
            "VALUES (?1, ?2, 'manual', '{\"kind\":\"manual\"}', 't', NULL, 'b', 'h', 1, '[]')"
        ))
        .bind(boot.track_id.as_str())
        .bind(format!("src_{i:08x}"))
        .execute(&pool)
        .await
        .expect("seed row");
    }
    let err = capture(
        &boot,
        json!({ "manual": { "text": "one more" }, "provenance": "manual", "title": "x" }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32403, "{err}");
    assert!(err.message.contains("quota"), "{err}");
    assert_eq!(list(&boot).await.len() as i64, MAX_SOURCES_PER_TRACK);
}

#[tokio::test]
async fn non_planner_roles_are_refused_with_invalid_params() {
    let boot = boot().await;
    record(
        &boot,
        PLUGIN_ID,
        TOOL_NAME,
        &json!({}),
        &ok_result(&["body"]),
    );
    let args =
        json!({ "call": { "tool": REGISTRY_NAME }, "provenance": "full_text", "title": "x" });
    for identity in [worker_identity(&boot), assistant_identity(&boot)] {
        let err = call_tool(&boot, TOOL_SOURCE_CAPTURE, identity.clone(), args.clone())
            .await
            .unwrap_err();
        assert_invalid_params(&err, "tool requires role=Planner");
        let err = call_tool(&boot, TOOL_SOURCE_LIST, identity, json!({}))
            .await
            .unwrap_err();
        assert_invalid_params(&err, "tool requires role=Planner");
    }
    assert!(list(&boot).await.is_empty());
    // The worker allowlist does not open these tools.
    let environment = calm_server::dedicated_codex::executor_environment();
    let allowed = environment["mcp_tools"].as_array().expect("mcp_tools");
    assert!(
        !allowed
            .iter()
            .any(|name| name.as_str().is_some_and(|n| n.starts_with("calm.source."))),
        "{allowed:?}"
    );
}

#[tokio::test]
async fn capture_only_leaves_the_report_unwritten() {
    let boot = boot().await;
    record(
        &boot,
        PLUGIN_ID,
        TOOL_NAME,
        &json!({}),
        &ok_result(&["body"]),
    );
    capture(
        &boot,
        json!({ "call": { "tool": REGISTRY_NAME }, "provenance": "full_text", "title": "x" }),
    )
    .await
    .expect("capture");
    let state = call_tool(&boot, TOOL_TRACK_STATE, planner_identity(&boot), json!({}))
        .await
        .expect("track state");
    assert_eq!(
        state["report_startup_read_required"],
        json!(false),
        "{state}"
    );
}
