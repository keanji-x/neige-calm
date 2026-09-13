//! The cross-crate half of the template roster's privacy story (#1318 S2,
//! restated for files by #1635 S4 — see `templates::Template`'s doc).
//!
//! `trybuild` compiles its samples as separate crates that depend on
//! `calm-server`, so what it pins is the *crate-external* statement, one
//! fixture per clause:
//!
//!   * `template_cannot_be_constructed.rs` — **field privacy** of `Template`:
//!     a struct literal is E0451 on `key`, `title` and `body`;
//!   * `template_roster_cannot_be_constructed.rs` — **field privacy** of
//!     `TemplateRoster`: a struct literal is E0451 on `entries`;
//!   * `template_roster_constructors_are_private.rs` — **constructor privacy,
//!     by name**: `TemplateRoster::from_sources`, `TemplateRoster::load` and
//!     (#1635 S5) `TemplateRoster::for_boot` are E0624. This clause is only as
//!     wide as the three names it spells; a fourth constructor added under
//!     another name is not covered until it is listed here;
//!   * `template_roster_is_readable.rs` — the green fixture: the public surface
//!     (`builtin`, `entries`, `get`, `key`, `title`, `recipe`) stays reachable.
//!
//! Together: the only `&'static Template` a downstream crate can name is a
//! borrow of a roster entry — `TemplateRoster::builtin()`'s, or (through the
//! `fixtures`-gated `AppState::with_templates_dir`, which runs the production
//! `for_boot` over a directory) `RouteState.templates`'. The in-crate half (accessor ↔
//! private field pointer identity, `get` returning the roster's own borrow)
//! lives in `templates::tests`, because those comparisons name the private
//! fields and cannot be written from outside the defining module.
//!
//! The `.stderr` files pin the *diagnostic*, not merely "it failed": a typo in
//! a sample also makes a build fail. They are toolchain-sensitive by
//! construction; the toolchain is pinned in `rust-toolchain.toml`, and a bump
//! that rewords the diagnostics regenerates them with
//! `TRYBUILD=overwrite cargo test -p calm-server --test track_suite templates_privacy`.
//!
//! Without the green fixture, the three `compile_fail` cases could be red for
//! a reason unrelated to privacy.

#[test]
fn a_downstream_crate_cannot_forge_a_template_or_a_roster() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/template_roster_is_readable.rs");
    t.compile_fail("tests/ui/template_cannot_be_constructed.rs");
    t.compile_fail("tests/ui/template_roster_cannot_be_constructed.rs");
    t.compile_fail("tests/ui/template_roster_constructors_are_private.rs");
}
