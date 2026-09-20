//! JSON-RPC framing for the kernel-as-MCP-server transport: a thin shim over the line-delimited
//! JSON helpers the plugin host uses, with the direction flipped (the kernel is the server).
use serde_json::Value;

pub(crate) use crate::plugin_host::mcp::{
    RequestId, RpcError, build_error_response_frame, build_ok_response_frame,
};

/// Decoded JSON-RPC frame for the kernel-as-MCP-server direction; request-level `_meta` is
/// preserved separately from `params`.
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
