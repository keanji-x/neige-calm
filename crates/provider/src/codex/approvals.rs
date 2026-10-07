//! Planner approvals over the Codex app-server (#2348): what a Planner turn tells codex about
//! approvals, which threads' approval requests a harness holds, and how each request becomes one
//! question and each chosen option a JSON-RPC answer.
//!
//! The kernel reads none of this: a request reaches it as a [`HeldRequestMessage::Open`] with one
//! question, and an answer comes back as an option index through the [`HeldResponder`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use calm_types::event::{AskQuestion, clip_ask_title};
use calm_types::harness::PlannerPermissionMode;
use serde_json::{Map, Value, json};

use super::shared::home::WORKSPACE_WRITE_NETWORK_ACCESS;
use crate::held_requests::{
    ConnectionId, HeldRequestMessage, HeldRequestSender, HeldResponder, RequestKey,
};

/// What a `turn/start` tells codex about approvals. Codex keeps all three settings on the thread,
/// across turns and across a daemon restart, so a Planner turn always says all three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnApprovals {
    /// Say nothing: the thread keeps the settings it was started with. Worker turns.
    Unchanged,
    /// The settings of a conversation's permission mode, read when the turn is issued.
    Explicit(PlannerPermissionMode),
}

impl TurnApprovals {
    pub(super) fn insert_into(self, params: &mut Map<String, Value>) {
        let Self::Explicit(mode) = self else {
            return;
        };
        let policy = match mode {
            PlannerPermissionMode::Never => json!("never"),
            // Sandbox escapes and exec-policy rules ask; MCP tool approvals arrive as
            // `mcpServer/elicitation/request` whatever `mcp_elicitations` says.
            PlannerPermissionMode::Ask => json!({ "granular": {
                "sandbox_approval": true,
                "rules": true,
                "request_permissions": false,
                "mcp_elicitations": false,
                "skill_approval": false,
            }}),
        };
        params.insert("approvalPolicy".into(), policy);
        // A codex-home `approvals_reviewer = "guardian_subagent"` would hand every request to a
        // codex subagent that decides alone; the person is the reviewer.
        params.insert("approvalsReviewer".into(), json!("user"));
        // The sandbox the shared codex home configures, so neither mode widens or narrows it.
        params.insert(
            "sandboxPolicy".into(),
            json!({ "type": "workspaceWrite", "networkAccess": WORKSPACE_WRITE_NETWORK_ACCESS }),
        );
    }
}

/// The threads whose approval requests a harness holds, by thread id. Its owner keeps it across
/// every connection it opens, so a route outlives a reconnect; a thread with no route is refused.
#[derive(Default)]
pub struct ApprovalRoutes {
    threads: StdMutex<HashMap<String, Route>>,
    next_owner: AtomicU64,
}

struct Route {
    owner: u64,
    sender: HeldRequestSender,
}

impl ApprovalRoutes {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Route>> {
        self.threads
            .lock()
            .expect("codex approval routes mutex poisoned")
    }

    /// Send `thread_id`'s approval requests to `sender` until the returned route is dropped. A
    /// later route for the same thread takes over; dropping the earlier one then removes nothing.
    pub fn route(self: &Arc<Self>, thread_id: &str, sender: HeldRequestSender) -> ApprovalRoute {
        let owner = self.next_owner.fetch_add(1, Ordering::Relaxed);
        self.lock()
            .insert(thread_id.to_owned(), Route { owner, sender });
        ApprovalRoute {
            routes: Arc::clone(self),
            thread_id: thread_id.to_owned(),
            owner,
        }
    }

    fn unroute(&self, thread_id: &str, owner: u64) {
        let mut threads = self.lock();
        if threads
            .get(thread_id)
            .is_some_and(|route| route.owner == owner)
        {
            threads.remove(thread_id);
        }
    }

    pub(super) fn sender(&self, thread_id: &str) -> Option<HeldRequestSender> {
        self.lock().get(thread_id).map(|route| route.sender.clone())
    }
}

/// One harness's claim on one thread's approval requests; dropping it ends the claim.
pub struct ApprovalRoute {
    routes: Arc<ApprovalRoutes>,
    thread_id: String,
    owner: u64,
}

impl Drop for ApprovalRoute {
    fn drop(&mut self) {
        self.routes.unroute(&self.thread_id, self.owner);
    }
}

/// The options every approval offers, in this order.
const OPTIONS: [&str; 3] = ["Allow", "Allow for this session", "Deny"];

/// The server requests that are approvals; every other request is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ApprovalKind {
    CommandExecution,
    FileChange,
    McpToolCall,
}

/// One approval request, read off its frame.
pub(super) struct ApprovalRequest {
    pub(super) thread_id: String,
    pub(super) kind: ApprovalKind,
    title: String,
}

impl ApprovalRequest {
    /// `None` for a request that is not an approval, or names no thread.
    pub(super) fn parse(method: &str, params: &Value) -> Option<Self> {
        let kind = match method {
            "item/commandExecution/requestApproval" => ApprovalKind::CommandExecution,
            "item/fileChange/requestApproval" => ApprovalKind::FileChange,
            "mcpServer/elicitation/request"
                if params.pointer("/_meta/codex_approval_kind")
                    == Some(&json!("mcp_tool_call")) =>
            {
                ApprovalKind::McpToolCall
            }
            _ => return None,
        };
        let thread_id = params.get("threadId")?.as_str()?.to_owned();
        Some(Self {
            thread_id,
            kind,
            title: clip_ask_title(&kind.title(params)),
        })
    }

    pub(super) fn questions(&self) -> Vec<AskQuestion> {
        vec![AskQuestion {
            title: self.title.clone(),
            options: OPTIONS.iter().map(|option| (*option).to_owned()).collect(),
        }]
    }
}

/// A field's text, or `None` when it is absent, not text or blank. A command may also come as
/// its argument list.
fn text(params: &Value, key: &str) -> Option<String> {
    let text = match params.get(key)? {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|part| part.as_str())
            .collect::<Option<Vec<_>>>()?
            .join(" "),
        _ => return None,
    };
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

impl ApprovalKind {
    fn title(self, params: &Value) -> String {
        let mut lines = Vec::new();
        match self {
            Self::CommandExecution => {
                lines.push(match text(params, "command") {
                    Some(command) => format!("Run `{command}`?"),
                    None => "Run a command?".to_owned(),
                });
                if let Some(cwd) = text(params, "cwd") {
                    lines.push(format!("In {cwd}"));
                }
            }
            // The request names no path; the turn's items do.
            Self::FileChange => {
                lines.push("Apply a file change?".to_owned());
                if let Some(root) = text(params, "grantRoot") {
                    lines.push(format!("Grants write access under {root}"));
                }
            }
            Self::McpToolCall => {
                lines.push(text(params, "message").unwrap_or_else(|| "Run an MCP tool?".into()));
            }
        }
        if self != Self::McpToolCall
            && let Some(reason) = text(params, "reason")
        {
            lines.push(format!("Reason: {reason}"));
        }
        lines.join("\n")
    }

    /// The JSON-RPC result for the option at `option` of [`OPTIONS`]; any other index denies.
    pub(super) fn answer(self, option: usize) -> Value {
        match (self, option) {
            (Self::CommandExecution | Self::FileChange, 0) => json!({ "decision": "accept" }),
            (Self::CommandExecution | Self::FileChange, 1) => {
                json!({ "decision": "acceptForSession" })
            }
            (Self::McpToolCall, 0) => json!({ "action": "accept", "content": {} }),
            (Self::McpToolCall, 1) => json!({
                "action": "accept",
                "content": {},
                "_meta": { "persist": "session" },
            }),
            _ => self.deny(),
        }
    }

    /// The answer a request gets when nobody chose an option.
    pub(super) fn deny(self) -> Value {
        match self {
            Self::CommandExecution | Self::FileChange => json!({ "decision": "decline" }),
            Self::McpToolCall => json!({ "action": "decline", "content": null }),
        }
    }
}

/// Where a responder's answer goes: one JSON-RPC result frame for one request id.
pub(super) trait ReplyPort: Send + 'static {
    fn reply(&self, frame: Value);
}

/// Answers one approval request; dropped unanswered, it denies it.
pub(super) struct ApprovalResponder<P: ReplyPort> {
    port: P,
    id: Value,
    kind: ApprovalKind,
    answered: bool,
}

impl<P: ReplyPort> ApprovalResponder<P> {
    pub(super) fn new(port: P, id: Value, kind: ApprovalKind) -> Self {
        Self {
            port,
            id,
            kind,
            answered: false,
        }
    }

    fn send(&self, result: Value) {
        self.port
            .reply(json!({ "jsonrpc": "2.0", "id": self.id, "result": result }));
    }
}

impl<P: ReplyPort> HeldResponder for ApprovalResponder<P> {
    fn respond(mut self: Box<Self>, option: usize) {
        self.answered = true;
        self.send(self.kind.answer(option));
    }
}

impl<P: ReplyPort> Drop for ApprovalResponder<P> {
    fn drop(&mut self) {
        if !self.answered {
            self.send(self.kind.deny());
        }
    }
}

/// The approval requests one connection handed to harnesses. Owned by the connection's reader:
/// when the reader ends, aborted or not, every harness it handed a request is told.
pub(super) struct HandedRequests {
    connection: ConnectionId,
    open: HashMap<RequestKey, HeldRequestSender>,
    told: Vec<HeldRequestSender>,
}

/// Connections are numbered for the life of the process.
static NEXT_CONNECTION: AtomicU64 = AtomicU64::new(1);

impl HandedRequests {
    pub(super) fn new() -> Self {
        let n = NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed);
        Self {
            connection: ConnectionId(format!("codex-connection-{n}")),
            open: HashMap::new(),
            told: Vec::new(),
        }
    }

    fn key(&self, id: &Value) -> RequestKey {
        RequestKey(format!("{}#{id}", self.connection.0))
    }

    /// Hand `request` to `sender`. A harness that is gone hands the message back; dropping it
    /// drops the responder, which denies the request.
    pub(super) fn open(
        &mut self,
        sender: HeldRequestSender,
        id: &Value,
        request: &ApprovalRequest,
        responder: Box<dyn HeldResponder>,
    ) {
        let request_key = self.key(id);
        let opened = sender.send(HeldRequestMessage::Open {
            request_key: request_key.clone(),
            connection: self.connection.clone(),
            questions: request.questions(),
            responder,
        });
        if opened.is_ok() {
            if !self.told.iter().any(|told| told.same_channel(&sender)) {
                self.told.push(sender.clone());
            }
            self.open.insert(request_key, sender);
        }
    }

    /// Codex settled `serverRequest/resolved`'s request: answered, or the turn ended.
    pub(super) fn resolved(&mut self, params: &Value) {
        let Some(id) = params.get("requestId") else {
            return;
        };
        let request_key = self.key(id);
        if let Some(sender) = self.open.remove(&request_key) {
            let _ = sender.send(HeldRequestMessage::Gone { request_key });
        }
    }
}

impl Drop for HandedRequests {
    fn drop(&mut self) {
        for sender in self.told.drain(..) {
            let _ = sender.send(HeldRequestMessage::ConnectionLost {
                connection: self.connection.clone(),
            });
        }
    }
}

#[cfg(test)]
#[path = "approvals_tests.rs"]
mod tests;
