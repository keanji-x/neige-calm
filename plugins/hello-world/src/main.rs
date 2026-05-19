//! Hello-world reference plugin — M3-mcp-apps M6 rewrite.
//!
//! Speaks line-delimited JSON-RPC 2.0 (MCP) over stdio with the Neige kernel.
//! After M6 the wire is the standard MCP Apps shape end-to-end:
//!
//!   1. Read NEIGE_PLUGIN_TOKEN / NEIGE_PLUGIN_ID / NEIGE_DEMO_WAVE from env.
//!   2. Answer `initialize` by mirroring
//!      `params._meta["dev.neige/auth"].expected_echo` back at
//!      `result._meta["dev.neige/auth"].echoed_token` and declaring the
//!      `experimental.dev.neige/kernel-callbacks` capability (M1 wire shape).
//!   3. Expose one tool, `make_status_card`, via `tools/list` whose entry
//!      carries `_meta.ui.resourceUri = "ui://dev.neige.hello-world/status"`
//!      so AddPanel knows it is card-producing (migration doc §1.4).
//!   4. On `tools/call { name: "make_status_card" }` return a `CallToolResult`
//!      carrying the same `_meta.ui.resourceUri` plus a small
//!      `structuredContent` greeting; the kernel writes the Card row keyed by
//!      that URI (M2 path).
//!   5. Idle on stdin, log unknown methods, exit on SIGTERM / EOF.
//!
//! NOTE on the autonomous overlay write: M3-mcp-apps moves the overlay
//! mutation from plugin-startup to user-driven iframe action. The
//! `neige.overlay.set` call now lives in `views/status.html`, dispatched by
//! AppBridge through the kernel's `/tool-call` route when the user mounts the
//! card. Nothing in this binary writes overlays.
//!
//! TODO(future-plugin): extract a `neige-plugin-ext` crate when there is a
//! second plugin worth the abstraction. With one consumer the inline wire is
//! cheaper than a single-call shim — see migration doc §4.3 + the
//! `feedback-decisive-cuts` memory note.

use std::env;
use std::io::Write as _;
use std::process::ExitCode;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;

const PLUGIN_ID: &str = "dev.neige.hello-world";
const VIEW_RESOURCE_URI: &str = "ui://dev.neige.hello-world/status";
const TOOL_NAME: &str = "make_status_card";

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> ExitCode {
    // -- 1. env intake ------------------------------------------------------
    let plugin_id = match env::var("NEIGE_PLUGIN_ID") {
        Ok(v) => v,
        Err(_) => {
            let _ = writeln!(
                std::io::stderr(),
                "hello-world: NEIGE_PLUGIN_ID env var missing (kernel sets this on spawn)"
            );
            return ExitCode::from(1);
        }
    };
    let plugin_token = match env::var("NEIGE_PLUGIN_TOKEN") {
        Ok(v) => v,
        Err(_) => {
            let _ = writeln!(
                std::io::stderr(),
                "hello-world: NEIGE_PLUGIN_TOKEN env var missing (kernel sets this on spawn)"
            );
            return ExitCode::from(1);
        }
    };
    // Demo wave is no longer required at startup — the iframe asks for it
    // through AppBridge at mount time, sourced from the host context. We still
    // log if it's missing because the demo.sh flow expects it to drive the
    // overlay write that the iframe will perform.
    if env::var("NEIGE_DEMO_WAVE")
        .ok()
        .filter(|s| !s.is_empty())
        .is_none()
    {
        eprintln!(
            "hello-world: NEIGE_DEMO_WAVE not set — the iframe's overlay button \
             will fall back to the wave id passed via the kernel's host context."
        );
    }
    eprintln!("hello-world: starting plugin_id={plugin_id} (token hidden)");

    // -- 2. stdio + signal plumbing ----------------------------------------
    let stdin = tokio::io::stdin();
    let mut stdin = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();

    let mut sigterm = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("hello-world: failed to install SIGTERM handler: {e}");
            return ExitCode::from(1);
        }
    };

    // -- 3. read the initialize request -----------------------------------
    let init_line = tokio::select! {
        line = stdin.next_line() => line,
        _ = sigterm.recv() => {
            eprintln!("hello-world: SIGTERM before initialize");
            return ExitCode::from(0);
        }
    };
    let init_line = match init_line {
        Ok(Some(l)) => l,
        Ok(None) => {
            eprintln!("hello-world: stdin closed before initialize");
            return ExitCode::from(0);
        }
        Err(e) => {
            eprintln!("hello-world: stdin read error: {e}");
            return ExitCode::from(1);
        }
    };

    let init_req: Value = match serde_json::from_str(&init_line) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("hello-world: bad initialize JSON: {e} (raw: {init_line:?})");
            return ExitCode::from(1);
        }
    };
    let req_id = init_req.get("id").cloned().unwrap_or(Value::Null);
    // M1 wire: `params._meta["dev.neige/auth"].expected_echo`. Hard cut from
    // the legacy `clientInfo.expected_echo` path (migration doc §7.6 row 2).
    let expected_echo = init_req
        .pointer("/params/_meta/dev.neige~1auth/expected_echo")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if expected_echo.is_empty() {
        eprintln!(
            "hello-world: initialize missing params._meta[\"dev.neige/auth\"].expected_echo"
        );
    } else if expected_echo != plugin_token {
        eprintln!(
            "hello-world: expected_echo from kernel does NOT match \
             NEIGE_PLUGIN_TOKEN; echoing anyway, kernel will disconnect"
        );
    }

    // -- 4. write the initialize response (echo + capabilities) -----------
    // `capabilities.tools` advertises the standard tools/list+tools/call wire.
    // `capabilities.experimental.dev.neige/kernel-callbacks` opts us into
    // calling `neige.*` back into the kernel (currently unused from this
    // binary, but the iframe relies on it for overlay writes routed through
    // the host's M5 tool-call fan-out).
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
                "name": "neige-hello-world",
                "version": "0.1.0",
            },
            "_meta": {
                "dev.neige/auth": { "echoed_token": expected_echo }
            }
        }
    });
    if let Err(e) = write_line(&mut stdout, &resp).await {
        eprintln!("hello-world: failed to write initialize response: {e}");
        return ExitCode::from(1);
    }
    eprintln!("hello-world: initialize handshake complete");

    // -- 5. main loop: handle tools/list + tools/call, log everything else --
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
                    eprintln!("hello-world: stdin reader error: {e}");
                    let _ = line_tx.send(None);
                    break;
                }
            }
        }
    });

    loop {
        tokio::select! {
            _ = sigterm.recv() => {
                eprintln!("hello-world: SIGTERM received, shutting down");
                return ExitCode::from(0);
            }
            msg = line_rx.recv() => {
                match msg {
                    Some(Some(line)) => {
                        if let Some(reply) = handle_inbound(&line) {
                            if let Err(e) = write_line(&mut stdout, &reply).await {
                                eprintln!("hello-world: failed to write reply: {e}");
                                return ExitCode::from(1);
                            }
                        }
                    }
                    Some(None) | None => {
                        eprintln!("hello-world: stdin EOF, shutting down");
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
            eprintln!("hello-world: ignoring un-parseable inbound line: {e}");
            return None;
        }
    };

    // Responses to our own outbound requests (we don't make any after M6, but
    // we log them defensively in case the kernel sends an unsolicited result).
    if v.get("result").is_some() || v.get("error").is_some() {
        if let Some(id) = v.get("id") {
            eprintln!(
                "hello-world: inbound response id={id} payload={}",
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
        eprintln!("hello-world: inbound notification method={method} (ignored)");
        return None;
    };

    match method {
        "tools/list" => Some(reply_tools_list(id)),
        "tools/call" => Some(reply_tools_call(id, &params)),
        other => {
            eprintln!("hello-world: unhandled method `{other}`, replying MethodNotFound");
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
                    "title": "Hello status card",
                    "description": "Mount a pulsing status indicator card for the hello-world demo.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "wave_id": {
                                "type": "string",
                                "description": "Optional wave id the iframe will overlay on."
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

/// Answer `tools/call` for `make_status_card` with the card-creation envelope
/// the kernel's M2 extractor expects: `_meta.ui.resourceUri` + a small
/// `structuredContent` greeting that AppBridge will forward to the iframe.
fn reply_tools_call(id: Value, params: &Value) -> Value {
    let tool_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
    if tool_name != TOOL_NAME {
        eprintln!("hello-world: tools/call for unknown name `{tool_name}`");
        return json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32602,
                "message": format!("Unknown tool: {tool_name}")
            }
        });
    }

    let arg_wave = params
        .pointer("/arguments/wave_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let env_wave = env::var("NEIGE_DEMO_WAVE").ok().unwrap_or_default();
    let wave_id = if !arg_wave.is_empty() {
        arg_wave.to_string()
    } else {
        env_wave
    };

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
                "greeting": "Hello, plugin world.",
                "source": PLUGIN_ID,
                "wave_id": wave_id
            }
        }
    })
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
