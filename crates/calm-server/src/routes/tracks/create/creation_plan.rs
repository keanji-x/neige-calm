//! One creation-owned binding of the original request to pure allocation data.
//! This is not Workspace preparation or an execution grant. Effects remain with
//! their actual owners; the dormant path explicitly supports managed blank data.
use super::super::{CreateTrackRequest, FolderClaim, TrackInit};
use super::*;
use crate::db::sqlite::TrackWorkspacePlan;
use std::path::PathBuf;

pub(in crate::routes::tracks) enum CreationStorage {
    FreshCanonical,
    CanonicalReplay,
    Historical,
}

/// Permanent receipts decide first; a new key uses the creation owner's static
/// capability. Runtime readiness and execution permission never choose storage.
pub(in crate::routes::tracks) async fn creation_storage_contract(
    state: &RouteState,
    source: &crate::auth::HumanInputSource,
    headers: &HeaderMap,
    request: &CreateTrackRequest,
) -> Result<CreationStorage> {
    let key = track_create_key(headers)?;
    if let Some(text) = &request.first_message {
        if key.is_none() {
            return Err(CalmError::BadRequest(
                "Idempotency-Key header is required when first input is accepted".into(),
            ));
        }
        validate_first_message(text)?;
    }
    let source = source.clone();
    let area_id = request.area_id.to_string();
    let shape = CreateRequestShape::from_request(request)?;
    let input = match &request.first_message {
        Some(_) => CreateShape::WithFirstMessage(()),
        None => CreateShape::MessageLess(()),
    };
    crate::db::write_in_tx_typed(state.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let principal = source.current_principal()?;
            if let Some(key) = &key {
                if crate::db::sqlite::conversation::human_track_creation_get_tx(tx, &area_id, key)
                    .await?
                    .is_some()
                {
                    return Ok(CreationStorage::CanonicalReplay);
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
                            CreationStorage::CanonicalReplay
                        } else {
                            CreationStorage::Historical
                        },
                    );
                }
                if super::dormant::prior_start_attempt_tx(tx, &area_id, key).await? {
                    return Ok(CreationStorage::Historical);
                }
            }
            // A storage decision grants no resource access. The immutable
            // replay and historical owners recheck their own admission; fresh
            // excluded shapes must retain that owner's error ordering too.
            if !canonical_creation_supported(&shape, input) {
                return Ok(CreationStorage::Historical);
            }
            let area = crate::organization_access::require_area_access_tx(
                tx,
                &area_id,
                &principal,
                calm_types::organization::AreaAccess::Collaborate,
            )
            .await?;
            Ok(if area.kind == crate::model::AreaKind::User {
                CreationStorage::FreshCanonical
            } else {
                CreationStorage::Historical
            })
        })
    })
    .await
}

// The one creation-owned capability rule; both dispatch and allocation consume
// it. Historical compatibility stops creating these variants when their full
// canonical preparation/start path is implemented.
fn canonical_creation_supported(request: &CreateRequestShape, input: CreateShape) -> bool {
    use calm_types::runtime::AgentProtocol;
    let supported = match (request.planner_provider.protocol(), input) {
        (AgentProtocol::Codex, CreateShape::WithFirstMessage(()))
        | (
            AgentProtocol::Claude,
            CreateShape::WithFirstMessage(()) | CreateShape::MessageLess(()),
        ) => true,
        (AgentProtocol::Codex, CreateShape::MessageLess(()))
        | (AgentProtocol::Acp, CreateShape::WithFirstMessage(()) | CreateShape::MessageLess(())) => {
            false
        }
    };
    supported
        && !request.admin
        && request.cwd.is_none()
        && !request.attach_folder
        && request.template_id.is_none()
        && request.recipe_id.is_none()
        && request.template_input.is_none()
        && request.fork_report_from.is_none()
        && request.allow_cross_area_cwd.is_none()
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
    pub(super) fn into_allocation(self) -> Result<(NewTrack, CreateTrackOptions, String)> {
        let (allocation, options) = blank_allocation(
            self.allocation,
            CreateShape::WithFirstMessage(()),
            Some(TrackCreateIdempotencyClaim {
                key: self.raw_key,
                create_request_sha256: self.create_sha,
                first_message_sha256: Some(self.first_sha),
            }),
        )?;
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

    pub(super) fn into_allocation(self) -> Result<(NewTrack, CreateTrackOptions)> {
        blank_allocation(
            self.allocation,
            CreateShape::MessageLess(()),
            self.claim.map(|claim| TrackCreateIdempotencyClaim {
                key: claim.key,
                create_request_sha256: claim.create_sha,
                first_message_sha256: None,
            }),
        )
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

fn blank_allocation(
    allocation: BlankAllocation,
    input: CreateShape,
    claim: Option<TrackCreateIdempotencyClaim>,
) -> Result<(NewTrack, CreateTrackOptions)> {
    let BlankAllocation {
        area_id,
        shape: request,
        workspace_root,
    } = allocation;
    if !canonical_creation_supported(&request, input) {
        return Err(CalmError::BadRequest(
            "dormant Human acceptance supports only a new blank managed Track Planner".into(),
        ));
    }
    // This configured root is pure allocation data. It neither resolves a
    // runnable environment nor grants access. No default HOME/cwd is used.
    if !workspace_root.is_absolute() {
        return Err(CalmError::BadRequest(
            "dormant creation requires an absolute configured workspace root".into(),
        ));
    }
    let root = workspace_root.to_str().ok_or_else(|| {
        CalmError::BadRequest("configured workspace root is not representable as text".into())
    })?;
    let normalized_root = super::super::normalize_path(root);
    let body_area_id = area_id.to_string();
    let allocation = NewTrack {
        area_id,
        title: request.title,
        sort: request.sort,
        // ManagedUnder derives the actual path from the allocated Track ID.
        cwd: normalized_root.clone(),
        template_id: None,
        plugin_scope: None,
        template_input: None,
        attach_folder: false,
        theme: request.theme,
    };
    let options = CreateTrackOptions {
        human_source: None,
        admin: false,
        managed_identity: None,
        creator: None,
        creation_message: None,
        planner_provider: request.planner_provider,
        model: request.model,
        reasoning_effort: request.reasoning_effort,
        folder_claim: FolderClaim::Skip,
        body_area_id,
        normalized_cwd: normalized_root,
        init: TrackInit::Blank,
        workspace_plan: TrackWorkspacePlan::ManagedUnder(workspace_root),
        idempotency_claim: claim,
    };
    Ok((allocation, options))
}
