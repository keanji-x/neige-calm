//! Feishu inspection adapter; background policy is owned by the feature component.
mod adapter;
#[cfg(test)]
mod tests;
mod tools;
use super::*;
pub use feature::PLUGIN_ID;
use plugin::builtin::feishu_watch::{self as feature, model::Config};
struct Run(Config);
#[async_trait::async_trait]
impl plugin::host::ports::BuiltinRun for Run {
    async fn tools_call(&self, tool: &str, args: &Value) -> Result<CallToolResult, RpcError> {
        if tool != "configuration" || args != &json!({}) {
            return Err(RpcError::method_not_found(tool));
        }
        serde_json::from_value(json!({"content":[],"structuredContent":self.0,"isError":false}))
            .map_err(|_| RpcError::internal("inspection configuration encoding"))
    }
    fn forge_tools_call(
        &self,
        _: &str,
        _: &Value,
        _: &ForgeCallerScope,
    ) -> Result<CallToolResult, RpcError> {
        Err(RpcError::method_not_found(
            "inspection has no forge actions",
        ))
    }
}
fn start(values: Value) -> StartFuture {
    Box::pin(async move {
        let config: Config = serde_json::from_value(values)
            .map_err(|_| "invalid inspection operator configuration")?;
        config.validate()?;
        Ok(Arc::new(Run(config)) as Arc<dyn plugin::host::ports::BuiltinRun>)
    })
}
pub(super) fn component(definition: &'static plugin::builtin::Definition) -> BuiltinPlugin {
    let mut local = ToolRegistry::new();
    tools::register(&mut local);
    let mut component = BuiltinPlugin::new(
        definition,
        local,
        |_, _| Err("inspection uses authenticated native tools".into()),
        |_, _, _| Err("inspection has no forge actions".into()),
        feature::INSTRUCTIONS,
    );
    component.start = Some(start);
    component.background = Some(spawn);
    component
}
struct GuardedReader<'a, R> {
    adapter: &'a adapter::Adapter,
    reader: R,
}
#[async_trait::async_trait]
impl<R: feature::reader::Reader> feature::reader::Reader for GuardedReader<'_, R> {
    async fn verify_identity(&self, config: &Config) -> Result<(), feature::reader::ReadError> {
        self.adapter
            .before_read()
            .await
            .map_err(|_| feature::reader::ReadError::Invocation)?;
        self.reader.verify_identity(config).await
    }
    async fn read(
        &self,
        config: &Config,
        source: &feature::model::Source,
        since: i64,
        until: i64,
    ) -> Result<Vec<feature::model::Message>, feature::reader::ReadError> {
        self.adapter
            .before_read()
            .await
            .map_err(|_| feature::reader::ReadError::Invocation)?;
        self.reader.read(config, source, since, until).await
    }
}
fn spawn(ctx: Arc<AppContext>) {
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(std::time::Duration::from_secs(30));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            let Ok(adapter) = adapter::Adapter::capture(&ctx, None).await else {
                continue;
            };
            let result = feature::service::poll(
                &adapter,
                &GuardedReader {
                    adapter: &adapter,
                    reader: feature::reader::CliReader,
                },
                &adapter.config,
                crate::model::now_ms(),
            )
            .await;
            let failed = result.is_err();
            if failed {
                tracing::warn!("Feishu inspection checkpoint failed; retained state preserved");
            }
            // Re-fences the captured runtime. A stopped/reloaded scan cannot publish diagnostics.
            let _ = adapter.diagnostic(failed).await;
        }
    });
}
