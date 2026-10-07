//! Boot wiring and integration-test observation of the plugin's wake service.
use super::adapter::Adapter;
#[cfg(test)]
use super::model::Entry;
#[cfg(test)]
use crate::error::Result;
use crate::mcp_server::registry::AppContext;
#[cfg(test)]
use chrono::{DateTime, Utc};
use std::sync::Arc;
pub(super) fn spawn(ctx: Arc<AppContext>) {
    plugin::builtin::calendar::wake::spawn(Arc::new(Adapter::new(&ctx)));
}
#[cfg(test)]
pub(super) async fn scan(ctx: &AppContext, now: DateTime<Utc>) -> Result<usize> {
    plugin::builtin::calendar::wake::scan(&Adapter::new(ctx), now).await
}
#[cfg(test)]
pub(super) async fn handle(ctx: &AppContext, entry: Entry, now: DateTime<Utc>) -> Result<bool> {
    plugin::builtin::calendar::wake::handle(&Adapter::new(ctx), entry, now).await
}
