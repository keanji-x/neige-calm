//! Worker-flow vocabulary — the normalized agent-activity stream (#695 PR1).
//!
//! [`WorkerFlowItem`] is the provider-agnostic unit a flow source emits while
//! *passively* draining a worker's wire (Codex/Claude transcript bytes,
//! tool-call traffic, exec output). It is the read-model counterpart to
//! [`crate::observation::Observation`]: observations are the kernel→agent push
//! direction; flow items are the agent→read-model capture direction. The
//! capture-seam traits live in calm-exec; this crate only owns the shape.
//!
//! Like the rest of calm-types it is pure data and IO-free. **Not TS-exported
//! and not `ToSchema`** — PR1 keeps the vocabulary off the wire (no
//! `generated-events.ts` / OpenAPI churn); a later PR surfaces it deliberately.
//!
//! Every variant of [`WorkerFlowItem`] carries a `#[serde(flatten)]`
//! [`FlowEnvelope`] (sequencing + provenance) followed by its payload. Unions
//! are internally tagged with a camelCase `"type"` discriminator.

use serde::{Deserialize, Serialize};

use crate::worker::{WorkerProviderKind, WorkerSessionId};

// ---------------------------------------------------------------------------
// ToolCallId
// ---------------------------------------------------------------------------

/// Provider-issued tool/function-call identifier. Same opaque
/// `#[serde(transparent)]` newtype pattern as [`crate::ids`] — the wire shape
/// stays a bare string.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolCallId(pub String);

impl ToolCallId {
    /// Borrow the underlying string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for ToolCallId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for ToolCallId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl std::fmt::Display for ToolCallId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

// ---------------------------------------------------------------------------
// FlowEnvelope + RawRef
// ---------------------------------------------------------------------------

/// Sequencing + provenance flattened onto every [`WorkerFlowItem`]. `seq`
/// orders items within a session; `turn` groups them by agent turn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FlowEnvelope {
    pub seq: u64,
    pub turn: u32,
    pub session_id: WorkerSessionId,
    pub provider: WorkerProviderKind,
    pub timestamp: Option<i64>,
    pub source_uuid: Option<String>,
    pub provider_extra: Option<serde_json::Value>,
    pub raw_ref: Option<RawRef>,
}

/// Back-pointer to the raw provider record a flow item was normalized from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RawRef {
    pub provider: WorkerProviderKind,
    pub source_path: Option<String>,
    pub line: Option<u64>,
    pub record_type: Option<String>,
}

// ---------------------------------------------------------------------------
// WorkerFlowItem
// ---------------------------------------------------------------------------

/// One normalized, provider-agnostic item in a worker's activity stream.
///
/// Flat union: each variant flattens a [`FlowEnvelope`] then carries its
/// payload. Internally tagged with a camelCase `"type"` discriminator.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WorkerFlowItem {
    UserMessage {
        #[serde(flatten)]
        env: FlowEnvelope,
        content: Vec<MessageBlock>,
    },
    AgentMessage {
        #[serde(flatten)]
        env: FlowEnvelope,
        text: String,
        is_final: bool,
        phase: Option<String>,
    },
    Reasoning {
        #[serde(flatten)]
        env: FlowEnvelope,
        summary: Vec<String>,
        content: Vec<String>,
        redacted: bool,
    },
    ToolCall {
        #[serde(flatten)]
        env: FlowEnvelope,
        call_id: ToolCallId,
        name: String,
        input: serde_json::Value,
        input_summary: Option<String>,
    },
    ToolResult {
        #[serde(flatten)]
        env: FlowEnvelope,
        call_id: ToolCallId,
        ok: bool,
        output: Vec<ToolResultBlock>,
        output_summary: Option<String>,
        error: Option<ToolError>,
    },
    CommandExecution {
        #[serde(flatten)]
        env: FlowEnvelope,
        call_id: Option<ToolCallId>,
        command: String,
        cwd: Option<String>,
        parsed_actions: Vec<CommandAction>,
        aggregated_output: Option<String>,
        exit_code: Option<i32>,
        duration_ms: Option<i64>,
        status: ExecStatus,
        source: ExecSource,
    },
    FileChange {
        #[serde(flatten)]
        env: FlowEnvelope,
        call_id: Option<ToolCallId>,
        changes: Vec<FileEdit>,
        status: PatchStatus,
    },
    McpToolCall {
        #[serde(flatten)]
        env: FlowEnvelope,
        call_id: ToolCallId,
        server: Option<String>,
        tool: String,
        arguments: serde_json::Value,
        status: McpStatus,
        result: Option<serde_json::Value>,
        error: Option<ToolError>,
        duration_ms: Option<i64>,
    },
    WebSearch {
        #[serde(flatten)]
        env: FlowEnvelope,
        call_id: Option<ToolCallId>,
        query: Option<String>,
        results_summary: Option<String>,
    },
    Plan {
        #[serde(flatten)]
        env: FlowEnvelope,
        entries: Vec<PlanEntry>,
    },
    Subagent {
        #[serde(flatten)]
        env: FlowEnvelope,
        child_session_id: Option<WorkerSessionId>,
        tool: Option<String>,
        prompt: Option<String>,
        model: Option<String>,
        status: Option<String>,
    },
    Compaction {
        #[serde(flatten)]
        env: FlowEnvelope,
        reason: Option<String>,
        summary: Option<String>,
    },
    ReviewBoundary {
        #[serde(flatten)]
        env: FlowEnvelope,
        kind: ReviewKind,
        label: Option<String>,
    },
    Unknown {
        #[serde(flatten)]
        env: FlowEnvelope,
        raw_type: String,
    },
}

impl WorkerFlowItem {
    /// Borrow the [`FlowEnvelope`] flattened onto every variant.
    pub fn env(&self) -> &FlowEnvelope {
        match self {
            WorkerFlowItem::UserMessage { env, .. }
            | WorkerFlowItem::AgentMessage { env, .. }
            | WorkerFlowItem::Reasoning { env, .. }
            | WorkerFlowItem::ToolCall { env, .. }
            | WorkerFlowItem::ToolResult { env, .. }
            | WorkerFlowItem::CommandExecution { env, .. }
            | WorkerFlowItem::FileChange { env, .. }
            | WorkerFlowItem::McpToolCall { env, .. }
            | WorkerFlowItem::WebSearch { env, .. }
            | WorkerFlowItem::Plan { env, .. }
            | WorkerFlowItem::Subagent { env, .. }
            | WorkerFlowItem::Compaction { env, .. }
            | WorkerFlowItem::ReviewBoundary { env, .. }
            | WorkerFlowItem::Unknown { env, .. } => env,
        }
    }

    /// The tool-call id this item is keyed by, when it has one. Variants that
    /// never carry a call id (messages, reasoning, plans, …) return `None`,
    /// as do the call-id-optional variants whose field is absent.
    pub fn call_id(&self) -> Option<&ToolCallId> {
        match self {
            WorkerFlowItem::ToolCall { call_id, .. }
            | WorkerFlowItem::ToolResult { call_id, .. }
            | WorkerFlowItem::McpToolCall { call_id, .. } => Some(call_id),
            WorkerFlowItem::CommandExecution { call_id, .. }
            | WorkerFlowItem::FileChange { call_id, .. }
            | WorkerFlowItem::WebSearch { call_id, .. } => call_id.as_ref(),
            WorkerFlowItem::UserMessage { .. }
            | WorkerFlowItem::AgentMessage { .. }
            | WorkerFlowItem::Reasoning { .. }
            | WorkerFlowItem::Plan { .. }
            | WorkerFlowItem::Subagent { .. }
            | WorkerFlowItem::Compaction { .. }
            | WorkerFlowItem::ReviewBoundary { .. }
            | WorkerFlowItem::Unknown { .. } => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

/// A block inside a user message (mirrors Codex/Claude multi-part input).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum MessageBlock {
    Text {
        text: String,
    },
    Image {
        url: Option<String>,
        path: Option<String>,
    },
    FileRef {
        path: String,
    },
    Mention {
        name: String,
        path: Option<String>,
    },
}

/// A block inside a tool result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ToolResultBlock {
    Text { text: String },
    Image { url: Option<String> },
    FileRef { path: String },
}

/// Structured tool/exec error payload.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolError {
    pub message: String,
    pub kind: Option<String>,
}

/// A parsed intent recognized inside a shell command (best-effort).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum CommandAction {
    Read {
        command: String,
        name: String,
        path: String,
    },
    ListFiles {
        command: String,
        path: Option<String>,
    },
    Search {
        command: String,
        query: Option<String>,
        path: Option<String>,
    },
    Unknown {
        command: String,
    },
}

/// Lifecycle of a command execution.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExecStatus {
    InProgress,
    Completed,
    Failed,
    Declined,
}

/// Who initiated a command execution.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExecSource {
    Agent,
    UserShell,
    Unknown,
}

/// One file mutation inside a [`WorkerFlowItem::FileChange`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileEdit {
    pub path: String,
    pub kind: FileChangeKind,
    pub diff: Option<String>,
}

/// The kind of edit applied to a file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FileChangeKind {
    Add,
    Delete,
    Update { move_path: Option<String> },
}

/// Lifecycle of a patch/file-change apply.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PatchStatus {
    InProgress,
    Completed,
    Failed,
    Declined,
}

/// Lifecycle of an MCP tool call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum McpStatus {
    InProgress,
    Completed,
    Failed,
}

/// One entry in an agent plan/todo list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlanEntry {
    pub text: String,
    pub status: PlanStatus,
}

/// Lifecycle of a plan entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PlanStatus {
    Pending,
    InProgress,
    Completed,
}

/// Whether a review boundary marks entry into or exit from a review phase.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewKind {
    Entered,
    Exited,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env() -> FlowEnvelope {
        FlowEnvelope {
            seq: 7,
            turn: 2,
            session_id: WorkerSessionId::from("sess-1"),
            provider: WorkerProviderKind::Codex,
            timestamp: Some(1_700_000_000),
            source_uuid: Some("u-1".to_string()),
            provider_extra: None,
            raw_ref: None,
        }
    }

    fn assert_round_trip(item: &WorkerFlowItem) {
        let value = serde_json::to_value(item).unwrap();
        let back: WorkerFlowItem = serde_json::from_value(value).unwrap();
        assert_eq!(&back, item);
    }

    #[test]
    fn user_message_round_trips_and_tag_is_camel_case() {
        let item = WorkerFlowItem::UserMessage {
            env: env(),
            content: vec![
                MessageBlock::Text {
                    text: "hello".to_string(),
                },
                MessageBlock::FileRef {
                    path: "src/lib.rs".to_string(),
                },
            ],
        };
        assert_round_trip(&item);

        let value = serde_json::to_value(&item).unwrap();
        // Internally-tagged camelCase discriminator.
        assert_eq!(value["type"], json!("userMessage"));
        // The envelope is flattened to the top level, not nested under `env`.
        assert_eq!(value["seq"], json!(7));
        assert!(value.get("env").is_none());
    }

    #[test]
    fn command_execution_round_trips() {
        let item = WorkerFlowItem::CommandExecution {
            env: env(),
            call_id: Some(ToolCallId::from("call-9")),
            command: "ls -la".to_string(),
            cwd: Some("/tmp".to_string()),
            parsed_actions: vec![CommandAction::ListFiles {
                command: "ls -la".to_string(),
                path: Some("/tmp".to_string()),
            }],
            aggregated_output: Some("total 0".to_string()),
            exit_code: Some(0),
            duration_ms: Some(12),
            status: ExecStatus::Completed,
            source: ExecSource::Agent,
        };
        assert_round_trip(&item);

        let value = serde_json::to_value(&item).unwrap();
        assert_eq!(value["type"], json!("commandExecution"));
        assert_eq!(value["status"], json!("completed"));
        assert_eq!(value["call_id"], json!("call-9"));
    }

    #[test]
    fn file_change_round_trips() {
        let item = WorkerFlowItem::FileChange {
            env: env(),
            call_id: None,
            changes: vec![
                FileEdit {
                    path: "a.rs".to_string(),
                    kind: FileChangeKind::Add,
                    diff: None,
                },
                FileEdit {
                    path: "b.rs".to_string(),
                    kind: FileChangeKind::Update {
                        move_path: Some("c.rs".to_string()),
                    },
                    diff: Some("@@ -1 +1 @@".to_string()),
                },
            ],
            status: PatchStatus::Completed,
        };
        assert_round_trip(&item);

        let value = serde_json::to_value(&item).unwrap();
        assert_eq!(value["type"], json!("fileChange"));
        // Nested union tag is camelCase too.
        assert_eq!(value["changes"][1]["kind"]["type"], json!("update"));
    }

    #[test]
    fn unknown_round_trips() {
        let item = WorkerFlowItem::Unknown {
            env: env(),
            raw_type: "some.future.provider.event".to_string(),
        };
        assert_round_trip(&item);

        let value = serde_json::to_value(&item).unwrap();
        assert_eq!(value["type"], json!("unknown"));
        assert_eq!(value["turn"], json!(2));
    }

    #[test]
    fn env_and_call_id_accessors() {
        let with_call = WorkerFlowItem::ToolCall {
            env: env(),
            call_id: ToolCallId::from("call-1"),
            name: "grep".to_string(),
            input: json!({"pattern": "fn"}),
            input_summary: None,
        };
        assert_eq!(with_call.env().seq, 7);
        assert_eq!(with_call.call_id(), Some(&ToolCallId::from("call-1")));

        let without_call = WorkerFlowItem::Plan {
            env: env(),
            entries: vec![PlanEntry {
                text: "do the thing".to_string(),
                status: PlanStatus::Pending,
            }],
        };
        assert_eq!(without_call.call_id(), None);

        let optional_absent = WorkerFlowItem::WebSearch {
            env: env(),
            call_id: None,
            query: Some("rust serde flatten".to_string()),
            results_summary: None,
        };
        assert_eq!(optional_absent.call_id(), None);
    }
}
