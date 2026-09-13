//! One statement: a downstream crate cannot write a `Template` literal. Every
//! field is private, and there is no constructor, no `Clone`, no `Copy` and no
//! `Default` — so the only `Template` a downstream crate can name is a borrow
//! of a roster entry (#1318 S2, restated for files by #1635 S4).
//!
//! What this pins is **field privacy**: making any one of the three fields
//! `pub` makes this file compile (E0451 names all three, so a single field
//! going `pub` changes the pinned diagnostic too). It says nothing about
//! constructors — a `pub fn new` would not make a struct literal compile;
//! constructor privacy is `template_roster_constructors_are_private.rs`'s
//! statement, pinned by name. (A forgery through `transmute` is outside what
//! this fixture can say; it is registered under `## KNOWN GAPS` on
//! `routes::tracks::admit_template`.)

use calm_server::templates::Template;

fn main() {
    let _forged: &'static Template = Box::leak(Box::new(Template {
        key: "forged",
        title: "Forged",
        body: "# Forged\n",
    }));
}
