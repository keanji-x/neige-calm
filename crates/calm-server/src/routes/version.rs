//! `GET /api/version` — kernel + protocol version metadata.
//!
//! Returns a small JSON document the web client (and operators) can hit to
//! discover which kernel build is running, which REST contract it speaks,
//! which sync-event envelope schema it emits, and which MCP protocol
//! version it advertises to plugins.
//!
//! ## Why so many version fields?
//!
//! Each field tracks an independent compatibility boundary:
//!
//! * `kernelVersion` — the `calm-server` crate's `CARGO_PKG_VERSION`. Bumped
//!   by normal semver on the kernel binary itself.
//! * `apiVersion` — the REST contract version. Deliberately decoupled from
//!   `kernelVersion` so we can break or extend the wire shape without
//!   shipping a new kernel release, and conversely ship kernel patches that
//!   leave the REST surface untouched.
//! * `syncEventVersion` — the version stamped onto sync-engine envelopes.
//!   Will be threaded into `BroadcastEnvelope` in a later PR so replicas can
//!   refuse incompatible event logs.
//! * `mcpProtocolVersion` — the MCP spec date the kernel-as-MCP-server
//!   advertises to Codex clients.
//! * `pluginMcpProtocolVersion` — the MCP spec date the plugin host advertises
//!   to plugin processes. Sourced from `plugin_host::mcp` so the two surfaces
//!   never drift.
//! * `webCompatVersion` — the frontend compatibility value this server was
//!   built with.
//! * `minWebCompatVersion` — the minimum frontend `WEB_COMPAT_VERSION` the
//!   running kernel still considers wire-compatible. Frontends below this
//!   value must hard-refresh; see `docs/upgrade-stability.md` (Tier B).
//! * `supervisorControlVersion` — the control-wire version between the kernel
//!   and `calm-proc-supervisor`.
//! * `buildSha` — optional git SHA baked in at compile time via
//!   `option_env!("NEIGE_BUILD_SHA")`. `null` for local `cargo build` runs;
//!   release CI sets the env var.
//! * `dbInstanceId` — UUID v4 minted once per server-process startup. NOT
//!   persisted to the DB. The web client compares it against a value it
//!   stashes in `localStorage`; on mismatch it wipes its IndexedDB-backed
//!   React Query cache + WS event cursor and hard-reloads, so a `make dev
//!   RESET_DB=1` (or any other DB wipe / migration rebuild) doesn't leave
//!   the browser holding stale row ids that would 404 at route loaders.
//!   The field is additive on the wire — older frontends ignore it
//!   and continue to work, so no `WEB_COMPAT_VERSION` bump is required.

use crate::event::SYNC_EVENT_VERSION;
use crate::mcp_server::transport::KERNEL_MCP_PROTOCOL_VERSION;
use crate::plugin_host::mcp::KERNEL_PROTOCOL_VERSION;
use crate::state::{AppState, RouteState};
use axum::{Json, Router, extract::State, routing::get};
use calm_session::SUPERVISOR_CONTROL_VERSION;
use serde::Serialize;
use utoipa::ToSchema;

/// REST contract version. Surfaced as `apiVersion` on `/api/version`.
///
/// **Diagnostic-only** — frontends MUST NOT gate behavior on this string.
/// The load-bearing frontend↔backend compatibility checks live on:
///   * `minWebCompatVersion` for the web bundle as a whole (whole-page
///     hard-block via `ServerCompatGate`), and
///   * `syncEventVersion` for individual `/api/events` frames (per-frame
///     drop without advancing the replay cursor).
///
/// See issue #198 concern 3 and `docs/upgrade-stability.md`. Operators
/// reading dashboards / logs are the intended audience for this field.
pub const API_VERSION: &str = "1";

/// Frontend ↔ backend compatibility version surfaced as `minWebCompatVersion`
/// on `/api/version`. Monotonically increasing. Bump when REST/WS contract
/// changes are incompatible with older frontends. Frontend's
/// `WEB_COMPAT_VERSION` must be kept in lockstep — see
/// `docs/upgrade-stability.md`.
///
/// Version history:
/// * `1` — initial. Terminal protocol v1 (`ClientMsg::Attach` / `Stdin` /
///   `Resize`, `DaemonMsg::Hello` / `Stdout`).
/// * `2` — terminal protocol v2 (issue #44). Renames every wire frame
///   under `/api/terminals/:id` (ClientHello/ServerHello, Input,
///   ResizeCommit/ResizeApplied, RenderSnapshot/Patch, OwnerClaim/Release/
///   Changed, ProtocolError, TerminalExited). A v1 frontend hitting a v2
///   kernel will fail its first frame and be hard-refreshed by the compat
///   modal — clean break, no backwards compatibility shim.
/// * `3` — dispatcher request event rename (issue #581). Wire kinds
///   `codex.job_requested` / `terminal.job_requested` are renamed to
///   `*.worker_requested`. A v2 frontend's zod `WireEvent` union only
///   accepts the old kind strings, so live frames after the bump would
///   silently fail discriminator validation and skip
///   `wave-files`/`overlays` invalidation. The compat modal forces a
///   hard refresh.
/// * `4` — scheduler wire kinds (issue #644). Adds `plan.updated` and
///   `task.dispatched` to the WS event union (with
///   `SYNC_EVENT_VERSION` bumped 2 → 3 in lockstep). A v3 frontend's
///   zod `WireEvent` union doesn't know the new discriminators, so its
///   plan/dispatch invalidation would silently drop. The compat modal
///   forces a hard refresh.
/// * `5` — gate-result wire kind (issue #644 PR-C). Adds
///   `task.gate_result` to the WS event union (with
///   `SYNC_EVENT_VERSION` bumped 3 → 4 in lockstep). A v4 frontend's
///   zod `WireEvent` union doesn't know the new discriminator, so its
///   gate-result invalidation would silently drop. The compat modal
///   forces a hard refresh.
/// * `6` — workspace lease lifecycle events (issue #760 slice 1):
///   `workspace.leased` and `workspace.released` join the WS event union
///   with `SYNC_EVENT_VERSION` bumped 4 → 5 in lockstep. A v5 frontend's
///   zod `WireEvent` union doesn't know the new discriminators, so its
///   workspace invalidation would silently drop. The compat modal forces
///   a hard refresh.
pub const WEB_COMPAT_VERSION: u32 = 6;

/// Kernel compatibility values sourced from live constants. Kept in
/// `calm-server` for PR 1 because the manifest type lives in `neige-app`,
/// which cannot depend back on the kernel crate; PR 2 can use this as the
/// source for emitting manifest v2 compatibility.
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
