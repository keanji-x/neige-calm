//! Required one-use final control exchange through the existing TaskLaunch fence.
use crate::db::RouteRepo;
use crate::dedicated_codex::{self, PreparedEndpoint, TurnAdmission, TurnLaunch};
use crate::error::{CalmError, Result};
use crate::operation::{Operation, Tx};
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct TurnIntent {
    endpoint: PreparedEndpoint,
    thread_id: String,
    request_key: String,
    prompt_digest: String,
}
impl TurnIntent {
    fn from_launch(launch: &TurnLaunch) -> Self {
        Self {
            endpoint: launch.endpoint().clone(),
            thread_id: launch.thread_id().into(),
            request_key: launch.request_key().into(),
            prompt_digest: launch.prompt_digest().into(),
        }
    }
}
pub(crate) async fn validate_tx(
    tx: &mut Tx<'_>,
    op: &Operation,
    intent: &TurnIntent,
) -> Result<()> {
    use crate::dedicated_codex::{RequestPhase, StopState};
    let record = super::journal::load_tx(tx, &op.id).await?;
    let session = record.session()?;
    if record.admission != super::record::Admission::Open
        || session.stop != StopState::Open
        || session.endpoint != intent.endpoint
        || !matches!(&session.phase,RequestPhase::IssuingTurn{thread_id,request_key,prompt_digest}
            if thread_id==&intent.thread_id && request_key==&intent.request_key && prompt_digest==&intent.prompt_digest)
    {
        return Err(CalmError::Conflict(
            "isolated turn does not match its admitted private intent".into(),
        ));
    }
    super::journal::require_owner_tx(tx, op).await?;
    super::admission::validate_start_tx(tx, op).await?;
    Ok(())
}

pub(crate) struct Guard {
    pub repo: Arc<dyn RouteRepo>,
    pub operation: Operation,
}
#[async_trait::async_trait]
impl TurnAdmission for Guard {
    async fn admit(
        &self,
        launch: TurnLaunch,
    ) -> dedicated_codex::Result<crate::codex_appserver::TurnStartResult> {
        let intent = TurnIntent::from_launch(&launch);
        let task_id = intent.endpoint.request.identity.attempt_id.clone();
        crate::operation::task_launch::TaskLaunch::new(&task_id, &self.operation)
            .run_isolated_observed(self.repo.as_ref(), intent, async move {
                launch.issue().await.map_err(super::config::provider_error)
            })
            .await
            .map_err(|failure| dedicated_codex::Error::Unknown(failure.error.to_string()))
    }
}
