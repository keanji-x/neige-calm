//! `neige-codex-bridge` — hook shim invoked by codex for every Pre/Post/Stop
//! event. Reads the hook JSON from stdin, forwards it to calm-server's
//! internal ingest endpoint, and exits 0. The stdout payload depends on
//! the hook type:
//!
//! * Stop hook (PR8 of #136) — long-polls
//!   `GET /internal/codex/pending_events?card_id=...&timeout_ms=30000`
//!   for up to 30s. When events come back, prints
//!   `{"decision":"block","reason":"<JSON of events>"}` so codex
//!   re-prompts the agent with the pending events as a turn input
//!   (the closed-loop story for the spec daemon's wait_for_events
//!   path). On empty / error: prints `{}` so codex lets the agent idle.
//!
//!   The pending_events long-poll is a parallel surface to
//!   `calm.wait_for_events` and shares the kernel's per-card cursor
//!   cache, so even if the spec daemon's MCP session has died, the
//!   Stop hook still keeps the wave's events flowing into the agent
//!   as observations.
//!
//! * Every other hook — POSTs the payload to `/internal/codex/hook`
//!   verbatim and prints `{}`. Failures are logged to stderr but
//!   never fail the hook — we don't want a flaky network call to
//!   stall the agent.
//!
//! Env contract (set by calm-server when spawning codex):
//!   * `NEIGE_CARD_ID`        — card uuid the hook belongs to (required)
//!   * `NEIGE_CALM_BASE_URL`  — e.g. `http://127.0.0.1:4040` (required)

use std::io::Read;
use std::time::Duration;

fn main() {
    // Read full stdin. Hooks send one JSON object per invocation; we
    // forward it verbatim as the POST body so backend can parse it as
    // an opaque payload and tag it with `hook_event_name`.
    let mut body = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut body) {
        eprintln!("neige-codex-bridge: read stdin failed: {e}");
        print!("{{}}");
        return;
    }

    let card_id = match std::env::var("NEIGE_CARD_ID") {
        Ok(v) if !v.is_empty() => v,
        _ => {
            eprintln!("neige-codex-bridge: NEIGE_CARD_ID not set");
            print!("{{}}");
            return;
        }
    };
    let base = match std::env::var("NEIGE_CALM_BASE_URL") {
        Ok(v) if !v.is_empty() => v,
        _ => {
            eprintln!("neige-codex-bridge: NEIGE_CALM_BASE_URL not set");
            print!("{{}}");
            return;
        }
    };

    // Discriminate by hook event name. PR8 of #136 — Stop hook is
    // special-cased: instead of (or in addition to) fire-and-forget
    // POSTing to /hook, it runs a synchronous long-poll against
    // /pending_events and emits a `{decision:"block"}` JSON to stdout
    // when events are pending.
    let hook_event_name = parse_hook_event_name(&body);

    if hook_event_name.as_deref() == Some("Stop") {
        handle_stop_hook(&base, &card_id);
        return;
    }

    // Default path — fire-and-forget POST to /hook for non-Stop events.
    post_hook(&base, &card_id, &body);

    // Always exit 0 with empty JSON — that's the codex hook contract for
    // "no behavior override, continue".
    print!("{{}}");
}

/// Best-effort extraction of `hook_event_name` from the raw hook JSON.
/// Returns `None` on malformed JSON / missing key — the caller falls
/// back to the default fire-and-forget path, which is safe.
fn parse_hook_event_name(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v.get("hook_event_name")
        .and_then(|n| n.as_str())
        .map(|s| s.to_string())
}

/// Stop-hook handler. Long-polls the kernel for pending events on the
/// caller's wave and, if any come back, prints
/// `{"decision":"block","reason":<JSON>}` so codex re-prompts the
/// agent with them as a turn input.
///
/// On any error or empty return, prints `{}` — codex's default
/// "agent goes idle" behavior. We never want a server outage / parse
/// glitch to lock the agent into a `decision:"block"` loop with no
/// payload.
fn handle_stop_hook(base: &str, card_id: &str) {
    // 30s long-poll matches the server-side cap (see
    // `mcp_server::tools::wait::MAX_TIMEOUT_MS`). Total wait time on
    // the codex side is ~30s + small request overhead, well inside
    // the 60s `Stop` hook timeout pinned in `build_hooks_json`.
    let url = format!(
        "{}/internal/codex/pending_events?card_id={}&timeout_ms=30000",
        base.trim_end_matches('/'),
        url_encode(card_id),
    );

    // ureq blocking client — bridge is short-lived (one fork per
    // hook), so no async runtime. Timeout has a 5s buffer over the
    // long-poll's 30s cap so a healthy server's response never trips
    // the client-side cutoff.
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(35))
        .build();

    let resp = match agent.get(&url).call() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("neige-codex-bridge: pending_events GET failed: {e}");
            print!("{{}}");
            return;
        }
    };

    let body = match resp.into_string() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("neige-codex-bridge: pending_events body read failed: {e}");
            print!("{{}}");
            return;
        }
    };

    // Parse and check whether the events array is non-empty.
    let parsed: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("neige-codex-bridge: pending_events body parse failed: {e}");
            print!("{{}}");
            return;
        }
    };

    let events = match parsed.get("events").and_then(|e| e.as_array()) {
        Some(arr) if !arr.is_empty() => arr.clone(),
        _ => {
            // Empty / missing `events` — let the agent idle.
            print!("{{}}");
            return;
        }
    };

    // Codex hook decision contract: stdout must be a single JSON
    // object. `decision: "block"` makes codex inject `reason` as a
    // turn input observation. We serialize the events array as a
    // string so the agent sees the raw payload it can reason over.
    //
    // Failure to serialize is unreachable in practice (every input
    // came from a successful serde_json::from_str above), but be
    // conservative.
    let reason = serde_json::to_string(&serde_json::Value::Array(events))
        .unwrap_or_else(|_| String::from("[]"));
    let out = serde_json::json!({
        "decision": "block",
        "reason": reason,
    });
    let out_str = serde_json::to_string(&out).unwrap_or_else(|_| String::from("{}"));
    print!("{out_str}");
}

/// Fire-and-forget POST of the raw hook body to `/internal/codex/hook`.
/// Same behavior as pre-PR8; lifted into a helper so the Stop branch
/// above can stay focused.
fn post_hook(base: &str, card_id: &str, body: &str) {
    let url = format!(
        "{}/internal/codex/hook?card_id={}",
        base.trim_end_matches('/'),
        url_encode(card_id),
    );

    // Short timeout — we'd rather drop one hook event than hold codex up
    // if the server is slow.
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(3))
        .build();
    // Scope β — declare the actor for every hook the bridge forwards.
    // The kernel's `actor_middleware` reads `X-Calm-Actor`, validates it,
    // and stamps the resulting event row with `actor = "ai:codex"`. Without
    // this header the middleware falls back to its `"user"` default, which
    // would misattribute codex's own lifecycle signal as a human write.
    match agent
        .post(&url)
        .set("content-type", "application/json")
        .set("X-Calm-Actor", "ai:codex")
        .send_string(body)
    {
        Ok(_) => {}
        Err(e) => eprintln!("neige-codex-bridge: POST failed: {e}"),
    }
}

/// Bare-bones percent-encoder so we don't need a `url` dep. Card ids are
/// uuid hex (no special chars), but we encode defensively in case the
/// format ever widens.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hook_event_name_extracts_field() {
        let body = r#"{"hook_event_name":"Stop","session_id":"abc"}"#;
        assert_eq!(parse_hook_event_name(body), Some("Stop".into()));
    }

    #[test]
    fn parse_hook_event_name_returns_none_for_malformed() {
        assert_eq!(parse_hook_event_name("not json at all"), None);
        assert_eq!(parse_hook_event_name(r#"{"other":"field"}"#), None);
    }

    #[test]
    fn url_encode_passes_safe_chars_through() {
        assert_eq!(url_encode("abc-XYZ_123.~"), "abc-XYZ_123.~");
    }

    #[test]
    fn url_encode_escapes_unsafe_chars() {
        assert_eq!(url_encode("a b"), "a%20b");
        assert_eq!(url_encode("a&b"), "a%26b");
    }
}
