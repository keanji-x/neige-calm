//! #2348 — one approval request a turn pauses on, scripted by a line of the turn's input text
//! like [`super::ReplyScript`]:
//!
//!   * `fake-approval: <kind>` — after `turn/started`, one `<kind>` approval request with the id
//!     `approval-<n>`, and the turn stays open. The client's answer is appended to
//!     `<sock>.approval-answers`; `serverRequest/resolved` and `turn/completed` follow, as codex
//!     sends them.
//!   * `fake-approval-resolved: <kind>` — the same request, then at once `serverRequest/resolved`
//!     for it (another subscriber answered it); the turn stays open. A later answer to it is
//!     recorded and otherwise ignored, as codex ignores it.
//!
//! `<kind>` is `command`, `file` or `mcp`.

use serde_json::{Value, json};

pub(super) struct ApprovalScript {
    kind: String,
    pub(super) resolve_at_once: bool,
}

impl ApprovalScript {
    pub(super) fn from_turn_start(req: &Value) -> Option<Self> {
        let input = req.pointer("/params/input")?.as_array()?;
        input
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .flat_map(str::lines)
            .find_map(|line| {
                let line = line.trim_start();
                let (resolve_at_once, kind) = match line.strip_prefix("fake-approval-resolved:") {
                    Some(kind) => (true, kind),
                    None => (false, line.strip_prefix("fake-approval:")?),
                };
                Some(Self {
                    kind: kind.trim().to_owned(),
                    resolve_at_once,
                })
            })
    }

    /// The request frame, as codex 0.159.2 sends it.
    pub(super) fn request(&self, id: &str, thread_id: &str, turn_id: &str) -> Value {
        let (method, params) = match self.kind.as_str() {
            "file" => (
                "item/fileChange/requestApproval",
                json!({ "threadId": thread_id, "turnId": turn_id, "itemId": "patch-1",
                        "reason": null, "grantRoot": "/outside" }),
            ),
            "mcp" => (
                "mcpServer/elicitation/request",
                json!({ "threadId": thread_id, "turnId": turn_id, "serverName": "neige",
                        "mode": "form", "message": "Allow the neige MCP server to run tool \"t\"?",
                        "_meta": { "codex_approval_kind": "mcp_tool_call" },
                        "requestedSchema": { "type": "object", "properties": {} } }),
            ),
            _ => (
                "item/commandExecution/requestApproval",
                json!({ "kind": "command", "threadId": thread_id, "turnId": turn_id,
                        "itemId": "call-1", "command": "cargo test --workspace", "cwd": "/work",
                        "reason": "needs the network" }),
            ),
        };
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }
}
