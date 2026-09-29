//! Shared REST contract/capability revision for the kernel and upgrade checks.

// Revision 14 (#1829): `POST /api/tracks/{id}/activity/dismissals` is new, and the transcript wire
// gains a required `turn_error_text`; a bundle of this revision would get 404 for Dismiss and reject
// an older kernel's transcript rows, so preflight refuses the pairing.
// Revision 15 (#1876): a track is open or closed; `closed_at` replaces `lifecycle`, `terminal_at` and
// `archived_at`, and `PATCH /api/tracks/{id}` takes `closed`.
pub const REST_API_VERSION: &str = "15";
