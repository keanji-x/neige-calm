//! # INV-6 — startup-entry symmetry (create-wave vs takeover)
//!
//! **Bug**: R3-B2 (from #318)
//! **Encoded contract**: the two paths that park a `SpecPushHandle` in
//! `SpecPushRegistry` — `routes::waves::spawn_push_appserver` (fresh
//! wave) and `lib::register_and_catch_up` (boot takeover) — must run
//! the SAME init hook. Concretely: every parked handle must have a
//! `WatermarkSink` installed; if either path forgets, queued-then-
//! flushed observations silently fail to advance the durable
//! `push_watermark`, and boot recovery double-pushes those envelopes
//! forever.
//!
//! **Why this design**: today both paths happen to call
//! `handle.install_watermark_sink(...)` correctly. But the symmetry is
//! enforced only by two open-coded call sites side-by-side with
//! `debug_assert!`s — there is no SHARED HELPER (e.g.
//! `Dispatcher::register_spec_handle_for_wave(wave, handle)`) that both
//! paths funnel through, no production-grade assertion that fails when
//! a sink-less handle lands in the registry, and `SpecPushRegistry::
//! insert` accepts any handle blindly. A future refactor that adds a
//! THIRD entry point (e.g. a hypothetical "manual restart" route for
//! inert waves under #313 problem #2, or a debug "re-attach" admin
//! command) will almost certainly omit the install — the bug-shape
//! that produced R3-B2 in the first place. INV-6 demands the
//! ARCHITECTURE enforce the install, not the convention.
//!
//! **Current behavior on main**: no shared helper exists; the registry
//! `insert` does not check `has_watermark_sink`; both call sites use
//! `debug_assert!` (compiled out in release) and rely on a comment.
//!
//! We encode the invariant as:
//!   (1) `SpecPushRegistry::insert` does NOT statically reject a
//!       handle without an installed `WatermarkSink` — there's no
//!       fallible-insert API to even call.
//!   (2) Both production sites use ad-hoc inline `install_watermark_sink`
//!       calls (no shared helper).
//!
//! Test approach: walk the source of `lib.rs` + `routes/waves.rs` and
//! count the bare `install_watermark_sink` call sites — INV-6 says
//! "must be 0 OR funnel through one shared helper". Currently 2
//! independent call sites, so the invariant fails.
//!
//! See: `src/lib.rs::register_and_catch_up` (line ~514) and
//! `src/routes/waves.rs::spawn_push_appserver` (line ~983).

use std::path::PathBuf;

fn read_repo_src(rel: &str) -> String {
    // The integration test crate's `CARGO_MANIFEST_DIR` points at
    // `crates/calm-server`. Relative source paths off that root resolve
    // identically in `cargo test` (whether run from workspace root or
    // crate dir).
    let root = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR in test ctx");
    let path = PathBuf::from(root).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Count occurrences of `needle` in `haystack` (non-overlapping).
fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

/// INV-6 strict: there must be at MOST ONE callsite of
/// `install_watermark_sink` in production source — the shared helper
/// both paths funnel through. Today there are TWO independent
/// callsites (one in `lib.rs`'s takeover, one in `routes/waves.rs`'s
/// create-wave), each with a colocated `debug_assert!` that does
/// nothing in release builds. A new entry point trivially forgets to
/// install; the symmetry is by convention, not architecture.
#[test]
fn inv6_single_shared_handle_init_callsite() {
    let lib_src = read_repo_src("src/lib.rs");
    let waves_src = read_repo_src("src/routes/waves.rs");

    // Count `install_watermark_sink(` — the production install call.
    // We intentionally include `(` so we count CALLS, not docstring
    // references / type definitions.
    let in_lib = count(&lib_src, "install_watermark_sink(");
    let in_waves = count(&waves_src, "install_watermark_sink(");

    // Sanity floor: at least one each today (the production code does
    // install in both paths — the bug isn't that nobody installs, it's
    // that there's no shared helper).
    assert!(
        in_lib >= 1,
        "expected at least one install_watermark_sink call in lib.rs (takeover); got {in_lib}",
    );
    assert!(
        in_waves >= 1,
        "expected at least one install_watermark_sink call in routes/waves.rs (create-wave); got {in_waves}",
    );

    // INV-6 strict: the TOTAL across BOTH files must be 1 — a single
    // shared helper that both paths call. Today the count is 2 (one
    // per path), proving there's no shared helper. A future fix would
    // extract `Dispatcher::register_spec_handle_for_wave(wave, handle)`
    // or similar, leaving exactly one install_watermark_sink call site
    // inside that helper.
    let total = in_lib + in_waves;
    assert_eq!(
        total, 1,
        "INV-6 violated: install_watermark_sink is called from {total} call sites \
         (lib.rs: {in_lib}, routes/waves.rs: {in_waves}). Each path open-codes the \
         sink install with a debug_assert! adjacent — a release build silently \
         accepts a forgotten install. INV-6 requires architectural enforcement: \
         both paths must funnel through a single shared `register_spec_handle` \
         helper that performs the install (and verifies it via a production-mode \
         assertion / Result, not debug_assert)."
    );
}

/// INV-6 strict (b): `SpecPushRegistry::insert` accepts any handle
/// blindly. The invariant says: a handle without an installed sink
/// must not be parkable. Today the API surface does not even allow
/// signalling that constraint — `insert` returns `Option<SpecPushHandle>`
/// (the prior entry), not `Result<…, NoSinkInstalled>`. We encode the
/// API constraint as a string match against the public signature.
#[test]
fn inv6_registry_insert_must_enforce_sink_present() {
    let src = read_repo_src("src/spec_appserver.rs");
    // Find the production `insert` signature on `SpecPushRegistry`.
    let needle = "pub fn insert(&self, wave_id: WaveId, handle: SpecPushHandle)";
    let pos = src.find(needle).expect(
        "SpecPushRegistry::insert signature not found in spec_appserver.rs — \
         test fixture is out of date",
    );
    // Capture the return type on the same line.
    let after = &src[pos + needle.len()..];
    let line_end = after.find('\n').unwrap_or(after.len());
    let return_clause = &after[..line_end];

    // INV-6 strict: the return type must encode the sink-installed
    // contract — e.g. `Result<Option<SpecPushHandle>, NoSinkInstalled>`
    // or take an explicit "sink-installed" witness token. Today the
    // return is plain `Option<SpecPushHandle>` (the prior handle on
    // replace).
    let encodes_constraint = return_clause.contains("Result<") || return_clause.contains("Sink");
    assert!(
        encodes_constraint,
        "INV-6 violated: SpecPushRegistry::insert signature is `{needle}{return_clause}` — \
         it returns `Option<SpecPushHandle>` (the replaced handle) with no encoding \
         of the sink-installed precondition. INV-6 requires the insert API to make \
         the precondition unforgeable: e.g. `insert(WaveId, HandleWithSink) -> …` \
         where `HandleWithSink` is a wrapper that can only be constructed by going \
         through the shared init helper, OR a fallible `Result<…, NoSinkInstalled>` \
         return. Today both paths can park a sink-less handle without complaint."
    );
}
