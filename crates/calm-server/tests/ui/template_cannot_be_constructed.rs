//! One statement: a downstream crate cannot write a `Template` literal. Every
//! field is private, and there is no constructor, no `Clone`, no `Copy` and no
//! `Default` — so the only `Template` a downstream crate can name is a borrow
//! of a roster entry (#1318 S2, restated for files by #1635 S4).
//!
//! Making any field `pub`, or adding a `pub fn new`, makes this file compile,
//! which is exactly the mutation this case exists to catch. (A forgery through
//! `transmute` is outside what this fixture can say; it is registered under
//! `## KNOWN GAPS` on `routes::tracks::admit_template`.)

use calm_server::templates::Template;

fn main() {
    let _forged: &'static Template = Box::leak(Box::new(Template {
        key: "forged",
        title: "Forged",
        body: "# Forged\n",
    }));
}
