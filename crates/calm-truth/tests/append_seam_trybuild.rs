//! #1252 S3′ PR-B — the **cross-crate** half of the append-seam guard.
//!
//! Read this together with `mod append_seam_escape_probe` in
//! `src/db/sqlite/events.rs`. The two halves guard two different statements and
//! neither subsumes the other:
//!
//!   * the in-crate probe (a `#[cfg(feature)]` module) guards *module-external,
//!     crate-internal* visibility — that a sibling module of `gated`, inside
//!     `calm-truth`, still cannot forge or retarget an `Authorized` and still
//!     cannot hand the appender a loose triple;
//!   * this suite guards *crate-external* visibility — that a downstream crate
//!     cannot name the capability type or the raw appender at all.
//!
//! `trybuild` can only carry the second one. Its samples are compiled as
//! separate crates that depend on `calm-truth`, so a sample aimed at the first
//! statement would fail on an unresolved path and the case would pass
//! vacuously — the same trap `crates/calm-proc-supervisor/src/lib.rs` records
//! next to its own probe. That is why the in-crate half is a feature-gated
//! module and not another `.rs` in `tests/ui/`.
//!
//! The `.stderr` files pin the *diagnostic*, not merely "it failed" — a typo in
//! a sample also makes a build fail. They are toolchain-sensitive by
//! construction; the toolchain is pinned in `rust-toolchain.toml`, and a
//! toolchain bump that rewords these diagnostics regenerates them with
//! `TRYBUILD=overwrite cargo test -p calm-truth --test integration_suite`.
//!
//! `ui/public_entrance_is_reachable.rs` is the green fixture. Without it, both
//! `compile_fail` cases could be red for a reason unrelated to the seam.

#[test]
fn append_seam_is_not_reachable_from_a_downstream_crate() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/public_entrance_is_reachable.rs");
    t.compile_fail("tests/ui/name_authorized_capability.rs");
    t.compile_fail("tests/ui/call_event_append_in_tx.rs");
}
