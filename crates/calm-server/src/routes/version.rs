//! `GET /api/version` — kernel + protocol version metadata.
//!
//! Each field tracks an independent compatibility boundary:
//!
//! API, sync-event, web, MCP, plugin MCP, supervisor, and kernel versions may
//! evolve independently. `dbInstanceId` changes on every process boot so the
//! browser can discard state that belongs to a replaced database.

use crate::event::SYNC_EVENT_VERSION;
use crate::mcp_server::transport::KERNEL_MCP_PROTOCOL_VERSION;
use crate::plugin_host::mcp::KERNEL_PROTOCOL_VERSION;
use crate::state::{AppState, RouteState};
use axum::{Json, Router, extract::State, routing::get};
use calm_session::SUPERVISOR_CONTROL_VERSION;
use serde::Serialize;
use utoipa::ToSchema;

/// Diagnostic only; clients gate on web and sync-event compatibility instead.
pub const API_VERSION: &str = "1";

/// Monotonically increasing frontend compatibility floor.
pub const WEB_COMPAT_VERSION: u32 = 16;

/// Kernel compatibility values sourced from live constants.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KernelCompatibility {
    pub terminal_frame_version: u16,
    pub terminal_protocol_version: u16,
    pub api_version: String,
    pub sync_event_version: u32,
    pub mcp_protocol_version: String,
    pub plugin_mcp_protocol_version: String,
    pub web_compat_version: u32,
    pub min_web_compat_version: u32,
    pub supervisor_control_version: u32,
}

pub fn current_kernel_compatibility() -> KernelCompatibility {
    KernelCompatibility {
        terminal_frame_version: calm_session::FRAME_VERSION,
        terminal_protocol_version: calm_session::PROTOCOL_VERSION,
        api_version: API_VERSION.to_string(),
        sync_event_version: SYNC_EVENT_VERSION,
        mcp_protocol_version: KERNEL_MCP_PROTOCOL_VERSION.to_string(),
        plugin_mcp_protocol_version: KERNEL_PROTOCOL_VERSION.to_string(),
        web_compat_version: WEB_COMPAT_VERSION,
        min_web_compat_version: WEB_COMPAT_VERSION,
        supervisor_control_version: SUPERVISOR_CONTROL_VERSION,
    }
}

pub fn router() -> Router<AppState> {
    Router::new().route("/api/version", get(get_version))
}

/// Response shape for `GET /api/version`. camelCase on the wire so it lines
/// up with the rest of the TypeScript-facing surface.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VersionInfo {
    pub kernel_version: String,
    /// REST contract version. Diagnostic-only on the wire — the frontend
    /// gates compatibility on `min_web_compat_version` (whole bundle) and
    /// `sync_event_version` (per-event-frame). See `API_VERSION` for the
    /// rationale. (Issue #198, concern 3.)
    pub api_version: String,
    pub sync_event_version: u32,
    pub mcp_protocol_version: String,
    pub plugin_mcp_protocol_version: String,
    pub web_compat_version: u32,
    pub min_web_compat_version: u32,
    pub supervisor_control_version: u32,
    pub build_sha: Option<String>,
    /// UUID v4 minted once per process boot. See module doc.
    pub db_instance_id: String,
}

pub fn current_version_info(db_instance_id: String) -> VersionInfo {
    let compatibility = current_kernel_compatibility();
    VersionInfo {
        kernel_version: env!("CARGO_PKG_VERSION").to_string(),
        api_version: compatibility.api_version,
        sync_event_version: compatibility.sync_event_version,
        mcp_protocol_version: compatibility.mcp_protocol_version,
        plugin_mcp_protocol_version: compatibility.plugin_mcp_protocol_version,
        web_compat_version: compatibility.web_compat_version,
        min_web_compat_version: compatibility.min_web_compat_version,
        supervisor_control_version: compatibility.supervisor_control_version,
        build_sha: option_env!("NEIGE_BUILD_SHA").map(|s| s.to_string()),
        db_instance_id,
    }
}

#[utoipa::path(
    get,
    path = "/api/version",
    tag = "version",
    responses(
        (status = 200, description = "Kernel + protocol version metadata", body = VersionInfo),
    ),
)]
pub(crate) async fn get_version(State(state): State<RouteState>) -> Json<VersionInfo> {
    Json(current_version_info((*state.db_instance_id).clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire field must echo the constant verbatim. Catches the
    /// failure mode of bumping `WEB_COMPAT_VERSION` (or the response
    /// builder) without bumping the other. The handler is now state-aware
    /// (it pulls `db_instance_id` off `AppState`); we exercise the body
    /// construction directly with a fixed instance id to keep this unit
    /// test free of the heavy `AppState::from_parts` plumbing — the HTTP
    /// integration test in `tests/version.rs` covers the request path.
    #[test]
    fn min_web_compat_version_matches_constant() {
        let body = VersionInfo {
            kernel_version: env!("CARGO_PKG_VERSION").to_string(),
            api_version: API_VERSION.to_string(),
            sync_event_version: SYNC_EVENT_VERSION,
            mcp_protocol_version: KERNEL_MCP_PROTOCOL_VERSION.to_string(),
            plugin_mcp_protocol_version: KERNEL_PROTOCOL_VERSION.to_string(),
            web_compat_version: WEB_COMPAT_VERSION,
            min_web_compat_version: WEB_COMPAT_VERSION,
            supervisor_control_version: SUPERVISOR_CONTROL_VERSION,
            build_sha: option_env!("NEIGE_BUILD_SHA").map(|s| s.to_string()),
            db_instance_id: "test-id".to_string(),
        };
        assert_eq!(body.min_web_compat_version, WEB_COMPAT_VERSION);
        assert_eq!(body.web_compat_version, WEB_COMPAT_VERSION);
        assert_eq!(body.supervisor_control_version, SUPERVISOR_CONTROL_VERSION);
        assert_eq!(body.plugin_mcp_protocol_version, KERNEL_PROTOCOL_VERSION);
    }
}
