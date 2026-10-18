//! Thin native registry and per-run host adaptation; protocol policy lives in plugin.
use super::*;
use crate::mcp_server::result::ToolResult;
use plugin::host::ports::BuiltinRun;
struct Run(Arc<plugin::builtin::opencode_history::History>);
#[async_trait::async_trait]
impl BuiltinRun for Run {
    async fn tools_call(&self, tool: &str, args: &Value) -> Result<CallToolResult, RpcError> {
        let history = self.0.clone();
        let tool = tool.to_string();
        let args = args.clone();
        tokio::task::spawn_blocking(move || history.call(&tool, args))
            .await
            .map_err(|_| RpcError::internal("history call interrupted"))?
            .map_err(RpcError::invalid_params)
    }
    fn forge_tools_call(
        &self,
        _: &str,
        _: &Value,
        _: &ForgeCallerScope,
    ) -> Result<CallToolResult, RpcError> {
        Err(RpcError::method_not_found("history has no forge actions"))
    }
}
fn start(values: Value) -> super::StartFuture {
    Box::pin(async move {
        let history = tokio::task::spawn_blocking(move || {
            plugin::builtin::opencode_history::History::load(values)
        })
        .await
        .map_err(|_| "history configuration interrupted")??;
        Ok(Arc::new(Run(Arc::new(history))) as Arc<dyn BuiltinRun>)
    })
}
pub(super) fn component(definition: &'static plugin::builtin::Definition) -> BuiltinPlugin {
    let mut local = ToolRegistry::new();
    for declaration in definition.tools() {
        let tool = declaration.name.clone();
        let id = definition.manifest().id.clone();
        local.register(
            super::descriptor(declaration.clone()),
            Arc::new(move |ctx, _identity, args| {
                let tool = tool.clone();
                let id = id.clone();
                Box::pin(async move {
                    let host = ctx
                        .plugin_host
                        .get()
                        .ok_or_else(|| RpcError::unavailable("plugin host unavailable"))?;
                    let Some(crate::plugin_host::ConnectorClient::Builtin(run)) =
                        host.connector_client(&id).await
                    else {
                        return Err(RpcError::unavailable("history is not running"));
                    };
                    ToolResult::from_plugin(run.tools_call(&tool, &args).await?)
                })
            }),
        );
    }
    let mut component = BuiltinPlugin::new(
        definition,
        local,
        |_, _| Err("history uses native tools".into()),
        |_, _, _| Err("history has no forge actions".into()),
        plugin::builtin::opencode_history::INSTRUCTIONS,
    );
    component.start = Some(start);
    component
}
#[cfg(test)]
mod tests;
