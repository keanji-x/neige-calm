//! AI-first editor crate scaffolded for issue #330.
//!
//! The Rust side (this crate) owns:
//!   * editor schema definitions,
//!   * schema validation,
//!   * the MCP tool surface the agent calls to mutate documents,
//!   * `ts-rs` bindings emitted into `web/src/editor/types/`.
//!
//! The web side (`web/src/editor/`) owns:
//!   * Plate integration and rendering,
//!   * the op-dispatcher that applies validated mutations,
//!   * the stable block-id middleware,
//!   * the `WaveReportEditor` component.
//!
//! The crate is intentionally empty at scaffold time — only one stub
//! type exists so the ts-rs export pipeline is wired end-to-end. Real
//! types and ops land in Spike Day 1-3 per issue #330.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Placeholder for the Plate/Slate-compatible editor value. Replaced in
/// Spike Day 1 with the real AST node tree. Kept as `serde_json::Value`
/// for v0 so the wire format is open-ended while the schema stabilizes.
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
// `TS_RS_EXPORT_DIR` is pinned to the workspace root in `.cargo/config.toml`,
// so `export_to` paths are workspace-relative — matching the convention in
// calm-server / calm-session. Trailing slash means "directory"; ts-rs uses
// the type name (`EditorDoc.ts`) as the filename.
#[ts(export, export_to = "web/src/editor/types/")]
pub struct EditorDoc {
    // `serde_json::Value` exports as a dangling `JsonValue` import by default;
    // mirror calm-server's convention (`payload: serde_json::Value` fields on
    // `Card`/`Overlay`) and override to `unknown` so the generated TS stays
    // self-contained. Replaced with the real AST union in Spike Day 1.
    #[ts(type = "unknown")]
    pub value: serde_json::Value,
}
