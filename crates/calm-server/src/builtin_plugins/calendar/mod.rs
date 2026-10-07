//! Calendar commitments, owned by a compiled component rather than the task scheduler.
mod adapter;
pub mod model;
pub mod routes;
mod store;
#[cfg(test)]
mod tests;
mod tools;
mod wake;

pub use plugin::builtin::calendar::PLUGIN_ID;
pub(super) fn component(definition: &'static plugin::builtin::Definition) -> super::BuiltinPlugin {
    let mut native = crate::mcp_server::registry::ToolRegistry::new();
    tools::register(&mut native);
    let mut component = super::BuiltinPlugin::new(
        definition,
        native,
        |_, _| Err("Calendar uses its authenticated native tools".into()),
        |_, _, _| Err("Calendar does not expose forge actions".into()),
        plugin::builtin::calendar::INSTRUCTIONS,
    );
    component.router = routes::router;
    component.background = Some(wake::spawn);
    component
}
