//! Aspect / Join-point framework (#322).
//!
//! Framework-level enforcement of cross-cutting invariants. The motivation
//! and design are written up on issue #322 ("Aspect / Join-point framework:
//! OCP-shaped invariant enforcement, replaces #317 v5"). Short version:
//!
//! - The framework **closes** over a small, empirically stable set of
//!   [`JoinPoint`]s — physical sites in the kernel where an invariant
//!   would have to fire if it were to fire at all — plus a per-join-point
//!   aspect trait (`*Aspect`) and a single [`AspectRegistry`] that holds
//!   `Arc<dyn …Aspect>` lists.
//! - The framework is **open** for new invariants: each invariant becomes
//!   one `impl …Aspect` + one `register_…` call. Adding an invariant
//!   touches zero framework code (modulo registering it at boot).
//!
//! This is the minimum-viable landing of #322: only one join point
//! (`BeforeHandleParkInRegistry`) and one aspect impl
//! ([`WatermarkSinkInstalledAspect`]) — the framework-level upgrade of the
//! `debug_assert!(handle.has_watermark_sink())` invariant introduced by
//! #313 / PR #315 and codified as INV-6 in #318 / PR #321. Adding the
//! other 4 join points from the #322 design (BeforeTxCommit / BeforeKill /
//! BeforeDecide / BeforeWriteColumn) is intentionally out of scope here;
//! that's the per-INV unblock work referenced in #318's INV table.
//!
//! ## Why panic on failure
//!
//! Aspects encode invariants the kernel believes are unconditionally true.
//! An aspect failing means *the program already corrupted its own state*
//! (e.g. a refactor split the watermark-sink install from the registry
//! park, so queued-then-flushed envelopes will silently fail to persist
//! their watermark — exactly the #313 bug class INV-6 guards against).
//! `debug_assert!` was a dev-only enforcement; the aspect lifts that to
//! release-mode fail-fast so production refactors that drop an invariant
//! crash on the first violation instead of corrupting durable state.
//!
//! ## Relationship to existing `debug_assert!`s
//!
//! The PR #315 / PR #321 `debug_assert!(handle.has_watermark_sink())`
//! sites stay in place as **local fast-fail**: they catch the bug at the
//! installation site in dev (closest to the offending diff) before the
//! handle even reaches the registry. The aspect on
//! `BeforeHandleParkInRegistry` is the **framework-level** enforcement
//! that survives release builds and any future install-site refactor.
//! Belt + suspenders, both pointing at the same invariant.

use std::sync::Arc;

use crate::ids::WaveId;
use crate::spec_appserver::SpecPushHandle;

/// Stable join-point enum. Closed: extending the framework with a new
/// invariant class means adding a variant here, and the framework code
/// (registry + per-JP aspect trait + per-JP dispatch) grows by one
/// vector. The set itself is OCP's hard boundary — when this enum needs
/// to grow, the framework needs an audit, not a typical PR.
///
/// **In scope today**: only [`Self::BeforeHandleParkInRegistry`]. The
/// other four variants from the #322 design (`BeforeTxCommit`,
/// `BeforeKill`, `BeforeDecide`, `BeforeWriteColumn`) are intentionally
/// not declared here yet: the #322 minimum-viable landing proves the
/// mechanism on INV-6 alone, and each remaining INV gets its own PR
/// (with its own unblock dependencies — see issue #318's INV table).
/// Adding a variant later is mechanical: declare it here, add the
/// matching `<Variant>Aspect` trait + registry field + dispatch fn.
///
/// The enum exists to make the closed/open split *explicit in the type
/// system* — any reader scanning `JoinPoint` sees the full set of
/// framework-blessed extension points and immediately knows what is and
/// isn't covered. Right now the only variant is the join point INV-6
/// guards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JoinPoint {
    /// Fired by [`crate::spec_appserver::SpecPushRegistry::park`] right
    /// before a [`SpecPushHandle`] is `insert`ed into the keyed
    /// `DashMap`. Aspects on this join point see the handle + the
    /// `WaveId` it'll be registered under, and decide whether the
    /// handle is in a state where parking it is safe (e.g. INV-6:
    /// watermark sink installed, so a future queue flush can persist
    /// its watermark).
    BeforeHandleParkInRegistry,
}

/// Read-only context handed to every [`BeforeHandleParkAspect::check`]
/// callback. References are borrowed for the call duration — aspects
/// cannot stash them anywhere (`'a` is the call's lifetime), which keeps
/// the registry-park hot path free of contention with future aspects.
pub struct HandleContext<'a> {
    /// The handle about to be parked. Aspects inspect handle state
    /// (e.g. `has_watermark_sink`) but never mutate it.
    pub handle: &'a SpecPushHandle,
    /// The key the handle is about to be inserted under. Carried so
    /// aspect failure messages can include the wave id without the
    /// aspect having to thread it independently.
    pub wave_id: &'a WaveId,
}

/// Failure produced by an aspect when its invariant is violated. The
/// `name` is the aspect's identifier (e.g. `"watermark-sink-installed"`),
/// `reason` is the human-readable explanation that lands in the panic
/// message. Both are owned `&'static str` for simplicity — aspects don't
/// allocate per check today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AspectViolation {
    pub aspect: &'static str,
    pub reason: &'static str,
}

impl std::fmt::Display for AspectViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "aspect `{}` failed: {}", self.aspect, self.reason)
    }
}

impl std::error::Error for AspectViolation {}

/// Aspect trait for the [`JoinPoint::BeforeHandleParkInRegistry`] slot.
///
/// Implementors:
/// - return a `&'static str` name so the panic message identifies which
///   aspect tripped;
/// - implement an async `check` because state lookups on
///   [`SpecPushHandle`] (e.g. `has_watermark_sink`) are async (they
///   take a `Mutex` async lock).
///
/// `Send + Sync` so an `Arc<dyn …>` can live on shared state
/// ([`crate::state::AppState::aspects`]) across the entire kernel.
#[async_trait::async_trait]
pub trait BeforeHandleParkAspect: Send + Sync {
    /// Stable identifier surfaced in panic messages. Conventionally
    /// kebab-case, e.g. `"watermark-sink-installed"`.
    fn name(&self) -> &'static str;

    /// Run the invariant check. `Ok(())` = invariant holds; `Err(...)`
    /// = invariant violated, the framework will panic.
    async fn check(&self, ctx: &HandleContext<'_>) -> Result<(), AspectViolation>;
}

/// Registry of all installed aspects, keyed by [`JoinPoint`]. Lives on
/// [`crate::state::AppState::aspects`] as `Arc<AspectRegistry>` so every
/// handler / actor sees the same set.
///
/// Construction is two-phase: [`Self::new`] starts empty, callers
/// register aspects via the per-join-point `register_…` methods (only
/// `register_before_handle_park` today), then the result is wrapped in
/// `Arc` and stashed on [`crate::state::AppState`]. The registry is
/// *not* mutated after that — aspect installation is a boot-time
/// concern, never a runtime one (no hot reload, no per-request
/// registration). The fields are `Vec<Arc<…>>` so the trait objects can
/// be shared across the registry's clones without re-wrapping.
#[derive(Default)]
pub struct AspectRegistry {
    /// Aspects to run before a [`SpecPushHandle`] is parked in
    /// [`crate::spec_appserver::SpecPushRegistry`]. Iterated in
    /// registration order — order matters only insofar as the first
    /// failing aspect determines which name lands in the panic
    /// message; semantically every aspect must pass.
    before_handle_park: Vec<Arc<dyn BeforeHandleParkAspect>>,
}

impl AspectRegistry {
    /// Fresh, empty registry. Caller registers aspects via the
    /// `register_…` methods before wrapping in `Arc`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Install an aspect on the [`JoinPoint::BeforeHandleParkInRegistry`]
    /// slot. Append-only.
    pub fn register_before_handle_park(&mut self, aspect: Arc<dyn BeforeHandleParkAspect>) {
        self.before_handle_park.push(aspect);
    }

    /// Run every aspect installed on the
    /// [`JoinPoint::BeforeHandleParkInRegistry`] slot. The first
    /// failure panics with a message identifying the aspect, the
    /// wave, and the invariant reason. Release-mode: this is the
    /// load-bearing enforcement (the `debug_assert!`s at install
    /// sites stay as local fast-fail in dev/test).
    ///
    /// Why panic instead of `Result`: an aspect violation means the
    /// kernel just discovered its own state is corrupted past the
    /// point a route handler could recover. The only safe action is
    /// to crash and let the process supervisor restart us, where the
    /// startup invariants (PR #315's boot-takeover, INV-6 itself)
    /// re-establish the contract from persistent state. Returning a
    /// `Result` would let a careless caller swallow the violation and
    /// keep running with the invariant broken — exactly the silent-
    /// failure mode INV-6 exists to prevent.
    pub async fn run_before_handle_park(&self, ctx: &HandleContext<'_>) {
        for aspect in &self.before_handle_park {
            if let Err(violation) = aspect.check(ctx).await {
                panic!(
                    "aspect framework: invariant violation on \
                     BeforeHandleParkInRegistry for wave={wave}: {violation}",
                    wave = ctx.wave_id,
                );
            }
        }
    }

    /// Test/diagnostic accessor: how many aspects are installed on the
    /// [`JoinPoint::BeforeHandleParkInRegistry`] slot. Public so unit
    /// tests can assert registration wiring without reaching the
    /// private field.
    pub fn before_handle_park_len(&self) -> usize {
        self.before_handle_park.len()
    }
}

// ---------------------------------------------------------------------------
// Aspect impls
// ---------------------------------------------------------------------------

/// INV-6 — every [`SpecPushHandle`] parked in
/// [`crate::spec_appserver::SpecPushRegistry`] must already have a
/// [`crate::spec_appserver::WatermarkSink`] installed. Without one, a
/// queued-then-flushed observation would deliver but the durable
/// `push_watermark` would never advance past it — boot replay would
/// then re-deliver it to the spec thread. See module doc + #313 round-3
/// for the original bug; #318 / PR #321's
/// `inv_06_startup_symmetry.rs` is the integration-test sibling of this
/// aspect (the aspect is the framework's enforcement; the test is the
/// regression net).
pub struct WatermarkSinkInstalledAspect;

#[async_trait::async_trait]
impl BeforeHandleParkAspect for WatermarkSinkInstalledAspect {
    fn name(&self) -> &'static str {
        "watermark-sink-installed"
    }

    async fn check(&self, ctx: &HandleContext<'_>) -> Result<(), AspectViolation> {
        if ctx.handle.has_watermark_sink().await {
            Ok(())
        } else {
            Err(AspectViolation {
                aspect: self.name(),
                reason: "SpecPushHandle reached SpecPushRegistry::park without an \
                        installed WatermarkSink — a queued-then-flushed envelope \
                        would silently fail to persist its watermark (#313 / INV-6)",
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A passing aspect, used as a control in the registration-wiring test.
    struct AlwaysOkAspect;

    #[async_trait::async_trait]
    impl BeforeHandleParkAspect for AlwaysOkAspect {
        fn name(&self) -> &'static str {
            "always-ok"
        }

        async fn check(&self, _ctx: &HandleContext<'_>) -> Result<(), AspectViolation> {
            Ok(())
        }
    }

    #[test]
    fn registry_starts_empty_and_grows_on_register() {
        let mut reg = AspectRegistry::new();
        assert_eq!(reg.before_handle_park_len(), 0);
        reg.register_before_handle_park(Arc::new(AlwaysOkAspect));
        assert_eq!(reg.before_handle_park_len(), 1);
        reg.register_before_handle_park(Arc::new(WatermarkSinkInstalledAspect));
        assert_eq!(reg.before_handle_park_len(), 2);
    }

    #[test]
    fn watermark_sink_installed_aspect_name_is_stable() {
        // The panic message includes this name; tests + tracing depend on
        // it. Pin it so a rename has to be intentional.
        assert_eq!(
            WatermarkSinkInstalledAspect.name(),
            "watermark-sink-installed"
        );
    }

    #[test]
    fn aspect_violation_display_includes_name_and_reason() {
        let v = AspectViolation {
            aspect: "foo",
            reason: "bar",
        };
        let s = format!("{v}");
        assert!(s.contains("foo"), "{s}");
        assert!(s.contains("bar"), "{s}");
    }
}
