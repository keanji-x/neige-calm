//! Model-facing names for a Neige MCP client with a function-name length limit.
use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const PREFIX: &str = "tool_";
const HASH_BYTES: usize = 12;
const MIN_LIMIT: usize = PREFIX.len() + 2 * HASH_BYTES + 1;

pub(crate) struct ToolNames {
    limit: usize,
    aliases: HashMap<String, String>,
}

impl ToolNames {
    pub(crate) fn new(limit: usize) -> Option<Self> {
        (limit >= MIN_LIMIT).then(|| Self {
            limit,
            aliases: HashMap::new(),
        })
    }

    /// Neige lists the full session catalog in one response. Keep schemas and all other tool
    /// fields as returned by the kernel. Keep advertised aliases stable for the shim lifetime,
    /// including across relists and reconnects.
    pub(crate) fn list_reply(&mut self, line: Vec<u8>) -> Vec<u8> {
        let Ok(mut reply) = serde_json::from_slice::<Value>(&line) else {
            return line;
        };
        let Some(tools) = reply
            .pointer_mut("/result/tools")
            .and_then(Value::as_array_mut)
        else {
            return line;
        };
        let mut aliases = HashMap::new();
        let mut names = HashSet::new();
        for tool in tools {
            let Some(name) = tool.get_mut("name") else {
                continue;
            };
            let Some(original) = name.as_str() else {
                continue;
            };
            let presented = if original.len() > self.limit {
                self.short_name(original)
            } else {
                original.to_owned()
            };
            if !names.insert(presented.clone())
                || self
                    .aliases
                    .get(&presented)
                    .is_some_and(|old| old != original)
            {
                return frame(&json!({
                    "jsonrpc":"2.0",
                    "id":reply["id"],
                    "error":{"code":-32603,"message":"ambiguous MCP tool aliases"}
                }));
            }
            if presented != original {
                aliases.insert(presented.clone(), original.to_owned());
                *name = Value::String(presented);
            }
        }
        let changed = !aliases.is_empty();
        self.aliases.extend(aliases);
        if changed { frame(&reply) } else { line }
    }

    /// Only a name from a correlated list reply can be expanded. The kernel still admits and
    /// authorizes the original call on the same card-bound socket.
    pub(crate) fn call(&self, line: Vec<u8>) -> Vec<u8> {
        if self.aliases.is_empty() {
            return line;
        }
        let Ok(mut request) = serde_json::from_slice::<Value>(&line) else {
            return line;
        };
        let Some(name) = request.pointer_mut("/params/name") else {
            return line;
        };
        let Some(original) = name.as_str().and_then(|name| self.aliases.get(name)) else {
            return line;
        };
        *name = Value::String(original.clone());
        frame(&request)
    }

    fn short_name(&self, name: &str) -> String {
        let digest = Sha256::digest(name.as_bytes());
        let hash: String = digest[..HASH_BYTES]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let suffix: String = name
            .chars()
            .rev()
            .take(self.limit - MIN_LIMIT)
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("{PREFIX}{hash}_{suffix}")
    }
}

fn frame(value: &Value) -> Vec<u8> {
    format!("{value}\n").into_bytes()
}
