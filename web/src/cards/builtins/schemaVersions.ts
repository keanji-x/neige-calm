// Per-kind `schemaVersion` constants for kernel-owned card and overlay
// payloads — Tier A persistence contract per `docs/upgrade-stability.md`.
//
// These mirror the Rust constants in
// `crates/calm-server/src/validation.rs`. They are NOT auto-generated
// (the schema lives in JSON, not Rust types) and so must be kept in
// sync by hand. A drift between the two sides is caught at runtime by
// the kernel's write-boundary validator — an old frontend writing a
// payload without `schemaVersion` is accepted (treated as `1`), but a
// newer frontend stamping a future version against an older kernel is
// rejected with a clear error.
//
// Reads use the helper below: missing field is `1`, since every kind
// started at version `1` — rows written before the field existed (or by
// a v1 writer) can only be v1. `wave-report` is at v2 since #960 PR2;
// its reader only rejects versions above the supported constant — v1 and
// v2 rows share the same optional-blocks parser.

/** `schemaVersion` for `kind: "terminal"` card payloads. */
export const TERMINAL_PAYLOAD_SCHEMA_VERSION = 1;
/** `schemaVersion` for `kind: "codex"` card payloads. */
export const CODEX_PAYLOAD_SCHEMA_VERSION = 1;
/** `schemaVersion` for spec-harness `kind: "codex"` card payloads. */
export const SPEC_PAYLOAD_SCHEMA_VERSION = 1;
/** `schemaVersion` for `kind: "claude"` card payloads. */
export const CLAUDE_PAYLOAD_SCHEMA_VERSION = 1;
/** `schemaVersion` for `kind: "wave-report"` card payloads (issue #229 PR B;
 *  v2 adds the `blocks` array — issue #960 PR2). */
export const WAVE_REPORT_PAYLOAD_SCHEMA_VERSION = 2;
/** `schemaVersion` for `kind: "status"` overlay payloads. */
export const OVERLAY_STATUS_SCHEMA_VERSION = 1;
/** `schemaVersion` for `kind: "progress"` overlay payloads. */
export const OVERLAY_PROGRESS_SCHEMA_VERSION = 1;
/** `schemaVersion` for `kind: "eta"` overlay payloads. */
export const OVERLAY_ETA_SCHEMA_VERSION = 1;
/** `schemaVersion` for `kind: "now"` overlay payloads. */
export const OVERLAY_NOW_SCHEMA_VERSION = 1;
/** `schemaVersion` for `kind: "layout"` overlay payloads. */
export const OVERLAY_LAYOUT_SCHEMA_VERSION = 1;
/** `schemaVersion` for `kind: "view-mode"` overlay payloads (Slice 9 of
 *  issue #56; #594 PR-A extends the enum domain to grid/list/report
 *  without changing the payload shape). */
export const OVERLAY_VIEW_MODE_SCHEMA_VERSION = 1;

/**
 * Read `schemaVersion` from a kernel-owned payload. Returns `1` when the
 * field is absent or not a number — every kind started at version `1`
 * and writers have stamped the field ever since, so an absent field can
 * only mean a v1-era row; absent-as-1 stays unambiguous even now that
 * `wave-report` is at v2 (#960 PR2 — v2 writers always stamp `2`).
 *
 * Migrators (if any kind ever needs an in-place upgrade) will live here.
 */
export function payloadSchemaVersion(payload: unknown): number {
  if (
    payload &&
    typeof payload === 'object' &&
    'schemaVersion' in payload &&
    typeof (payload as { schemaVersion: unknown }).schemaVersion === 'number'
  ) {
    return (payload as { schemaVersion: number }).schemaVersion;
  }
  return 1;
}
