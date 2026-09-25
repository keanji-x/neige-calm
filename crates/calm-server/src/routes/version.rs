//! `GET /api/version` — kernel + protocol version metadata.
//!
//! `dbInstanceId` changes on every process boot; `databaseId` is the database's own
//! stable id; `nowMs` is the server clock at response time.

use crate::event::SYNC_EVENT_VERSION;
use crate::mcp_server::transport::KERNEL_MCP_PROTOCOL_VERSION;
use crate::plugin_host::mcp::KERNEL_PROTOCOL_VERSION;
use crate::state::{AppState, RouteState};
use axum::{Json, Router, extract::State, routing::get};
use calm_session::SUPERVISOR_CONTROL_VERSION;
use serde::Serialize;
use utoipa::ToSchema;

/// Diagnostic only on the wire, but `neige-app`'s `compute_verdict` compares it against
/// the installed release, so a REST contract break must bump it.
pub use calm_types::compatibility::REST_API_VERSION as API_VERSION;

/// Monotonically increasing frontend compatibility floor. Must equal `WEB_COMPAT_VERSION`
/// in both bundles (`web/src/api/version.ts`, `fe/web/src/app/providers/public.tsx`);
/// only a textual CI gate relates the three.
pub const WEB_COMPAT_VERSION: u32 = 31;

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

/// Response shape for `GET /api/version`; camelCase on the wire.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VersionInfo {
    /// True only when Area creation binds Idempotency-Key atomically and permanently.
    pub area_create_idempotency: bool,
    /// First-message model selection is part of atomic conversation creation.
    pub conversation_create_model: bool,
    pub kernel_version: String,
    /// REST contract version. Diagnostic-only on the wire — the frontend gates on
    /// `min_web_compat_version` and `sync_event_version`.
    pub api_version: String,
    pub sync_event_version: u32,
    pub mcp_protocol_version: String,
    pub plugin_mcp_protocol_version: String,
    pub web_compat_version: u32,
    pub min_web_compat_version: u32,
    pub supervisor_control_version: u32,
    pub build_sha: Option<String>,
    /// UUID v4 minted once per process boot.
    pub db_instance_id: String,
    /// The database's stable id, minted once into the one-row `database_identity` table;
    /// survives restarts where `db_instance_id` does not.
    pub database_id: String,
    /// The server clock (unix ms) when this response was built.
    pub now_ms: i64,
}

pub fn current_version_info(db_instance_id: String, database_id: String) -> VersionInfo {
    let compatibility = current_kernel_compatibility();
    VersionInfo {
        area_create_idempotency: true,
        conversation_create_model: true,
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
        database_id,
        now_ms: crate::model::now_ms(),
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
    Json(current_version_info(
        (*state.db_instance_id).clone(),
        (*state.database_id).clone(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire field must echo the constant verbatim.
    #[test]
    fn min_web_compat_version_matches_constant() {
        let body = VersionInfo {
            area_create_idempotency: true,
            conversation_create_model: true,
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
            database_id: "test-database-id".to_string(),
            now_ms: 0,
        };
        assert_eq!(body.min_web_compat_version, WEB_COMPAT_VERSION);
        assert_eq!(body.web_compat_version, WEB_COMPAT_VERSION);
        assert_eq!(body.supervisor_control_version, SUPERVISOR_CONTROL_VERSION);
        assert_eq!(body.plugin_mcp_protocol_version, KERNEL_PROTOCOL_VERSION);
    }
}
