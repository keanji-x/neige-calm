//! Compile-time privacy of `Template` / `TemplateRoster` as seen from a downstream crate.
//! The `.stderr` files are toolchain-sensitive; regenerate with `TRYBUILD=overwrite cargo test -p calm-server --test track_suite templates_privacy`.

#[test]
fn a_downstream_crate_cannot_forge_a_template_or_a_roster() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/template_roster_is_readable.rs");
    t.compile_fail("tests/ui/template_cannot_be_constructed.rs");
    t.compile_fail("tests/ui/template_roster_cannot_be_constructed.rs");
    t.compile_fail("tests/ui/template_roster_constructors_are_private.rs");
}
