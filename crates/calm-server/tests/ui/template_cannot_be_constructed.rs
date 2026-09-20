//! A downstream crate cannot write a `Template` literal: every field is private.
//! Making any one of the three fields `pub` makes this file compile.

use calm_server::templates::Template;

fn main() {
    let _forged: &'static Template = Box::leak(Box::new(Template {
        key: "forged",
        title: "Forged",
        body: "# Forged\n",
    }));
}
