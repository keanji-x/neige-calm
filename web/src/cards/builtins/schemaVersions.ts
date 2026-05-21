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
// Reads use the helper below: missing field is `1`, since `1` is the
// only version that exists today across all kernel-owned kinds.

/** `schemaVersion` for `kind: "terminal"` card payloads. */
export const TERMINAL_PAYLOAD_SCHEMA_VERSION = 1;
/** `schemaVersion` for `kind: "codex"` card payloads. */
export const CODEX_PAYLOAD_SCHEMA_VERSION = 1;
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

/**
 * Read `schemaVersion` from a kernel-owned payload. Returns `1` when the
 * field is absent or not a number — version `1` is the only version
 * that has ever shipped, so absent-as-1 is unambiguous and stays
 * backward-compatible with rows written before this field existed.
 *
 * Migrators will live here when v2 is introduced.
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
