//! Current admission for private model metadata reads. This holds a live source
//! and exact input/target facts, not a reusable grant or persisted operation.
use super::*;
use crate::db::sqlite::conversation::{
    RuntimeExecutionFence, StoredSubmissionPlan, current_input_carrier_tx, submission_identity_tx,
    submission_plan_tx,
};
use crate::db::{Repo, write_in_tx_typed};
use calm_types::conversation::{ConversationExecutionProvenance, InputSubmissionId, PendingInput};
use std::sync::Arc;

#[derive(Clone)]
enum InputScope {
    Pending(SubmissionRenderPlan),
    RetryPending {
        id: calm_types::conversation::RetryIntentId,
        plan: SubmissionRenderPlan,
    },
    Stored {
        id: InputSubmissionId,
        plan: StoredSubmissionPlan,
    },
}

#[derive(Clone, PartialEq)]
struct ReadTarget {
    track: crate::ids::TrackId,
    area: crate::ids::AreaId,
    cwd: String,
    execution: ConversationExecutionProvenance,
}

/// Constructed only by this owner while its current carrier is protected.
/// Every private RPC re-reads these facts and the real source after native gates.
#[derive(Clone)]
pub(crate) struct SelectionReadAdmission {
    repo: Arc<dyn Repo>,
    source: InputSource,
    card: String,
    worker: String,
    fence: RuntimeExecutionFence,
    input: InputScope,
    target: ReadTarget,
}

impl SelectionReadAdmission {
    pub(crate) fn check_current(&self) -> Result<()> {
        self.source.check_current()
    }

    pub(crate) async fn pending_tx(
        tx: &mut Transaction<'_, Sqlite>,
        repo: Arc<dyn Repo>,
        source: impl Into<InputSource>,
        carrier: &CurrentInputCarrier,
        plan: SubmissionRenderPlan,
    ) -> Result<Self> {
        Self::bind(tx, repo, source.into(), carrier, InputScope::Pending(plan)).await
    }

    pub(crate) async fn retry_pending_tx(
        tx: &mut Transaction<'_, Sqlite>,
        repo: Arc<dyn Repo>,
        source: InputSource,
        carrier: &CurrentInputCarrier,
        id: calm_types::conversation::RetryIntentId,
    ) -> Result<Self> {
        let intent = conversation::retry_intent_tx(tx, carrier, &id).await?;
        Self::bind(
            tx,
            repo,
            source,
            carrier,
            InputScope::RetryPending {
                id,
                plan: intent.plan.plan,
            },
        )
        .await
    }

    pub(crate) async fn stored_tx(
        tx: &mut Transaction<'_, Sqlite>,
        repo: Arc<dyn Repo>,
        source: impl Into<InputSource>,
        carrier: &CurrentInputCarrier,
        id: InputSubmissionId,
    ) -> Result<Self> {
        let plan = submission_plan_tx(tx, &id).await?;
        Self::bind(
            tx,
            repo,
            source.into(),
            carrier,
            InputScope::Stored { id, plan },
        )
        .await
    }

    async fn bind(
        tx: &mut Transaction<'_, Sqlite>,
        repo: Arc<dyn Repo>,
        source: InputSource,
        carrier: &CurrentInputCarrier,
        input: InputScope,
    ) -> Result<Self> {
        source.require_repo(repo.as_ref())?;
        let target = check_input_and_target(tx, &source, carrier, &input).await?;
        Ok(Self {
            repo,
            source,
            card: carrier.card_id().into(),
            worker: carrier.worker_session_id().into(),
            fence: RuntimeExecutionFence::from_carrier(carrier),
            input,
            target,
        })
    }

    pub(crate) fn cwd(&self) -> &str {
        &self.target.cwd
    }

    pub(crate) async fn revalidate(&self) -> Result<()> {
        self.current_payload().await.map(drop)
    }

    pub(crate) async fn current_payload(&self) -> Result<serde_json::Value> {
        self.source.require_repo(self.repo.as_ref())?;
        let bound = self.clone();
        write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move {
                let carrier = current_input_carrier_tx(tx, &bound.card, &bound.worker).await?;
                // This fence intentionally excludes appended pending entries.
                if RuntimeExecutionFence::from_carrier(&carrier) != bound.fence {
                    return Err(CalmError::Conflict("model read execution changed".into()));
                }
                let target =
                    check_input_and_target(tx, &bound.source, &carrier, &bound.input).await?;
                if target != bound.target {
                    return Err(CalmError::Conflict("model read target changed".into()));
                }
                Ok(crate::db::sqlite::card_execution_shape_tx(tx, &bound.card)
                    .await?
                    .payload)
            })
        })
        .await
    }
}

async fn check_input_and_target(
    tx: &mut Transaction<'_, Sqlite>,
    source: &InputSource,
    carrier: &CurrentInputCarrier,
    input: &InputScope,
) -> Result<ReadTarget> {
    let mut original_execution = None;
    let plan = match input {
        InputScope::RetryPending { id, plan } => {
            source.require_submission_intent(Some(id))?;
            let intent = conversation::retry_intent_tx(tx, carrier, id).await?;
            if intent.unavailable.is_some()
                || carrier.checkpoint().pending.first() != Some(&intent.pending)
                || intent.plan.plan != *plan
            {
                return Err(CalmError::Conflict("retry pending head changed".into()));
            }
            plan
        }
        InputScope::Pending(plan) => {
            source.require_submission_intent(None)?;
            let refs = plan
                .input_refs()
                .map_err(|error| CalmError::Conflict(error.into()))?;
            let actual = carrier
                .checkpoint()
                .pending
                .iter()
                .take(refs.len())
                .map(PendingInput::identity)
                .collect::<Vec<_>>();
            if refs.is_empty()
                || (carrier.checkpoint().active.is_some()
                    && !carrier.checkpoint().resolved_execution_without_target())
                || refs
                    .into_iter()
                    .map(calm_types::conversation::CanonicalPendingIdentity::from)
                    .collect::<Vec<_>>()
                    != actual
            {
                return Err(CalmError::Conflict(
                    "model read pending prefix changed".into(),
                ));
            }
            plan
        }
        InputScope::Stored { id, plan } => {
            let retry = conversation::submission_retry_intent_tx(tx, id).await?;
            source.require_submission_intent(retry.as_ref())?;
            let identity = submission_identity_tx(tx, id).await?;
            let active = carrier.checkpoint().active.as_ref().ok_or_else(|| {
                CalmError::Conflict("model read has no current claimed execution".into())
            })?;
            if identity.card_id != carrier.card_id()
                || identity.ordinal != 0
                || identity.logical_turn_id != active.logical_turn_id
                || active.submission_ids.first() != Some(id)
                || carrier.checkpoint().attempt(id).is_some()
                || submission_plan_tx(tx, id).await? != *plan
            {
                return Err(CalmError::Conflict(
                    "model read original submission changed".into(),
                ));
            }
            original_execution = Some(identity.execution);
            &plan.plan
        }
    };
    let (execution, session) = authorize_execution_tx(tx, source, carrier, plan).await?;
    if original_execution
        .as_ref()
        .is_some_and(|original| original != &execution)
    {
        return Err(CalmError::Conflict(
            "model read execution provenance changed".into(),
        ));
    }
    let track = calm_truth::db::sqlite::track_get_tx(tx, &session.track_id).await?;
    Ok(ReadTarget {
        track: track.id,
        area: track.area_id,
        cwd: track.workspace.agent_cwd().to_owned(),
        execution,
    })
}
