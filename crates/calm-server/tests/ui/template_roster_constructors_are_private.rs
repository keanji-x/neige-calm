//! One statement: the three functions that turn template sources into a
//! roster (`from_sources`, the panicking `load` the builtin path uses, and
//! #1635 S5's `for_boot`, which reads `--templates-dir`) are not reachable
//! from outside the crate. A downstream crate cannot construct or feed the
//! roster except through the boot loader, which validates every file.
//!
//! `for_boot` is `pub(crate)`. Every downstream road onto it — `AppState::boot`
//! with `Config.templates_dir` in production, the `fixtures`-gated
//! `AppState::with_templates_dir` on a `from_parts` state in tests — runs that
//! same loader over a *directory*. Making any of the three `pub` compiles this.

use calm_server::templates::TemplateRoster;

fn main() {
    let source: &'static str = "+++\nid = \"forged\"\ntitle = \"Forged\"\n+++\n# Forged\n";
    let _built = TemplateRoster::from_sources(&[source]);
    let _loaded = TemplateRoster::load(&[source]);
    let _booted = TemplateRoster::for_boot(Some(std::path::Path::new("/nonexistent")));
}
