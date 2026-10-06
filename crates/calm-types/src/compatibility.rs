//! Shared REST contract/capability revision for the kernel and upgrade checks.

// Revision 14 (#1829): `POST /api/tracks/{id}/activity/dismissals` is new, and the transcript wire
// gains a required `turn_error_text`; a bundle of this revision would get 404 for Dismiss and reject
// an older kernel's transcript rows, so preflight refuses the pairing.
// Revision 15 (#1876): a track is open or closed; `closed_at` replaces `lifecycle`, `terminal_at` and
// `archived_at`, and `PATCH /api/tracks/{id}` takes `closed`.
// Revision 16 (#1897): plugin list rows declare required can_uninstall for built-in capabilities.
// Revision 17 (#1913): Calendar REST and plugin data invalidation.
// Revision18 (#1913 follow-up): plugin list requires lifecycle can_disable.
// Revision 19 (#1967): calendar schedules may be weekly, and listed entries carry required occurrences.
// Revision 20 (#2043): `POST /api/cards/{id}/planner/input` requires an `Idempotency-Key`; an older
// bundle sends none and every message would be refused.
// Revision 21 (#2043): Edit is `POST /planner/input` with `replaces_turn`; `POST /planner/rewind` is
// gone. An older kernel would ignore the field and queue the edit as a new message.
// Revision 22 (#2209): `POST /api/cards/{id}/ratify` is gone and
// `POST /api/tracks/{id}/asks/{ask_id}/answer` is new; a bundle of this revision would get 404 from
// an older kernel when it answers a question.
pub const REST_API_VERSION: &str = "22";
