//! JSON-RPC framing for the kernel-as-MCP-server transport.
//!
//! PR7a (#136) — this is a thin shim over the line-delimited JSON helpers
//! the plugin host already uses (`crate::plugin_host::mcp::parse_frame`
//! and friends). The wire format is identical; only the direction
//! flips: in `plugin_host`, the kernel is the *client* talking to a
//! plugin server. Here, the kernel is the *server* and the codex daemon
//! (via `neige-mcp-stdio-shim`) is the client.
//!
//! Centralizing the framing helpers here:
//!
//!   * makes `mcp_server/transport.rs` and `mcp_server/handshake.rs`
//!     callers depend on this module rather than reaching across into
//!     `plugin_host::*` — keeps the layering one-directional;
//!   * gives PR7b/PR8 a single seat to add server-specific frame
//!     helpers (e.g. canceled-notification synthesizers) without
//!     bloating the plugin-host module.
//!
use serde_json::Value;

pub(crate) use crate::plugin_host::mcp::{
    RequestId, RpcError, build_error_response_frame, build_ok_response_frame,
};

/// Decoded JSON-RPC frame for the kernel-as-MCP-server direction.
///
/// This mirrors the plugin-host framing shape, with one server-specific
/// addition: request-level `_meta` is preserved separately from `params`
/// so per-request MCP metadata can flow down the dispatch path.
#[derive(Debug)]
pub(crate) enum Frame {
    Response {
        id: RequestId,
        _body: Result<Value, RpcError>,
    },
    Request {
        id: RequestId,
        method: String,
        params: Value,
        request_meta: Option<Value>,
    },
    Notification {
        method: String,
        _params: Value,
    },
}

pub(crate) fn parse_frame(s: &str) -> Result<Frame, String> {
    let v: Value = serde_json::from_str(s).map_err(|e| format!("json parse: {e}"))?;
    let obj = v
        .as_object()
        .ok_or_else(|| "frame is not an object".to_string())?;

    let _jsonrpc = obj.get("jsonrpc");

    let id = obj.get("id").cloned();
    let method = obj.get("method").and_then(|v| v.as_str()).map(String::from);

    match (id, method) {
        (Some(id_v), Some(m)) => {
            let id = serde_json::from_value::<RequestId>(id_v.clone())
                .map_err(|e| format!("invalid id: {e}"))?;
            let params = obj.get("params").cloned().unwrap_or(Value::Null);
            let request_meta = obj.get("_meta").cloned();
            Ok(Frame::Request {
                id,
                method: m,
                params,
                request_meta,
            })
        }
        (Some(id_v), None) => {
            let id = serde_json::from_value::<RequestId>(id_v.clone())
                .map_err(|e| format!("invalid id: {e}"))?;
            if let Some(err_v) = obj.get("error") {
                let rpc: RpcError = serde_json::from_value(err_v.clone())
                    .map_err(|e| format!("invalid error object: {e}"))?;
                Ok(Frame::Response {
                    id,
                    _body: Err(rpc),
                })
            } else if let Some(result_v) = obj.get("result") {
                Ok(Frame::Response {
                    id,
                    _body: Ok(result_v.clone()),
                })
            } else {
                Err("response has neither result nor error".into())
            }
        }
        (None, Some(m)) => {
            let params = obj.get("params").cloned().unwrap_or(Value::Null);
            Ok(Frame::Notification {
                method: m,
                _params: params,
            })
        }
        (None, None) => Err("frame has neither id nor method".into()),
    }
}
