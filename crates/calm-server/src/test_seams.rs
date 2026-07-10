//! Deterministic crash injection for out-of-process crash-recovery tests
//! (#840 e2/e3).
//!
//! Prod-safety contract:
//!   * `crash_point` only exists under the `fixtures` feature, which the
//!     production `calm-server` binary never enables, and every call site
//!     must be wrapped in `#[cfg(feature = "fixtures")]` — there is no
//!     unconditional stub, so a non-gated call site fails to compile in a
//!     release build. A `cargo build --release` therefore compiles zero
//!     code for the seam: no call, no argument construction, and no "an
//!     env var could crash prod" surface at all.
//!     `CARGO_BIN_EXE_calm-server` under `cargo test` IS built with
//!     `fixtures` on (the `[dev-dependencies]` self-loop in Cargo.toml), so
//!     the harness-spawned binary can reach it with zero CI plumbing.
//!   * Even in a fixtures build it is double-gated: it fires only when the
//!     process env var `CALM_TEST_CRASH_AT` equals `point` exactly. When the
//!     env var is unset (a fixtures build outside a crash test), each call
//!     costs one `env::var` lookup plus the call site's argument
//!     construction — nothing more.
//!   * It aborts rather than panics: a panic unwinds, so `Drop` impls would
//!     roll transactions back gracefully and only the calling task would die
//!     while the server keeps serving — not a crash. `abort()` kills the
//!     process instantly (SIGABRT), no destructors — SIGKILL durability
//!     semantics, deterministically placed.

/// Crash the process here iff `CALM_TEST_CRASH_AT` equals `point` exactly.
///
/// Call sites MUST be gated with `#[cfg(feature = "fixtures")]` (the whole
/// statement, so the argument expression is compiled out too) and qualify
/// `point` with enough context (e.g. the typed event kind) that a test can
/// target one specific operation without tripping on other operations
/// flowing through the same completion path.
#[cfg(feature = "fixtures")]
pub fn crash_point(point: &str) {
    if std::env::var("CALM_TEST_CRASH_AT").is_ok_and(|v| v == point) {
        eprintln!("CALM_TEST_CRASH_AT={point}: aborting for crash-recovery test");
        std::process::abort();
    }
}
