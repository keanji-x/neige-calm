//! Test stub plugin: exercises the `neige.*` host-callback router.
//!
//! After `initialize`, this stub issues a deterministic sequence of `neige.*`
//! callbacks at the kernel and records the responses to its stderr (which
//! the host captures into the per-plugin ring buffer). The corresponding
//! integration test asserts on the kernel-side repo state, not on the stub's
//! own state — so the stub can stay dead-simple.
//!
//! Sequence (driven by env `NEIGE_DEMO_WAVE`):
//!   1. `neige.kv.set { key: "answer", value: 42 }`
//!   2. `neige.kv.get { key: "answer" }`
//!   3. `neige.overlay.set { entity_kind: "wave", entity_id: <wave>, kind:
//!      "status", payload: { state: "running" } }`
//!   4. `neige.card.create { wave_id: <wave>, kind: "plugin:<self>:demo" }`
//!   5. `neige.card.create { wave_id: <wave>, kind: "terminal" }`
//!      (this one is expected to succeed; permissions allow terminal)
//!   6. `neige.card.create { wave_id: <wave>, kind: "plugin:other:x" }`
//!      (this one is expected to be REJECTED — exercises the deny path)
//!
//! The stub stays alive (continues reading initialize-style responses) until
//! stdin closes, so the test can stop() it cleanly.

use std::io::{BufRead, BufReader, BufWriter, Write};

fn send(out: &mut impl Write, value: &serde_json::Value) {
    let mut s = serde_json::to_string(value).expect("static json");
    s.push('\n');
    let _ = out.write_all(s.as_bytes());
    let _ = out.flush();
}

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let plugin_id =
        std::env::var("NEIGE_PLUGIN_ID").unwrap_or_else(|_| "test.caller".to_string());
    let wave_id = std::env::var("NEIGE_DEMO_WAVE")
        .unwrap_or_else(|_| "MISSING-WAVE-ENV".to_string());
    eprintln!(
        "stub-caller: started plugin_id={} wave_id={}",
        plugin_id, wave_id
    );

    let mut reader = BufReader::new(stdin.lock());

    // ---- 1. Wait for the kernel's `initialize` request and respond. -------
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.is_empty() {
        return;
    }
    let init: serde_json::Value = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(_) => return,
    };
    let init_id = init.get("id").cloned().unwrap_or(serde_json::Value::Null);
    // M1: mirror the kernel's expected_echo from
    // `params._meta["dev.neige/auth"].expected_echo` to
    // `result._meta["dev.neige/auth"].echoed_token`. We declare the
    // `dev.neige/kernel-callbacks` capability because this stub issues
    // `neige.*` host callbacks immediately after handshake.
    let echoed = init
        .pointer("/params/_meta/dev.neige~1auth/expected_echo")
        .and_then(|s| s.as_str())
        .map(String::from);
    // M1: `STUB_OMIT_CAPABILITY=1` simulates a plugin that doesn't declare
    // the kernel-callbacks capability — the kernel should install the
    // MethodNotFound drainer and every `neige.*` call we issue below should
    // come back as -32601 instead of touching kernel state. Used by
    // plugin_auth.rs's no-capability gating test.
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
        "protocolVersion": "2025-11-25",
        "serverInfo": { "name": "stub-caller", "version": "0.0.0" },
        "capabilities": capabilities,
    });
    if let Some(e) = echoed {
        result["_meta"] = serde_json::json!({
            "dev.neige/auth": { "echoed_token": e }
        });
    }
    send(
        &mut out,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": init_id,
            "result": result
        }),
    );

    // ---- 2. Skip the kernel's `notifications/initialized` notification ----
    line.clear();
    if reader.read_line(&mut line).is_err() {
        return;
    }

    // ---- 3. Issue our neige.* callbacks. Send all six up front; the kernel
    //         will pipeline responses. We track id → label so the asserts on
    //         stderr remain readable in test failures. We do NOT block on
    //         specific responses — the kernel-side repo state is the source
    //         of truth for the integration test.
    let calls: Vec<(u64, &str, serde_json::Value)> = vec![
        (
            10,
            "neige.kv.set",
            serde_json::json!({ "key": "answer", "value": 42 }),
        ),
        (11, "neige.kv.get", serde_json::json!({ "key": "answer" })),
        (
            12,
            "neige.overlay.set",
            serde_json::json!({
                "entity_kind": "wave",
                "entity_id": wave_id,
                "kind": "status",
                "payload": { "state": "running" }
            }),
        ),
        (
            13,
            "neige.card.create",
            serde_json::json!({
                "wave_id": wave_id,
                "kind": format!("plugin:{}:demo", plugin_id),
                "payload": { "hello": "world" }
            }),
        ),
        (
            14,
            "neige.card.create",
            serde_json::json!({
                "wave_id": wave_id,
                "kind": "terminal"
            }),
        ),
        (
            15,
            "neige.card.create",
            serde_json::json!({
                "wave_id": wave_id,
                "kind": "plugin:other.plugin:x"
            }),
        ),
    ];
    for (id, method, params) in &calls {
        send(
            &mut out,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            }),
        );
    }

    // ---- 4. Drain replies (and the initialize-initiated notifications).
    //         Log them so test failures have a forensic trail.
    //         Stay alive until stdin closes — the test calls host.stop().
    loop {
        line.clear();
        let n = match reader.read_line(&mut line) {
            Ok(n) => n,
            Err(_) => return,
        };
        if n == 0 {
            return;
        }
        eprintln!("stub-caller: <- {}", line.trim());
    }
}
