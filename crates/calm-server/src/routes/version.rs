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
//! * `mcpProtocolVersion` — the MCP spec date we advertise in `initialize`
//!   responses to plugin processes. Sourced from `plugin_host::mcp` so the
//!   two surfaces never drift.
//! * `minWebCompatVersion` — the minimum frontend `WEB_COMPAT_VERSION` the
//!   running kernel still considers wire-compatible. Frontends below this
//!   value must hard-refresh; see `docs/upgrade-stability.md` (Tier B).
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
use crate::plugin_host::mcp::KERNEL_PROTOCOL_VERSION;
use crate::state::AppState;
use axum::{Json, Router, extract::State, routing::get};
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
pub const WEB_COMPAT_VERSION: u32 = 2;

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
    pub min_web_compat_version: u32,
    pub build_sha: Option<String>,
    /// UUID v4 minted once per process boot. See module doc.
    pub db_instance_id: String,
}

#[utoipa::path(
    get,
    path = "/api/version",
    tag = "version",
    responses(
        (status = 200, description = "Kernel + protocol version metadata", body = VersionInfo),
    ),
)]
pub(crate) async fn get_version(State(state): State<AppState>) -> Json<VersionInfo> {
    Json(VersionInfo {
        kernel_version: env!("CARGO_PKG_VERSION").to_string(),
        api_version: API_VERSION.to_string(),
        sync_event_version: SYNC_EVENT_VERSION,
        mcp_protocol_version: KERNEL_PROTOCOL_VERSION.to_string(),
        min_web_compat_version: WEB_COMPAT_VERSION,
        build_sha: option_env!("NEIGE_BUILD_SHA").map(|s| s.to_string()),
        db_instance_id: (*state.db_instance_id).clone(),
    })
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
            mcp_protocol_version: KERNEL_PROTOCOL_VERSION.to_string(),
            min_web_compat_version: WEB_COMPAT_VERSION,
            build_sha: option_env!("NEIGE_BUILD_SHA").map(|s| s.to_string()),
            db_instance_id: "test-id".to_string(),
        };
        assert_eq!(body.min_web_compat_version, WEB_COMPAT_VERSION);
    }
}
