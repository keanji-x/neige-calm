//! Test stub plugin: a minimal MCP server that **keeps whatever the kernel
//! handed it at `initialize.params._meta["dev.neige/config"]` and hands it
//! back on request**. Written for #1284 S2, whose whole claim is "the plugin
//! really receives its effective configuration"; the only way to witness that
//! claim is to ask the plugin, after a real spawn and a real handshake.
//!
//! Behaviour:
//!
//!   * `initialize` — standard handshake (echoes the kernel's auth token from
//!     `params._meta["dev.neige/auth"].expected_echo`, mirrors the protocol
//!     version, declares the kernel-callbacks capability), and **captures**
//!     `params._meta["dev.neige/config"]` verbatim.
//!
//!   * `tools/call` — returns the captured node under
//!     `structuredContent.config_meta`, plus its JSON text in `content[0]`.
//!     Verbatim and un-normalized on purpose: the test asserts on the
//!     `{"values": …}` envelope the kernel actually wrote, so a stub that
//!     unwrapped or re-shaped it would be asserting on itself. `null` when the
//!     kernel sent no config namespace at all, which is a distinguishable
//!     answer from "the namespace was there and empty".
//!
//! Anything else replies `{"echo": method}`, like the sibling stubs.
//!
//! Built via the `[[bin]] name = "plugin-host-stub-config"` declaration in
//! `calm-server/Cargo.toml`; tests locate it with
//! `env!("CARGO_BIN_EXE_plugin-host-stub-config")`.

use std::io::{BufRead, BufWriter, Write};

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    eprintln!(
        "stub-config: hello, plugin id={:?}",
        std::env::var("NEIGE_PLUGIN_ID")
    );

    // What the kernel sent at `_meta["dev.neige/config"]`, or `null` if it
    // sent nothing there.
    let mut captured_config = serde_json::Value::Null;

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
                eprintln!("stub-config: bad json: {e}");
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
            // `~1` is the JSON-Pointer escape for the `/` inside the
            // namespaced key `dev.neige/config`.
            captured_config = v
                .pointer("/params/_meta/dev.neige~1config")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            eprintln!("stub-config: received config meta {captured_config}");

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
                "serverInfo": { "name": "stub-config", "version": "0.0.0" },
                "capabilities": {
                    "experimental": {
                        "dev.neige/kernel-callbacks": { "version": 1 }
                    }
                },
            });
            if let Some(e) = echoed {
                result["_meta"] = serde_json::json!({
                    "dev.neige/auth": { "echoed_token": e }
                });
            }
            serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result })
        } else if method == "tools/call" {
            let text = serde_json::to_string(&captured_config)
                .unwrap_or_else(|_| "\"<unserializable>\"".to_string());
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [ { "type": "text", "text": text } ],
                    "isError": false,
                    "structuredContent": { "config_meta": captured_config }
                }
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
