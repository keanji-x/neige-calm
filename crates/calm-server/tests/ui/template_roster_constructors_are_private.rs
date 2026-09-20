//! `from_sources`, `load` and `for_boot` are not reachable from outside the crate:
//! a downstream crate feeds the roster only through the boot loader. Making any of the three `pub` compiles this.

use calm_server::templates::TemplateRoster;

fn main() {
    let source: &'static str = "+++\nid = \"forged\"\ntitle = \"Forged\"\n+++\n# Forged\n";
    let _built = TemplateRoster::from_sources(&[source]);
    let _loaded = TemplateRoster::load(&[source]);
    let _booted = TemplateRoster::for_boot(Some(std::path::Path::new("/nonexistent")));
}
