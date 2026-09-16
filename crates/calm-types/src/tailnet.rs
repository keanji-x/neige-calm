//! Narrow private-tailnet status and local control vocabulary. No credentials in status.
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
#[serde(rename_all = "kebab-case")]
pub enum TailnetPhase {
    Disabled,
    Starting,
    NeedsLogin,
    NeedsApproval,
    Online,
    Degraded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
#[serde(rename_all = "kebab-case")]
pub enum TailnetNodeState {
    Stopped,
    Starting,
    NeedsLogin,
    NeedsApproval,
    Online,
    Offline,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TailnetStatus {
    pub desired_enabled: bool,
    pub phase: TailnetPhase,
    pub process_running: bool,
    #[schema(required = true, nullable = true)]
    pub child_pid: Option<u32>,
    pub node_state: TailnetNodeState,
    pub https_ready: bool,
    pub upstream_ready: bool,
    #[schema(required = true, nullable = true)]
    pub origin: Option<String>,
    #[schema(required = true, nullable = true)]
    pub dns_name: Option<String>,
    #[schema(required = true, nullable = true)]
    pub node_id: Option<String>,
    pub addresses: Vec<String>,
    pub detail: String,
}

impl TailnetStatus {
    pub fn stopped(desired_enabled: bool, failed: bool) -> Self {
        Self {
            desired_enabled,
            phase: if failed {
                TailnetPhase::Failed
            } else if desired_enabled {
                TailnetPhase::Starting
            } else {
                TailnetPhase::Disabled
            },
            process_running: false,
            child_pid: None,
            node_state: TailnetNodeState::Stopped,
            https_ready: false,
            upstream_ready: false,
            origin: None,
            dns_name: None,
            node_id: None,
            addresses: vec![],
            detail: if failed {
                "Tailnet could not start. Disable and enable to retry."
            } else if desired_enabled {
                "Starting private Tailnet node"
            } else {
                "Remote access is disabled; node identity is retained"
            }
            .into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TailnetLogin {
    pub login_url: String,
    pub display_for_seconds: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TailnetAction {
    Status,
    Enable,
    Disable,
    Login,
    Logout,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TailnetRequest {
    pub version: u32,
    pub action: TailnetAction,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TailnetResponse {
    pub version: u32,
    pub status: TailnetStatus,
    pub login_url: Option<String>,
    pub error: Option<String>,
}
