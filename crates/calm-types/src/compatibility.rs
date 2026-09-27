//! Shared REST contract/capability revision for the kernel and upgrade checks.

// Revision 13 (#1822): `GET /api/models` answers a Claude Planner's catalog as the CLI's live
// list (`source: "live"`, `default_source: "claude_cli"`); `source: "built_in"` is gone, which a
// bundle of this revision's schema rejects from an older kernel; preflight refuses the pairing.
pub const REST_API_VERSION: &str = "13";
