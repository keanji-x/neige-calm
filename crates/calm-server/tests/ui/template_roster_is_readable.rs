//! Green fixture: without it the compile-fail cases beside it could be red for a reason that has nothing
//! to do with privacy. It runs `TemplateRoster::builtin()` for real, so a builtin file that does not parse is red here too.

use calm_server::templates::{Template, TemplateRoster};

fn main() {
    let roster: &'static TemplateRoster = TemplateRoster::builtin();
    let first: &'static Template = &roster.entries()[0];
    let _key: &'static str = first.key();
    let _title: &'static str = first.title();
    let _recipe = first.recipe();
    let _found: Option<&'static Template> = roster.get("small-change");
}
