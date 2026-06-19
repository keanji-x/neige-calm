//! Test stub plugin: a minimal MCP server with one configurable `tools/call`
//! handler. Built for M2's `via_tool_call` integration tests.
//!
//! Behaviour (controlled by env vars at spawn time):
//!
//!   * `initialize` — standard handshake: echoes the kernel's auth token from
//!     `params._meta["dev.neige/auth"].expected_echo` and (by default)
//!     declares the `dev.neige/kernel-callbacks` capability so the kernel
//!     installs the real `neige.*` router. This stub doesn't issue any
//!     callbacks itself, but declaring the capability matches what a real
//!     card-creating plugin would do.
//!
//!   * `tools/call` — reads the requested tool name from `params.name` and
//!     responds based on `STUB_TOOLCALL_MODE`:
//!       - `"card"` (default) — returns a CallToolResult with
//!         `_meta.ui.resourceUri = STUB_TOOLCALL_RESOURCE_URI` (default:
//!         `"ui://stub/status"`) and
//!         `structuredContent = STUB_TOOLCALL_STRUCTURED_JSON` (default:
//!         `{"msg":"hi"}`).
//!       - `"no_uri"` — returns a CallToolResult with NO `_meta.ui.resourceUri`,
//!         simulating "this isn't a card-creating tool". Used by the 422 test.
//!       - `"is_error"` — returns `isError: true` with the configured content.
//!         Used by the 502 test.
//!
//! Anything else (e.g. another method) replies with a generic `{"echo": method}`
//! so the test can still smoke the wire.

use std::io::{BufRead, BufWriter, Write};

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    eprintln!(
        "stub-toolcall: hello, plugin id={:?} mode={:?}",
        std::env::var("NEIGE_PLUGIN_ID"),
        std::env::var("STUB_TOOLCALL_MODE")
    );

    let mode = std::env::var("STUB_TOOLCALL_MODE").unwrap_or_else(|_| "card".to_string());
    let resource_uri = std::env::var("STUB_TOOLCALL_RESOURCE_URI")
        .unwrap_or_else(|_| "ui://stub/status".to_string());
    let structured_json = std::env::var("STUB_TOOLCALL_STRUCTURED_JSON")
        .unwrap_or_else(|_| r#"{"msg":"hi"}"#.to_string());
    // Same capability omit knob the other stubs use, in case a test wants to
    // exercise the kernel's no-callbacks drainer alongside tools/call.
    let omit_capability = std::env::var("STUB_OMIT_CAPABILITY")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

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
                eprintln!("stub-toolcall: bad json: {e}");
                continue;
            }
        };
        let id = match v.get("id") {
            Some(id) => id.clone(),
            None => continue,
        };
        let method = v
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();

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
            let capabilities = if omit_capability {
                serde_json::json!({})
            } else {
                serde_json::json!({
                    "experimental": {
                        "dev.neige/kernel-callbacks": { "version": 1 }
                    }
                })
            };
            let mut result = serde_json::json!({
                "protocolVersion": protocol,
                "serverInfo": { "name": "stub-toolcall", "version": "0.0.0" },
                "capabilities": capabilities,
            });
            if let Some(e) = echoed {
                result["_meta"] = serde_json::json!({
                    "dev.neige/auth": { "echoed_token": e }
                });
            }
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result
            })
        } else if method == "tools/call" {
            let requested_name = v
                .pointer("/params/name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let result = match mode.as_str() {
                "no_uri" => serde_json::json!({
                    "content": [],
                    "isError": false,
                    "_meta": { "requested_name": requested_name },
                    "structuredContent": { "msg": "no uri here" }
                }),
                "is_error" => serde_json::json!({
                    "content": [
                        { "type": "text", "text": "tool failed on purpose" }
                    ],
                    "isError": true,
                    "_meta": { "requested_name": requested_name }
                }),
                _ /* "card" or anything else */ => {
                    let structured: serde_json::Value =
                        serde_json::from_str(&structured_json)
                            .unwrap_or_else(|_| serde_json::json!({}));
                    serde_json::json!({
                        "content": [],
                        "isError": false,
                        "_meta": {
                            "ui": { "resourceUri": resource_uri },
                            "requested_name": requested_name
                        },
                        "structuredContent": structured
                    })
                }
            };
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result
            })
        } else {
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "echo": method }
            })
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
