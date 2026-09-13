//! One statement: the three functions that turn template sources into a
//! roster (`from_sources`, the panicking `load` the builtin path uses, and
//! #1635 S5's `for_boot`, which reads `--templates-dir`) are not reachable
//! from outside the crate. A downstream crate cannot feed the roster its own
//! bytes; it gets `TemplateRoster::builtin()` or nothing.
//!
//! `for_boot` is `pub(crate)`: its production caller is `AppState::new`, and
//! the `fixtures`-gated `AppState::with_templates_dir` is the only downstream
//! road onto it — a *directory*, loaded by the production loader, never bytes
//! handed in directly. Making any of the three `pub` makes this compile.

use calm_server::templates::TemplateRoster;

fn main() {
    let source: &'static str = "+++\nid = \"forged\"\ntitle = \"Forged\"\n+++\n# Forged\n";
    let _built = TemplateRoster::from_sources(&[source]);
    let _loaded = TemplateRoster::load(&[source]);
    let _booted = TemplateRoster::for_boot(Some(std::path::Path::new("/nonexistent")));
}
