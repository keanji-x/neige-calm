use super::store::Access;
use crate::{
    event::EventScope,
    mcp_server::{
        framing::RpcError,
        registry::{ToolCallIdentity, ToolRegistry},
        result::ToolResult,
    },
};
use std::sync::Arc;
fn access(identity: &ToolCallIdentity) -> Result<Access, RpcError> {
    let track = identity
        .track_id
        .clone()
        .ok_or_else(|| RpcError::forbidden("calendar requires a Track"))?;
    Ok(Access {
        track: Some(track.clone()),
        actor: identity.to_actor_id(),
        scope: EventScope::Card {
            card: identity.card_id.clone().into(),
            track: track.into(),
            area: identity.area_id.clone().into(),
        },
        creator: format!("card:{}", identity.card_id),
    })
}
pub fn register(registry: &mut ToolRegistry) {
    for declaration in plugin::builtin::calendar::tools::descriptors() {
        let action = declaration.name.clone();
        registry.register(
            super::super::descriptor(declaration),
            Arc::new(move |ctx, identity, args| {
                let action = action.clone();
                Box::pin(async move {
                    let access = access(&identity)?;
                    plugin::builtin::calendar::tools::call(
                        &super::adapter::Adapter::new(&ctx),
                        access,
                        &action,
                        args,
                        crate::time_format::rewrite_at,
                    )
                    .await
                    .map(ToolResult::structured)
                })
            }),
        );
    }
}
