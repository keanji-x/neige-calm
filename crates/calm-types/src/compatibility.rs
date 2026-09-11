//! Shared REST contract/capability revision for the kernel and upgrade checks.

// Revision 7 adds MCP setup fields that older install handlers silently
// ignore. Upgrade preflight must not pair this web bundle with those kernels.
pub const REST_API_VERSION: &str = "7";
