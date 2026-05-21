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

use crate::event::SYNC_EVENT_VERSION;
use crate::plugin_host::mcp::KERNEL_PROTOCOL_VERSION;
use crate::state::AppState;
use axum::{Json, Router, routing::get};
use serde::Serialize;
use utoipa::ToSchema;

/// REST contract version. Bumped by hand when the wire shape changes in a
/// way the web client needs to gate on; independent of `CARGO_PKG_VERSION`.
pub const API_VERSION: &str = "1";

/// Frontend ↔ backend compatibility version surfaced as `minWebCompatVersion`
/// on `/api/version`. Monotonically increasing. Bump when REST/WS contract
/// changes are incompatible with older frontends. Frontend's
/// `WEB_COMPAT_VERSION` must be kept in lockstep — see
/// `docs/upgrade-stability.md`.
pub const WEB_COMPAT_VERSION: u32 = 1;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/version", get(get_version))
}

/// Response shape for `GET /api/version`. camelCase on the wire so it lines
/// up with the rest of the TypeScript-facing surface.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VersionInfo {
    pub kernel_version: String,
    pub api_version: String,
    pub sync_event_version: u32,
    pub mcp_protocol_version: String,
    pub min_web_compat_version: u32,
    pub build_sha: Option<String>,
}

#[utoipa::path(
    get,
    path = "/api/version",
    tag = "version",
    responses(
        (status = 200, description = "Kernel + protocol version metadata", body = VersionInfo),
    ),
)]
pub(crate) async fn get_version() -> Json<VersionInfo> {
    Json(VersionInfo {
        kernel_version: env!("CARGO_PKG_VERSION").to_string(),
        api_version: API_VERSION.to_string(),
        sync_event_version: SYNC_EVENT_VERSION,
        mcp_protocol_version: KERNEL_PROTOCOL_VERSION.to_string(),
        min_web_compat_version: WEB_COMPAT_VERSION,
        build_sha: option_env!("NEIGE_BUILD_SHA").map(|s| s.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire field must echo the constant verbatim. Catches the
    /// failure mode of bumping `WEB_COMPAT_VERSION` (or the response
    /// builder) without bumping the other; the integration test in
    /// `tests/version.rs` covers the same property at the HTTP layer,
    /// this one fails loudly on the unit-test path too.
    #[tokio::test]
    async fn min_web_compat_version_matches_constant() {
        let body = get_version().await;
        assert_eq!(body.min_web_compat_version, WEB_COMPAT_VERSION);
    }
}
