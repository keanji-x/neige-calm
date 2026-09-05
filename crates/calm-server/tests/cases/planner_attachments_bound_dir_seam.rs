//! The type-level half of "`bound/` has no deletion path".
//!
//! `trybuild` compiles its samples as separate crates that depend on
//! `calm-server`, so what it pins is the *crate-external* statement: nothing
//! downstream can turn a `BoundDir` into the `StagingDir` a deletion wants.
//! The in-crate half is enforced by the module's own privacy — `StagingDir`'s
//! and `BoundDir`'s fields are private, so even inside `planner_attachments`
//! there is no conversion to write except the one this case forbids.
//!
//! The `.stderr` file pins the *diagnostic*, not merely "it failed": a typo in
//! a sample also makes a build fail. It is toolchain-sensitive by construction;
//! the toolchain is pinned in `rust-toolchain.toml`, and a bump that rewords
//! the diagnostic regenerates it with
//! `TRYBUILD=overwrite cargo test -p calm-server --test planner_harness_suite`.

#[test]
fn a_bound_directory_cannot_be_handed_to_a_deletion() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/staging_dir_is_deletable.rs");
    t.compile_fail("tests/ui/bound_dir_cannot_be_deleted.rs");
}
