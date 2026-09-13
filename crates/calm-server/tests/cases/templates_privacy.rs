//! The cross-crate half of the template roster's privacy story (#1318 S2,
//! restated for files by #1635 S4 — see `templates::Template`'s doc).
//!
//! `trybuild` compiles its samples as separate crates that depend on
//! `calm-server`, so what it pins is the *crate-external* statement: nothing
//! downstream can write a `Template` or a `TemplateRoster` literal, and nothing
//! downstream can call the roster's constructors — the only `&'static Template`
//! a downstream crate can name is a borrow of a `TemplateRoster::builtin()`
//! entry. The in-crate half (accessor ↔ private field pointer identity,
//! `get` returning the roster's own borrow) lives in `templates::tests`,
//! because those comparisons name the private fields and cannot be written
//! from outside the defining module.
//!
//! The `.stderr` files pin the *diagnostic*, not merely "it failed": a typo in
//! a sample also makes a build fail. They are toolchain-sensitive by
//! construction; the toolchain is pinned in `rust-toolchain.toml`, and a bump
//! that rewords the diagnostics regenerates them with
//! `TRYBUILD=overwrite cargo test -p calm-server --test track_suite templates_privacy`.
//!
//! `ui/template_roster_is_readable.rs` is the green fixture. Without it, the
//! three `compile_fail` cases could be red for a reason unrelated to privacy.

#[test]
fn a_downstream_crate_cannot_forge_a_template_or_a_roster() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/template_roster_is_readable.rs");
    t.compile_fail("tests/ui/template_cannot_be_constructed.rs");
    t.compile_fail("tests/ui/template_roster_cannot_be_constructed.rs");
    t.compile_fail("tests/ui/template_roster_constructors_are_private.rs");
}
