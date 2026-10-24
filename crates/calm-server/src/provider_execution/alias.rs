//! Non-Planner owners cannot enter the historical Planner alias or marker namespaces.
use super::*;

#[derive(Clone, Copy, Debug)]
pub struct AliasPrefix {
    prefix: &'static str,
    planner: bool,
}
impl AliasPrefix {
    pub fn new(prefix: &'static str) -> Result<Self> {
        if prefix.is_empty()
            || prefix.contains([':', '=', '\0'])
            || "planner:".starts_with(prefix)
            || prefix.starts_with("planner:")
        {
            return Err(conflict(
                "owner alias prefix overlaps Planner or is invalid",
            ));
        }
        Ok(Self {
            prefix,
            planner: false,
        })
    }
    pub(crate) fn planner() -> Self {
        Self {
            prefix: "planner",
            planner: true,
        }
    }
    pub fn alias(self, session: &str, id: &str) -> String {
        format!("{}:{session}:{id}", self.prefix)
    }
    pub(super) fn validate(
        self,
        owner: &dyn ProviderExecutionOwner,
        session: &str,
        intent: &ExecutionIntent,
    ) -> Result<()> {
        let metadata = &intent.metadata;
        if metadata.consumer.0 != owner.consumer()
            || metadata.alias.0 != owner.alias(session, metadata.id.as_str())
            || metadata.alias.0 != self.alias(session, metadata.id.as_str())
        {
            return Err(conflict("intent does not belong to provider owner"));
        }
        // The pre-C4 scanner matches exact environment entries, not execution aliases.
        if !self.planner
            && metadata.program.environment.iter().any(|(key, _)| {
                key == crate::claude_planner::config::MARKER_KEY
                    || key == crate::acp_planner::config::MARKER_KEY
            })
        {
            return Err(conflict(
                "non-Planner owner carries a Planner upgrade marker",
            ));
        }
        Ok(())
    }
}
