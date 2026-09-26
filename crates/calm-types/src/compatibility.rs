//! Shared REST contract/capability revision for the kernel and upgrade checks.

// Revision 12 (#1817) adds `GET /api/agent-providers`. A bundle whose new-track picker and
// Settings › Planners read it gets a 404 from an older kernel; preflight refuses the pairing.
pub const REST_API_VERSION: &str = "12";
