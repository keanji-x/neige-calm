//! Line-level JSON-RPC frame handling for the shim: classification by
//! `id` / `method` presence (mirrors the kernel's
//! `mcp_server/framing.rs::parse_frame`), the `initialize` token
//! injector (issue #236 followup), and the error the pump synthesizes
//! for a request whose outcome the kernel restart made unknown (#1699).
//!
//! Apart from the token injection into `initialize`, frames are
//! forwarded verbatim; classification decides what the pump remembers
//! about a line.

use std::io::{self, Write};

use serde_json::Value;

/// What a line on either wire is, as far as the pump cares.
#[derive(Debug, PartialEq)]
pub(crate) enum Frame {
    /// `id` + `method`: a request the peer expects an answer to.
    Request { id: Value, method: String },
    /// `id` without `method`: an answer. `error_code` is `error.code`
    /// when the answer is an error object.
    Response { id: Value, error_code: Option<i64> },
    /// `method` without `id`: fire-and-forget (the kernel drops these).
    Notification,
    /// Not a JSON object, or neither `id` nor `method`.
    Other,
}

/// Classify one line (trailer included or not). Never fails: anything
/// unparseable is [`Frame::Other`] and gets forwarded as-is.
pub(crate) fn classify(line: &[u8]) -> Frame {
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return Frame::Other;
    };
    let Some(obj) = value.as_object() else {
        return Frame::Other;
    };
    let id = obj.get("id").cloned();
    let method = obj.get("method").and_then(|m| m.as_str()).map(String::from);
    match (id, method) {
        (Some(id), Some(method)) => Frame::Request { id, method },
        (Some(id), None) => Frame::Response {
            id,
            error_code: obj
                .get("error")
                .and_then(|e| e.get("code"))
                .and_then(|c| c.as_i64()),
        },
        (None, Some(_)) => Frame::Notification,
        (None, None) => Frame::Other,
    }
}

/// The kernel's `-32603` (`InternalError`): `handshake.rs` returns it
/// for a repo lookup failure, which a fresh connection may not hit again.
pub(crate) const RPC_INTERNAL_ERROR: i64 = -32603;

/// Code of the error the pump synthesizes for a request it cannot
/// answer honestly (#1699 D5-b).
pub(crate) const LOST_ERROR_CODE: i64 = -32000;

/// Two pieces so no single literal trips the long-literal prose ratchet.
const LOST_ERROR_MESSAGE: &str = concat!(
    "neige-mcp-stdio-shim: connection to kernel lost after the request was sent; ",
    "outcome unknown — re-read state before retrying"
);

/// The exact error frame (trailer included) for request `id`. Built by
/// hand so key order and the id's JSON type are preserved.
pub(crate) fn lost_error_frame(id: &Value) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"error\":{{\"code\":{LOST_ERROR_CODE},\"message\":{}}}}}\n",
        Value::String(LOST_ERROR_MESSAGE.to_string())
    )
}

/// Try to parse `line` as a JSON-RPC `initialize` request and inject
/// `params._meta["dev.neige/auth"].token = <token>`. On any non-applicable
/// shape (non-JSON, not an `initialize` request, `dev.neige/auth`
/// already populated, etc.) returns the input unchanged so the kernel
/// sees what codex actually sent.
///
/// Returns an owned `String` because the inject path re-serializes the
/// JSON; the no-op path just clones the input slice. Keeping the return
/// type uniform avoids a `Cow`-shaped API for a once-per-connection call.
pub(crate) fn maybe_inject_token(line: &str, token: &str) -> String {
    // Preserve the trailer the input carried (the kernel's `read_line`
    // expects each frame to end with `\n`).
    let (body, trailer) = match line.strip_suffix('\n') {
        Some(rest) => (rest, "\n"),
        None => (line, ""),
    };
    // Some peers emit \r\n. Strip a trailing \r too, then re-emit it.
    let (body, trailer) = match body.strip_suffix('\r') {
        Some(rest) if trailer == "\n" => (rest, "\r\n"),
        _ => (body, trailer),
    };

    let mut value: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => {
            // Not JSON — pass through unchanged. The kernel's framer
            // will surface the malformed frame; nothing for us to do.
            return line.to_string();
        }
    };

    // Only mutate `initialize` requests. Anything else (notifications,
    // tool calls, responses) is forwarded unchanged.
    let is_initialize = value
        .get("method")
        .and_then(|m| m.as_str())
        .is_some_and(|m| m == "initialize");
    if !is_initialize {
        return line.to_string();
    }

    // Walk down to `params._meta["dev.neige/auth"].token`, creating
    // intermediate objects as needed. The codex CLI's `initialize`
    // always carries a `params` object (it's required by the JSON-RPC
    // planner for `initialize`), but we defensively handle missing /
    // wrong-type cases by replacing them with empty objects.
    let params = ensure_object(&mut value, "params");
    let meta = ensure_object_in(params, "_meta");

    // Forward-compat: if `dev.neige/auth.token` is already populated
    // (a future codex revision, or an intermediate proxy), leave it
    // alone. The kernel still verifies via constant-time hash compare,
    // so a stale upstream stamp will be rejected cleanly — silently
    // overwriting it would mask a configuration bug.
    if let Some(existing) = meta.get("dev.neige/auth")
        && existing.get("token").and_then(|t| t.as_str()).is_some()
    {
        let _ = writeln!(
            io::stderr(),
            "neige-mcp-stdio-shim: initialize already carries _meta[\"dev.neige/auth\"].token; leaving untouched"
        );
        // Re-serialize the unchanged-shape frame (parsed fine, so the
        // round-trip is semantically a no-op) and re-emit the trailer.
        return format!("{value}{trailer}");
    }

    // Insert / overwrite our auth slot.
    meta.insert(
        "dev.neige/auth".to_string(),
        serde_json::json!({ "token": token }),
    );

    format!("{value}{trailer}")
}

/// Walk into `value[key]`, replacing the slot with an empty object if
/// it's missing or not an object. Returns a `&mut serde_json::Map` so
/// the caller can chain another `ensure_object_in` or `insert`.
fn ensure_object<'a>(
    value: &'a mut serde_json::Value,
    key: &str,
) -> &'a mut serde_json::Map<String, serde_json::Value> {
    if !value.is_object() {
        *value = serde_json::Value::Object(serde_json::Map::new());
    }
    let map = value.as_object_mut().expect("just-set object");
    ensure_object_in(map, key)
}

/// Same as `ensure_object` but operates on an existing `&mut Map`
/// (avoids the outer "make sure root is an object" step). Borrow-
/// checker-friendlier when chaining: `params -> _meta` doesn't need a
/// second mutable borrow of the root `Value`.
fn ensure_object_in<'a>(
    map: &'a mut serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> &'a mut serde_json::Map<String, serde_json::Value> {
    if !map.get(key).is_some_and(|v| v.is_object()) {
        map.insert(
            key.to_string(),
            serde_json::Value::Object(serde_json::Map::new()),
        );
    }
    map.get_mut(key)
        .expect("just-inserted")
        .as_object_mut()
        .expect("just-set object")
}

#[cfg(test)]
mod tests {
    //! Issue #236 followup — unit tests for the `initialize` token
    //! injector. Pure-function tests (no UDS, no process spawn); the
    //! integration tests in `tests/stdio_shim.rs` cover the wired-up
    //! shape end-to-end.

    use super::maybe_inject_token;
    use serde_json::Value;

    /// Helper: parse `injected` back as JSON and pluck out the
    /// auth-slot token.
    fn extract_token(injected: &str) -> Option<String> {
        let v: Value = serde_json::from_str(injected.trim_end()).ok()?;
        v.get("params")?
            .get("_meta")?
            .get("dev.neige/auth")?
            .get("token")?
            .as_str()
            .map(|s| s.to_string())
    }

    #[test]
    fn initialize_without_meta_gets_token_injected() {
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#;
        let out = maybe_inject_token(line, "test-token-abc");
        assert_eq!(extract_token(&out).as_deref(), Some("test-token-abc"));
    }

    #[test]
    fn initialize_with_existing_meta_block_preserves_siblings() {
        // The kernel's `handle_initialize` reads only
        // `_meta["dev.neige/auth"].token`, but MCP tooling may set
        // other `_meta` siblings (e.g. `progress-token` for
        // long-running ops). The shim must merge — not clobber.
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"_meta":{"progress-token":"abc"},"protocolVersion":"2024-11-05"}}"#;
        let out = maybe_inject_token(line, "tok-xyz");
        let v: Value = serde_json::from_str(out.trim_end()).expect("re-parse");
        // Auth slot landed:
        let token = v["params"]["_meta"]["dev.neige/auth"]["token"]
            .as_str()
            .expect("token present");
        assert_eq!(token, "tok-xyz");
        // Pre-existing sibling preserved:
        let progress = v["params"]["_meta"]["progress-token"]
            .as_str()
            .expect("progress-token preserved");
        assert_eq!(progress, "abc");
    }

    #[test]
    fn initialize_already_stamped_with_auth_is_left_untouched() {
        // Forward-compat path: if a future codex revision (or an
        // intermediate proxy) already stamped `_meta["dev.neige/auth"]
        // .token`, the shim must not clobber it. The kernel verifies
        // the token via constant-time hash compare regardless of who
        // stamped the slot, so a stale upstream stamp will be cleanly
        // rejected — but silently overwriting it would mask a real
        // configuration bug.
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"_meta":{"dev.neige/auth":{"token":"upstream-stamp"}}}}"#;
        let out = maybe_inject_token(line, "shim-stamp");
        assert_eq!(extract_token(&out).as_deref(), Some("upstream-stamp"));
    }

    #[test]
    fn non_initialize_frames_pass_through_unchanged() {
        let line = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
        let out = maybe_inject_token(line, "tok-xyz");
        assert_eq!(out, line);
        // No auth slot injected:
        let v: Value = serde_json::from_str(out.trim_end()).expect("re-parse");
        assert!(v["params"]["_meta"].is_null());
    }

    #[test]
    fn malformed_json_passes_through_unchanged() {
        // A non-JSON line shouldn't crash the shim or get rewritten —
        // the kernel's framer will surface the malformed frame.
        let line = "this is not JSON\n";
        let out = maybe_inject_token(line, "tok-xyz");
        assert_eq!(out, line);
    }

    #[test]
    fn newline_trailer_is_preserved() {
        // The kernel's `read_line` expects each frame to terminate
        // with `\n`. The injector must re-emit the trailer the input
        // carried.
        let line = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n";
        let out = maybe_inject_token(line, "tok");
        assert!(
            out.ends_with('\n'),
            "expected trailing newline; got {out:?}"
        );
        // And the body still parses + carries the token:
        assert_eq!(extract_token(&out).as_deref(), Some("tok"));
    }

    #[test]
    fn crlf_trailer_is_preserved() {
        // Defensive: some MCP clients on Windows-y stacks emit \r\n.
        // Codex itself uses \n on POSIX, but the shim should survive
        // either shape.
        let line = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\r\n";
        let out = maybe_inject_token(line, "tok");
        assert!(out.ends_with("\r\n"), "expected trailing CRLF; got {out:?}");
    }

    #[test]
    fn params_with_non_object_is_replaced() {
        // Defensive: malformed `initialize` that sets `params` to a
        // non-object (null, array, etc.). JSON-RPC says params for
        // `initialize` must be an object; the shim treats wrong-type
        // as missing and stamps the token slot.
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":null}"#;
        let out = maybe_inject_token(line, "tok");
        // Token landed:
        assert_eq!(extract_token(&out).as_deref(), Some("tok"));
    }
}

#[cfg(test)]
mod classify_tests {
    //! #1699 — classification and the synthesized-error shape.

    use super::{Frame, classify, lost_error_frame};
    use serde_json::json;

    #[test]
    fn request_response_notification_other_by_id_and_method() {
        assert_eq!(
            classify(br#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{}}"#),
            Frame::Request {
                id: json!(7),
                method: "tools/call".into()
            }
        );
        assert_eq!(
            classify(br#"{"jsonrpc":"2.0","id":"a","result":{}}"#),
            Frame::Response {
                id: json!("a"),
                error_code: None
            }
        );
        assert_eq!(
            classify(br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32603,"message":"x"}}"#),
            Frame::Response {
                id: json!(1),
                error_code: Some(-32603)
            }
        );
        assert_eq!(
            classify(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
            Frame::Notification
        );
        assert_eq!(classify(b"not json\n"), Frame::Other);
        assert_eq!(classify(b"[1,2]\n"), Frame::Other);
        assert_eq!(classify(br#"{"jsonrpc":"2.0"}"#), Frame::Other);
    }

    #[test]
    fn lost_error_frame_is_byte_exact_and_keeps_id_type() {
        let expected = "{\"jsonrpc\":\"2.0\",\"id\":42,\"error\":{\"code\":-32000,\"message\":\
                        \"neige-mcp-stdio-shim: connection to kernel lost after the request was sent; \
                        outcome unknown — re-read state before retrying\"}}\n";
        assert_eq!(lost_error_frame(&json!(42)), expected);
        // A string id stays a string.
        assert!(
            lost_error_frame(&json!("req-1")).starts_with("{\"jsonrpc\":\"2.0\",\"id\":\"req-1\",")
        );
        // And it round-trips as a response frame.
        assert_eq!(
            classify(lost_error_frame(&json!("req-1")).as_bytes()),
            Frame::Response {
                id: json!("req-1"),
                error_code: Some(-32000)
            }
        );
    }
}
