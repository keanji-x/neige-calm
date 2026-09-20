//! Cross-crate half of the append-seam guard: a downstream crate cannot name the capability type
//! or the raw appender. The `.stderr` files pin the diagnostic and are toolchain-sensitive; a toolchain
//! bump regenerates them with `TRYBUILD=overwrite cargo test -p calm-truth --test integration_suite`.

#[test]
fn append_seam_is_not_reachable_from_a_downstream_crate() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/public_entrance_is_reachable.rs");
    t.compile_fail("tests/ui/name_authorized_capability.rs");
    t.compile_fail("tests/ui/call_event_append_in_tx.rs");
}
