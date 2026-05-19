//! Test stub plugin: a minimal MCP server.
//!
//! Reads line-delimited JSON-RPC from stdin and responds:
//!   * `initialize` → returns a `serverInfo` blob and the same `protocolVersion`
//!     the kernel sent.
//!   * any other request with an `id` → returns `{"echo": method}`.
//!   * notifications → ignored.
//!
//! Exits when stdin closes (kernel side closed) or on SIGTERM (handled
//! implicitly by tokio's default signal disposition — a SIGTERM kills the
//! process, which is the behavior the supervisor expects).
//!
//! This stub is built by `cargo test` via the `[[bin]] name = "plugin-host-stub-echo"`
//! declaration in `calm-server/Cargo.toml`; tests locate it via
//! `env!("CARGO_BIN_EXE_plugin-host-stub-echo")`.

use std::io::{BufRead, BufWriter, Write};

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    // Stderr line so the kernel's ring buffer captures something useful.
    eprintln!("stub-echo: hello, plugin id={:?}", std::env::var("NEIGE_PLUGIN_ID"));

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
                eprintln!("stub-echo: bad json: {e}");
                continue;
            }
        };
        // Only respond to requests (have id + method). Notifications and
        // responses we receive (we never send requests in this stub) drop.
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
            // M1: mirror the kernel's expected echo token from
            // `params._meta["dev.neige/auth"].expected_echo` to
            // `result._meta["dev.neige/auth"].echoed_token`.
            // STUB_ECHO_OVERRIDE forces a wrong value for auth-failure tests.
            let echoed = std::env::var("STUB_ECHO_OVERRIDE").ok().or_else(|| {
                v.pointer("/params/_meta/dev.neige~1auth/expected_echo")
                    .and_then(|s| s.as_str())
                    .map(String::from)
            });
            // M1: `STUB_OMIT_CAPABILITY=1` simulates a plugin that doesn't
            // declare the kernel-callbacks capability — the kernel should
            // then install the MethodNotFound drainer instead of the real
            // `neige.*` router.
            let omit_capability = std::env::var("STUB_OMIT_CAPABILITY")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
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
                "serverInfo": { "name": "stub-echo", "version": "0.0.0" },
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
