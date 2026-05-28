//! `neige-codex-bridge` — hook shim invoked by codex or Claude Code for
//! lifecycle hooks. Reads the hook JSON from stdin, forwards it to
//! calm-server's internal ingest endpoint, and exits 0.
//!
//! Every hook — Stop included — takes the same fire-and-forget path: POST
//! the payload to `/internal/codex/hook` and print `{}` (the codex hook
//! contract for "no behavior override, continue"). Stop payloads may be
//! enriched with `last_assistant_message` from the transcript before POST.
//! In Claude mode, selected by `--provider claude` or
//! `NEIGE_HOOK_PROVIDER=claude`, POST to `/internal/claude/hook` and print
//! `{"continue":true}`. Failures are logged to stderr but never fail the hook
//! — we don't want a flaky network call to stall the agent.
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

const MAX_TRANSCRIPT_BYTES: u64 = 256 * 1024 * 1024;

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
    // fire-and-forget path. Stop payloads may be enriched before POST so
    // downstream projections still read only persisted event rows.
    let post_body = maybe_enrich_stop_payload(&body).unwrap_or_else(|| body.clone());
    post_hook(provider, &base, &card_id, hook_url.as_deref(), &post_body);

    print!("{}", provider.ack());
}

fn maybe_enrich_stop_payload(body: &str) -> Option<String> {
    let mut payload: serde_json::Value = match serde_json::from_str(body) {
        Ok(payload) => payload,
        Err(e) => {
            eprintln!("neige-codex-bridge: hook payload is not JSON; skipping enrichment: {e}");
            return None;
        }
    };

    if payload
        .get("hook_event_name")
        .and_then(serde_json::Value::as_str)
        != Some("Stop")
    {
        return None;
    }

    if payload
        .get("last_assistant_message")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|message| !message.is_empty())
    {
        return None;
    }

    let Some(transcript_path) = payload
        .get("transcript_path")
        .and_then(serde_json::Value::as_str)
    else {
        eprintln!("neige-codex-bridge: Stop payload has no transcript_path; skipping enrichment");
        return None;
    };

    let metadata = match std::fs::metadata(transcript_path) {
        Ok(metadata) => metadata,
        Err(e) => {
            eprintln!(
                "neige-codex-bridge: transcript metadata failed for {transcript_path:?}; skipping enrichment: {e}"
            );
            return None;
        }
    };
    if metadata.len() > MAX_TRANSCRIPT_BYTES {
        eprintln!(
            "neige-codex-bridge: transcript {transcript_path:?} is {} bytes; skipping enrichment",
            metadata.len()
        );
        return None;
    }

    let jsonl = match std::fs::read_to_string(transcript_path) {
        Ok(jsonl) => jsonl,
        Err(e) => {
            eprintln!(
                "neige-codex-bridge: transcript read failed for {transcript_path:?}; skipping enrichment: {e}"
            );
            return None;
        }
    };

    let Some(message) = extract_last_assistant_text(&jsonl) else {
        eprintln!(
            "neige-codex-bridge: transcript {transcript_path:?} had no assistant text; skipping enrichment"
        );
        return None;
    };

    let Some(object) = payload.as_object_mut() else {
        eprintln!("neige-codex-bridge: Stop payload is not an object; skipping enrichment");
        return None;
    };
    object.insert(
        "last_assistant_message".to_string(),
        serde_json::Value::String(message),
    );

    match serde_json::to_string(&payload) {
        Ok(enriched) => Some(enriched),
        Err(e) => {
            eprintln!("neige-codex-bridge: failed to serialize enriched payload: {e}");
            None
        }
    }
}

fn extract_last_assistant_text(jsonl: &str) -> Option<String> {
    for line in jsonl.lines().rev() {
        if line.trim().is_empty() {
            continue;
        }

        let record: serde_json::Value = match serde_json::from_str(line) {
            Ok(record) => record,
            Err(e) => {
                eprintln!("neige-codex-bridge: skipping malformed transcript JSONL record: {e}");
                continue;
            }
        };

        if matches!(
            record.get("type").and_then(serde_json::Value::as_str),
            Some("summary" | "system" | "queue_operation" | "attachment")
        ) {
            continue;
        }

        if let Some(text) = assistant_text_from_record(&record) {
            return Some(text);
        }
    }

    None
}

fn assistant_text_from_record(record: &serde_json::Value) -> Option<String> {
    // Claude shape: top-level `type == "assistant"`.
    if record.get("type").and_then(serde_json::Value::as_str) == Some("assistant") {
        return concat_text_blocks(
            record
                .get("message")
                .and_then(|message| message.get("content")),
            "text",
        );
    }

    // Codex rollout shape: `type == "response_item"` with payload role.
    if record.get("type").and_then(serde_json::Value::as_str) == Some("response_item")
        && record
            .get("payload")
            .and_then(|payload| payload.get("role"))
            .and_then(serde_json::Value::as_str)
            == Some("assistant")
    {
        return concat_text_blocks(
            record
                .get("payload")
                .and_then(|payload| payload.get("content")),
            "output_text",
        );
    }

    None
}

fn concat_text_blocks(
    content: Option<&serde_json::Value>,
    text_block_type: &str,
) -> Option<String> {
    let mut text = String::new();
    let content = content.and_then(serde_json::Value::as_array)?;

    for block in content {
        if block.get("type").and_then(serde_json::Value::as_str) == Some(text_block_type)
            && let Some(block_text) = block.get("text").and_then(serde_json::Value::as_str)
        {
            text.push_str(block_text);
        }
    }

    (!text.is_empty()).then_some(text)
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
    use serde_json::json;
    use std::io::Write;

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

    #[test]
    fn extracts_single_assistant_text_block() {
        let jsonl = json!({
            "type": "assistant",
            "message": {
                "content": [
                    { "type": "text", "text": "hello" }
                ]
            }
        })
        .to_string();

        assert_eq!(
            extract_last_assistant_text(&jsonl).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn extracts_text_blocks_and_drops_tool_use() {
        let jsonl = json!({
            "type": "assistant",
            "message": {
                "content": [
                    { "type": "text", "text": "hello " },
                    { "type": "tool_use", "name": "read" },
                    { "type": "text", "text": "world" }
                ]
            }
        })
        .to_string();

        assert_eq!(
            extract_last_assistant_text(&jsonl).as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn extracts_codex_response_item_output_text() {
        let jsonl = json!({
            "type": "response_item",
            "payload": {
                "role": "assistant",
                "content": [
                    { "type": "output_text", "text": "hello from codex" }
                ]
            }
        })
        .to_string();

        assert_eq!(
            extract_last_assistant_text(&jsonl).as_deref(),
            Some("hello from codex")
        );
    }

    #[test]
    fn extracts_codex_response_item_with_mixed_blocks() {
        let jsonl = json!({
            "type": "response_item",
            "payload": {
                "role": "assistant",
                "content": [
                    { "type": "output_text", "text": "hello " },
                    { "type": "function_call", "name": "lookup" },
                    { "type": "output_text", "text": "world" }
                ]
            }
        })
        .to_string();

        assert_eq!(
            extract_last_assistant_text(&jsonl).as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn codex_response_item_with_non_assistant_role_is_ignored() {
        let jsonl = json!({
            "type": "response_item",
            "payload": {
                "role": "user",
                "content": [
                    { "type": "output_text", "text": "ignored" }
                ]
            }
        })
        .to_string();

        assert_eq!(extract_last_assistant_text(&jsonl), None);
    }

    #[test]
    fn claude_shape_and_codex_shape_interleaved() {
        let claude = json!({
            "type": "assistant",
            "message": { "content": [{ "type": "text", "text": "claude answer" }] }
        });
        let codex = json!({
            "type": "response_item",
            "payload": {
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "codex answer" }]
            }
        });
        let jsonl = format!("{claude}\n{codex}\n");

        assert_eq!(
            extract_last_assistant_text(&jsonl).as_deref(),
            Some("codex answer")
        );
    }

    #[test]
    fn extracts_last_assistant_line() {
        let first = json!({
            "type": "assistant",
            "message": { "content": [{ "type": "text", "text": "first" }] }
        });
        let second = json!({
            "type": "assistant",
            "message": { "content": [{ "type": "text", "text": "second" }] }
        });
        let jsonl = format!("{first}\n{second}\n");

        assert_eq!(
            extract_last_assistant_text(&jsonl).as_deref(),
            Some("second")
        );
    }

    #[test]
    fn skips_non_content_records_while_finding_last_assistant() {
        let assistant = json!({
            "type": "assistant",
            "message": { "content": [{ "type": "text", "text": "answer" }] }
        });
        let summary = json!({ "type": "summary", "message": "summary" });
        let system = json!({ "type": "system", "message": "system" });
        let queue_operation = json!({ "type": "queue_operation" });
        let attachment = json!({ "type": "attachment" });
        let jsonl = format!("{system}\n{assistant}\n{summary}\n{queue_operation}\n{attachment}\n");

        assert_eq!(
            extract_last_assistant_text(&jsonl).as_deref(),
            Some("answer")
        );
    }

    #[test]
    fn empty_or_no_assistant_records_return_none() {
        assert_eq!(extract_last_assistant_text(""), None);
        assert_eq!(
            extract_last_assistant_text(
                &json!({
                    "type": "system",
                    "message": { "content": [{ "type": "text", "text": "ignored" }] }
                })
                .to_string()
            ),
            None
        );
    }

    #[test]
    fn skips_malformed_jsonl_lines() {
        let assistant = json!({
            "type": "assistant",
            "message": { "content": [{ "type": "text", "text": "after malformed" }] }
        });
        let jsonl = format!("{{not-json\n{assistant}\n");

        assert_eq!(
            extract_last_assistant_text(&jsonl).as_deref(),
            Some("after malformed")
        );
    }

    #[test]
    fn thinking_only_assistant_falls_back_to_earlier_assistant_text() {
        let earlier = json!({
            "type": "assistant",
            "message": { "content": [{ "type": "text", "text": "earlier" }] }
        });
        let thinking = json!({
            "type": "assistant",
            "message": { "content": [{ "type": "thinking", "thinking": "hidden" }] }
        });
        let jsonl = format!("{earlier}\n{thinking}\n");

        assert_eq!(
            extract_last_assistant_text(&jsonl).as_deref(),
            Some("earlier")
        );
    }

    #[test]
    fn non_stop_hook_does_not_enrich() {
        let body = json!({ "hook_event_name": "PreToolUse" }).to_string();

        assert_eq!(maybe_enrich_stop_payload(&body), None);
    }

    #[test]
    fn stop_hook_with_existing_last_assistant_message_does_not_overwrite() {
        let body = json!({
            "hook_event_name": "Stop",
            "last_assistant_message": "native",
            "transcript_path": "/does/not/matter"
        })
        .to_string();

        assert_eq!(maybe_enrich_stop_payload(&body), None);
    }

    #[test]
    fn stop_hook_with_null_last_assistant_message_injects_from_transcript() {
        let transcript = write_transcript("from transcript");
        let body = json!({
            "hook_event_name": "Stop",
            "last_assistant_message": null,
            "transcript_path": transcript.path()
        })
        .to_string();

        let enriched = maybe_enrich_stop_payload(&body).expect("enriched payload");
        let parsed: serde_json::Value = serde_json::from_str(&enriched).unwrap();
        assert_eq!(
            parsed
                .get("last_assistant_message")
                .and_then(serde_json::Value::as_str),
            Some("from transcript")
        );
    }

    #[test]
    fn stop_hook_with_empty_string_last_assistant_message_injects_from_transcript() {
        let transcript = write_transcript("from empty string");
        let body = json!({
            "hook_event_name": "Stop",
            "last_assistant_message": "",
            "transcript_path": transcript.path()
        })
        .to_string();

        let enriched = maybe_enrich_stop_payload(&body).expect("enriched payload");
        let parsed: serde_json::Value = serde_json::from_str(&enriched).unwrap();
        assert_eq!(
            parsed
                .get("last_assistant_message")
                .and_then(serde_json::Value::as_str),
            Some("from empty string")
        );
    }

    #[test]
    fn stop_hook_without_last_assistant_message_injects_from_transcript() {
        let transcript = write_transcript("from missing field");
        let body = json!({
            "hook_event_name": "Stop",
            "transcript_path": transcript.path()
        })
        .to_string();

        let enriched = maybe_enrich_stop_payload(&body).expect("enriched payload");
        let parsed: serde_json::Value = serde_json::from_str(&enriched).unwrap();
        assert_eq!(
            parsed
                .get("last_assistant_message")
                .and_then(serde_json::Value::as_str),
            Some("from missing field")
        );
    }

    #[test]
    fn stop_hook_with_missing_transcript_file_returns_none() {
        let body = json!({
            "hook_event_name": "Stop",
            "transcript_path": "/definitely/not/a/transcript.jsonl"
        })
        .to_string();

        assert_eq!(maybe_enrich_stop_payload(&body), None);
    }

    #[test]
    fn malformed_body_returns_none() {
        assert_eq!(maybe_enrich_stop_payload("not-json"), None);
    }

    fn write_transcript(text: &str) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().expect("tempfile");
        let record = json!({
            "type": "assistant",
            "message": { "content": [{ "type": "text", "text": text }] }
        });
        writeln!(file, "{record}").expect("write transcript");
        file
    }
}
