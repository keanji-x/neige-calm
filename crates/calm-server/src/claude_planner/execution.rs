//! Claude owns provider completion and per-execution diagnostics.
use crate::{
    error::{CalmError, Result},
    provider_execution::*,
};
use async_trait::async_trait;
use std::{path::PathBuf, sync::Arc};
pub const CONSUMER: &str = "claude-planner";
struct Owner {
    root: PathBuf,
}
pub fn owner(root: PathBuf) -> ProviderExecution {
    ProviderExecution {
        owner: Arc::new(Owner { root }),
        drivers: Default::default(),
    }
}
#[async_trait]
impl ProviderExecutionOwner for Owner {
    fn consumer(&self) -> &'static str {
        CONSUMER
    }
    async fn complete(
        &self,
        _: &str,
        record: &Record,
        _: CompletionEvidence,
        _: Option<KnownEnd>,
    ) -> Result<()> {
        // Turn and compaction are still native in C4.1; never silently complete one.
        if record.purpose != Purpose::RewindCheck {
            return Err(CalmError::Conflict(
                "unsupported Claude execution purpose".into(),
            ));
        }
        let id = execution_id(record)?;
        stderr::cleanup(&self.root, id.as_str());
        Ok(())
    }
}
