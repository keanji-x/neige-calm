//! Green fixture. Without it the compile-fail cases beside it could be red for
//! a reason that has nothing to do with privacy — a typo, a moved module, a
//! renamed item — and the guard would pass vacuously.
//!
//! It does everything a downstream crate is *allowed* to do with the roster:
//! reach the built-in one, walk its entries, look one up, and read an entry's
//! key, title and recipe. A `pass` fixture is also executed, and this one runs
//! `TemplateRoster::builtin()` for real — so a builtin file that does not
//! parse is red here too (a panic at first use), on top of the unit tests.

use calm_server::templates::{Template, TemplateRoster};

fn main() {
    let roster: &'static TemplateRoster = TemplateRoster::builtin();
    let first: &'static Template = &roster.entries()[0];
    let _key: &'static str = first.key();
    let _title: &'static str = first.title();
    let _recipe = first.recipe();
    let _found: Option<&'static Template> = roster.get("small-change");
}
