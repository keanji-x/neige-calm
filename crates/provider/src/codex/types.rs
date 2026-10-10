//! Codex JSON-RPC wire vocabulary and notification decoding.

use super::*;

/// `clientInfo` block for `initialize`. Required by the schema.
#[derive(Debug, Clone, Serialize)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

/// All methods we call are `[experimental]`, so `experimentalApi` is always true.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct InitializeCapabilities {
    pub(super) experimental_api: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct InitializeParams {
    pub(super) client_info: ClientInfo,
    pub(super) capabilities: InitializeCapabilities,
}

/// `initialize` result; tolerates extra fields.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct InitializeResult {
    pub user_agent: String,
    pub codex_home: String,
    pub platform_family: String,
    pub platform_os: String,
}

#[derive(Clone)]
pub struct ThreadStartParams {
    pub cwd: String,
    pub approval_policy: String,
    pub sandbox_mode: String,
    pub developer_instructions: Option<String>,
    pub config: Option<serde_json::Value>,
}

impl std::fmt::Debug for ThreadStartParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThreadStartParams")
            .field("cwd", &self.cwd)
            .field("approval_policy", &self.approval_policy)
            .field("sandbox_mode", &self.sandbox_mode)
            .field("developer_instructions", &self.developer_instructions)
            .field("config", &redact_thread_start_config(&self.config))
            .finish()
    }
}

pub fn redact_thread_start_config(cfg: &Option<serde_json::Value>) -> serde_json::Value {
    let Some(cfg) = cfg else {
        return Value::Null;
    };
    let mut redacted = cfg.clone();
    if let Some(policy) = redacted
        .pointer_mut("/shell_environment_policy")
        .and_then(Value::as_object_mut)
    {
        for value in policy.values_mut() {
            if let Value::Object(map) = value {
                for value in map.values_mut() {
                    if value.is_string() {
                        *value = Value::String("[REDACTED]".into());
                    }
                }
            }
        }
    }
    if let Some(servers) = redacted
        .get_mut("mcp_servers")
        .and_then(Value::as_object_mut)
    {
        for server in servers.values_mut() {
            if let Some(map) = server.as_object_mut() {
                for key in ["env", "http_headers", "bearer_token", "env_http_headers"] {
                    if let Some(value) = map.get_mut(key) {
                        *value = json!("[REDACTED]");
                    }
                }
            }
        }
    }
    redacted
}

/// `thread/start` / `thread/resume` result; only `thread.id` and `model` are ever read.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ThreadResult {
    /// Raw `thread` object from the server.
    pub thread: Value,
    /// Resolved model (e.g. `gpt-5.5`); `None` when the answer names none.
    pub model: Option<String>,
}

impl ThreadResult {
    /// The thread id (`thread.id`); `None` only if the server returned a shape without it.
    pub fn thread_id(&self) -> Option<&str> {
        self.thread.get("id").and_then(Value::as_str)
    }
}

/// `turn/start` result — `{ "turn": { "id": …, … } }`.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct TurnStartResult {
    /// Raw `turn` object; we expose its id via [`TurnStartResult::turn_id`].
    pub turn: Value,
}

impl TurnStartResult {
    /// Needed as `expectedTurnId` for `turn/steer` and as `turnId` for `turn/interrupt`.
    pub fn turn_id(&self) -> Option<&str> {
        self.turn.get("id").and_then(Value::as_str)
    }
}

/// `turn/steer` result — `{ "turnId": "…" }`.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct TurnSteerResult {
    pub turn_id: String,
}

// `thread/read` + `thread/loaded/list` responses: narrowed mirrors of upstream `app-server-protocol` v2, defining only the fields the death arbiter and the rollout capture read.

/// Upstream `ThreadStatus`: internally tagged on `type`, camelCase variants. The arbiter keys on `Active`; the other arms mean "no turn running".
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ThreadStatus {
    NotLoaded,
    Idle,
    SystemError,
    #[serde(rename_all = "camelCase")]
    Active {
        active_flags: Vec<ThreadActiveFlag>,
    },
}

/// Upstream `ThreadActiveFlag`: either flag on an `Active` thread means blocked on a human — never reap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ThreadActiveFlag {
    WaitingOnApproval,
    WaitingOnUserInput,
}

/// `thread/read` response, narrowed to the fields the arbiter and the rollout capture need.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ThreadReadResponse {
    pub thread: ThreadView,
}

/// Narrowed mirror of upstream `Thread`. Upstream `turns` is a non-optional `Vec` (empty when not requested); `Option` + `#[serde(default)]` lets both `[]` and an absent field parse, and the arbiter treats both as "no turns".
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadView {
    pub status: ThreadStatus,
    #[serde(default)]
    pub turns: Option<Vec<TurnView>>,
    /// Upstream `path: string | null`, marked `[UNSTABLE]` there: the thread's rollout file on
    /// disk, known before the file exists. `null` for a thread with no rollout (an ephemeral one).
    #[serde(default)]
    pub path: Option<String>,
}

/// Narrowed mirror of upstream `Turn`: `completedAt` (`null` = died mid-turn) and the turn's
/// own `status`, which upstream requires.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnView {
    #[serde(default)]
    pub completed_at: Option<i64>,
    pub status: TurnStatus,
}

/// Mirror of upstream `TurnStatus`. The field stays required; `Unknown` only absorbs a value
/// this build does not model, so one new upstream status cannot fail the whole `thread/read`
/// and silence the liveness recheck (#1813).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnStatus {
    Completed,
    Interrupted,
    Failed,
    InProgress,
    #[serde(other)]
    Unknown,
}

/// `thread/loaded/list` response; only `data` is kept, the pagination cursor is dropped.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ThreadLoadedListResponse {
    pub data: Vec<String>,
}

/// `thread/unsubscribe` response: `notLoaded`, `notSubscribed` or `unsubscribed`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ThreadUnsubscribeResponse {
    pub status: String,
}

/// One page of `model/list`; `SharedCodexAppServer::model_list` drains the pages.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelListPage {
    /// Left undecoded on purpose: the catalog versions independently of us, so one unreadable preset must not empty the page — the caller decodes entries one by one.
    pub data: Vec<Value>,
    /// `None` (or an empty string) means "no further pages".
    pub next_cursor: Option<String>,
}

/// One catalog entry, narrowed to the fields `GET /api/models` proxies.
/// `id` is the preset identifier and `model` the slug the model is invoked by; only `model` may ever reach `turn/start` or `cards.payload_json`. `id` travels outward for presentation and must never come back as a selection.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexModel {
    pub id: String,
    pub model: String,
    pub display_name: String,
    pub description: String,
    pub supported_reasoning_efforts: Vec<CodexReasoningEffortOption>,
    /// Bare string, not a closed enum: codex's `ReasoningEffort` accepts any non-empty string, and a closed enum here would fail the whole catalog on a new effort.
    pub default_reasoning_effort: String,
    /// Which entry the picker highlights, NOT which model this installation follows (that comes from `config/read`).
    pub is_default: bool,
}

/// One selectable reasoning effort for a model; `description` is codex's copy.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexReasoningEffortOption {
    /// Bare string for the same reason as [`CodexModel::default_reasoning_effort`].
    pub reasoning_effort: String,
    pub description: String,
}

/// `config/read` response, narrowed to `config`. The envelope is camelCase but the wrapped `Config` is snake_case: writing `modelReasoningEffort` here parses to `None` forever, silently.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigReadResponse {
    pub config: CodexConfig,
}

/// `account/read` narrowed to what "logged in" is decided from (#1817). `account` is decoded as
/// present-or-null only, so the account identity codex returns (email, plan) is never kept.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountRead {
    #[serde(deserialize_with = "present_or_null")]
    account: bool,
    requires_openai_auth: bool,
}

impl AccountRead {
    /// Codex's own rule: an account is present, or this configuration needs no OpenAI auth.
    pub fn logged_in(&self) -> bool {
        self.account || !self.requires_openai_auth
    }

    #[cfg(feature = "fixtures")]
    pub fn for_test(account: bool, requires_openai_auth: bool) -> Self {
        Self {
            account,
            requires_openai_auth,
        }
    }
}

/// `true` for any non-null value, without keeping it.
fn present_or_null<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<bool, D::Error> {
    Ok(Option::<serde::de::IgnoredAny>::deserialize(deserializer)?.is_some())
}

/// The layer-merged effective config, narrowed to the two keys the model picker needs; field names are snake_case verbatim. Both are genuinely optional on codex's side.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct CodexConfig {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub model_reasoning_effort: Option<String>,
}

/// A server→client notification; anything not modeled lands in [`Notification::Other`] so codex version drift never breaks the consumer.
#[derive(Debug, Clone)]
pub enum Notification {
    /// `thread/started` — a thread was created/loaded on this connection.
    ThreadStarted { params: Value },
    /// `thread/status/changed` — the raw status `Value` (`{ "type": "idle" | "active" | ... }`) plus the thread id.
    ThreadStatusChanged { thread_id: String, status: Value },
    /// `turn/started` — carries `threadId` + the full `turn` object.
    TurnStarted { thread_id: String, turn: Value },
    /// `turn/completed` — the terminal event of a turn.
    TurnCompleted { thread_id: String, turn: Value },
    /// Any `item/*` event; the exact method is preserved so a consumer can branch on it.
    Item { method: String, params: Value },
    /// Any method we don't model; `method` + `params` are preserved.
    Other { method: String, params: Value },
}

impl Notification {
    pub fn thread_id(&self) -> Option<&str> {
        match self {
            Notification::ThreadStarted { params } => thread_id_from_started(params),
            Notification::ThreadStatusChanged { thread_id, .. }
            | Notification::TurnStarted { thread_id, .. }
            | Notification::TurnCompleted { thread_id, .. } => Some(thread_id.as_str()),
            Notification::Item { params, .. } | Notification::Other { params, .. } => {
                other_thread_id(params)
            }
        }
    }

    /// Never fails: unknown / malformed shapes degrade to [`Notification::Other`].
    pub fn parse(method: String, params: Value) -> Self {
        let thread_id = |p: &Value| {
            p.get("threadId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        match method.as_str() {
            "thread/started" => Notification::ThreadStarted { params },
            "thread/status/changed" => Notification::ThreadStatusChanged {
                thread_id: thread_id(&params),
                status: params.get("status").cloned().unwrap_or(Value::Null),
            },
            "turn/started" => Notification::TurnStarted {
                thread_id: thread_id(&params),
                turn: params.get("turn").cloned().unwrap_or(Value::Null),
            },
            "turn/completed" => Notification::TurnCompleted {
                thread_id: thread_id(&params),
                turn: params.get("turn").cloned().unwrap_or(Value::Null),
            },
            m if m.starts_with("item/") => Notification::Item { method, params },
            _ => Notification::Other { method, params },
        }
    }
}
