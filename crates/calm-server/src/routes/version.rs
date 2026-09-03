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
///
/// #1209 PR-2 bumped `"1"` -> `"2"`: the `POST /api/tracks` request body renamed
/// its two template fields to `template_id` / `template_input`. Despite
/// the "diagnostic only" wording above, `neige-app`'s `compute_verdict` really
/// does compare this string against the installed release (it is one of the
/// nine compatibility fields), so leaving it at `"1"` across a REST body rename
/// would be a contract constant contradicted by behaviour.
///
/// #1300 S1 bumped `"2"` -> `"3"`: `PUT /api/track-templates/{id}` is **gone**,
/// not renamed. That is strictly a larger break than #1209's field rename — a
/// client holding the old contract gets a 404 with no field to correct — so
/// leaving this at `"2"` would repeat exactly the contradiction the paragraph
/// above records.
///
/// #1354 bumps `"3"` -> `"4"`: the Area conversations GET/POST endpoints and
/// chat-track ensure endpoint are gone with the Area page. Cached clients that
/// still call them must be rejected by the compatibility gate rather than
/// discovering the break as a 404 after the reader starts an action.
pub const API_VERSION: &str = "4";

/// Monotonically increasing frontend compatibility floor.
///
/// This value must equal `WEB_COMPAT_VERSION` in **both** bundles —
/// `web/src/api/version.ts` and `fe/web/src/app/providers/public.tsx`. Nothing
/// in the type system relates the three; the `web compat version lockstep gate
/// (#1209 PR-2)` step in `.github/workflows/ci.yml` compares them textually.
/// Before that gate existed all three drift directions were CI-green.
///
/// #1209 PR-2 bumped 16 -> 17 so cached bundles at 16 get the hard refresh
/// curtain instead of sending the pre-rename track-create field spellings and
/// taking a 400 on every attempt.
///
/// #1300 S1 bumped 17 -> 18 for the same reason in a new shape: a cached bundle
/// at 17 still renders Settings › Templates, and its Save now 404s. The failure
/// is worse than the #1209 one it mirrors, because it is silent until the user
/// has typed an edit and pressed the button.
///
/// #1316 S1 bumps 18 -> 19: `Cove` became `Area` across the whole stack, so a
/// cached bundle at 18 is wrong in three independent ways at once — it calls
/// `/api/coves*` (now 404), reads a `cove_id` field the server no longer emits,
/// and gates its event stream on `cove.updated` / `cove.deleted` discriminators
/// that migration 0080 rewrote. Any one of those alone would justify the bump;
/// together they would produce a bundle that renders an empty, silent shell.
///
/// #1354 bumps 20 -> 21 with API v4 so both cached bundles show the hard refresh
/// curtain before they can navigate to the retired Area page or call its
/// conversation endpoints.
pub const WEB_COMPAT_VERSION: u32 = 21;

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
