//! `neige-codex-bridge` — hook shim invoked by codex for every Pre/Post/Stop
//! event. Reads the hook JSON from stdin, forwards it to calm-server's
//! internal ingest endpoint, and exits 0 with an empty stdout so codex
//! never blocks on us. Failures are logged to stderr (visible in codex's
//! own logs) but never fail the hook — we don't want a flaky network
//! call to stall the agent.
//!
//! Env contract (set by calm-server when spawning codex):
//!   * `NEIGE_CARD_ID`        — card uuid the hook belongs to (required)
//!   * `NEIGE_CALM_BASE_URL`  — e.g. `http://127.0.0.1:4040` (required)

use std::io::Read;

fn main() {
    // Read full stdin. Hooks send one JSON object per invocation; we
    // forward it verbatim as the POST body so backend can parse it as
    // an opaque payload and tag it with `hook_event_name`.
    let mut body = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut body) {
        eprintln!("neige-codex-bridge: read stdin failed: {e}");
        return;
    }

    let card_id = match std::env::var("NEIGE_CARD_ID") {
        Ok(v) if !v.is_empty() => v,
        _ => {
            eprintln!("neige-codex-bridge: NEIGE_CARD_ID not set");
            return;
        }
    };
    let base = match std::env::var("NEIGE_CALM_BASE_URL") {
        Ok(v) if !v.is_empty() => v,
        _ => {
            eprintln!("neige-codex-bridge: NEIGE_CALM_BASE_URL not set");
            return;
        }
    };

    let url = format!(
        "{}/internal/codex/hook?card_id={}",
        base.trim_end_matches('/'),
        url_encode(&card_id),
    );

    // Short timeout — we'd rather drop one hook event than hold codex up
    // if the server is slow.
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(3))
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
        .send_string(&body)
    {
        Ok(_) => {}
        Err(e) => eprintln!("neige-codex-bridge: POST failed: {e}"),
    }

    // Always exit 0 with empty JSON — that's the codex hook contract for
    // "no behavior override, continue".
    print!("{{}}");
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
