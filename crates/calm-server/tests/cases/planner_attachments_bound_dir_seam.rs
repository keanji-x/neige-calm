//! The type-level half of "`bound/` has no deletion path": a `trybuild` sample pinning that nothing downstream can
//! turn a `BoundFd` into a `StagingFd`. A toolchain bump that rewords the diagnostic regenerates the `.stderr` with
//! `TRYBUILD=overwrite cargo test -p calm-server --test planner_harness_suite`.

#[test]
fn a_bound_directory_cannot_be_handed_to_a_deletion() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/staging_fd_is_deletable.rs");
    t.compile_fail("tests/ui/bound_fd_cannot_be_deleted.rs");
}
