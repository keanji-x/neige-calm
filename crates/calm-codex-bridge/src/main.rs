//! `neige-codex-bridge` — hook shim invoked by codex for every lifecycle
//! hook (SessionStart / PreToolUse / PostToolUse / PermissionRequest /
//! UserPromptSubmit / Stop). Reads the hook JSON from stdin, forwards it to
//! calm-server's internal ingest endpoint, and exits 0.
//!
//! Every hook — Stop included — takes the same fire-and-forget path: POST
//! the raw payload to `/internal/codex/hook` and print `{}` (the codex hook
//! contract for "no behavior override, continue"). Failures are logged to
//! stderr but never fail the hook — we don't want a flaky network call to
//! stall the agent.
//!
//! #293 cutover: the Stop hook used to long-poll
//! `/internal/codex/pending_events` and emit `{decision:"block",...}` to
//! re-prompt the spec agent (the pull model). Pull is gone — spec agents are
//! now driven by observations pushed onto their codex thread by the kernel —
//! so Stop is no longer special-cased here.
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

    // #293 cutover: every hook (Stop included) takes the same
    // fire-and-forget path. POST the payload to /hook, then print `{}` —
    // the codex hook contract for "no behavior override, continue".
    post_hook(&base, &card_id, &body);

    print!("{{}}");
}

/// Fire-and-forget POST of the raw hook body to `/internal/codex/hook`.
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
    fn url_encode_passes_safe_chars_through() {
        assert_eq!(url_encode("abc-XYZ_123.~"), "abc-XYZ_123.~");
    }

    #[test]
    fn url_encode_escapes_unsafe_chars() {
        assert_eq!(url_encode("a b"), "a%20b");
        assert_eq!(url_encode("a&b"), "a%26b");
    }
}
