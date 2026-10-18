//! One creation-owned binding of the original request to pure allocation data.
//! This is not Workspace preparation or an execution grant. Effects remain with
//! their actual owners; the dormant path explicitly supports managed blank data.
use super::super::{CreateTrackRequest, FolderClaim, TrackInit};
use super::*;
use crate::db::sqlite::TrackWorkspacePlan;
use std::path::PathBuf;

/// Permanent receipts decide first; a new key uses the creation owner's static
/// capability. Runtime readiness and execution permission never choose storage.
pub(in crate::routes::tracks) async fn first_message_storage_contract(
    state: &RouteState,
    source: &crate::auth::HumanInputSource,
    headers: &HeaderMap,
    request: &CreateTrackRequest,
) -> Result<crate::conversation_inputs::InputStorageContract> {
    use crate::conversation_inputs::InputStorageContract;
    let key = track_create_key(headers)?.ok_or_else(|| {
        CalmError::BadRequest(
            "Idempotency-Key header is required when first input is accepted".into(),
        )
    })?;
    let text = request
        .first_message
        .as_deref()
        .ok_or_else(|| CalmError::BadRequest("first input is missing".into()))?;
    validate_first_message(text)?;
    let source = source.clone();
    let area_id = request.area_id.to_string();
    let supports_canonical = canonical_creation_supported(request);
    crate::db::write_in_tx_typed(state.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let principal = source.current_principal()?;
            let area = crate::organization_access::require_area_access_tx(
                tx,
                &area_id,
                &principal,
                calm_types::organization::AreaAccess::Collaborate,
            )
            .await?;
            if crate::db::sqlite::conversation::human_track_creation_get_tx(tx, &area_id, &key)
                .await?
                .is_some()
            {
                return Ok(InputStorageContract::Canonical);
            }
            if let Some(binding) =
                crate::db::sqlite::track_create_idempotency_get_tx(tx, &area_id, &key).await?
            {
                return Ok(
                    if crate::db::sqlite::conversation::card_has_canonical_storage(
                        tx,
                        &binding.planner_card_id,
                    )
                    .await?
                    {
                        InputStorageContract::Canonical
                    } else {
                        InputStorageContract::Historical
                    },
                );
            }
            if super::dormant::prior_start_attempt_tx(tx, &area_id, &key).await? {
                return Ok(InputStorageContract::Historical);
            }
            Ok(
                if supports_canonical && area.kind == crate::model::AreaKind::User {
                    InputStorageContract::Canonical
                } else {
                    InputStorageContract::Historical
                },
            )
        })
    })
    .await
}

// The one creation-owned capability rule; both dispatch and allocation consume
// it. Historical compatibility stops creating these variants when their full
// canonical preparation/start path is implemented.
fn canonical_creation_supported(request: &CreateTrackRequest) -> bool {
    request.planner_provider == crate::session_projection_repo::AgentProvider::Codex
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
    pub(in crate::routes::tracks) fn from_request(request: &CreateTrackRequest) -> Self {
        Self {
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
        }
    }
}

pub(super) struct HumanTrackCreationPlan {
    request: CreateTrackRequest,
    workspace_root: PathBuf,
    raw_key: String,
    create_sha: String,
    first_sha: String,
    text: String,
}

impl HumanTrackCreationPlan {
    pub(super) fn prepare(
        request: CreateTrackRequest,
        headers: &HeaderMap,
        workspace_root: PathBuf,
    ) -> Result<Self> {
        let raw_key = track_create_key(headers)?.ok_or_else(|| {
            CalmError::BadRequest(
                "Idempotency-Key header is required when first input is accepted".into(),
            )
        })?;
        let mut request = request;
        let text = request.first_message.take().ok_or_else(|| {
            CalmError::BadRequest("dormant Human creation requires its first input".into())
        })?;
        validate_first_message(&text)?;
        let create_sha = create_request_digest(&CreateRequestShape::from_request(&request))?;
        let first_sha = first_message_digest(&text);
        Ok(Self {
            request,
            workspace_root,
            raw_key,
            create_sha,
            first_sha,
            text,
        })
    }

    pub(super) fn area_id(&self) -> &str {
        self.request.area_id.as_str()
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
        let request = self.request;
        if !canonical_creation_supported(&request) {
            return Err(CalmError::BadRequest(
                "dormant Human acceptance supports only a new blank keyed managed Track Planner"
                    .into(),
            ));
        }
        // This configured root is pure allocation data. It neither resolves a
        // runnable environment nor grants access. No default HOME/cwd is used.
        if !self.workspace_root.is_absolute() {
            return Err(CalmError::BadRequest(
                "dormant creation requires an absolute configured workspace root".into(),
            ));
        }
        let root = self.workspace_root.to_str().ok_or_else(|| {
            CalmError::BadRequest("configured workspace root is not representable as text".into())
        })?;
        let normalized_root = super::super::normalize_path(root);
        let area_id = request.area_id.to_string();
        let allocation = NewTrack {
            area_id: request.area_id,
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
            body_area_id: area_id,
            normalized_cwd: normalized_root,
            init: TrackInit::Blank,
            workspace_plan: TrackWorkspacePlan::ManagedUnder(self.workspace_root),
            idempotency_claim: Some(TrackCreateIdempotencyClaim {
                key: self.raw_key,
                create_request_sha256: self.create_sha,
                first_message_sha256: Some(self.first_sha),
            }),
        };
        Ok((allocation, options, self.text))
    }
}
