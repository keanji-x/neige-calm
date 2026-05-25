//! # INV-6 — startup-entry symmetry (create-wave vs takeover)
//!
//! **Bug**: R3-B2 (from #318)
//! **Encoded contract**: the two paths that park a `SpecPushHandle` in
//! `SpecPushRegistry` — `routes::waves::spawn_push_appserver` (fresh
//! wave) and `lib::register_and_catch_up` (boot takeover) — must run
//! the SAME init hook. Concretely: every parked handle must have a
//! `WatermarkSink` installed before it can be reached by a push;
//! without it, queued-then-flushed observations silently fail to
//! advance the durable `push_watermark`, and boot recovery
//! double-pushes those envelopes forever.
//!
//! The architectural enforcement of this contract — not the
//! convention — is what INV-6 demands. A future refactor that adds a
//! THIRD entry point (e.g. a "manual restart" route for inert waves,
//! a debug "re-attach" admin command, or a new spec wave kind) will
//! almost certainly omit the install — that's the bug-shape that
//! produced R3-B2 in the first place.
//!
//! ## Why this encoding (vs the v1 version)
//!
//! v1 asserted (a) at most ONE `install_watermark_sink(` callsite
//! across `lib.rs` + `routes/waves.rs`, and (b)
//! `SpecPushRegistry::insert` returns a type containing `Result<` or
//! `Sink`. Codex flagged both:
//!
//! - (a) A correct fix CAN keep two callsites (one per path) and
//!   enforce the install via a runtime check at registration; the v1
//!   count-callsites test would fail that fix.
//! - (b) `Sink` matching the return-type string is brittle (a fix that
//!   names the witness type `WithSink` matches; one named
//!   `RegisteredHandle` doesn't).
//!
//! The honest INV-6 encoding has two halves:
//!
//! - **(a)** A behavioral test that PROVES the symmetry by exercising
//!   both paths and asserting the same observable invariant (sink
//!   installed) holds on both. **Blocked**: neither
//!   `spawn_push_appserver` nor `register_and_catch_up` is `pub`, and
//!   the `SpecPushHandle::has_watermark_sink` getter is `pub` but
//!   `SpecPushRegistry::status` (the registry's only public observer)
//!   doesn't expose it. Encoded as `#[ignore]`.
//!
//! - **(b)** An API-shape test that asserts `SpecPushRegistry::insert`
//!   either makes the sink-installed precondition unforgeable
//!   (returning a `Result<…>` whose error type names the missing
//!   precondition, OR taking a witness wrapper that can only be
//!   constructed via the shared init helper) OR exposes an alternate
//!   registration fn that does. We accept either shape; today neither
//!   exists. Active, fails on main.
//!
//! ## What a correct fix looks like
//!
//! A correct fix introduces a fallible registration surface OR a
//! witness type, e.g.:
//!
//! ```ignore
//! // Option A — fallible registration with named precondition:
//! pub enum RegisterError { SinkNotInstalled }
//! impl SpecPushRegistry {
//!     pub fn insert(&self, wave_id: WaveId, handle: SpecPushHandle)
//!         -> Result<Option<SpecPushHandle>, RegisterError>;
//! }
//!
//! // Option B — witness type, register accepts only post-init handle:
//! pub struct RegisteredSpecPushHandle(SpecPushHandle); // ctor private,
//!     // built only via register_spec_handle that installs the sink.
//! impl SpecPushRegistry {
//!     pub fn insert(&self, wave_id: WaveId, handle: RegisteredSpecPushHandle)
//!         -> Option<RegisteredSpecPushHandle>;
//! }
//!
//! // Option C — explicit register_spec_handle helper that both
//! // paths must call (and that performs the install + insert
//! // atomically, returning a Result):
//! pub fn register_spec_handle(
//!     registry: &SpecPushRegistry, dispatcher: &Dispatcher,
//!     wave_id: WaveId, handle: SpecPushHandle,
//! ) -> Result<(), RegisterError>;
//! ```
//!
//! Any of these would make the sink-installed precondition
//! architecturally unforgeable. Today the precondition lives in two
//! open-coded `debug_assert!` blocks that compile out in release.
//!
//! See: `src/lib.rs::register_and_catch_up` (line ~470: install +
//! `debug_assert!`), `src/routes/waves.rs::spawn_push_appserver`
//! (line ~963: install + `debug_assert!`),
//! `src/spec_appserver.rs::SpecPushRegistry::insert`
//! (line ~714: `pub fn insert(&self, wave_id: WaveId, handle:
//! SpecPushHandle) -> Option<SpecPushHandle>`).

use std::path::PathBuf;

fn read_repo_src(rel: &str) -> String {
    let root = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR in test ctx");
    let path = PathBuf::from(root).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// INV-6 (a): **behavioral both-paths-symmetric test** —
/// `#[ignore]`'d because no public API exposes the "sink installed?"
/// bit at the registry boundary.
///
/// Sketch of the test the fix would unlock:
///
/// ```ignore
/// // Path A: create-wave route end-to-end (requires a recorder/fake
/// // codex bin like spec_push_boot_recovery_e2e.rs uses).
/// let (state_A, _) = build_state_with_fake_codex(&tmp_A).await;
/// let resp = post_create_wave_with_spec_goal(state_A).await;
/// let handle_A = state_A.spec_push.get(&wave_A).expect("registered");
/// assert!(handle_A.has_watermark_sink().await,
///     "INV-6: create-wave path must leave sink installed");
///
/// // Path B: takeover end-to-end (same fixture, kill kernel mid-turn,
/// // rebuild AppState, run takeover_spec_appservers_on_boot).
/// drop(state_A);  // simulated kernel exit
/// let (state_B, _) = build_state_with_fake_codex(&tmp_A).await;
/// takeover_spec_appservers_on_boot(&state_B).await;
/// let handle_B = state_B.spec_push.get(&wave_A).expect("re-registered");
/// assert!(handle_B.has_watermark_sink().await,
///     "INV-6: takeover path must leave sink installed");
///
/// // Symmetry: any post-init observable that holds on A must hold on B.
/// // (Sink installed is the load-bearing one for R3-B2; the framework
/// // would iterate any additional invariants — e.g. push_cursor seeded.)
/// ```
///
/// Today neither (i) `SpecPushRegistry::get(&WaveId) -> Option<&SpecPushHandle>`
/// nor (ii) `SpecPushRegistry::has_sink(&WaveId) -> Option<bool>` is
/// `pub`. Adding one is a production change.
#[test]
#[ignore = "blocked-by: SpecPushRegistry exposes neither a `get(&WaveId) -> Option<&SpecPushHandle>` \
            nor a `has_sink(&WaveId) -> Option<bool>` public surface for tests to observe \
            'sink installed?' after registration. A behavioral both-paths-symmetric test \
            (run create-wave end-to-end, run takeover end-to-end, assert sink installed in \
            both) needs one of those getters. The existing pub `SpecPushRegistry::status` \
            returns SpecPushStatus (phase + thread_id), which doesn't include sink presence. \
            #318 forbids production changes. See: spec_appserver.rs::SpecPushHandle::has_watermark_sink \
            is pub (~line 635) but the registry doesn't expose the handle. INV-6 in #318."]
fn inv6_both_init_paths_must_leave_sink_installed() {
    // Sketch in the file header; this body is intentionally empty
    // pending the production seam. `#[ignore]` ensures the test
    // doesn't pretend to pass.
    panic!(
        "INV-6 violated: no public way to observe 'sink installed?' on a \
         registered handle. The behavioral both-paths-symmetric test cannot be \
         written without either SpecPushRegistry::get/has_sink, or a `pub fn` \
         that runs each path and returns the parked handle for inspection. \
         The symmetry today is enforced only by colocated `debug_assert!` calls \
         (compiled out in release) at the two install sites — a future third \
         entry point will silently omit the install. See file header for the \
         test sketch."
    );
}

/// INV-6 (b): **API-shape test** — the registration surface must make
/// the sink-installed precondition architecturally unforgeable
/// (return a `Result<…>` naming the precondition, take a witness
/// wrapper type, or accept an alternate `register_spec_handle` helper
/// that performs the install atomically with the insert).
///
/// We accept ANY of these shapes by scanning `spec_appserver.rs` for
/// the canonical signatures. This is broader than v1 (which pinned a
/// specific `Result<` substring on `insert`'s return); it accepts any
/// architectural enforcement, not a specific one.
///
/// On main, none of the shapes are present: `insert` is the only
/// registration fn, returns plain `Option<SpecPushHandle>`, and the
/// sink-enforcement lives in `debug_assert!` calls at the two
/// callsites. Fails.
#[test]
fn inv6_registry_must_architecturally_enforce_sink_installed() {
    let src = read_repo_src("src/spec_appserver.rs");

    // Search for any of three acceptable enforcement shapes:
    //
    // (A) `SpecPushRegistry::insert` returns a `Result<…>` (callers
    //     forced to handle the failure to register a sinkless handle).
    //     We allow any spelling of the error type.
    //
    // (B) A witness wrapper type whose name suggests "post-init" /
    //     "registered" / "with-sink" handle, used as the `insert`
    //     argument type instead of bare `SpecPushHandle`. We accept
    //     several spellings.
    //
    // (C) An alternate registration fn — `register_spec_handle` /
    //     `register_handle` / `install_and_insert` — that takes the
    //     dispatcher (for the sink) plus the handle and performs both
    //     operations atomically. We accept several spellings.

    // (A) `pub fn insert(... ) -> Result<...>`
    let insert_has_result = src.contains("pub fn insert(&self, wave_id: WaveId")
        && src
            .lines()
            .filter(|l| l.contains("pub fn insert(&self, wave_id: WaveId"))
            .any(|l| l.contains("-> Result<"));

    // (B) witness-wrapper arg type on insert.
    let witness_wrappers = [
        "handle: RegisteredSpecPushHandle",
        "handle: SpecPushHandleWithSink",
        "handle: InitializedSpecPushHandle",
        "handle: SpecPushHandlePostInit",
    ];
    let insert_has_witness = witness_wrappers.iter().any(|w| src.contains(w));

    // (C) alternate registration helper fn names.
    let helper_names = [
        "fn register_spec_handle(",
        "fn register_handle(",
        "fn install_and_insert(",
        "fn register_with_sink(",
    ];
    let helper_present = helper_names.iter().any(|n| src.contains(n));

    let architecturally_enforced = insert_has_result || insert_has_witness || helper_present;

    assert!(
        architecturally_enforced,
        "INV-6 violated: SpecPushRegistry exposes only `pub fn insert(&self, \
         wave_id: WaveId, handle: SpecPushHandle) -> Option<SpecPushHandle>` — \
         no architectural enforcement of the sink-installed precondition. The \
         convention today is two open-coded `install_watermark_sink` calls \
         (one in lib.rs::register_and_catch_up, one in \
         routes/waves.rs::spawn_push_appserver) each followed by a \
         `debug_assert!(handle.has_watermark_sink().await)` — debug_assert \
         compiles out in release, so a future third entry point that forgets \
         the install silently parks a sink-less handle. A correct fix uses one \
         of: \
         (A) `insert -> Result<…, SinkNotInstalled>`, \
         (B) a witness type `RegisteredSpecPushHandle` (or similar) that can \
             only be constructed by the shared init helper, OR \
         (C) a `fn register_spec_handle(&Dispatcher, &SpecPushRegistry, …)` \
             helper that performs the install + insert atomically. \
         Probe outcomes: insert_has_result={insert_has_result}, \
         insert_has_witness={insert_has_witness}, helper_present={helper_present}."
    );
}
