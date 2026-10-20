//! Registration of the Gitforge rerun tool; its forge action runs through the same kernel
//! adapter as publish (`ForgeServices`), so the approval gate holds without naming it.
use crate::mcp_server::registry::{ToolDescriptor, ToolRegistry};
use plugin::builtin::gitforge::rerun;
use std::sync::Arc;

fn descriptor() -> ToolDescriptor {
    super::super::descriptor(rerun::descriptor())
}

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(
        descriptor(),
        Arc::new(|ctx, identity, args| {
            Box::pin(async move {
                rerun::call(
                    &super::publish::ForgeServices {
                        ctx: ctx.as_ref(),
                        caller: &identity,
                    },
                    &identity.card_id,
                    args,
                )
                .await
                .map(crate::mcp_server::result::ToolResult::structured)
            })
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CardRole;

    #[test]
    fn rerun_descriptor_is_planner_only() {
        let d = descriptor();
        assert_eq!(d.name, rerun::RERUN_FAILED);
        assert_eq!(d.roles, &[CardRole::Planner]);
        assert_eq!(d.listed_for, &[CardRole::Planner]);
    }
}
