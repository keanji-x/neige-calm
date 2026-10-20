//! Current source qualifications. Author attribution stays in Truth; each source
//! can authorize only the exact inputs its owning producer actually binds.
use super::*;
use calm_types::conversation::{AcceptedInputRef, SubmissionRenderInput};

#[derive(Clone)]
pub(crate) enum InputSource {
    Human(HumanInputSource),
    Dispatcher(Box<crate::dispatcher::canonical_inputs::DispatcherInputSource>),
}
impl From<HumanInputSource> for InputSource {
    fn from(source: HumanInputSource) -> Self {
        Self::Human(source)
    }
}
impl From<&HumanInputSource> for InputSource {
    fn from(source: &HumanInputSource) -> Self {
        Self::Human(source.clone())
    }
}
impl From<&InputSource> for InputSource {
    fn from(source: &InputSource) -> Self {
        source.clone()
    }
}
impl InputSource {
    pub(crate) fn require_repo(&self, repo: &dyn crate::db::RepoEventWrite) -> Result<()> {
        match self {
            Self::Human(_) => Ok(()),
            Self::Dispatcher(source) => source.require_repo(repo),
        }
    }

    pub(crate) fn check_current(&self) -> Result<()> {
        match self {
            Self::Human(source) => source.current_principal().map(drop),
            Self::Dispatcher(source) => source.check_current().map(drop),
        }
    }
    pub(super) async fn authorizes_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        carrier: &CurrentInputCarrier,
        input: &SubmissionRenderInput,
    ) -> Result<bool> {
        match (self, input) {
            (Self::Human(source), SubmissionRenderInput::Human { message, author }) => Ok(source
                .current_principal()?
                .user_id
                == author.registered_user_id
                && super::execution::current_human_ref_matches_tx(tx, source, carrier, message)
                    .await?),
            (Self::Dispatcher(source), SubmissionRenderInput::System { .. }) => {
                source.authorizes_tx(tx, carrier, input).await
            }
            _ => Ok(false),
        }
    }
}

pub(crate) async fn authorize_resource_tx(
    tx: &mut Transaction<'_, Sqlite>,
    source: &InputSource,
    carrier: &CurrentInputCarrier,
) -> Result<String> {
    match source {
        InputSource::Human(source) => {
            authorize_human_resource_tx(tx, source, carrier).await?;
            Ok(source.current_principal()?.user_id)
        }
        InputSource::Dispatcher(source) => source.authorize_resource_tx(tx, carrier).await,
    }
}

pub(crate) struct SourceInputs {
    refs: Vec<AcceptedInputRef>,
    unattempted_initial: bool,
}
impl SourceInputs {
    pub(crate) fn refs(&self) -> &[AcceptedInputRef] {
        &self.refs
    }
    pub(crate) fn is_unattempted_initial(&self) -> bool {
        self.unattempted_initial
    }
}

pub(crate) async fn source_inputs_tx(
    tx: &mut Transaction<'_, Sqlite>,
    source: &InputSource,
    carrier: &CurrentInputCarrier,
) -> Result<SourceInputs> {
    let mut refs = vec![];
    let mut unattempted_initial = false;
    if carrier.phase()? == crate::harness::HarnessPhaseTag::IssuingTurn
        && let Some(active) = &carrier.checkpoint().active
        && let [id] = active.submission_ids.as_slice()
        && carrier.checkpoint().attempt(id).is_none()
    {
        let original = conversation::submission_identity_tx(tx, id).await?;
        let stored = conversation::submission_plan_tx(tx, id).await?;
        let mut qualifies_original = true;
        for segment in stored.plan.segments() {
            if let calm_types::conversation::SubmissionRenderSegment::Input { input, .. } = segment
                && !source.authorizes_tx(tx, carrier, input).await?
            {
                qualifies_original = false;
                break;
            }
        }
        if qualifies_original {
            let (execution, _) = authorize_execution_tx(tx, source, carrier, &stored.plan).await?;
            if original.card_id != carrier.card_id()
                || original.logical_turn_id != active.logical_turn_id
                || original.ordinal != 0
                || original.execution != execution
            {
                return Err(CalmError::Conflict(
                    "original unattempted claim changed".into(),
                ));
            }
            refs.extend(
                stored
                    .plan
                    .input_refs()
                    .map_err(|error| CalmError::Conflict(error.into()))?,
            );
            unattempted_initial = true;
        }
    }
    for pending in &carrier.checkpoint().pending {
        let qualified = match (source, pending) {
            (
                InputSource::Human(source),
                calm_types::conversation::PendingInput::Human { message, .. },
            ) => {
                super::execution::current_human_ref_matches_tx(tx, source, carrier, message).await?
            }
            (
                InputSource::Dispatcher(source),
                calm_types::conversation::PendingInput::System {
                    observation_id,
                    observation,
                    ..
                },
            ) => {
                source
                    .authorizes_tx(
                        tx,
                        carrier,
                        &SubmissionRenderInput::System {
                            observation_id: observation_id.clone(),
                            payload: calm_types::conversation::PrivateHistoryPayload::Present {
                                value: observation.clone(),
                            },
                        },
                    )
                    .await?
            }
            _ => false,
        };
        if qualified {
            refs.push(pending.input_ref());
        }
    }
    // Resource admission can reopen read/control access even when this source
    // grants no input. Only these exact refs reach the dispatch source registry.
    Ok(SourceInputs {
        refs,
        unattempted_initial,
    })
}

pub(crate) async fn authorize_execution_tx(
    tx: &mut Transaction<'_, Sqlite>,
    source: impl Into<InputSource>,
    carrier: &CurrentInputCarrier,
    plan: &SubmissionRenderPlan,
) -> Result<(
    calm_types::conversation::ConversationExecutionProvenance,
    calm_types::worker::WorkerSession,
)> {
    let source = source.into();
    let owner = authorize_resource_tx(tx, &source, carrier).await?;
    plan.input_refs()
        .map_err(|error| CalmError::Conflict(error.into()))?;
    for segment in plan.segments() {
        if let calm_types::conversation::SubmissionRenderSegment::Input { input, .. } = segment
            && !source.authorizes_tx(tx, carrier, input).await?
        {
            return Err(CalmError::Forbidden(
                "current source does not qualify this exact input".into(),
            ));
        }
    }
    let (binding, runtime) = crate::harness::profile::input_carrier_binding_tx(tx, carrier).await?;
    source.check_current()?;
    Ok((
        calm_types::conversation::ConversationExecutionProvenance::LegacySingleUserDeployment {
            deployment_owner_user_id: owner,
            provider: binding.provider,
            profile: binding.profile,
        },
        runtime,
    ))
}

pub(crate) async fn claim_initial_tx(
    tx: &mut Transaction<'_, Sqlite>,
    source: impl Into<InputSource>,
    carrier: &mut CurrentInputCarrier,
    plan: SubmissionRenderPlan,
) -> Result<ClaimedSubmission> {
    let source = source.into();
    let (execution, _) = authorize_execution_tx(tx, &source, carrier, &plan).await?;
    Ok(conversation::claim_initial_submission_tx(tx, carrier, plan, &execution).await?)
}
