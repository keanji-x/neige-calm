//! `neige-codex-bridge` — hook shim invoked by codex or Claude Code for
//! lifecycle hooks. Reads the hook JSON from stdin, forwards it to
//! calm-server's internal ingest endpoint, and exits 0.
//!
//! Every hook — Stop included — takes the same fire-and-forget path: POST
//! the raw payload to `/internal/codex/hook` and print `{}` (the codex hook
//! contract for "no behavior override, continue"). In Claude mode, selected
//! by `--provider claude` or `NEIGE_HOOK_PROVIDER=claude`, POST to
//! `/internal/claude/hook` and print `{"continue":true}`. Failures are
//! logged to stderr but never fail the hook — we don't want a flaky network
//! call to stall the agent.
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
//!   * `NEIGE_HOOK_PROVIDER`  — `codex` or `claude` (optional; codex default)
//!   * `NEIGE_HOOK_URL`       — full ingest URL override (optional)

use std::io::Read;
use std::time::Duration;

fn main() {
    let provider = Provider::from_env_and_args();
    // Read full stdin. Hooks send one JSON object per invocation; we
    // forward it verbatim as the POST body so backend can parse it as
    // an opaque payload and tag it with `hook_event_name`.
    let mut body = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut body) {
        eprintln!("neige-codex-bridge: read stdin failed: {e}");
        print!("{}", provider.ack());
        return;
    }

    let card_id = match std::env::var("NEIGE_CARD_ID") {
        Ok(v) if !v.is_empty() => v,
        _ => {
            eprintln!("neige-codex-bridge: NEIGE_CARD_ID not set");
            print!("{}", provider.ack());
            return;
        }
    };
    let base = match std::env::var("NEIGE_CALM_BASE_URL") {
        Ok(v) if !v.is_empty() => v,
        _ => {
            eprintln!("neige-codex-bridge: NEIGE_CALM_BASE_URL not set");
            print!("{}", provider.ack());
            return;
        }
    };
    let hook_url = std::env::var("NEIGE_HOOK_URL")
        .ok()
        .filter(|v| !v.is_empty());

    // #293 cutover: every hook (Stop included) takes the same
    // fire-and-forget path. POST the payload to /hook, then print `{}` —
    // the codex hook contract for "no behavior override, continue".
    post_hook(provider, &base, &card_id, hook_url.as_deref(), &body);

    print!("{}", provider.ack());
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Provider {
    Codex,
    Claude,
}

impl Provider {
    fn from_env_and_args() -> Self {
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            if arg == "--provider" {
                if let Some(value) = args.next() {
                    return Self::parse(&value);
                }
            } else if let Some(value) = arg.strip_prefix("--provider=") {
                return Self::parse(value);
            }
        }
        std::env::var("NEIGE_HOOK_PROVIDER")
            .ok()
            .as_deref()
            .map(Self::parse)
            .unwrap_or(Self::Codex)
    }

    fn parse(value: &str) -> Self {
        match value {
            "claude" => Self::Claude,
            "codex" | "" => Self::Codex,
            other => {
                eprintln!("neige-codex-bridge: unknown provider {other:?}; using codex");
                Self::Codex
            }
        }
    }

    fn endpoint(self) -> &'static str {
        match self {
            Self::Codex => "/internal/codex/hook",
            Self::Claude => "/internal/claude/hook",
        }
    }

    fn actor_header(self) -> &'static str {
        match self {
            Self::Codex => "ai:codex",
            Self::Claude => "ai:claude",
        }
    }

    fn ack(self) -> &'static str {
        match self {
            Self::Codex => "{}",
            Self::Claude => "{\"continue\":true}",
        }
    }
}

/// Fire-and-forget POST of the raw hook body to the provider ingest route.
fn post_hook(provider: Provider, base: &str, card_id: &str, hook_url: Option<&str>, body: &str) {
    let url = hook_url.map(String::from).unwrap_or_else(|| {
        format!(
            "{}{}?card_id={}",
            base.trim_end_matches('/'),
            provider.endpoint(),
            url_encode(card_id),
        )
    });

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
        .set("X-Calm-Actor", provider.actor_header())
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

    #[test]
    fn provider_defaults_to_codex_for_unknown_values() {
        assert_eq!(Provider::parse("codex"), Provider::Codex);
        assert_eq!(Provider::parse("claude"), Provider::Claude);
        assert_eq!(Provider::parse("bogus"), Provider::Codex);
    }

    #[test]
    fn provider_endpoints_and_acks_match_hook_contracts() {
        assert_eq!(Provider::Codex.endpoint(), "/internal/codex/hook");
        assert_eq!(Provider::Codex.ack(), "{}");
        assert_eq!(Provider::Claude.endpoint(), "/internal/claude/hook");
        assert_eq!(Provider::Claude.ack(), "{\"continue\":true}");
    }
}
