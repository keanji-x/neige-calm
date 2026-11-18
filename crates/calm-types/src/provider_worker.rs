//! Task-worker admission and transport capabilities. This is the single registry used by
//! declarations, scheduling, session ownership and API projection; it does not assert readiness.
use crate::runtime::AgentProvider;
use crate::worker::{SessionMode, WorkerContract};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum WorkerOperationKind {
    CodexWorker,
    ClaudeWorker,
    AcpWorker,
    TerminalWorker,
}
impl WorkerOperationKind {
    pub const ALL: [Self; 4] = [
        Self::CodexWorker,
        Self::ClaudeWorker,
        Self::AcpWorker,
        Self::TerminalWorker,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CodexWorker => "codex-worker",
            Self::ClaudeWorker => "claude-worker",
            Self::AcpWorker => "acp-worker",
            Self::TerminalWorker => "terminal-worker",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }
    pub fn takes_terminal(self) -> bool {
        self == Self::TerminalWorker
            || PROVIDER_WORKER_CAPABILITIES.iter().any(|cap| {
                cap.operation == self && cap.message_transport == WorkerMessageTransport::Terminal
            })
    }
    /// SQL list derived from typed operations. Values are kernel constants, never user input.
    pub fn sql_list() -> String {
        Self::ALL
            .map(|kind| format!("'{}'", kind.as_str()))
            .join(",")
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum WorkerMessageTransport {
    Terminal,
    NativeSession,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum WorkerCancelStrategy {
    ProviderStop,
    AcpCancel,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ProviderWorkerCapabilities {
    pub provider: AgentProvider,
    pub operation: WorkerOperationKind,
    pub message_transport: WorkerMessageTransport,
    pub cancel_strategy: WorkerCancelStrategy,
    pub resumable: bool,
    pub executor: bool,
    pub validator: bool,
}
pub const PROVIDER_WORKER_CAPABILITIES: &[ProviderWorkerCapabilities] = &[
    ProviderWorkerCapabilities {
        provider: AgentProvider::Codex,
        operation: WorkerOperationKind::CodexWorker,
        message_transport: WorkerMessageTransport::Terminal,
        cancel_strategy: WorkerCancelStrategy::ProviderStop,
        resumable: true,
        executor: true,
        validator: true,
    },
    ProviderWorkerCapabilities {
        provider: AgentProvider::Claude,
        operation: WorkerOperationKind::ClaudeWorker,
        message_transport: WorkerMessageTransport::Terminal,
        cancel_strategy: WorkerCancelStrategy::ProviderStop,
        resumable: false,
        executor: true,
        validator: true,
    },
    ProviderWorkerCapabilities {
        provider: AgentProvider::Antigravity,
        operation: WorkerOperationKind::AcpWorker,
        message_transport: WorkerMessageTransport::NativeSession,
        cancel_strategy: WorkerCancelStrategy::AcpCancel,
        resumable: true,
        executor: true,
        validator: true,
    },
];
impl ProviderWorkerCapabilities {
    pub fn for_provider(provider: AgentProvider) -> Option<&'static Self> {
        PROVIDER_WORKER_CAPABILITIES
            .iter()
            .find(|cap| cap.provider == provider)
    }
    pub fn admits(&self, contract: WorkerContract, mode: SessionMode) -> bool {
        let role = match contract {
            WorkerContract::Executor => self.executor,
            WorkerContract::Validator => self.validator,
            _ => false,
        };
        role && mode
            == if self.resumable {
                SessionMode::Resumable
            } else {
                SessionMode::Ephemeral
            }
    }
}
/// Legacy spellings are interpreted here only. Persisted kind/hash/event bytes are preserved.
pub fn legacy_task_provider(kind: &str) -> Option<AgentProvider> {
    match kind {
        "codex" => Some(AgentProvider::Codex),
        "claude" => Some(AgentProvider::Claude),
        _ => None,
    }
}
pub fn is_agent_task_kind(kind: &str) -> bool {
    kind == "agent" || legacy_task_provider(kind).is_some()
}
pub fn task_provider(kind: &str, provider: Option<&str>) -> Result<Option<AgentProvider>, String> {
    let explicit = provider
        .map(|value| {
            serde_json::from_value::<AgentProvider>(serde_json::Value::String(value.to_owned()))
                .map_err(|_| format!("provider: unknown agent provider {value:?}"))
        })
        .transpose()?;
    let resolved = if kind == "agent" {
        Some(
            explicit
                .clone()
                .ok_or("provider: required for kind agent")?,
        )
    } else if let Some(alias) = legacy_task_provider(kind) {
        if explicit.is_some_and(|value| value != alias) {
            return Err("provider: conflicts with historical task kind".into());
        }
        Some(alias)
    } else {
        if explicit.is_some() {
            return Err("provider: only agent tasks accept Worker selection".into());
        }
        None
    };
    if resolved.as_ref().is_some_and(|provider| {
        ProviderWorkerCapabilities::for_provider(provider.clone()).is_none()
    }) {
        return Err("provider: does not support task workers".into());
    }
    Ok(resolved)
}
pub fn task_worker_contract(
    access: crate::task_execution::TaskAccess,
    result_contract: Option<&str>,
) -> WorkerContract {
    if access == crate::task_execution::TaskAccess::ReadOnly && result_contract == Some("review-v1")
    {
        WorkerContract::Validator
    } else {
        WorkerContract::Executor
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn task_execution_provider_normalization_and_capability_admission() {
        for provider in AgentProvider::ALL {
            let value = task_provider("agent", Some(provider.wire_name()));
            assert_eq!(
                value.is_ok(),
                ProviderWorkerCapabilities::for_provider(provider.clone()).is_some()
            );
        }
        assert_eq!(
            task_provider("codex", None).unwrap(),
            Some(AgentProvider::Codex)
        );
        assert!(task_provider("claude", Some("codex")).is_err());
        assert!(task_provider("agent", None).is_err());
        assert!(task_provider("terminal", Some("antigravity")).is_err());
        let cap = ProviderWorkerCapabilities::for_provider(AgentProvider::Antigravity).unwrap();
        assert!(cap.admits(WorkerContract::Validator, SessionMode::Resumable));
        assert!(!cap.admits(WorkerContract::Planner, SessionMode::Resumable));
        assert!(!cap.admits(WorkerContract::Executor, SessionMode::Ephemeral));
        assert!(WorkerOperationKind::sql_list().contains("'acp-worker'"));
    }
}

#[cfg(test)]
mod declaration_tests {
    use crate::report_blocks::{KIND_TASK, kinds::validate_payload};
    use serde_json::json;
    #[test]
    fn report_blocks_agent_selection_rejects_conflicts_null_and_subtrack() {
        let base = json!({"key":"work", "kind":"agent", "provider":"antigravity", "goal":"inspect", "ready":true,"declared_by":"spec"});
        assert!(validate_payload(KIND_TASK, &base).is_ok());
        for field in ["provider", "model", "reasoning_effort"] {
            for value in [json!(null), json!(" ")] {
                let mut input = base.clone();
                input[field] = value;
                assert!(
                    validate_payload(KIND_TASK, &input).is_err(),
                    "{field}: {input}"
                );
            }
        }
        let mut alias = base.clone();
        alias["kind"] = json!("codex");
        assert!(validate_payload(KIND_TASK, &alias).is_err());
        alias["provider"] = json!("codex");
        assert!(validate_payload(KIND_TASK, &alias).is_ok());
        alias["spawn"] = json!("sub-track");
        assert!(validate_payload(KIND_TASK, &alias).is_err());
        alias.as_object_mut().unwrap().remove("provider");
        assert!(validate_payload(KIND_TASK, &alias).is_ok());
        let mut review = base;
        review["result_contract"] = json!("review-v1");
        assert!(validate_payload(KIND_TASK, &review).is_err());
        review["access"] = json!("read_only");
        assert!(validate_payload(KIND_TASK, &review).is_ok());
    }
}

/// Frozen request plus provider-confirmed selection; absent actuals never imply an explicit default.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct TaskWorkerSelection {
    pub mechanism: String,
    pub provider: AgentProvider,
    #[schema(required = true)]
    pub requested_model: Option<String>,
    #[schema(required = true)]
    pub requested_effort: Option<String>,
    #[schema(required = true)]
    pub effective_model: Option<String>,
    #[schema(required = true)]
    pub effective_effort: Option<String>,
    #[schema(required = true)]
    pub selection_source: Option<crate::acp_worker::WorkerSelectionSources>,
    pub capabilities: ProviderWorkerCapabilities,
    #[schema(required = true)]
    pub turn_phase: Option<crate::acp_worker::WorkerTurnPhase>,
    pub cleanup_pending: bool,
}
