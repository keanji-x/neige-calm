//! Test stub plugin that responds to `initialize` then exits with code 1.
//!
//! Used to drive the supervisor's crash-loop branch: each spawn surfaces
//! Spawning → Running (after handshake) → Crashed (after wait) → respawn.
//! The smoke test asserts the state event sequence and that after the
//! configured crash-window limit the plugin stops respawning.
//!
//! Behavior:
//!   1. Read the first frame (we expect `initialize`), respond.
//!   2. Read one more frame (the `notifications/initialized` from the kernel).
//!   3. Exit with status code 1.

use std::io::{BufRead, BufWriter, Write};

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    eprintln!("stub-crash: started, will crash after initialize");

    let lock = stdin.lock();
    let mut lines = lock.lines();

    // 1. initialize.
    if let Some(Ok(line)) = lines.next() {
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => std::process::exit(2),
        };
        let id = v.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let protocol = v
            .get("params")
            .and_then(|p| p.get("protocolVersion"))
            .cloned()
            .unwrap_or(serde_json::Value::String("2025-11-25".into()));
        // M1: mirror the kernel's expected_echo from `_meta` so the auth
        // handshake passes; we want the supervisor to see Running→Crashed,
        // not an immediate AuthMismatch. This stub doesn't issue any neige.*
        // callbacks, but we still declare the capability so the kernel
        // installs the real router rather than the MethodNotFound drainer —
        // crash semantics are identical either way and this keeps the
        // pre-M1 surface tested.
        let echoed = v
            .pointer("/params/_meta/dev.neige~1auth/expected_echo")
            .and_then(|s| s.as_str())
            .map(String::from);
        let mut result = serde_json::json!({
            "protocolVersion": protocol,
            "serverInfo": { "name": "stub-crash", "version": "0.0.0" },
            "capabilities": {
                "experimental": {
                    "dev.neige/kernel-callbacks": { "version": 1 }
                }
            }
        });
        if let Some(e) = echoed {
            result["_meta"] = serde_json::json!({
                "dev.neige/auth": { "echoed_token": e }
            });
        }
        let reply = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        });
        let mut s = serde_json::to_string(&reply).expect("static json");
        s.push('\n');
        let _ = out.write_all(s.as_bytes());
        let _ = out.flush();
    }

    // 2. Read one more line (the `notifications/initialized` from the kernel).
    //    This guarantees the kernel's handshake completed before we exit, so
    //    the test sees a clean Running → Crashed transition instead of an
    //    InitializeRejected.
    let _ = lines.next();

    eprintln!("stub-crash: bye");
    std::process::exit(1);
}
