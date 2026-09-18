//! Shared REST contract/capability revision for the kernel and upgrade checks.

// Revision 7 adds MCP setup fields that older install handlers silently
// ignore. Upgrade preflight must not pair this web bundle with those kernels.
//
// Revision 8 (#1625 P3) adds `POST /api/cards/{id}/planner/input/{entry_id}/steer`.
// A bundle that offers "Say it now" against a kernel without the route gets a
// 404 it cannot tell from "the entry is gone"; preflight refuses the pairing.
//
// Revision 9 (#1722 S1b) adds required response fields: `lastTurnCompletedAt`
// on `GET /api/tracks/{id}/conversations` rows and `databaseId` / `nowMs` on
// `GET /api/version`. A bundle whose row parser requires the first rejects
// every conversation list from an older kernel; preflight refuses the pairing.
pub const REST_API_VERSION: &str = "9";
