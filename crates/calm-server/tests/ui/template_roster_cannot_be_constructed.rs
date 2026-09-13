//! One statement: a downstream crate cannot write a `TemplateRoster` literal,
//! so the only roster a `RouteState` can carry is one `crate::templates`
//! built. The field is private and there is no public constructor.

use calm_server::templates::TemplateRoster;

fn main() {
    let _forged: &'static TemplateRoster = Box::leak(Box::new(TemplateRoster {
        entries: Vec::new(),
    }));
}
