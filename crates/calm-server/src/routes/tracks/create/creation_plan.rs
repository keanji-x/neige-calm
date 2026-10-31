//! One creation-owned binding of the original request to pure allocation data.
//! This is not Workspace preparation or an execution grant. Effects remain with
//! their actual owners; the dormant path consumes the shared resource preparation.
use super::super::CreateTrackRequest;
use super::*;
use crate::harness::backend::factory::{PlannerFactory, RetainedIdentityPreparation};
use crate::session_projection_repo::AgentProvider;
use std::path::PathBuf;

/// Permanent receipts decide first; a new key uses the creation owner's static
/// capability. Runtime readiness and execution permission never choose storage.
pub(in crate::routes::tracks) async fn creation_plan(
    state: &RouteState,
    source: Option<&crate::auth::HumanInputSource>,
    headers: &HeaderMap,
    request: CreateTrackRequest,
) -> Result<CreatePlan> {
    let Some(source) = source else {
        if request.first_message.is_some() {
            return Err(CalmError::Unauthorized);
        }
        let shape = CreateRequestShape::from_request(&request)?;
        if canonical_creation_supported(&shape) {
            return Err(CalmError::Unauthorized);
        }
        return Ok(CreatePlan::Historical(request));
    };
    let key = track_create_key(headers)?;
    if let Some(text) = &request.first_message {
        if key.is_none() {
            return Err(CalmError::BadRequest(
                "Idempotency-Key header is required when first input is accepted".into(),
            ));
        }
        validate_first_message(text)?;
    }
    let shape = CreateRequestShape::from_request(&request)?;
    let preparation_source = source.clone();
    let source = source.clone();
    let area_id = request.area_id.to_string();
    let workspace_root = state.workspace_root.clone();
    let admission_root = state.owned_location_roots();
    let teardown_targets = state.teardown_targets.clone();
    let headers = headers.clone();
    let plan = crate::db::write_in_tx_typed(state.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let principal = source.current_principal()?;
            if let Some(key) = &key {
                if crate::db::sqlite::conversation::human_track_creation_get_tx(tx, &area_id, key)
                    .await?
                    .is_some()
                {
                    return Ok(CreatePlan::Canonical(KeyedPlan::Resume(
                        prepare_canonical_creation(request, &headers, workspace_root)?,
                    )));
                }
                if let Some(binding) =
                    crate::db::sqlite::track_create_idempotency_get_tx(tx, &area_id, key).await?
                {
                    return Ok(
                        if crate::db::sqlite::conversation::card_has_canonical_storage(
                            tx,
                            &binding.planner_card_id,
                        )
                        .await?
                        {
                            CreatePlan::Canonical(KeyedPlan::Resume(prepare_canonical_creation(
                                request,
                                &headers,
                                workspace_root,
                            )?))
                        } else {
                            CreatePlan::HistoricalReceipt { binding, request }
                        },
                    );
                }
                if canonical_creation_supported(&shape)
                    && super::dormant::prior_start_attempt_tx(tx, &area_id, key).await?
                {
                    // Preserve the historical owner's retry selection and error
                    // ordering for native-thread creation. Local canonical creation
                    // cannot mint through that owner; a lost binding is a server error.
                    return match PlannerFactory::retained_identity(
                        &crate::harness::profile::PlannerBinding {
                            profile: crate::harness::profile::HarnessProfile::Planner,
                            provider: shape.planner_provider.clone(),
                        },
                    )? {
                        RetainedIdentityPreparation::NativeThread => {
                            Ok(CreatePlan::Historical(request))
                        }
                        RetainedIdentityPreparation::LocalSession => {
                            Err(super::historical_plan::binding_lost(
                                &derive_track_create_operation_key(&area_id, key),
                            ))
                        }
                    };
                }
            }
            // A storage decision grants no resource access. The immutable
            // replay and historical owners recheck their own admission; fresh
            // excluded shapes must retain that owner's error ordering too.
            if !canonical_creation_supported(&shape) {
                return Ok(CreatePlan::Historical(request));
            }
            let access = crate::organization_access::require_area_access_tx(
                tx,
                &area_id,
                &principal,
                calm_types::organization::AreaAccess::Collaborate,
            )
            .await;
            match access {
                Ok(area) => area,
                Err(error) => {
                    // Preserve the existing none teardown fence even after its
                    // Area row disappeared; this refusal never grants admission.
                    if matches!(error, CalmError::NotFound(_))
                        && shape.isolation
                            == super::super::CreateIsolation::Attached(
                                super::super::AttachedIsolation::None,
                            )
                    {
                        calm_truth::db::sqlite::admit_attached_workspace_tx(
                            tx,
                            &super::super::normalize_path(
                                shape.cwd.as_deref().expect("decoded attached cwd"),
                            ),
                            &admission_root,
                            &teardown_targets.snapshot(),
                            &super::super::TrackWorkspacePlan::AttachedFromCwd,
                        )
                        .await?;
                    }
                    return Err(error);
                }
            };
            if shape.cwd.is_some() {
                crate::workspace_service::require_borrowed_admission_tx(
                    tx,
                    &source,
                    &area_id,
                    shape
                        .allow_cross_area_cwd
                        .as_ref()
                        .map(|cross| cross.area_id.as_str()),
                )
                .await?;
            }
            Ok(CreatePlan::Canonical(KeyedPlan::Mint(
                prepare_canonical_creation(request, &headers, workspace_root)?,
            )))
        })
    })
    .await?;
    match plan {
        CreatePlan::Canonical(KeyedPlan::Mint(plan)) => Ok(CreatePlan::Canonical(KeyedPlan::Mint(
            plan.prepare_resources(state, &preparation_source).await?,
        ))),
        other => Ok(other),
    }
}

/// Fresh blank managed creation accepts unavailable providers with a retained
/// Planner identity. Both local sessions and native threads allocate canonically
/// without native work; first input owns activation.
/// This says nothing about excluded creation shapes or live model writes.
pub(crate) fn accepts_unavailable_blank_track(provider: &AgentProvider) -> bool {
    match PlannerFactory::retained_identity(&crate::harness::profile::PlannerBinding {
        profile: crate::harness::profile::HarnessProfile::Planner,
        provider: provider.clone(),
    }) {
        Ok(
            RetainedIdentityPreparation::NativeThread | RetainedIdentityPreparation::LocalSession,
        ) => true,
        Err(_) => false,
    }
}

// The creation owner admits qualified canonical shapes here. Excluded
// creation kinds retain historical preparation; receipts retain their own storage.
fn canonical_creation_supported(request: &CreateRequestShape) -> bool {
    // The activation owner queries ACP registration policy. Local bindings are
    // process-free: native session/new waits for the first prompt, so an empty
    // conversation is valid. Native threads likewise wait for first input; named
    // creation retains its frozen context in the same empty carrier.
    let named = request.template_id.is_some()
        || request.recipe_id.is_some()
        || request.template_input.is_some()
        || request.fork_report_from.is_some();
    let supported = PlannerFactory::retained_identity(&crate::harness::profile::PlannerBinding {
        profile: crate::harness::profile::HarnessProfile::Planner,
        provider: request.planner_provider.clone(),
    })
    .is_ok();
    // Named sources reuse canonical allocation and its frozen Context for every
    // retained binding. Admin/borrowed rollout still uses its original entry contract.
    // Blank attachment without cwd still selects Managed preparation and borrows
    // nothing. Named/admin attachment retains its existing admission owner.
    supported
        && (!request.attach_folder || request.cwd.is_some() || (!named && !request.admin))
        && (!(request.admin || request.cwd.is_some() || request.allow_cross_area_cwd.is_some())
            || matches!(
                request.planner_provider,
                AgentProvider::Codex | AgentProvider::OpenCode
            ))
}

impl CreateRequestShape {
    /// Capture the caller's spelling before any normalization or admission.
    /// The existing digest owner, including its legacy omissions, stays shared.
    pub(in crate::routes::tracks) fn from_request(request: &CreateTrackRequest) -> Result<Self> {
        Ok(Self {
            isolation: super::super::CreateIsolation::decode(&request.cwd, request.isolation)?,
            admin: request.admin,
            planner_provider: request.planner_provider.clone(),
            model: request.model.clone(),
            reasoning_effort: request.reasoning_effort.clone(),
            title: request.title.clone(),
            sort: request.sort,
            cwd: request.cwd.clone(),
            template_id: request.template_id.clone(),
            recipe_id: request.recipe_id.clone(),
            template_input: request.template_input.clone(),
            attach_folder: request.attach_folder,
            allow_cross_area_cwd: request.allow_cross_area_cwd.clone(),
            theme: request.theme,
            fork_report_from: request.fork_report_from.clone(),
        })
    }
}

/// The raw request is split once. Only the allocation facts survive in either
/// plan; first text and its required fingerprint belong exclusively to Human.
pub(in crate::routes::tracks) fn prepare_canonical_creation(
    request: CreateTrackRequest,
    headers: &HeaderMap,
    workspace_root: PathBuf,
) -> Result<CreateShape<HumanTrackCreationPlan, EmptyTrackCreationPlan>> {
    let raw_key = track_create_key(headers)?;
    let shape = CreateRequestShape::from_request(&request)?;
    let create_sha = create_request_digest(&shape)?;
    let allocation = BlankAllocation {
        area_id: request.area_id,
        shape,
        workspace_root,
        conflict: super::super::FolderConflictSlot::default(),
        preparation: ResourcePreparation::Deferred,
    };
    Ok(match request.first_message {
        Some(text) => {
            let raw_key = raw_key.ok_or_else(|| {
                CalmError::BadRequest(
                    "Idempotency-Key header is required when first input is accepted".into(),
                )
            })?;
            validate_first_message(&text)?;
            CreateShape::WithFirstMessage(HumanTrackCreationPlan {
                allocation,
                raw_key,
                create_sha,
                first_sha: first_message_digest(&text),
                text,
            })
        }
        None => CreateShape::MessageLess(EmptyTrackCreationPlan {
            allocation,
            claim: raw_key.map(|key| MessageLessClaim { key, create_sha }),
        }),
    })
}

struct BlankAllocation {
    area_id: crate::ids::AreaId,
    shape: CreateRequestShape,
    workspace_root: PathBuf,
    conflict: super::super::FolderConflictSlot,
    preparation: ResourcePreparation,
}

enum ResourcePreparation {
    Deferred,
    Prepared(Box<super::super::preparation::PreparedCreate>),
}

impl CreateShape<HumanTrackCreationPlan, EmptyTrackCreationPlan> {
    async fn prepare_resources(
        mut self,
        state: &RouteState,
        source: &crate::auth::HumanInputSource,
    ) -> Result<Self> {
        let allocation = match &mut self {
            Self::WithFirstMessage(plan) => &mut plan.allocation,
            Self::MessageLess(plan) => &mut plan.allocation,
        };
        if allocation.shape.cwd.is_some() {
            // Preparation consumes the actual admitted Area kind. Organization
            // policy remains authoritative, including System owner access;
            // dormant allocation checks it again in its committing transaction.
            let area = crate::organization_access::require_area_access(
                state.repo.as_ref(),
                allocation.area_id.as_str(),
                &source.current_principal()?,
                calm_types::organization::AreaAccess::Collaborate,
            )
            .await?;
            allocation.preparation = ResourcePreparation::Prepared(Box::new(
                prepare_allocation(state, allocation, area.kind).await?,
            ));
        }
        Ok(self)
    }

    pub(in crate::routes::tracks) fn folder_conflict(&self) -> super::super::FolderConflictSlot {
        match self {
            Self::WithFirstMessage(plan) => plan.allocation.conflict.clone(),
            Self::MessageLess(plan) => plan.allocation.conflict.clone(),
        }
    }
}

pub(in crate::routes::tracks) struct HumanTrackCreationPlan {
    allocation: BlankAllocation,
    raw_key: String,
    create_sha: String,
    first_sha: String,
    text: String,
}

impl HumanTrackCreationPlan {
    pub(super) fn area_id(&self) -> &str {
        self.allocation.area_id.as_str()
    }

    pub(super) fn selection(&self) -> (Option<&str>, Option<&str>) {
        self.allocation.selection()
    }

    pub(super) fn key(&self) -> &str {
        &self.raw_key
    }

    pub(super) fn create_sha(&self) -> &str {
        &self.create_sha
    }

    pub(super) fn first_sha(&self) -> &str {
        &self.first_sha
    }

    /// Called only after original-key replay has been decided. No mutable
    /// catalog, filesystem or provider check can make an accepted replay mint.
    pub(super) async fn into_allocation(
        self,
        state: &RouteState,
        area_kind: crate::model::AreaKind,
    ) -> Result<(NewTrack, CreateTrackOptions, String)> {
        let (allocation, options) = blank_allocation(
            state,
            area_kind,
            self.allocation,
            Some(TrackCreateIdempotencyClaim {
                key: self.raw_key,
                create_request_sha256: self.create_sha,
                first_message_sha256: Some(self.first_sha),
            }),
        )
        .await?;
        Ok((allocation, options, self.text))
    }
}

/// A message-less binding has no slot for a first-message fingerprint.
struct MessageLessClaim {
    key: String,
    create_sha: String,
}

/// Empty creation has no first-text or Human acknowledgement to manufacture.
pub(in crate::routes::tracks) struct EmptyTrackCreationPlan {
    allocation: BlankAllocation,
    claim: Option<MessageLessClaim>,
}
impl EmptyTrackCreationPlan {
    pub(super) fn area_id(&self) -> &str {
        self.allocation.area_id.as_str()
    }

    pub(super) fn selection(&self) -> (Option<&str>, Option<&str>) {
        self.allocation.selection()
    }

    pub(super) fn binding(&self) -> Option<(&str, &str)> {
        self.claim
            .as_ref()
            .map(|claim| (claim.key.as_str(), claim.create_sha.as_str()))
    }

    pub(super) async fn into_allocation(
        self,
        state: &RouteState,
        area_kind: crate::model::AreaKind,
    ) -> Result<(NewTrack, CreateTrackOptions)> {
        blank_allocation(
            state,
            area_kind,
            self.allocation,
            self.claim.map(|claim| TrackCreateIdempotencyClaim {
                key: claim.key,
                create_request_sha256: claim.create_sha,
                first_message_sha256: None,
            }),
        )
        .await
    }
}

impl BlankAllocation {
    fn selection(&self) -> (Option<&str>, Option<&str>) {
        (
            self.shape.model.as_deref(),
            self.shape.reasoning_effort.as_deref(),
        )
    }
}

async fn blank_allocation(
    state: &RouteState,
    area_kind: crate::model::AreaKind,
    allocation: BlankAllocation,
    claim: Option<TrackCreateIdempotencyClaim>,
) -> Result<(NewTrack, CreateTrackOptions)> {
    if !canonical_creation_supported(&allocation.shape) {
        return Err(CalmError::BadRequest(
            "dormant acceptance requires an admitted Track Planner".into(),
        ));
    }
    let mut allocation = allocation;
    if matches!(allocation.preparation, ResourcePreparation::Deferred) {
        if allocation.shape.cwd.is_some() {
            return Err(CalmError::BadRequest(
                "borrowed creation requires preparation outside the allocation transaction".into(),
            ));
        }
        allocation.preparation = ResourcePreparation::Prepared(Box::new(
            prepare_allocation(state, &allocation, area_kind).await?,
        ));
    }
    let ResourcePreparation::Prepared(prepared) = allocation.preparation else {
        unreachable!("preparation completed above");
    };
    let prepared = *prepared;
    let request = allocation.shape;
    let conflict = allocation.conflict;
    let super::super::preparation::PreparedCreate {
        p: allocation,
        mut folder_claim,
        body_area_id,
        normalized_cwd,
        init,
        workspace_plan,
        ..
    } = prepared;
    if let super::super::FolderClaim::Enforce { conflict: slot, .. } = &mut folder_claim {
        *slot = conflict;
    }
    let options = CreateTrackOptions {
        human_source: None,
        admin: request.admin,
        managed_identity: None,
        creator: None,
        creation_message: None,
        planner_provider: request.planner_provider,
        model: request.model,
        reasoning_effort: request.reasoning_effort,
        folder_claim,
        body_area_id,
        normalized_cwd,
        init,
        workspace_plan,
        idempotency_claim: claim,
    };
    Ok((allocation, options))
}

/// Resource validation is outside the writer for borrowed locators; managed
/// preparation remains process-free. Both consume the original preparation owner.
async fn prepare_allocation(
    state: &RouteState,
    allocation: &BlankAllocation,
    area_kind: crate::model::AreaKind,
) -> Result<super::super::preparation::PreparedCreate> {
    let request = &allocation.shape;
    // This configured root is pure allocation data. It neither resolves a
    // runnable environment nor grants access. No default HOME/cwd is used.
    if !allocation.workspace_root.is_absolute() {
        return Err(CalmError::BadRequest(
            "dormant creation requires an absolute configured workspace root".into(),
        ));
    }
    let root = allocation.workspace_root.to_str().ok_or_else(|| {
        CalmError::BadRequest("configured workspace root is not representable as text".into())
    })?;
    let normalized_root = super::super::normalize_path(root);
    let named_source = super::super::preparation::NamedSource::from_request(
        request.template_id.clone(),
        request.recipe_id.clone(),
        request.fork_report_from.clone(),
    )?;
    let allocation = NewTrack {
        area_id: allocation.area_id.clone(),
        title: request.title.clone(),
        sort: request.sort,
        // ManagedUnder derives the actual path from the allocated Track ID.
        cwd: request.cwd.clone().unwrap_or(normalized_root),
        template_id: None,
        plugin_scope: None,
        template_input: request.template_input.clone(),
        attach_folder: request.attach_folder,
        theme: request.theme,
    };
    super::super::preparation::prepare_in_area(
        state,
        allocation,
        named_source,
        area_kind,
        request.isolation,
        request.allow_cross_area_cwd.clone(),
    )
    .await
}
