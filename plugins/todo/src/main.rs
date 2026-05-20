//! Todo-list plugin — MCP-Apps wire, mirrors plugins/hello-world.
//!
//! Speaks line-delimited JSON-RPC 2.0 (MCP) over stdio with the Neige kernel.
//! The wire shape is identical to hello-world; only the tool name, the resource
//! URI, and the structuredContent payload differ. Everything that mutates
//! state happens in the iframe (`views/todo.html`) via `neige.kv.*` calls
//! routed through the host's AppBridge fan-out — this binary stays
//! deliberately passive.
//!
//!   1. Read NEIGE_PLUGIN_TOKEN / NEIGE_PLUGIN_ID from env (fail loudly if
//!      missing — the kernel sets both on spawn).
//!   2. Answer `initialize` by mirroring
//!      `params._meta["dev.neige/auth"].expected_echo` back at
//!      `result._meta["dev.neige/auth"].echoed_token`, declaring the
//!      `experimental.dev.neige/kernel-callbacks` capability so the iframe's
//!      `neige.*` writes are accepted by the kernel-side gate.
//!   3. Expose one tool, `make_todo_card`, via `tools/list` whose entry
//!      carries `_meta.ui.resourceUri = "ui://dev.neige.todo/list"`. AddPanel
//!      filters on this to surface the card-producing tool.
//!   4. On `tools/call { name: "make_todo_card" }` return a `CallToolResult`
//!      carrying the same `_meta.ui.resourceUri` plus a tiny
//!      `structuredContent` ({ title, created_at }); the kernel writes the
//!      Card row keyed by the URI and forwards the payload into the iframe.
//!   5. Handle `notifications/initialized` / `notifications/shutdown` by
//!      ignoring them (the kernel doesn't expect a reply). Log unknown
//!      methods, exit on SIGTERM / EOF.
//!
//! Persistence note: todo items are not held in this process. They live in
//! the plugin KV under `card/<card_id>/items` and are read/written exclusively
//! from the iframe. That keeps the plugin binary stateless across kernel
//! restarts and matches the per-card scope of the view (`scope: "card"`).

use std::env;
use std::io::Write as _;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;

const PLUGIN_ID: &str = "dev.neige.todo";
const VIEW_RESOURCE_URI: &str = "ui://dev.neige.todo/list";
const TOOL_NAME: &str = "make_todo_card";

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> ExitCode {
    // -- 1. env intake ------------------------------------------------------
    let plugin_id = match env::var("NEIGE_PLUGIN_ID") {
        Ok(v) => v,
        Err(_) => {
            let _ = writeln!(
                std::io::stderr(),
                "todo: NEIGE_PLUGIN_ID env var missing (kernel sets this on spawn)"
            );
            return ExitCode::from(1);
        }
    };
    let plugin_token = match env::var("NEIGE_PLUGIN_TOKEN") {
        Ok(v) => v,
        Err(_) => {
            let _ = writeln!(
                std::io::stderr(),
                "todo: NEIGE_PLUGIN_TOKEN env var missing (kernel sets this on spawn)"
            );
            return ExitCode::from(1);
        }
    };
    eprintln!("todo: starting plugin_id={plugin_id} (token hidden)");

    // -- 2. stdio + signal plumbing ----------------------------------------
    let stdin = tokio::io::stdin();
    let mut stdin = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();

    let mut sigterm = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("todo: failed to install SIGTERM handler: {e}");
            return ExitCode::from(1);
        }
    };

    // -- 3. read the initialize request ------------------------------------
    let init_line = tokio::select! {
        line = stdin.next_line() => line,
        _ = sigterm.recv() => {
            eprintln!("todo: SIGTERM before initialize");
            return ExitCode::from(0);
        }
    };
    let init_line = match init_line {
        Ok(Some(l)) => l,
        Ok(None) => {
            eprintln!("todo: stdin closed before initialize");
            return ExitCode::from(0);
        }
        Err(e) => {
            eprintln!("todo: stdin read error: {e}");
            return ExitCode::from(1);
        }
    };

    let init_req: Value = match serde_json::from_str(&init_line) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("todo: bad initialize JSON: {e} (raw: {init_line:?})");
            return ExitCode::from(1);
        }
    };
    let req_id = init_req.get("id").cloned().unwrap_or(Value::Null);
    // M1 wire: `params._meta["dev.neige/auth"].expected_echo`.
    let expected_echo = init_req
        .pointer("/params/_meta/dev.neige~1auth/expected_echo")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if expected_echo.is_empty() {
        eprintln!(
            "todo: initialize missing params._meta[\"dev.neige/auth\"].expected_echo"
        );
    } else if expected_echo != plugin_token {
        eprintln!(
            "todo: expected_echo from kernel does NOT match NEIGE_PLUGIN_TOKEN; \
             echoing anyway, kernel will disconnect"
        );
    }

    // -- 4. write the initialize response (echo + capabilities) ------------
    let resp = json!({
        "jsonrpc": "2.0",
        "id": req_id,
        "result": {
            "protocolVersion": "2025-11-25",
            "capabilities": {
                "tools": { "listChanged": false },
                "experimental": {
                    "dev.neige/kernel-callbacks": { "version": 1 }
                }
            },
            "serverInfo": {
                "name": "neige-todo",
                "version": "0.1.0",
            },
            "_meta": {
                "dev.neige/auth": { "echoed_token": expected_echo }
            }
        }
    });
    if let Err(e) = write_line(&mut stdout, &resp).await {
        eprintln!("todo: failed to write initialize response: {e}");
        return ExitCode::from(1);
    }
    eprintln!("todo: initialize handshake complete");

    // -- 5. main loop ------------------------------------------------------
    let (line_tx, mut line_rx) = mpsc::unbounded_channel::<Option<String>>();
    tokio::spawn(async move {
        loop {
            match stdin.next_line().await {
                Ok(Some(l)) => {
                    if line_tx.send(Some(l)).is_err() {
                        break;
                    }
                }
                Ok(None) => {
                    let _ = line_tx.send(None);
                    break;
                }
                Err(e) => {
                    eprintln!("todo: stdin reader error: {e}");
                    let _ = line_tx.send(None);
                    break;
                }
            }
        }
    });

    loop {
        tokio::select! {
            _ = sigterm.recv() => {
                eprintln!("todo: SIGTERM received, shutting down");
                return ExitCode::from(0);
            }
            msg = line_rx.recv() => {
                match msg {
                    Some(Some(line)) => {
                        if let Some(reply) = handle_inbound(&line) {
                            if let Err(e) = write_line(&mut stdout, &reply).await {
                                eprintln!("todo: failed to write reply: {e}");
                                return ExitCode::from(1);
                            }
                        }
                    }
                    Some(None) | None => {
                        eprintln!("todo: stdin EOF, shutting down");
                        return ExitCode::from(0);
                    }
                }
            }
        }
    }
}

/// Handle one inbound JSON-RPC frame. Returns `Some(reply)` when the kernel
/// expects a response (i.e. the frame had an `id`), otherwise `None`.
fn handle_inbound(line: &str) -> Option<Value> {
    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("todo: ignoring un-parseable inbound line: {e}");
            return None;
        }
    };

    if v.get("result").is_some() || v.get("error").is_some() {
        if let Some(id) = v.get("id") {
            eprintln!(
                "todo: inbound response id={id} payload={}",
                v.get("result").or_else(|| v.get("error")).unwrap_or(&Value::Null)
            );
        }
        return None;
    }

    let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = v.get("id").cloned();
    let params = v.get("params").cloned().unwrap_or(Value::Null);

    // Notifications carry no `id` — log and drop.
    let Some(id) = id else {
        eprintln!("todo: inbound notification method={method} (ignored)");
        return None;
    };

    match method {
        "tools/list" => Some(reply_tools_list(id)),
        "tools/call" => Some(reply_tools_call(id, &params)),
        other => {
            eprintln!("todo: unhandled method `{other}`, replying MethodNotFound");
            Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("Method not found: {other}")
                }
            }))
        }
    }
}

/// Answer `tools/list` with our single card-producing tool. The entry's
/// `_meta.ui.resourceUri` is what AddPanel filters on (migration doc §1.4).
fn reply_tools_list(id: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "tools": [
                {
                    "name": TOOL_NAME,
                    "title": "Todo list card",
                    "description": "Mount a todo list card. Items persist in the plugin KV per card.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "title": {
                                "type": "string",
                                "description": "Optional title for the todo list (defaults to \"Todo\")."
                            }
                        },
                        "additionalProperties": false
                    },
                    "_meta": {
                        "ui": { "resourceUri": VIEW_RESOURCE_URI }
                    }
                }
            ]
        }
    })
}

/// Answer `tools/call` for `make_todo_card`. Carries `_meta.ui.resourceUri`
/// (the kernel's M2 card-creation extractor reads this) plus a tiny
/// `structuredContent` payload that AppBridge forwards into the iframe via
/// `ui/notifications/tool-result`. The iframe uses the card id from the host
/// context (not from this payload) to key its KV reads.
fn reply_tools_call(id: Value, params: &Value) -> Value {
    let tool_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
    if tool_name != TOOL_NAME {
        eprintln!("todo: tools/call for unknown name `{tool_name}`");
        return json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32602,
                "message": format!("Unknown tool: {tool_name}")
            }
        });
    }

    let title = params
        .pointer("/arguments/title")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Todo")
        .to_string();

    let created_at = iso8601_now();

    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [],
            "isError": false,
            "_meta": {
                "ui": { "resourceUri": VIEW_RESOURCE_URI }
            },
            "structuredContent": {
                "title": title,
                "source": PLUGIN_ID,
                "created_at": created_at
            }
        }
    })
}

/// Best-effort ISO-8601 UTC timestamp without pulling in `chrono`. Output
/// shape: `"2026-05-20T12:34:56Z"`. Drops sub-second precision — the payload
/// is for display only, not for ordering.
fn iso8601_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Convert seconds-since-epoch to a y/m/d/h/m/s tuple. Algorithm: Howard
    // Hinnant's date library, civil_from_days. Inlined to avoid a dep.
    let days = (secs / 86_400) as i64;
    let secs_of_day = (secs % 86_400) as u32;
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day / 60) % 60;
    let second = secs_of_day % 60;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, m, d, hour, minute, second
    )
}

async fn write_line(
    out: &mut tokio::io::Stdout,
    value: &Value,
) -> std::io::Result<()> {
    let mut buf = serde_json::to_vec(value)?;
    buf.push(b'\n');
    out.write_all(&buf).await?;
    out.flush().await?;
    Ok(())
}
