//! The plugin's stdio MCP: newline-delimited JSON-RPC with the kernel.

use serde_json::{Map, Value, json};

/// The JSON-RPC code for a request before `initialize`.
pub const NOT_INITIALIZED: i64 = -32002;

/// One frame from the kernel.
pub enum Inbound {
    /// A request that needs an answer.
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    /// A notification, or a reply to nothing this plugin asked.
    Other,
}

pub fn parse(line: &str) -> Result<Inbound, String> {
    let frame: Value = serde_json::from_str(line).map_err(|e| format!("bad JSON: {e}"))?;
    let (Some(id), Some(method)) = (
        frame.get("id").cloned(),
        frame.get("method").and_then(Value::as_str),
    ) else {
        return Ok(Inbound::Other);
    };
    Ok(Inbound::Request {
        id,
        method: method.to_owned(),
        params: frame.get("params").cloned().unwrap_or(Value::Null),
    })
}

pub fn ok(id: &Value, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

pub fn error(id: &Value, code: i64, message: &str, data: Option<Value>) -> String {
    let mut error = json!({ "code": code, "message": message });
    if let Some(data) = data {
        error["data"] = data;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": error }).to_string()
}

/// `initialize`'s configuration, `_meta["dev.neige/config"].values`.
pub fn config_values(params: &Value) -> Result<&Map<String, Value>, String> {
    params
        .pointer("/_meta/dev.neige~1config/values")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            "initialize carries no _meta[\"dev.neige/config\"].values; \
             this plugin needs a kernel that delivers its configuration"
                .into()
        })
}

/// The `initialize` result: the kernel's protocol version, and the auth echo
/// the kernel checks (`docs/architecture/plugin-handshake-meta.md`).
pub fn initialize_result(params: &Value, token: &str) -> Value {
    json!({
        "protocolVersion": params.get("protocolVersion").cloned().unwrap_or(Value::Null),
        "serverInfo": { "name": "desktop", "version": env!("CARGO_PKG_VERSION") },
        "capabilities": { "tools": {} },
        "_meta": { "dev.neige/auth": { "echoed_token": token } },
    })
}

/// `tools/call` params: the tool name and its arguments.
pub fn tool_call(params: &Value) -> Result<(String, Value), String> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or("tools/call: missing `name`")?;
    let arguments = match params.get("arguments") {
        None | Some(Value::Null) => json!({}),
        Some(args @ Value::Object(_)) => args.clone(),
        Some(_) => return Err("tools/call: `arguments` must be an object".into()),
    };
    Ok((name.to_owned(), arguments))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_parse_into_requests_and_others() {
        let Inbound::Request { id, method, params } =
            parse(r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"x"}}"#)
                .unwrap()
        else {
            panic!("a request");
        };
        assert_eq!((id, method.as_str()), (json!(7), "tools/call"));
        assert_eq!(params["name"], "x");
        assert!(matches!(
            parse(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).unwrap(),
            Inbound::Other
        ));
        assert!(parse("{").is_err());
    }

    #[test]
    fn initialize_echoes_the_protocol_and_the_token() {
        let params = json!({ "protocolVersion": "2025-11-25",
            "_meta": { "dev.neige/config": { "values": { "a": 1 } } } });
        let result = initialize_result(&params, "tok");
        assert_eq!(result["protocolVersion"], "2025-11-25");
        assert_eq!(result["_meta"]["dev.neige/auth"]["echoed_token"], "tok");
        assert_eq!(config_values(&params).unwrap()["a"], 1);
        assert!(config_values(&json!({})).is_err());
    }

    #[test]
    fn tool_calls_need_a_name_and_object_arguments() {
        assert_eq!(
            tool_call(&json!({ "name": "window_ls" })).unwrap(),
            ("window_ls".into(), json!({}))
        );
        assert!(tool_call(&json!({ "arguments": {} })).is_err());
        assert!(tool_call(&json!({ "name": "x", "arguments": [1] })).is_err());
    }
}
