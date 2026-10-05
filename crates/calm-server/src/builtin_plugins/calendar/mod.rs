//! Calendar commitments, owned by a compiled component rather than the task scheduler.
pub mod model;
pub mod routes;
mod store;
#[cfg(test)]
mod tests;
mod tools;
mod wake;

pub const PLUGIN_ID: &str = "calendar";
pub(super) fn component() -> super::BuiltinPlugin {
    let mut native = crate::mcp_server::registry::ToolRegistry::new();
    tools::register(&mut native);
    let mut component = super::BuiltinPlugin::new(
        include_str!("manifest.json"),
        native,
        |_, _| Err("Calendar uses its authenticated native tools".into()),
        |_, _, _| Err("Calendar does not expose forge actions".into()),
        include_str!("instructions.md"),
    )
    .always_enabled();
    component.router = routes::router;
    component.background = Some(wake::spawn);
    component
}
