//! Calendar entry points adapt the kernel context to the owning plugin service.
use super::{adapter::Adapter, model::*};
use crate::{error::Result, mcp_server::registry::AppContext};
pub use plugin::builtin::calendar::store::{Access, Change};
pub async fn list(ctx: &AppContext, access: &Access, window: Window) -> Result<Vec<Listed>> {
    plugin::builtin::calendar::store::list(&Adapter::new(ctx), access, window).await
}
pub async fn read(ctx: &AppContext, access: &Access, id: &str) -> Result<Entry> {
    plugin::builtin::calendar::store::read(&Adapter::new(ctx), access, id).await
}
pub async fn create(ctx: &AppContext, access: Access, request: Create) -> Result<Entry> {
    plugin::builtin::calendar::store::create(&Adapter::new(ctx), access, request).await
}
pub async fn update(
    ctx: &AppContext,
    access: Access,
    id: String,
    version: i64,
    change: Change,
) -> Result<Entry> {
    plugin::builtin::calendar::store::update(&Adapter::new(ctx), access, id, version, change).await
}
