//! #1628 S2 test stub: a minimal MCP server whose `tools/call` behaviour is
//! programmed through files, so one running process can play every reply
//! the resolver tests need without being respawned.
//!
//! Control directory `STUB_SERIES_DIR` (required) holds two files.
//!
//! `reply.json` is read fresh on EVERY `tools/call` and is one of:
//!
//! ```text
//! { "mode": "structured", "structured": <object> }
//!     -> { content: [], isError: false, structuredContent: <object> }
//! { "mode": "raw", "result": <CallToolResult> }
//!     -> the given result verbatim (for "not an object" shapes)
//! { "mode": "is_error", "text": "<text>" }
//!     -> { content: [{type: text, text}], isError: true }
//! { "mode": "hang" }
//!     -> no reply at all; the request is swallowed and the loop keeps
//!        serving later requests
//! { "mode": "sequence", "replies": [<program>, ...] }
//!     -> the n-th tools/call this process has seen picks
//!        replies[min(n, len - 1)] (each entry is one of the above)
//! ```
//!
//! Any program may carry `"delay_ms": <n>` to sleep before replying. A
//! missing or unreadable file behaves like `hang`, so a test that forgot to
//! program a reply times out instead of getting an accidental success.
//!
//! `calls.jsonl` is appended with one line per `tools/call` received: the
//! request's `params` (`name`, `arguments`, `_meta`). Line count is the call
//! count.
//!
//! `initialize` echoes the auth token like the other stubs and declares no
//! kernel-callbacks capability (this plugin never calls back).

use std::fs::OpenOptions;
use std::io::{BufRead, BufWriter, Write};
use std::path::PathBuf;

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let dir = PathBuf::from(std::env::var("STUB_SERIES_DIR").expect("STUB_SERIES_DIR is set"));
    eprintln!(
        "stub-series: hello, plugin id={:?} dir={}",
        std::env::var("NEIGE_PLUGIN_ID"),
        dir.display()
    );

    let mut calls_seen: usize = 0;
    let lock = stdin.lock();
    for line in lock.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => return,
        };
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("stub-series: bad json: {e}");
                continue;
            }
        };
        let Some(id) = v.get("id").cloned() else {
            continue;
        };
        let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");

        let reply = if method == "initialize" {
            let protocol = v
                .get("params")
                .and_then(|p| p.get("protocolVersion"))
                .cloned()
                .unwrap_or(serde_json::Value::String("2025-11-25".into()));
            let echoed = v
                .pointer("/params/_meta/dev.neige~1auth/expected_echo")
                .and_then(|s| s.as_str())
                .map(String::from);
            let mut result = serde_json::json!({
                "protocolVersion": protocol,
                "serverInfo": { "name": "stub-series", "version": "0.0.0" },
                "capabilities": {},
            });
            if let Some(e) = echoed {
                result["_meta"] = serde_json::json!({
                    "dev.neige/auth": { "echoed_token": e }
                });
            }
            serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result })
        } else if method == "tools/call" {
            let params = v.get("params").cloned().unwrap_or(serde_json::Value::Null);
            if let Ok(mut log) = OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("calls.jsonl"))
            {
                // One `write` per line, newline included: the fixture's
                // `read_calls` treats "ends with '\n'" as "line complete".
                let _ = log.write_all(format!("{params}\n").as_bytes());
                let _ = log.flush();
            }
            let mut program: serde_json::Value = std::fs::read_to_string(dir.join("reply.json"))
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_else(|| serde_json::json!({ "mode": "hang" }));
            let index = calls_seen;
            calls_seen += 1;
            if program.get("mode").and_then(|m| m.as_str()) == Some("sequence") {
                let replies = program
                    .get("replies")
                    .and_then(|r| r.as_array())
                    .cloned()
                    .unwrap_or_default();
                program = replies
                    .get(index.min(replies.len().saturating_sub(1)))
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({ "mode": "hang" }));
            }
            if let Some(delay) = program.get("delay_ms").and_then(|d| d.as_u64()) {
                std::thread::sleep(std::time::Duration::from_millis(delay));
            }
            let result = match program.get("mode").and_then(|m| m.as_str()) {
                Some("structured") => serde_json::json!({
                    "content": [],
                    "isError": false,
                    "structuredContent": program.get("structured").cloned()
                        .unwrap_or(serde_json::Value::Null),
                }),
                Some("raw") => program
                    .get("result")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
                Some("is_error") => serde_json::json!({
                    "content": [{
                        "type": "text",
                        "text": program.get("text").and_then(|t| t.as_str()).unwrap_or("error"),
                    }],
                    "isError": true,
                }),
                _ => {
                    eprintln!("stub-series: hang mode; swallowing tools/call");
                    continue;
                }
            };
            serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result })
        } else {
            serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": { "echo": method } })
        };

        let mut s = serde_json::to_string(&reply).expect("static json");
        s.push('\n');
        if out.write_all(s.as_bytes()).is_err() {
            return;
        }
        if out.flush().is_err() {
            return;
        }
    }
}
