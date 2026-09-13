//! One statement: the two functions that turn template sources into a roster
//! (`from_sources`, and the panicking `load` the builtin path uses) are
//! private associated functions. A downstream crate cannot feed the roster
//! its own bytes; it gets `TemplateRoster::builtin()` or nothing.
//!
//! Making either `pub` — say, for an operator-directory loader that should
//! instead live inside `crate::templates` (#1635 S5) — makes this compile.

use calm_server::templates::TemplateRoster;

fn main() {
    let source: &'static str = "+++\nid = \"forged\"\ntitle = \"Forged\"\n+++\n# Forged\n";
    let _built = TemplateRoster::from_sources(&[source]);
    let _loaded = TemplateRoster::load(&[source]);
}
