//! Conversation admission and publication hooks for the shared ACP attachment owner.
use super::*;
use crate::{
    acp_session::attachment::{self, Role},
    harness::native_admission::NativePromptAdmission,
};

#[allow(clippy::too_many_arguments)]
pub(super) async fn prepare<'a>(
    shared: &'a Arc<Shared>,
    registration: Registration<'_>,
    original_native: Option<String>,
    thread: &str,
    selection: &provider::TurnModelSelection,
    permission: PlannerPermissionMode,
    source: &NativePromptAdmission<'_>,
) -> std::result::Result<attachment::Attached<'a>, StartFailure> {
    let params = &shared.params;
    attachment::attach(
        &params.host,
        registration,
        attachment::Request {
            worker: &params.worker_session_id,
            cwd: &params.cwd,
            original_native: original_native.as_deref(),
            reuse_binding: true,
            selection,
        },
        &mut PlannerRole {
            shared,
            thread,
            permission,
            source,
        },
    )
    .await
    .map_err(Into::into)
}

struct PlannerRole<'a, 'b> {
    shared: &'a Shared,
    thread: &'a str,
    permission: PlannerPermissionMode,
    source: &'a NativePromptAdmission<'b>,
}
#[async_trait::async_trait]
impl Role for PlannerRole<'_, '_> {
    async fn check(&self, native: Option<&str>) -> Result<()> {
        let owner = self
            .shared
            .admission(self.thread, Some(self.permission), native, None);
        self.source.combined(&owner).check_local().await
    }
    async fn mint(&mut self) -> Result<String> {
        mint_token(&self.shared.params).await
    }
    fn pending(&mut self, _: CleanupWitness) {}
    async fn bind(&mut self, native: &str) -> Result<()> {
        bind_native(&self.shared.params, self.thread, native).await
    }
    fn observe(&self, configuration: &Configuration) {
        self.shared
            .params
            .host
            .record_configuration(&self.shared.params.card_id, configuration.clone());
    }
    async fn publish(&mut self, _: &str, _: &Configuration) -> Result<()> {
        Ok(())
    }
}
