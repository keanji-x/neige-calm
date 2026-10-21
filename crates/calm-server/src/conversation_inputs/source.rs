//! Current source qualifications. Author attribution stays in Truth; each source
//! can authorize only the exact inputs its owning producer actually binds.
use super::*;
use calm_types::conversation::{CanonicalPendingIdentity, SubmissionRenderInput};

#[derive(Clone)]
pub(crate) enum InputSource {
    Human(HumanInputSource),
    HumanRepeat(HumanRepeatSource),
    Dispatcher(Box<crate::dispatcher::canonical_inputs::DispatcherInputSource>),
}
#[derive(Clone)]
pub(crate) struct HumanRepeatSource {
    human: HumanInputSource,
    retry_intent_id: calm_types::conversation::RetryIntentId,
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
    pub(crate) fn require_submission_intent(
        &self,
        id: Option<&calm_types::conversation::RetryIntentId>,
    ) -> Result<()> {
        match (self, id) {
            (Self::HumanRepeat(repeat), Some(id)) if &repeat.retry_intent_id == id => Ok(()),
            (Self::Human(_), None) | (Self::Dispatcher(_), None) => Ok(()),
            (Self::HumanRepeat(_), Some(_) | None)
            | (Self::Human(_), Some(_))
            | (Self::Dispatcher(_), Some(_)) => Err(CalmError::Forbidden(
                "source does not qualify this exact retry intent".into(),
            )),
        }
    }

    pub(crate) fn for_identity(&self, identity: &CanonicalPendingIdentity) -> Self {
        match (self, identity) {
            (
                Self::Human(human),
                CanonicalPendingIdentity::ExplicitSuccessor { retry_intent_id },
            ) => Self::HumanRepeat(HumanRepeatSource {
                human: human.clone(),
                retry_intent_id: retry_intent_id.clone(),
            }),
            (Self::Human(_), CanonicalPendingIdentity::Accepted { .. })
            | (
                Self::HumanRepeat(_) | Self::Dispatcher(_),
                CanonicalPendingIdentity::Accepted { .. }
                | CanonicalPendingIdentity::ExplicitSuccessor { .. },
            ) => self.clone(),
        }
    }

    async fn repeat_intent_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        carrier: &CurrentInputCarrier,
    ) -> Result<Option<conversation::StoredRetryIntent>> {
        match self {
            Self::Human(_) | Self::Dispatcher(_) => Ok(None),
            Self::HumanRepeat(repeat) => {
                authorize_human_resource_tx(tx, &repeat.human, carrier.card_id()).await?;
                let intent =
                    conversation::retry_intent_tx(tx, carrier, &repeat.retry_intent_id).await?;
                if intent.principal != repeat.human.current_principal()?.user_id
                    || intent.unavailable.is_some()
                {
                    return Err(CalmError::Forbidden(
                        "Human repeat no longer qualifies its original intent".into(),
                    ));
                }
                let (binding, _) =
                    crate::harness::profile::input_carrier_binding_tx(tx, carrier).await?;
                let original =
                    conversation::submission_identity_tx(tx, &intent.predecessor_submission_id)
                        .await?;
                let calm_types::conversation::ConversationExecutionProvenance::LegacySingleUserDeployment { provider, profile, .. } = original.execution;
                if binding.provider != provider || binding.profile != profile {
                    return Err(CalmError::Conflict(
                        "Human repeat provider binding changed".into(),
                    ));
                }
                Ok(Some(intent))
            }
        }
    }

    pub(crate) fn require_repo(&self, repo: &dyn crate::db::RepoEventWrite) -> Result<()> {
        match self {
            Self::Human(_) | Self::HumanRepeat(_) => Ok(()),
            Self::Dispatcher(source) => source.require_repo(repo),
        }
    }

    pub(crate) fn check_current(&self) -> Result<()> {
        match self {
            Self::Human(source) => source.current_principal().map(drop),
            Self::HumanRepeat(source) => source.human.current_principal().map(drop),
            Self::Dispatcher(source) => source.check_current().map(drop),
        }
    }
    async fn authorizes_plan_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        carrier: &CurrentInputCarrier,
        plan: &SubmissionRenderPlan,
    ) -> Result<bool> {
        match self {
            Self::HumanRepeat(_) => Ok(self
                .repeat_intent_tx(tx, carrier)
                .await?
                .is_some_and(|intent| intent.plan.plan == *plan)),
            Self::Human(_) | Self::Dispatcher(_) => {
                for segment in plan.segments() {
                    if let calm_types::conversation::SubmissionRenderSegment::Input {
                        input, ..
                    } = segment
                        && !self.authorizes_tx(tx, carrier, input).await?
                    {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
        }
    }

    async fn authorizes_tx(
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
            (Self::Human(_), SubmissionRenderInput::System { .. })
            | (Self::Dispatcher(_), SubmissionRenderInput::Human { .. })
            | (
                Self::HumanRepeat(_),
                SubmissionRenderInput::Human { .. } | SubmissionRenderInput::System { .. },
            ) => Ok(false),
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
            authorize_human_resource_tx(tx, source, carrier.card_id()).await?;
            Ok(source.current_principal()?.user_id)
        }
        InputSource::HumanRepeat(repeat) => {
            source.repeat_intent_tx(tx, carrier).await?;
            Ok(repeat.human.current_principal()?.user_id)
        }
        InputSource::Dispatcher(source) => source.authorize_resource_tx(tx, carrier).await,
    }
}

pub(crate) struct SourceInputs {
    identities: Vec<CanonicalPendingIdentity>,
    unattempted_initial: bool,
}
impl SourceInputs {
    pub(crate) fn identities(&self) -> &[CanonicalPendingIdentity] {
        &self.identities
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
    let mut identities = vec![];
    let mut unattempted_initial = false;
    if carrier.phase()? == crate::harness::HarnessPhaseTag::IssuingTurn
        && let Some(active) = &carrier.checkpoint().active
        && let [id] = active.submission_ids.as_slice()
        && carrier.checkpoint().attempt(id).is_none()
    {
        let original = conversation::submission_identity_tx(tx, id).await?;
        let stored = conversation::submission_plan_tx(tx, id).await?;
        let retry = conversation::submission_retry_intent_tx(tx, id).await?;
        let scoped = match &retry {
            Some(retry_intent_id) => {
                source.for_identity(&CanonicalPendingIdentity::ExplicitSuccessor {
                    retry_intent_id: retry_intent_id.clone(),
                })
            }
            None => source.clone(),
        };
        let qualifies_original = scoped.require_submission_intent(retry.as_ref()).is_ok()
            && scoped.authorizes_plan_tx(tx, carrier, &stored.plan).await?;
        if qualifies_original {
            let (execution, _) = authorize_execution_tx(tx, &scoped, carrier, &stored.plan).await?;
            if original.card_id != carrier.card_id()
                || original.logical_turn_id != active.logical_turn_id
                || original.ordinal != 0
                || original.execution != execution
            {
                return Err(CalmError::Conflict(
                    "original unattempted claim changed".into(),
                ));
            }
            if let Some(retry_intent_id) = retry {
                identities.push(CanonicalPendingIdentity::ExplicitSuccessor { retry_intent_id });
            } else {
                identities.extend(
                    stored
                        .plan
                        .input_refs()
                        .map_err(|error| CalmError::Conflict(error.into()))?
                        .into_iter()
                        .map(|input| CanonicalPendingIdentity::Accepted { input }),
                );
            }
            unattempted_initial = true;
        }
    }
    for pending in &carrier.checkpoint().pending {
        let scoped = source.for_identity(&pending.identity());
        let qualified = match (&scoped, pending) {
            (
                InputSource::HumanRepeat(repeat),
                calm_types::conversation::PendingInput::ExplicitSuccessor {
                    retry_intent_id, ..
                },
            ) => {
                repeat.retry_intent_id == *retry_intent_id
                    && scoped
                        .repeat_intent_tx(tx, carrier)
                        .await?
                        .is_some_and(|intent| intent.pending == *pending)
            }
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
            (
                InputSource::Human(_),
                calm_types::conversation::PendingInput::System { .. }
                | calm_types::conversation::PendingInput::ExplicitSuccessor { .. },
            )
            | (
                InputSource::Dispatcher(_),
                calm_types::conversation::PendingInput::Human { .. }
                | calm_types::conversation::PendingInput::ExplicitSuccessor { .. },
            )
            | (
                InputSource::HumanRepeat(_),
                calm_types::conversation::PendingInput::Human { .. }
                | calm_types::conversation::PendingInput::System { .. },
            ) => false,
        };
        if qualified {
            identities.push(pending.identity());
        }
    }
    // Resource admission can reopen read/control access even when this source
    // grants no input. Only these exact refs reach the dispatch source registry.
    Ok(SourceInputs {
        identities,
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
    if !source.authorizes_plan_tx(tx, carrier, plan).await? {
        return Err(CalmError::Forbidden(
            "current source does not qualify this exact input".into(),
        ));
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
    selection: impl Into<conversation::InitialClaim>,
) -> Result<ClaimedSubmission> {
    let source = source.into();
    let selection = selection.into();
    let plan = match &selection {
        conversation::InitialClaim::PendingPrefix { plan } => {
            source.require_submission_intent(None)?;
            plan.clone()
        }
        conversation::InitialClaim::ExplicitSuccessor { retry_intent_id } => {
            source.require_submission_intent(Some(retry_intent_id))?;
            conversation::retry_intent_tx(tx, carrier, retry_intent_id)
                .await?
                .plan
                .plan
        }
    };
    let (execution, _) = authorize_execution_tx(tx, &source, carrier, &plan).await?;
    Ok(conversation::claim_initial_submission_tx(tx, carrier, selection, &execution).await?)
}
