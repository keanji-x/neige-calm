//! `/api/tracks`, `/api/areas/:id/tracks` — Track CRUD.

use crate::AREA_CHAT_PURPOSE;
use crate::actor::Actor;
use crate::auth::Principal;
use crate::db::sqlite::{
    MAX_TRACK_TREE_DEPTH, MAX_TREE_TASK_BUDGET, TRACK_TREE_MEMBERS_SQL, TrackRecipeOrigin,
    TrackTreeTerm, TrackWorkspacePlan, area_folder_create_tx, area_folders_list_all_tx,
    begin_immediate_tx, card_create_with_id_tx, overlay_delete_by_entity_tx,
    overlay_delete_card_overlays_by_track_tx, overlay_upsert_tx, terminal_delete_tx,
    track_create_idempotency_claim_tx, track_create_tx, track_delete_tx, track_recipe_get_tx,
    track_tree_term, track_update_tx,
};
use crate::db::write_with_actor_events_typed;
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, CardId, TrackId};
use crate::model::{
    AreaKind, Card, CardRole, FolderConflict, FolderConflictKind, NewCard, NewOverlay, NewTrack,
    RequestTheme, Track, TrackDetail, TrackPatch, TrackWorkspace, TrackWorkspaceKind,
    TrackWorkspacePatch, new_id,
};
use crate::operation::planner_harness_start_adapter::PlannerHarnessStartOperationPayload;
use crate::operation::workspace_lease::{
    WorkspaceTrackSweep, release_workspace_leases_for_track_tx,
    sweep_workspace_worktrees_for_tracks_repo, track_has_active_forge_action,
};
use crate::operation::{OperationKey, OperationOutcome};
use crate::plugin_host::manifest::Manifest;
use crate::report_backlinks;
use crate::routes::area_folders::{find_owner, is_descendant_of, normalize_path};
use crate::routes::cards::quiesce_shared_card_active_turn;
use crate::routes::codex_cards::default_cwd;
use crate::routes::terminal_cards::stable_payload_hash;
use crate::session_projection_lookup::project_runtime_into_cards_payload;
use crate::state::{AppState, CodexShellState, RouteState, WorkerState};
use crate::templates::{Template, TemplateRoster};
use crate::terminal_sweeper::quiesce_terminal_artifacts_for_deletion;
use crate::track_fs_view::{TrackFsContent, TrackFsEntry, TrackFsView};
use crate::track_lifecycle::{
    track_get_tx, validate_transition, validate_transition_snapshot_in_tx,
};
use crate::track_report::{
    self, ReportBlock, TrackReportPayload, report_blocks_snapshot_tx, resolve_report_for_track,
    tasks_rebuild_tree_after_member_removal_tx, tasks_rebuild_tree_tx, tasks_rebuild_tx,
    validate_task_rebuild_source_tx,
};
use crate::track_report_doc::ReportDoc;
use crate::track_report_read::load_report_read_snapshot;
use crate::validation::CODEX_PAYLOAD_SCHEMA_VERSION;
use crate::workspace_recycle;
use crate::workspace_repoint::{PristineVerdict, workspace_pristine};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use futures::FutureExt;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use utoipa::{IntoParams, ToSchema};

#[cfg(feature = "fixtures")]
use std::collections::HashMap;
#[cfg(feature = "fixtures")]
use std::sync::{Mutex as StdMutex, OnceLock};
#[cfg(feature = "fixtures")]
use tokio::sync::Notify;

mod claude_permissions;
mod create;
mod fork_guard;

/// Test seam that makes the cross-instance same-key mint race deterministic.
pub type TrackCreateMintRendezvous = Option<std::sync::Arc<TrackCreateMintGate>>;

/// The two meeting points of [`TrackCreateMintRendezvous`]; every wait is bounded so a
/// mis-driven test fails an assertion instead of hanging.
pub struct TrackCreateMintGate {
    /// The minting request has passed lookup 1 and selected the `Mint` arm; it
    /// has not opened the create transaction yet.
    pub reached: tokio::sync::Barrier,
    /// Released once the peer has committed its own mint.
    pub released: tokio::sync::Barrier,
}

impl TrackCreateMintGate {
    /// Both barriers admit exactly two participants: the held request and the
    /// test task driving the winner.
    pub fn new() -> Self {
        Self {
            reached: tokio::sync::Barrier::new(2),
            released: tokio::sync::Barrier::new(2),
        }
    }

    /// Park the minting request until the peer has committed, or until the bound elapses.
    pub(crate) async fn hold(&self) {
        const BOUND: std::time::Duration = std::time::Duration::from_secs(30);
        let _ = tokio::time::timeout(BOUND, self.reached.wait()).await;
        let _ = tokio::time::timeout(BOUND, self.released.wait()).await;
    }
}

impl Default for TrackCreateMintGate {
    fn default() -> Self {
        Self::new()
    }
}

use claude_permissions::validate_policy_patch;
use fork_guard::guard_forked_blocks;

#[derive(Clone)]
struct TrackDeletePlan {
    track_id: TrackId,
    area_id: crate::ids::AreaId,
    cards: Vec<Card>,
    terminals: Vec<crate::model::Terminal>,
}

struct PreparedTrackDeletion {
    track: Track,
    area_kind: Option<AreaKind>,
    plan: TrackDeletePlan,
    turn_daemon: std::sync::Arc<crate::shared_codex_appserver::SharedCodexAppServer>,
    _operation_guard: tokio::sync::OwnedMutexGuard<()>,
    _delete_guard: crate::per_card_lock::KeyedLockGuard,
}

struct QuiescedTrackDeletion {
    prepared: PreparedTrackDeletion,
    sealed_thread_ids: Vec<String>,
}

struct RecycledTrackDeletion {
    prepared: PreparedTrackDeletion,
    decision: workspace_recycle::RecycleDecision,
    sealed_thread_ids: Vec<String>,
}

#[cfg(feature = "fixtures")]
#[derive(Clone)]
pub struct TrackDeleteTeardownHook {
    pub entered: std::sync::Arc<Notify>,
    pub release: std::sync::Arc<Notify>,
}

#[cfg(feature = "fixtures")]
#[derive(Clone)]
pub struct TrackDeleteCommitHook {
    pub entered: std::sync::Arc<Notify>,
    pub release: std::sync::Arc<Notify>,
    pub panic_after_release: bool,
}

/// Test seam for the lifecycle PATCH pre-read/transaction boundary.
#[cfg(feature = "fixtures")]
#[derive(Clone)]
pub struct TrackLifecyclePatchRaceHook {
    pub entered: std::sync::Arc<Notify>,
    pub release: std::sync::Arc<Notify>,
}

#[cfg(feature = "fixtures")]
fn track_delete_teardown_hooks() -> &'static StdMutex<HashMap<String, TrackDeleteTeardownHook>> {
    static HOOKS: OnceLock<StdMutex<HashMap<String, TrackDeleteTeardownHook>>> = OnceLock::new();
    HOOKS.get_or_init(|| StdMutex::new(HashMap::new()))
}

#[cfg(feature = "fixtures")]
fn track_delete_commit_hooks() -> &'static StdMutex<HashMap<String, TrackDeleteCommitHook>> {
    static HOOKS: OnceLock<StdMutex<HashMap<String, TrackDeleteCommitHook>>> = OnceLock::new();
    HOOKS.get_or_init(|| StdMutex::new(HashMap::new()))
}

#[cfg(feature = "fixtures")]
fn track_lifecycle_patch_race_hooks()
-> &'static StdMutex<HashMap<String, TrackLifecyclePatchRaceHook>> {
    static HOOKS: OnceLock<StdMutex<HashMap<String, TrackLifecyclePatchRaceHook>>> =
        OnceLock::new();
    HOOKS.get_or_init(|| StdMutex::new(HashMap::new()))
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn install_track_delete_teardown_hook_for_test(track_id: &str, hook: TrackDeleteTeardownHook) {
    track_delete_teardown_hooks()
        .lock()
        .expect("track delete hook mutex")
        .insert(track_id.to_string(), hook);
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn install_track_delete_commit_hook_for_test(track_id: &str, hook: TrackDeleteCommitHook) {
    track_delete_commit_hooks()
        .lock()
        .expect("track delete commit hook mutex")
        .insert(track_id.to_string(), hook);
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn install_track_lifecycle_patch_race_hook_for_test(
    track_id: &str,
    hook: TrackLifecyclePatchRaceHook,
) {
    track_lifecycle_patch_race_hooks()
        .lock()
        .expect("track lifecycle patch hook mutex")
        .insert(track_id.to_string(), hook);
}

async fn wait_at_track_delete_teardown_hook(track_id: &str) {
    #[cfg(feature = "fixtures")]
    {
        let hook = track_delete_teardown_hooks()
            .lock()
            .expect("track delete hook mutex")
            .remove(track_id);
        if let Some(hook) = hook {
            hook.entered.notify_one();
            hook.release.notified().await;
        }
    }
    #[cfg(not(feature = "fixtures"))]
    let _ = track_id;
}

async fn wait_at_track_delete_commit_hook(track_id: &str) -> bool {
    #[cfg(feature = "fixtures")]
    {
        let hook = track_delete_commit_hooks()
            .lock()
            .expect("track delete commit hook mutex")
            .remove(track_id);
        if let Some(hook) = hook {
            hook.entered.notify_one();
            hook.release.notified().await;
            return hook.panic_after_release;
        }
    }
    #[cfg(not(feature = "fixtures"))]
    let _ = track_id;
    false
}

async fn wait_at_track_lifecycle_patch_race_hook(track_id: &str) {
    #[cfg(feature = "fixtures")]
    {
        let hook = track_lifecycle_patch_race_hooks()
            .lock()
            .expect("track lifecycle patch hook mutex")
            .remove(track_id);
        if let Some(hook) = hook {
            hook.entered.notify_one();
            hook.release.notified().await;
        }
    }
    #[cfg(not(feature = "fixtures"))]
    let _ = track_id;
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateTrackRequest {
    #[schema(value_type = String)]
    pub area_id: crate::ids::AreaId,
    /// Omitted stores the empty string; the planner agent names the track via
    /// `calm.track.rename` while the stored title is still blank.
    #[serde(default)]
    #[schema(required = false)]
    pub title: String,
    pub sort: Option<f64>,
    /// Omitted / null persists `default_cwd()` and skips `area_folders`; present values
    /// (including the empty string) keep the absolute-path + claim rules.
    #[serde(default)]
    pub cwd: Option<String>,
    /// A built-in roster template to instantiate the report from; mutually exclusive with
    /// `recipe_id` and `fork_report_from` (naming two is a 400).
    #[serde(default)]
    pub template_id: Option<String>,
    /// A user-defined recipe (`track_recipes` row) to start from. Not folded into
    /// `template_id`: a recipe has no plugin manifest to resolve against.
    #[serde(default)]
    pub recipe_id: Option<String>,
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub template_input: Option<serde_json::Value>,
    #[serde(default)]
    pub attach_folder: bool,
    /// Explicit authorization to create this track in `area_id` while its cwd remains covered
    /// by the exact conflicting claim named here; checked inside the create transaction.
    #[serde(default)]
    pub allow_cross_area_cwd: Option<CrossAreaCwdAuthorization>,
    pub theme: RequestTheme,
    /// One-time creation instruction: copy this track's report snapshot into the new report
    /// inside the track-create transaction.
    #[serde(default)]
    pub fork_report_from: Option<String>,
    /// The user's first sentence, seeded into the planner harness as a user message. Supplying
    /// it makes `Idempotency-Key` required and turns a harness-start failure into a 500 (the
    /// committed track is not undone).
    #[serde(default)]
    pub first_message: Option<String>,
    /// Model slug for the planner's first and subsequent turns. Omitted or null
    /// follows installation defaults, as on the conversation model endpoint.
    #[serde(default)]
    pub model: Option<String>,
    /// Reasoning effort. Values unsupported by the chosen model's current
    /// catalog entry are rejected with 400 before creation; no silent adjustment.
    /// Omitted or null follows installation defaults.
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CrossAreaCwdAuthorization {
    pub folder_id: i64,
    #[schema(value_type = String)]
    pub area_id: crate::ids::AreaId,
}

impl CreateTrackRequest {
    /// `(body, named source, cwd_omitted)`. `cwd_omitted` is true when the client sent no
    /// `cwd` / `null`; an explicit empty string still 400s. Provenance columns stay `None` here.
    fn into_parts(self) -> Result<(NewTrack, NamedSource, bool)> {
        let cwd_omitted = self.cwd.is_none();
        let source =
            NamedSource::from_request(self.template_id, self.recipe_id, self.fork_report_from)?;
        Ok((
            NewTrack {
                area_id: self.area_id,
                title: self.title,
                sort: self.sort,
                cwd: self.cwd.unwrap_or_else(default_cwd),
                template_id: None,
                plugin_scope: None,
                template_input: self.template_input,
                attach_folder: if cwd_omitted {
                    false
                } else {
                    self.attach_folder
                },
                theme: self.theme,
            },
            source,
            cwd_omitted,
        ))
    }
}

/// The **one** starting point a create request names, before admission.
enum NamedSource {
    /// No starting point named; the track keeps the default skeleton.
    Blank,
    /// A built-in roster template, still the caller's spelling — not yet admitted.
    Template(String),
    /// A user-defined recipe row.
    Recipe(String),
    /// An existing track whose report is copied.
    Fork(String),
}

impl NamedSource {
    /// Collapse the three request fields, or refuse and say which two collided.
    fn from_request(
        template_id: Option<String>,
        recipe_id: Option<String>,
        fork_report_from: Option<String>,
    ) -> Result<Self> {
        let mut named: Vec<(&'static str, Self)> = [
            template_id.map(|id| ("template_id", Self::Template(id))),
            recipe_id.map(|id| ("recipe_id", Self::Recipe(id))),
            fork_report_from.map(|id| ("fork_report_from", Self::Fork(id))),
        ]
        .into_iter()
        .flatten()
        .collect();
        match named.len() {
            0 => Ok(Self::Blank),
            1 => Ok(named.pop().expect("one named source").1),
            _ => {
                let fields: Vec<&str> = named.iter().map(|(field, _)| *field).collect();
                let (last, rest) = fields.split_last().expect("at least two named sources");
                Err(CalmError::BadRequest(format!(
                    "track create: `{}` and `{last}` each name a starting point for the \
                     new track's report; give at most one",
                    rest.join("`, `"),
                )))
            }
        }
    }

    /// Admit the named source, producing the value that decides both which initialization
    /// runs and what the row records about it.
    async fn resolve(self, s: &RouteState) -> Result<CreationSource> {
        Ok(CreationSource {
            init: match self {
                Self::Blank => TrackInit::Blank,
                Self::Recipe(recipe_id) => TrackInit::Recipe { recipe_id },
                Self::Fork(source_track_id) => TrackInit::Fork { source_track_id },
                Self::Template(template_id) => {
                    // Roster membership is the whole admission test; a plugin binding is an
                    // attribute of the template, not a second way in.
                    let admission = admit_template(s, &template_id).await.ok_or_else(|| {
                        CalmError::BadRequest(format!(
                            "track create: `template_id` must reference a known track template; got `{template_id}`"
                        ))
                    })?;
                    TrackInit::Template {
                        key: admission.key(),
                        binding: admission.binding.map(Box::new),
                    }
                }
            },
        })
    }
}

/// An admitted creation source: the initialization that will run, and — derived from that
/// same value — the provenance the `tracks` row records.
struct CreationSource {
    init: TrackInit,
}

impl CreationSource {
    /// `None` binding has two causes the 400 must distinguish: no `template_id` at all, or an
    /// admitted one whose owning plugin is not running and trusted.
    fn template_input_owner(&self) -> crate::plugin_host::template_input::TemplateInputOwner<'_> {
        use crate::plugin_host::template_input::TemplateInputOwner;
        match &self.init {
            TrackInit::Template {
                binding: Some(manifest),
                ..
            } => TemplateInputOwner::Plugin(manifest),
            TrackInit::Template { binding: None, .. } => TemplateInputOwner::NoBoundPlugin,
            TrackInit::Blank | TrackInit::Recipe { .. } | TrackInit::Fork { .. } => {
                TemplateInputOwner::NoTemplateId
            }
        }
    }

    /// Write this source's provenance onto the row; both columns come out of one `match` arm
    /// so they cannot name different sources.
    fn stamp(&self, p: &mut NewTrack) {
        let (template_id, plugin_scope) = match &self.init {
            TrackInit::Template { key, binding } => (
                Some((*key).to_string()),
                binding.as_ref().map(|manifest| manifest.id.clone()),
            ),
            TrackInit::Blank | TrackInit::Recipe { .. } | TrackInit::Fork { .. } => (None, None),
        };
        p.template_id = template_id;
        p.plugin_scope = plugin_scope;
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/tracks", get(list_tracks_window).post(create_track))
        .route(
            "/api/tracks/{id}",
            get(get_track_detail)
                .patch(update_track)
                .delete(delete_track),
        )
        // Session-authenticated; only `ActorId::User` is accepted (worker / planner /
        // plugin actors are 403).
        .route(
            "/api/tracks/{id}/report",
            get(get_track_report).post(update_track_report),
        )
        .route("/api/tracks/{id}/backlinks", get(get_track_backlinks))
        .route("/api/tracks/{id}/files/ls", get(list_track_files))
        .route("/api/tracks/{id}/files/cat", get(cat_track_file))
        .route("/api/areas/{area_id}/tracks", get(list_tracks_by_area))
}

#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct TrackFsLsQuery {
    /// Logical path to list. Omitted or `/` lists the track root.
    pub path: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct TrackFsCatQuery {
    /// Logical path to read. Required.
    pub path: Option<String>,
}

#[utoipa::path(
    get,
    path = "/api/tracks/{id}/files/ls",
    tag = "tracks",
    params(("id" = String, Path, description = "Track id"), TrackFsLsQuery),
    responses(
        (status = 200, description = "Track file view directory entries", body = Vec<TrackFsEntry>),
        (status = 400, description = "Logical path not available", body = ErrorBody),
        (status = 401, description = "Missing or invalid session", body = ErrorBody),
        (status = 403, description = "Referenced card is outside the track", body = ErrorBody),
        (status = 404, description = "Track not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
// No `Principal` extractor here: the replay binary's a11y suite drives these GET
// routes without a session, and a 401 would redirect it to login.
pub(crate) async fn list_track_files(
    State(s): State<RouteState>,
    Path(id): Path<String>,
    Query(q): Query<TrackFsLsQuery>,
) -> Result<Json<Vec<TrackFsEntry>>> {
    let track = s
        .repo
        .track_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {id}")))?;
    // TODO(multi-user): ownership check
    let view = TrackFsView::new(s.repo.as_ref(), &s.write);
    let entries = view.ls(&track, q.path.as_deref()).await?;
    Ok(Json(entries))
}

#[utoipa::path(
    get,
    path = "/api/tracks/{id}/files/cat",
    tag = "tracks",
    params(("id" = String, Path, description = "Track id"), TrackFsCatQuery),
    responses(
        (status = 200, description = "Track file view content", body = TrackFsContent),
        (status = 400, description = "Missing path or logical path not available", body = ErrorBody),
        (status = 401, description = "Missing or invalid session", body = ErrorBody),
        (status = 403, description = "Referenced card is outside the track", body = ErrorBody),
        (status = 404, description = "Track not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
// Intentionally no `Principal` extractor (see `list_track_files`).
pub(crate) async fn cat_track_file(
    State(s): State<RouteState>,
    Path(id): Path<String>,
    Query(q): Query<TrackFsCatQuery>,
) -> Result<Json<TrackFsContent>> {
    let path = q
        .path
        .as_deref()
        .ok_or_else(|| CalmError::BadRequest("calm.track.cat: missing `path` (string)".into()))?;
    let track = s
        .repo
        .track_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {id}")))?;
    // TODO(multi-user): ownership check
    let view = TrackFsView::new(s.repo.as_ref(), &s.write);
    let content = view.cat(&track, path).await?;
    Ok(Json(content))
}

#[utoipa::path(
    get,
    path = "/api/areas/{area_id}/tracks",
    tag = "tracks",
    params(("area_id" = String, Path, description = "Area id")),
    responses(
        (status = 200, description = "Tracks under area", body = Vec<Track>),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_tracks_by_area(
    State(s): State<RouteState>,
    Path(area_id): Path<String>,
) -> Result<Json<Vec<Track>>> {
    let mut tracks = s.repo.tracks_by_area(&area_id).await?;
    tracks.retain(user_visible_track);
    Ok(Json(tracks))
}

/// Public track lists hide retired Area-conversation containers; repository readers such
/// as area deletion and backlink resolution require the complete set.
fn user_visible_track(track: &Track) -> bool {
    match track.purpose.as_deref() {
        None => true,
        Some(purpose) => purpose != AREA_CHAT_PURPOSE,
    }
}

/// Build the initial report a template instantiates to. `Internal`, not `BadRequest`:
/// every byte comes from a roster file, so a failure is a kernel defect.
fn prepare_template_report(
    templates: &'static TemplateRoster,
    key: &str,
) -> Result<InitialReportSnapshot> {
    let template = templates
        .get(key)
        .ok_or_else(|| CalmError::Internal(format!("track create: unknown template `{key}`")))?;
    compile_template(template)
}

/// Compile one roster entry: recipe bytes in, validated report plus task declarations out.
pub(crate) fn compile_template(template: &Template) -> Result<InitialReportSnapshot> {
    prepare_initial_report_payload(template.key(), template.recipe())
}

/// The recipe-to-snapshot core, taking the payload rather than the key.
fn prepare_initial_report_payload(
    label: &str,
    payload: TrackReportPayload,
) -> Result<InitialReportSnapshot> {
    let mut doc = ReportDoc::from_payload(&payload);
    doc.ensure_blocks_layout(payload.blocks.as_deref())
        .map_err(|error| {
            CalmError::Internal(format!("track create: migrate template `{label}`: {error}"))
        })?;
    let mut blocks = doc.blocks_snapshot().map_err(|error| {
        CalmError::Internal(format!("track create: template `{label}` blocks: {error}"))
    })?;
    let (summary, body) = doc.project().map_err(|error| {
        CalmError::Internal(format!("track create: project template `{label}`: {error}"))
    })?;
    // `+++` opens a template file's front matter, never a report body; fail closed for a
    // stored recipe row that predates the write-boundary check.
    if body.starts_with("+++") {
        return Err(CalmError::BadRequest(format!(
            "track create: recipe or template `{label}` body must not start with `+++`; that \
             prefix is reserved for template files' front matter (#1635 D1)"
        )));
    }
    crate::track_report_guard::validate_body_fences(&body).map_err(|error| {
        CalmError::Internal(format!("track create: template `{label}` body: {error}"))
    })?;
    let template_context = crate::template_context::TemplateContext::new(
        payload.summary.clone(),
        payload.body.clone(),
    );
    // Named sources describe a working method, not queued work. Preserve
    // every non-task block, including the paragraph boundary a removed fence
    // supplied even when its neighbors had no blank lines.
    let has_tasks = blocks
        .iter()
        .any(|block| block.kind == calm_types::report_blocks::KIND_TASK);
    let body = if has_tasks {
        // Elision must not turn a misplaced source header into a valid one.
        calm_types::report_contract::check_document(&body).map_err(|error| {
            CalmError::Internal(format!(
                "track create: template `{label}` contract: {error}"
            ))
        })?;
        let mut body = String::new();
        let mut pending_break = false;
        for block in &blocks {
            if block.kind == calm_types::report_blocks::KIND_TASK {
                pending_break = true;
                continue;
            }
            if pending_break {
                super::track_recipes::restore_paragraph_break(&mut body);
                pending_break = false;
            }
            calm_types::report_blocks::append_block_text(
                &mut body,
                &calm_types::report_blocks::flat_text(block),
            );
        }
        body
    } else {
        body
    };
    let mut prepared = TrackReportPayload::new(summary, body);
    if has_tasks {
        doc = ReportDoc::from_payload(&prepared);
        doc.ensure_blocks_layout(None).map_err(|error| {
            CalmError::Internal(format!("track create: template `{label}` report: {error}"))
        })?;
        blocks = doc.blocks_snapshot().map_err(|error| {
            CalmError::Internal(format!(
                "track create: template `{label}` report blocks: {error}"
            ))
        })?;
        (prepared.summary, prepared.body) = doc.project().map_err(|error| {
            CalmError::Internal(format!(
                "track create: template `{label}` report projection: {error}"
            ))
        })?;
    }
    let (declarations, diagnostics) =
        calm_types::report_blocks::tasks::project_task_declarations(&blocks);
    prepared.blocks = Some(blocks);
    Ok(InitialReportSnapshot {
        payload: prepared,
        doc,
        declarations,
        diagnostics,
        template_context: Some(template_context),
    })
}

/// Calendar window query parameters for `GET /api/tracks`; `since` / `until` are
/// inclusive at both endpoints.
#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct TracksWindowQuery {
    /// Lower bound (inclusive) in unix milliseconds. Track is included
    /// when `terminal_at IS NULL OR terminal_at >= since`. Omitting
    /// disables the lower-bound filter.
    pub since: Option<i64>,
    /// Upper bound (inclusive) in unix milliseconds. Track is included
    /// when `created_at <= until`. Omitting disables the upper-bound
    /// filter.
    pub until: Option<i64>,
    /// Optional per-area filter. Mirrors `list_tracks_by_area` for
    /// callers that want one area's window in a single endpoint.
    pub area_id: Option<String>,
}

#[utoipa::path(
    get,
    path = "/api/tracks",
    tag = "tracks",
    params(TracksWindowQuery),
    responses(
        (status = 200, description = "Tracks overlapping the window, sorted by created_at", body = Vec<Track>),
        (status = 400, description = "Inverted window (since > until)", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_tracks_window(
    State(state): State<RouteState>,
    Query(q): Query<TracksWindowQuery>,
) -> Result<Json<Vec<Track>>> {
    if let (Some(since), Some(until)) = (q.since, q.until)
        && since > until
    {
        return Err(CalmError::BadRequest(format!(
            "window query: `since` ({since}) must be <= `until` ({until})"
        )));
    }
    let mut tracks = state
        .repo
        .tracks_window(q.area_id.as_deref(), q.since, q.until)
        .await?;
    tracks.retain(user_visible_track);
    Ok(Json(tracks))
}

#[utoipa::path(
    get,
    path = "/api/tracks/{id}",
    tag = "tracks",
    params(("id" = String, Path, description = "Track id")),
    responses(
        (status = 200, description = "Track detail (track + its cards + overlays)", body = TrackDetail),
        (status = 404, description = "Track not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn get_track_detail(
    State(s): State<RouteState>,
    Path(id): Path<String>,
) -> Result<Json<TrackDetail>> {
    let mut detail = s
        .repo
        .track_detail(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {id}")))?;
    // Mirror `list_overlays` so kernel-owned overlay rows with a `schemaVersion` past what
    // this binary supports never reach the frontend.
    detail.overlays = crate::routes::overlays::filter_unsupported_overlay_versions(detail.overlays);
    project_runtime_into_cards_payload(s.repo.as_ref(), &mut detail.cards).await?;
    Ok(Json(detail))
}

#[utoipa::path(
    post,
    path = "/api/tracks",
    tag = "tracks",
    params(
        ("Idempotency-Key" = Option<String>, Header, description = "**Required if the body carries `first_message`; optional otherwise, and honoured when sent.** A create that sends no key at all is not idempotent — a retry mints a second track — which is the unchanged behaviour of every caller that sends none.\n\nWithout `first_message`, a key binds the minted track and nothing else: an identical repeat returns that same track with 201, a different create shape under the same key is 409 `conflict`, and no retry slot is consumed (so this shape cannot exhaust a key). Adding or removing `first_message` under a key already bound by the other shape is itself a 409 `conflict`, in both directions.\n\nWith `first_message`, one transaction persists the minted track/card ids and a versioned fingerprint of the original create. The fingerprint covers every mint input (`title`, `sort`, the original `cwd`, `template_id`, `recipe_id`, `template_input`, `attach_folder`, `theme`, `fork_report_from`, and non-null `model` / `reasoning_effort`) plus the initial message digest, so it still constrains the key when no operation row exists. A different create shape is always 409 `conflict`, including after a terminal operation failure. A different `first_message` is also a conflict after success, `Stuck`, or a pre-operation failure; it may be edited only after a persisted terminal `Failed` attempt, where the fresh `#N` operation key represents a new delivery attempt against the same track.\n\nAn identical request after success returns the same track without re-delivery. A persisted terminal failure genuinely retries; a `Stuck` attempt keeps replaying its recorded 500; 64 failed attempts exhaust the key. Resume does not rerun mutable create-path validation, so repointing or deleting an attached workspace does not change request identity. Bindings created before request fingerprints existed fail closed with 409 because the server cannot prove request equality."),
    ),
    request_body = CreateTrackRequest,
    responses(
        (status = 201, description = "Track created. With `first_message`, the message is also queued for the planner agent inside the harness-start transaction; a retry under the same `Idempotency-Key` returns the same track without re-delivering it.", body = Track),
        (status = 400, description = "Malformed create (bad `cwd`, unknown `template_id`, invalid `template_input`, or `reasoning_effort` unsupported by the selected model's current catalog entry; no track is minted and no effort is silently adjusted), more than one of `template_id` / `recipe_id` / `fork_report_from` (each names a starting point; give at most one — naming none is the ordinary blank create), a malformed `Idempotency-Key` header (empty or non-ASCII) on any create, or — with `first_message` — a missing `Idempotency-Key` or an empty/over-long message. Decided before anything is minted; the multi-source refusal, like every other create-path check, is not re-run on an `Idempotency-Key` replay, which mints nothing.", body = ErrorBody),
        (status = 404, description = "Area not found", body = ErrorBody),
        (status = 409, description = "Folder-claim conflict (structured `FolderConflict` body), `conflict` when an `Idempotency-Key` is bound to a different create or to a legacy binding whose request cannot be proven, or `idempotency_key_exhausted` when the key used all 64 retry slots, when the track it names has been deleted, or when its managed workspace can no longer be materialized. Recovery depends on `code`: fix a folder conflict and retry the same key; preserve the original request for a payload conflict (use a new key only for an explicit new create); use a new key after `idempotency_key_exhausted`.", body = ErrorBody),
        (status = 500, description = "Internal error. One case leaves the track behind: when the request carried a `first_message` and the planner harness start did not complete, the track, its cards and its workspace are already committed, and whether the message reached the agent is **unknown to the server** — depending on how far the start got, it may never have been handed over, or it may already have been delivered and answered. Nothing is rolled back and nothing compensates. What the server *can* promise, and this is what the `Idempotency-Key` buys: retrying the identical request under the **same** key creates no second track and delivers no second copy of the message. It does not promise the track is usable — a replay does not repair an attached workspace whose directory was deleted. Without `first_message` the same harness failure is logged and still returns 201, because no user text was riding on it — which also holds when such a create sends an `Idempotency-Key` and is answered from its binding.", body = ErrorBody),
    ),
)]
#[allow(deprecated)]
pub(crate) async fn create_track(
    State(s): State<RouteState>,
    actor: Actor,
    headers: HeaderMap,
    State(codex): State<CodexShellState>,
    Json(mut request): Json<CreateTrackRequest>,
) -> Result<Response> {
    // Common area lifecycle fence for legacy mint, first-message mint, and
    // idempotent replay. Holding it through workspace materialization and
    // operation submission makes area deletion snapshot a closed member set.
    let create_area_id = request.area_id.clone();
    let _area_delete_guard =
        crate::per_card_lock::lock_key(&s.area_delete_locks, create_area_id.as_str()).await;
    // First, before every other check: a rejected first message must leave no track,
    // no cards, no folder claim and no materialized workspace behind.
    let plan = create::plan_first_message(
        &s,
        &headers,
        request.first_message.take(),
        request.area_id.as_str(),
        // The caller's raw strings, cloned here before `into_parts()` moves them; the binding
        // must not depend on `admit_template`, so a replay whose template left the roster still replays.
        create::CreateRequestShape {
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
        },
    )
    .await?;
    // The arm decision comes BEFORE the create path's request validation: the resuming arms
    // mint nothing, and re-running checks against mutable state would fail a byte-identical replay.
    // `message_less` carries the per-key claim guard that must be held until after the mint.
    let (plan, message_less) = match plan {
        create::CreatePlan::Resume(resume) => {
            return create::resume_prior_attempt(s, actor, resume).await;
        }
        create::CreatePlan::MessageLessResume(resume) => {
            return create::resume_message_less(s, actor, resume).await;
        }
        create::CreatePlan::Legacy => (None, None),
        create::CreatePlan::Mint(plan) => (Some(plan), None),
        create::CreatePlan::MessageLessMint(plan) => (None, Some(plan)),
    };
    let allow_cross_area_cwd = request.allow_cross_area_cwd.clone();
    // Resolve mutable catalog advice only for a new mint, never on replay.
    let model = request.model.take();
    let reasoning_effort = request.reasoning_effort.take();
    let advice =
        super::planner_model::catalog_advice(&codex, model.as_deref(), reasoning_effort.as_deref())
            .await;
    if let (Some(default), Some(model), Some(effort)) = (
        advice.adjusted_to,
        model.as_deref(),
        reasoning_effort.as_deref(),
    ) {
        return Err(CalmError::BadRequest(format!(
            "track create: reasoning_effort `{effort}` is unsupported for model `{model}`; \
             its current catalog default is `{default}`. Refresh the model list and choose a supported effort."
        )));
    }
    let (mut p, named_source, cwd_omitted) = request.into_parts()?;

    // Validate cwd before opening the tx; the route owns every cross-area check so
    // `track_create_tx` stays a mechanical row insert. `materialize_workspace` runs after the
    // commit, so "non-201 ⇒ no side effect" is not a property of this handler.

    // The request's named source becomes the admitted source here, and only here.
    let source = named_source.resolve(&s).await?;
    // `template_input` is only accepted against a bound template whose owning plugin
    // Manifest declares an `input_schema`.
    validate_template_input_binding(source.template_input_owner(), p.template_input.as_ref())?;
    // Both provenance columns, from the one value that also decides the init.
    source.stamp(&mut p);

    // Omitted cwd stores `default_cwd()` and skips the claim scan. Never claim `$HOME` —
    // longest-prefix would poison every other area.
    if !cwd_omitted && !p.cwd.starts_with('/') {
        return Err(CalmError::BadRequest(format!(
            "track create: `cwd` must be absolute (start with `/`); got `{}`",
            p.cwd
        )));
    }
    let normalized_cwd = normalize_path(&p.cwd);
    // Validated before the transaction, deliberately: `materialize_workspace` runs after
    // the commit, and a failure there leaves an orphan track row.
    if !cwd_omitted {
        crate::workspace_materialize::validate_attached_workspace(std::path::Path::new(
            &normalized_cwd,
        ))?;
    }
    // Stamp the normalized cwd back before the row is minted; `area_folder.path` is the
    // normalized form too.
    p.cwd = normalized_cwd.clone();

    // The system area is exempt from the `area_folders` claim namespace: claiming a path
    // under it would poison every real area's descendant check.
    let area = s
        .repo
        .area_get(p.area_id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("area `{}`", p.area_id)))?;
    let is_system_area = area.kind == AreaKind::System;
    if allow_cross_area_cwd.is_some() && (is_system_area || cwd_omitted) {
        return Err(CalmError::BadRequest(
            "cross-area cwd authorization requires an explicit cwd in a user area".into(),
        ));
    }
    if is_system_area {
        p.attach_folder = false;
    }

    let attach_folder = p.attach_folder;
    let body_area_id = p.area_id.as_str().to_string();

    // The cwd-vs-claim decision runs inside the track-create transaction: a pre-tx scan let
    // two concurrent creates for `/a` and `/a/b` commit overlapping claims.
    let conflict = FolderConflictSlot::default();
    let folder_claim = if is_system_area || cwd_omitted {
        FolderClaim::Skip
    } else {
        FolderClaim::Enforce {
            attach: attach_folder,
            allow_cross_area_cwd,
            conflict: conflict.clone(),
        }
    };

    let init = source.init;

    let workspace_root = s.workspace_root.clone();
    let options = CreateTrackOptions {
        model,
        reasoning_effort,
        folder_claim,
        body_area_id,
        normalized_cwd,
        init,
        // Omitted `cwd` is the managed-default branch (server picks the directory); an
        // explicit `cwd` is the attached branch.
        workspace_plan: if cwd_omitted {
            TrackWorkspacePlan::ManagedUnder(workspace_root)
        } else {
            TrackWorkspacePlan::AttachedFromCwd
        },
        // Conditioned on the plan: `Mint` sets it later inside `create_track_with_first_message`;
        // `Legacy` has nothing to bind.
        idempotency_claim: message_less.as_ref().map(create::MessageLessPlan::claim),
    };
    // The resuming arms returned above, so `Some` here is always a mint.
    let created = match plan {
        None => create_track_with_planner_harness(s, actor, p, options).await,
        Some(plan) => create::create_track_with_first_message(s, actor, p, options, plan).await,
    };
    // The per-key claim guard rides here so two same-key message-less creates in
    // one process cannot both read "no binding" and each mint a track. Dropped
    // after the mint settles, exactly as the `first_message` arms drop theirs.
    drop(message_less);
    match created {
        Err(error) => match conflict.take() {
            Some(body) => Ok((StatusCode::CONFLICT, Json(body)).into_response()),
            None => Err(error),
        },
        ok => ok,
    }
}

/// The single answer to "may this id create a track", plus the optional plugin binding
/// that comes with it.
pub(crate) struct TemplateAdmission {
    /// The admitted roster entry itself — not a key copied off it.
    template: &'static Template,
    /// The owning plugin, when a running trusted one claims this id. `None` is
    /// an ordinary template, not a rejection.
    pub binding: Option<Manifest>,
}

impl TemplateAdmission {
    /// The roster's own `&'static` key, not the caller's string; all three consumers of an
    /// admitted id read it.
    pub(crate) fn key(&self) -> &'static str {
        self.template.key()
    }
}

/// Admit a caller-supplied `template_id`. Roster membership is the only admission test;
/// there is deliberately no fallback for a plugin declaring an id the roster lacks. The
/// binding is resolved from the admitted roster entry, not from `id`.
pub(crate) async fn admit_template(s: &RouteState, id: &str) -> Option<TemplateAdmission> {
    let template = s.templates.get(id)?;
    Some(TemplateAdmission {
        template,
        binding: resolve_template_binding(s, template).await,
    })
}

/// Resolve an admitted roster [`Template`] to the owning plugin Manifest iff a running
/// trusted plugin registers its key; `None` covers stopped and untrusted alike.
pub(crate) async fn resolve_template_binding(
    s: &RouteState,
    template: &'static Template,
) -> Option<Manifest> {
    let running_plugin_ids = s.plugin.running_plugin_ids().await;
    s.plugin.registry().list().into_iter().find(|manifest| {
        crate::track_binding::plugin_is_eligible_owner(&running_plugin_ids, &manifest.id)
            && manifest
                .templates
                .iter()
                .any(|descriptor| descriptor.id == template.key())
    })
}

/// Adds only the route's error vocabulary to the shared `template_input` validation.
fn validate_template_input_binding(
    owner: crate::plugin_host::template_input::TemplateInputOwner<'_>,
    input: Option<&serde_json::Value>,
) -> Result<()> {
    crate::plugin_host::template_input::validate_template_input_binding(owner, input)
        .map_err(|reason| CalmError::BadRequest(format!("track create: {reason}")))
}

/// The cwd claim scan runs inside the track-create transaction, so its structured 409 has
/// to travel back out through `Err`; the closure parks the body here.
/// `Mutex` is only ever locked between `await` points.
#[derive(Clone, Default)]
struct FolderConflictSlot(std::sync::Arc<std::sync::Mutex<Option<FolderConflict>>>);

impl FolderConflictSlot {
    /// Park `body` and return the error that unwinds (and rolls back)
    /// the transaction. The message is a fallback only: the route reads
    /// the slot first and never surfaces this string.
    fn park(&self, body: FolderConflict) -> CalmError {
        let message = format!(
            "track create: cwd conflicts with folder claim `{}` (area `{}`)",
            body.conflict_path, body.area_id
        );
        *self.0.lock().expect("folder conflict slot poisoned") = Some(body);
        CalmError::Conflict(message)
    }

    fn take(&self) -> Option<FolderConflict> {
        self.0.lock().expect("folder conflict slot poisoned").take()
    }
}

/// What the track-create transaction does about `area_folders`.
#[derive(Clone)]
enum FolderClaim {
    /// Don't scan, don't insert. The system area is exempt from the claim
    /// namespace entirely.
    Skip,
    /// Scan inside the track tx (`BEGIN IMMEDIATE`, so scan and insert are atomic against a
    /// concurrent claim); `attach` mints the claim when nothing covers the cwd.
    Enforce {
        attach: bool,
        allow_cross_area_cwd: Option<CrossAreaCwdAuthorization>,
        conflict: FolderConflictSlot,
    },
}

/// Which route is asking, purely so the refusal message names it. The RULES do
/// not vary — that is the point of there being one function.
#[derive(Clone, Copy)]
enum FolderClaimIntent {
    Create,
    /// `PATCH /api/tracks/{id}` pointing a track at an existing repository.
    Repoint,
}

impl FolderClaimIntent {
    fn label(self) -> &'static str {
        match self {
            FolderClaimIntent::Create => "track create",
            FolderClaimIntent::Repoint => "track workspace",
        }
    }
}

/// Whether this pass may mint an `area_folders` row. The re-point runs the claim rules
/// twice and the first pass's transaction commits, so a claim minted there would survive
/// a later refusal.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FolderClaimPass {
    /// Report the same conflicts, write nothing. Fail-fast only.
    ScanOnly,
    /// Report conflicts AND mint the claim. Must share a transaction with the
    /// write it authorises.
    Authoritative,
}

/// The claim rules, in one place. Must run first in its transaction: every branch either
/// rolls back or leaves the claim table consistent for the write that follows.
async fn enforce_folder_claim_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    claim: &FolderClaim,
    area_id: &str,
    normalized_cwd: &str,
    intent: FolderClaimIntent,
    pass: FolderClaimPass,
) -> Result<()> {
    let FolderClaim::Enforce {
        attach,
        allow_cross_area_cwd,
        conflict,
    } = claim
    else {
        return Ok(());
    };
    let existing = area_folders_list_all_tx(tx).await?;
    match find_owner(&existing, normalized_cwd) {
        // Some other area already covers this cwd. `Descendant` is the right
        // label from the cwd's point of view: the cwd is a descendant of an
        // existing folder owned by another area.
        Some(f)
            if f.area_id.as_str() != area_id
                && allow_cross_area_cwd.as_ref().is_some_and(|authorization| {
                    authorization.folder_id == f.id && authorization.area_id == f.area_id
                }) =>
        {
            Ok(())
        }
        Some(f) if f.area_id.as_str() != area_id || allow_cross_area_cwd.is_some() => Err(conflict
            .park(FolderConflict {
                folder_id: f.id,
                area_id: f.area_id.clone(),
                conflict_path: f.path.clone(),
                conflict_kind: FolderConflictKind::Descendant,
            })),
        // Same area already covers it — `attach_folder` is a no-op; falling through to the
        // insert would mint an overlapping row.
        Some(_) => Ok(()),
        None if *attach || allow_cross_area_cwd.is_some() => {
            // Check the reverse overlap first: an existing folder that is a descendant of the
            // proposed cwd (`/a/b` exists, claim `/a`).
            if let Some(f) = existing
                .iter()
                .find(|f| is_descendant_of(normalized_cwd, &f.path))
            {
                return Err(conflict.park(FolderConflict {
                    folder_id: f.id,
                    area_id: f.area_id.clone(),
                    conflict_path: f.path.clone(),
                    conflict_kind: FolderConflictKind::Ancestor,
                }));
            }
            // A reuse confirmation is bound to the original claim. Its
            // disappearance must not turn consent into a new attachment.
            if allow_cross_area_cwd.is_some() {
                return Err(CalmError::BadRequest(
                    "track create: the authorized folder claim no longer covers this cwd".into(),
                ));
            }
            if pass == FolderClaimPass::Authoritative {
                area_folder_create_tx(tx, area_id, normalized_cwd).await?;
            }
            Ok(())
        }
        // Nothing covers the cwd and the caller didn't opt in to attach.
        // Refuse so accidentally typing a stray path doesn't create a
        // "homeless" track.
        None => Err(CalmError::Conflict(format!(
            "{}: cwd `{normalized_cwd}` is not claimed by any area. Set \
             `attach_folder: true` to claim it for area `{area_id}`.",
            intent.label()
        ))),
    }
}

/// Where a new track's report comes from.
enum TrackInit {
    /// No report content; the track keeps the default skeleton.
    Blank,
    /// Instantiate a template recipe; the key is the roster's own, never the caller's string.
    Template {
        key: &'static str,
        /// The owning plugin at creation time, when the admitted template is bound to a running
        /// and trusted one. Lives in this arm so a non-`Template` source has nowhere to put an
        /// owner. Boxed: a `Manifest` is ~864 bytes (`clippy::large_enum_variant`).
        binding: Option<Box<Manifest>>,
    },
    /// Instantiate a user-defined recipe (`track_recipes` row). Distinct from `Template`
    /// because this row may have been deleted between the picker's read and the create.
    Recipe { recipe_id: String },
    /// Copy an existing track's report.
    Fork { source_track_id: String },
}

struct CreateTrackOptions {
    model: Option<String>,
    reasoning_effort: Option<String>,
    folder_claim: FolderClaim,
    body_area_id: String,
    normalized_cwd: String,
    init: TrackInit,
    /// Managed (server allocates under the workspace root) vs attached (the caller pointed
    /// at an existing directory).
    workspace_plan: TrackWorkspacePlan,
    /// The caller's key plus the request identity that makes binding it safe; `None` when
    /// the caller sent no `Idempotency-Key`.
    idempotency_claim: Option<TrackCreateIdempotencyClaim>,
}

pub(super) struct TrackCreateIdempotencyClaim {
    pub(super) key: String,
    pub(super) create_request_sha256: String,
    /// `None` on the message-less arm. It selects fingerprint version 2 in the binding row,
    /// which lets the two create shapes refuse each other's keys.
    pub(super) first_message_sha256: Option<String>,
}

#[allow(deprecated)]
/// The message-less create, end to end. A harness start failure is a `warn!` + 201.
async fn create_track_with_planner_harness(
    s: RouteState,
    actor: Actor,
    p: NewTrack,
    options: CreateTrackOptions,
) -> Result<Response> {
    let area_id = options.body_area_id.clone();
    if s.repo.area_get(&area_id).await?.is_none() {
        return Err(CalmError::NotFound(format!("area {area_id}")));
    }
    let (track, _, planner_card_id, report_card_id) =
        create_track_structure(s.clone(), actor.clone(), p, options).await?;
    start_planner_harness(&s, &actor, &track, planner_card_id, report_card_id).await?;
    Ok((StatusCode::CREATED, Json(track)).into_response())
}

#[allow(deprecated)]
async fn create_track_structure(
    s: RouteState,
    actor: Actor,
    p: NewTrack,
    options: CreateTrackOptions,
) -> Result<(Track, bool, String, String)> {
    let CreateTrackOptions {
        model,
        reasoning_effort,
        folder_claim,
        body_area_id,
        normalized_cwd,
        init,
        workspace_plan,
        idempotency_claim,
    } = options;
    let workspace_root_for_materialize = s.workspace_root.clone();
    let templates = s.templates;
    let planner_card_id = new_id();
    let report_card_id = new_id();
    let actor_id = actor.to_actor_id();
    let actor_id_for_tx = actor_id.clone();
    let write_for_tx = s.write.clone();
    let planner_card_id_for_tx = planner_card_id.clone();
    let report_card_id_for_tx = report_card_id.clone();
    let area_id_for_attach = body_area_id;
    let normalized_cwd_for_tx = normalized_cwd;
    let idempotency_claim_for_tx = idempotency_claim;
    // The fork path deliberately derives no `EditAuthor`: the fork's normalization and
    // guard are author-independent, so nothing here may classify the caller.
    let ((track, created), _event_ids) = write_with_actor_events_typed(
        s.repo.as_ref(),
        None,
        &s.events,
        &s.write,
        move |tx| {
            Box::pin(async move {
                // Claim scan + insert, atomic with the track row; must stay first so every branch
                // either rolls back or leaves the claim table consistent.
                enforce_folder_claim_tx(
                    tx,
                    &folder_claim,
                    &area_id_for_attach,
                    &normalized_cwd_for_tx,
                    FolderClaimIntent::Create,
                    FolderClaimPass::Authoritative,
                )
                .await?;

                // The recipe is read inside the transaction, once, before the INSERT: its `revision` is
                // stamped onto the row, and reading twice would let a concurrent edit split the
                // recorded revision from the report.
                let recipe_source = match &init {
                    TrackInit::Recipe { recipe_id } => {
                        Some(track_recipe_get_tx(tx, recipe_id).await?.ok_or_else(|| {
                            CalmError::BadRequest(format!(
                                "track create: recipe `{recipe_id}` does not exist"
                            ))
                        })?)
                    }
                    _ => None,
                };
                let recipe_origin = recipe_source.as_ref().map(|recipe| TrackRecipeOrigin {
                    recipe_id: recipe.id.clone(),
                    revision: recipe.revision,
                });

                let track = track_create_tx(
                    tx,
                    p,
                    None,
                    &workspace_plan,
                    recipe_origin.as_ref(),
                    write_for_tx.area_cache(),
                )
                .await?;
                let track_id = track.id.clone();
                let area_id = track.area_id.clone();

                // The `Idempotency-Key` → track binding is written in the same transaction that mints
                // the id; the `operations` row cannot carry it (written after validation on a pooled connection).
                if let Some(claim) = idempotency_claim_for_tx.as_ref() {
                    track_create_idempotency_claim_tx(
                        tx,
                        area_id.as_str(),
                        &claim.key,
                        &calm_truth::db::sqlite::TrackCreateBindingClaim {
                            track_id: track_id.to_string(),
                            planner_card_id: planner_card_id_for_tx.clone(),
                            report_card_id: report_card_id_for_tx.clone(),
                            create_request_sha256: claim.create_request_sha256.clone(),
                            first_message_sha256: claim.first_message_sha256.clone(),
                        },
                    )
                    .await
                    .map_err(|error| {
                        // Fail closed on the primary-key violation: this is the cross-instance racer, and its
                        // retry resolves to `Resume`. Not recovered in place because this transaction already
                        // wrote a track row it must not keep.
                        CalmError::Internal(format!(
                            "track create: this Idempotency-Key was claimed by a concurrent \
                             create; retry, and the retry will resolve to the track that won \
                             ({error})"
                        ))
                    })?;
                }

                // Matched on `(&init, recipe_source)` as one value so the `Recipe` arm binds its
                // recipe by pattern.
                let init_snapshot = match (&init, recipe_source) {
                    (TrackInit::Blank, _) => None,
                    (TrackInit::Template { key, .. }, _) => {
                        Some(prepare_template_report(templates, key)?)
                    }
                    (TrackInit::Recipe { recipe_id }, None) => {
                        // Not a caller error, so not a 400.
                        return Err(CalmError::Internal(format!(
                            "track create: recipe `{recipe_id}` was resolved to a Recipe init \
                             without the recipe row the same `init` was supposed to read"
                        )));
                    }
                    (TrackInit::Recipe { recipe_id }, Some(recipe)) => {
                        // The stored body is already normalized by the write boundary; nothing is re-normalized here.
                        Some(prepare_initial_report_payload(
                            recipe_id,
                            TrackReportPayload::new(recipe.title, recipe.body),
                        )?)
                    }
                    (TrackInit::Fork { source_track_id }, _) => {
                    // A fork records no recipe provenance, even when the source was recipe-born: a fork was
                    // instantiated from a track, not from a recipe.
                    let source_track_id = source_track_id.as_str();
                    let source_id = TrackId::from(source_track_id.to_string());
                    let source_track = track_get_tx(tx, &source_id).await.map_err(|error| {
                        if matches!(error, CalmError::NotFound(_)) {
                            CalmError::BadRequest(format!(
                                "track create: fork source track `{source_track_id}` does not exist"
                            ))
                        } else {
                            error
                        }
                    })?;
                    let source_area_kind: String =
                        sqlx::query_scalar("SELECT kind FROM areas WHERE id=?1")
                            .bind(source_track.area_id.as_str())
                            .fetch_one(&mut **tx)
                            .await?;
                    if source_track.area_id != area_id
                        && source_area_kind != AreaKind::System.as_db_str()
                    {
                        return Err(CalmError::BadRequest(format!(
                            "track create: fork source track `{source_track_id}` must be in the target area or the system area"
                        )));
                    }
                    let (summary, blocks) =
                        report_blocks_snapshot_tx(tx, source_track_id).await?;
                    // The resolved `chart.series` rows travel with the report (block ids survive the fork).
                    // Pinned rows stay immutable in the child; unpinned rows refresh per track from here on.
                    crate::report_series::store::copy_rows_tx(
                        tx,
                        source_track_id,
                        track_id.as_str(),
                    )
                    .await?;
                    // The captured sources travel too, verbatim, so the child resolves them independently
                    // of the parent's lifetime.
                    crate::report_sources::store::copy_rows_tx(
                        tx,
                        source_track_id,
                        track_id.as_str(),
                    )
                    .await?;
                    Some(prepare_fork_report(
                        summary,
                        blocks,
                        source_track_id,
                        track_id.as_str(),
                    )?)
                    }
                };

                let mut planner_payload = planner_harness_card_payload(None);
                if let Some(context) = init_snapshot.as_ref().and_then(|snapshot| snapshot.template_context.as_ref()) {
                    planner_payload[crate::validation::PLANNER_TEMPLATE_CONTEXT_PAYLOAD_KEY] = serde_json::to_value(context)?;
                }
                if model.is_some() || reasoning_effort.is_some() {
                    crate::planner_model::CardModelSelection::apply_to_payload(
                        planner_payload.as_object_mut().ok_or_else(|| {
                            CalmError::Internal("planner payload is not an object".into())
                        })?,
                        model.as_deref(),
                        reasoning_effort.as_deref(),
                    );
                }
                let planner_card = card_create_with_id_tx(
                    tx,
                    planner_card_id_for_tx.clone(),
                    NewCard {
                        title: None,
                        track_id: track_id.clone(),
                        kind: "codex".into(),
                        sort: None,
                        // Create seeds no `prompt` here; the parameter stays because child tracks pass the
                        // task goal their parent planner declared.
                        payload: planner_payload,
                    },
                    CardRole::Planner,
                    false,
                    write_for_tx.role_cache(),
                )
                .await?;

                let report_payload =
                    serde_json::to_value(TrackReportPayload::initial()).map_err(|e| {
                        CalmError::Internal(format!(
                            "track_create: serialize track-report payload: {e}"
                        ))
                    })?;
                let mut report_card = card_create_with_id_tx(
                    tx,
                    report_card_id_for_tx.clone(),
                    NewCard {
                        title: None,
                        track_id: track_id.clone(),
                        kind: "track-report".into(),
                        sort: Some(-1.0),
                        payload: report_payload,
                    },
                    CardRole::ReportCard,
                    false,
                    write_for_tx.role_cache(),
                )
                .await?;

                let mut init_projection = None;
                if let Some(InitialReportSnapshot {
                    payload,
                    mut doc,
                    declarations,
                    diagnostics,
                    template_context: _,
                }) = init_snapshot
                {
                    // The structural door takes no author, actor, event bus or CAS input, so this closure
                    // can neither emit `track.report_edited` nor reach `guard_task_declarations`.
                    let (persisted_report, projection) =
                        crate::track_report::write::structural_init_report_tx(
                            tx,
                            crate::track_report::write::InitialReportTarget {
                                report_card_id: report_card.id.as_str(),
                                track_id: track_id.as_str(),
                                payload: &payload,
                                doc: &mut doc,
                                declarations: &declarations,
                                diagnostics: &diagnostics,
                            },
                        )
                        .await?;
                    report_card = persisted_report;
                    init_projection = Some(projection);
                }

                let track_scope = EventScope::Track {
                    track: track_id.clone(),
                    area: area_id.clone(),
                };
                let planner_card_scope = EventScope::Card {
                    card: planner_card.id.clone(),
                    track: track_id.clone(),
                    area: area_id.clone(),
                };
                let report_card_scope = EventScope::Card {
                    card: report_card.id.clone(),
                    track: track_id.clone(),
                    area: area_id.clone(),
                };
                let layout_overlay = overlay_upsert_tx(
                    tx,
                    NewOverlay {
                        plugin_id: "kernel".into(),
                        entity_kind: "view".into(),
                        entity_id: track_id.as_str().to_string(),
                        kind: "layout".into(),
                        payload: planner_harness_layout_payload(
                            planner_card.id.as_str(),
                            report_card.id.as_str(),
                        ),
                    },
                )
                .await?;
                let mut events = vec![
                    (
                        actor_id_for_tx.clone(),
                        track_scope.clone(),
                        Event::TrackUpdated(crate::event::TrackUpdatedPayload::new(
                            track.clone(),
                            None,
                        )),
                    ),
                    (
                        actor_id_for_tx.clone(),
                        planner_card_scope,
                        Event::CardAdded(planner_card),
                    ),
                    (
                        actor_id_for_tx.clone(),
                        report_card_scope,
                        Event::CardAdded(report_card),
                    ),
                    (
                        actor_id_for_tx.clone(),
                        track_scope,
                        Event::OverlaySet(layout_overlay),
                    ),
                ];
                if let Some(projection) = init_projection {
                    if !projection.changed_keys.is_empty() {
                        events.push((
                            actor_id_for_tx.clone(),
                            EventScope::Track {
                                track: track_id.clone(),
                                area: area_id.clone(),
                            },
                            Event::PlanUpdated {
                                track_id,
                                changed_keys: projection.changed_keys,
                                agent_message: None,
                            },
                        ));
                    }
                    events.extend(projection.kernel_events);
                }
                Ok(((track, true), events))
            })
        },
    )
    .await?;

    // Materialize outside the transaction and before the planner harness starts. A failure
    // here MUST surface as a non-2xx; a 201 would leave a track whose first worker dies
    // with `spawn-failed`.
    crate::workspace_materialize::materialize_workspace(
        &track.workspace,
        &workspace_root_for_materialize,
        track.id.as_str(),
    )
    .map_err(|error| {
        tracing::error!(
            track_id = %track.id,
            path = %track.workspace.path,
            error = %error,
            "track create: workspace materialization failed"
        );
        error
    })?;

    Ok((track, created, planner_card_id, report_card_id))
}

/// Start the planner harness for a create that carried no `first_message`. Best-effort:
/// a failed start is a `warn!` and the create still answers 201.
async fn start_planner_harness(
    s: &RouteState,
    actor: &Actor,
    track: &Track,
    planner_card_id: String,
    report_card_id: String,
) -> Result<()> {
    // No goal is seeded on this user-driven create path; child tracks do NOT come through here.
    let request = PlannerHarnessStartOperationPayload {
        actor: actor.to_actor_id(),
        track_id: track.id.to_string(),
        planner_card_id: CardId::from(planner_card_id.clone()),
        report_card_id: Some(report_card_id),
        sort: None,
        cwd: track.workspace.path.clone(),
        goal: None,
        reset_harness_items: false,
        force_new_thread: false,
        profile: Default::default(),
        create_card: None,
        // `None` is `skip_serializing_if`-dropped, so a message-less create writes
        // byte-identical payload JSON and `payload_hash`.
        first_message: None,
        create_request_sha256: None,
        // Not a conversation create; nothing to brief.
        opening_briefing: None,
    };
    let op_payload = serde_json::to_value(&request)?;
    let payload_hash = stable_payload_hash(&serde_json::json!({
        "actor": actor.as_str(),
        "request": &request,
    }))?;
    match s
        .operation_runtime
        .submit(
            "planner-harness-start",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: None,
                payload_hash,
            },
            op_payload,
        )
        .await
    {
        Ok(op_id) => match s.operation_runtime.wait(&op_id).await {
            Ok(result) => match result.outcome {
                // `SucceededViaCollision` is unreachable here: this path submits `idempotency_key: None`,
                // and nothing in this repository writes the `idempotency_collision` completion that produces it.
                OperationOutcome::Succeeded { .. }
                | OperationOutcome::SucceededViaCollision { .. } => {}
                OperationOutcome::Failed {
                    last_error,
                    from_phase,
                    ..
                } => {
                    tracing::warn!(
                        planner_card_id,
                        track_id = %track.id,
                        ?from_phase,
                        error = %last_error,
                        "planner harness start operation failed; track created but planner agent is inert"
                    );
                }
                OperationOutcome::Stuck { reason, from_phase } => {
                    tracing::warn!(
                        planner_card_id,
                        track_id = %track.id,
                        ?from_phase,
                        reason,
                        "planner harness start operation stuck; track created but planner agent is inert"
                    );
                }
            },
            Err(e) => {
                tracing::warn!(
                    planner_card_id,
                    track_id = %track.id,
                    error = %e,
                    "planner harness start wait failed; track created but planner agent may be inert"
                );
            }
        },
        Err(e) => {
            tracing::warn!(
                planner_card_id,
                track_id = %track.id,
                error = %e,
                "planner harness start submission failed; track created but planner agent is inert"
            );
        }
    }

    Ok(())
}

/// The compiled starting report a create instantiates, before it is persisted.
pub(crate) struct InitialReportSnapshot {
    payload: TrackReportPayload,
    doc: ReportDoc,
    declarations: Vec<calm_types::report_blocks::tasks::TaskDeclaration>,
    diagnostics: Vec<Vec<calm_types::report_blocks::tasks::Diagnostic>>,
    template_context: Option<crate::template_context::TemplateContext>,
}

impl InitialReportSnapshot {
    /// The compiled `task` blocks' payloads, in document order. `None` blocks is `Internal`,
    /// not an empty list: both construction sites set them.
    pub(super) fn task_block_payloads(&self) -> Result<Vec<&serde_json::Value>> {
        let blocks = self.payload.blocks.as_ref().ok_or_else(|| {
            CalmError::Internal(
                "compiled initial report carries no blocks snapshot to project".to_string(),
            )
        })?;
        Ok(blocks
            .iter()
            .filter(|block| block.kind == calm_types::report_blocks::KIND_TASK)
            .map(|block| &block.payload)
            .collect())
    }

    /// The compiled (projected) body — the exact text the persist funnel runs `check_document` over.
    pub(crate) fn body(&self) -> &str {
        &self.payload.body
    }
}

fn prepare_fork_report(
    summary: String,
    mut blocks: Vec<ReportBlock>,
    source_track_id: &str,
    target_track_id: &str,
) -> Result<InitialReportSnapshot> {
    use std::collections::HashSet;

    use calm_types::report_blocks::{KIND_PROSE, KIND_TASK, flat_text, validate_payload};
    use calm_types::report_links::{
        UnsafeTrackLink, rewrite_track_destination, rewrite_track_links,
    };

    let copied_block_ids: HashSet<String> = blocks.iter().map(|block| block.id.clone()).collect();
    let mut unsafe_links: Vec<(String, &'static str, UnsafeTrackLink)> = Vec::new();
    for block in &mut blocks {
        let block_id = block.id.clone();
        if block.kind == KIND_PROSE {
            if let Some(markdown) = block.payload.get_mut("markdown")
                && let Some(source) = markdown.as_str()
            {
                match rewrite_track_links(
                    source,
                    source_track_id,
                    target_track_id,
                    &copied_block_ids,
                ) {
                    Ok(rewritten) => *markdown = serde_json::Value::String(rewritten),
                    Err(errors) => unsafe_links.extend(
                        errors
                            .into_iter()
                            .map(|error| (block_id.clone(), "markdown", error)),
                    ),
                }
            }
            // This arm `continue`s past the `validate_payload` call at the bottom of the loop, so
            // the prose fences are checked here. Deliberately only the fence check: refusing
            // well-formed fences too would reject already-persisted source tracks.
            if let Some(markdown) = block.payload.get("markdown").and_then(|v| v.as_str()) {
                crate::track_report_guard::validate_body_fences(markdown).map_err(|error| {
                    CalmError::BadRequest(format!(
                        "track create: invalid forked report block {block_id}: {error}"
                    ))
                })?;
            }
            continue;
        }

        if block.kind == KIND_TASK
            && let Some(payload) = block.payload.as_object_mut()
        {
            for field in ["goal", "command", "acceptance"] {
                if let Some(value) = payload.get_mut(field)
                    && let Some(source) = value.as_str()
                {
                    match rewrite_track_links(
                        source,
                        source_track_id,
                        target_track_id,
                        &copied_block_ids,
                    ) {
                        Ok(rewritten) => *value = serde_json::Value::String(rewritten),
                        Err(errors) => unsafe_links.extend(
                            errors
                                .into_iter()
                                .map(|error| (block_id.clone(), field, error)),
                        ),
                    }
                }
            }
            if let Some(serde_json::Value::Array(references)) = payload.get_mut("refs") {
                for reference in references {
                    if let Some(source) = reference.as_str() {
                        *reference = serde_json::Value::String(rewrite_track_destination(
                            source,
                            source_track_id,
                            target_track_id,
                            &copied_block_ids,
                        ));
                    }
                }
            }
            crate::task_privilege::normalize_task_privilege_fields(payload);
        }

        validate_payload(&block.kind, &block.payload).map_err(|error| {
            CalmError::BadRequest(format!(
                "track create: invalid forked report block {}: {error}",
                block.id
            ))
        })?;
    }

    if !unsafe_links.is_empty() {
        let details = unsafe_links
            .into_iter()
            .map(|(block_id, field, link)| {
                format!(
                    "- block {block_id} field {field}: destination source `{}` (decoded `{}`)",
                    link.source, link.decoded_destination
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Err(CalmError::BadRequest(format!(
            "track create: cannot safely rewrite fork link destinations:\n{details}\n\
             Write each link target in plain form (without character entities or backslash escapes, and without inline HTML in its label) and retry."
        )));
    }

    guard_forked_blocks(&blocks)?;
    let doc = ReportDoc::from_blocks_exact(&summary, &blocks).map_err(|error| {
        CalmError::BadRequest(format!(
            "track create: invalid fork report snapshot: {error}"
        ))
    })?;
    let (summary, body) = doc.project().map_err(|error| {
        CalmError::Internal(format!("track create: project fork report CRDT: {error}"))
    })?;
    let (declarations, diagnostics) =
        calm_types::report_blocks::tasks::project_task_declarations(&blocks);
    let mut payload = TrackReportPayload::new(summary, body);
    payload.blocks = Some(blocks);
    debug_assert_eq!(
        payload.body,
        payload
            .blocks
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(flat_text)
            .fold(String::new(), |mut body, text| {
                calm_types::report_blocks::append_block_text(&mut body, &text);
                body
            })
    );
    Ok(InitialReportSnapshot {
        payload,
        doc,
        declarations,
        diagnostics,
        template_context: None,
    })
}

/// The payload production writes on a planner-harness card. `pub` so integration
/// fixtures can mint the production shape instead of a partial literal.
pub fn planner_harness_card_payload(goal: Option<String>) -> serde_json::Value {
    let mut card_payload = serde_json::Map::new();
    card_payload.insert(
        "schemaVersion".into(),
        serde_json::Value::from(CODEX_PAYLOAD_SCHEMA_VERSION),
    );
    card_payload.insert(
        "codex_source".into(),
        serde_json::Value::String("shared".into()),
    );
    card_payload.insert("planner_harness".into(), serde_json::Value::Bool(true));
    if let Some(goal) = goal.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        card_payload.insert("prompt".into(), serde_json::Value::String(goal.to_string()));
    }
    serde_json::Value::Object(card_payload)
}

pub(crate) fn planner_harness_layout_payload(
    planner_card_id: &str,
    report_card_id: &str,
) -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": 1,
        "positions": {
            planner_card_id: {
                "x": 0, "y": 0, "w": 6, "h": 12
            },
            report_card_id: {
                "x": 6, "y": 0, "w": 6, "h": 12
            }
        }
    })
}

/// Test seam for the one timing predicate: fires after the fence transaction has
/// committed, before the pre-move re-check.
#[cfg(feature = "fixtures")]
#[derive(Clone)]
pub struct WorkspaceRepointRaceHook {
    pub entered: std::sync::Arc<Notify>,
    pub release: std::sync::Arc<Notify>,
}

#[cfg(feature = "fixtures")]
fn workspace_repoint_race_hooks() -> &'static StdMutex<HashMap<String, WorkspaceRepointRaceHook>> {
    static HOOKS: OnceLock<StdMutex<HashMap<String, WorkspaceRepointRaceHook>>> = OnceLock::new();
    HOOKS.get_or_init(|| StdMutex::new(HashMap::new()))
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn install_workspace_repoint_race_hook_for_test(
    track_id: &str,
    hook: WorkspaceRepointRaceHook,
) {
    workspace_repoint_race_hooks()
        .lock()
        .expect("workspace repoint hook mutex")
        .insert(track_id.to_string(), hook);
}

async fn wait_at_workspace_repoint_race_hook(track_id: &str) {
    #[cfg(feature = "fixtures")]
    {
        let hook = workspace_repoint_race_hooks()
            .lock()
            .expect("workspace repoint hook mutex")
            .remove(track_id);
        if let Some(hook) = hook {
            hook.entered.notify_one();
            hook.release.notified().await;
        }
    }
    #[cfg(not(feature = "fixtures"))]
    let _ = track_id;
}

/// Test seam for the shutdown-failure branch of the fence, which an integration test
/// cannot provoke otherwise.
#[cfg(feature = "fixtures")]
fn workspace_repoint_shutdown_failures() -> &'static StdMutex<HashMap<String, ()>> {
    static FAILURES: OnceLock<StdMutex<HashMap<String, ()>>> = OnceLock::new();
    FAILURES.get_or_init(|| StdMutex::new(HashMap::new()))
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn fail_workspace_repoint_shutdown_for_test(track_id: &str) {
    workspace_repoint_shutdown_failures()
        .lock()
        .expect("workspace repoint shutdown failure mutex")
        .insert(track_id.to_string(), ());
}

async fn shutdown_fenced_harness(
    harness: &crate::harness::PlannerHarness,
    track_id: &str,
) -> Result<()> {
    #[cfg(feature = "fixtures")]
    {
        let forced = workspace_repoint_shutdown_failures()
            .lock()
            .expect("workspace repoint shutdown failure mutex")
            .remove(track_id)
            .is_some();
        if forced {
            return Err(CalmError::Internal(
                "injected planner harness shutdown failure (#1147 S3 test seam)".into(),
            ));
        }
    }
    #[cfg(not(feature = "fixtures"))]
    let _ = track_id;
    harness.shutdown().await
}

/// What the fence transaction decided, carried out to the filesystem half.
struct RepointFence {
    /// The workspace as read *inside* the transaction — the authority, not the
    /// unlocked read the route did to answer 404.
    old_workspace: TrackWorkspace,
    /// Every runtime the fence superseded, so the process-side shutdown below
    /// knows which live handles to kill and the compensating restart knows
    /// something was torn down.
    superseded_runtime_ids: Vec<String>,
}

/// Point a track at a repository the user already has (`managed → attached`).
///
/// SQLite transactions do not isolate the filesystem: every active runtime is fenced in
/// the same transaction as the criteria, the criteria are re-checked before the move, and
/// the row is written before the directory moves so every abort is a clean 409.
async fn repoint_track_workspace(
    s: &RouteState,
    w: &WorkerState,
    actor: &Actor,
    track: &Track,
    requested: &TrackWorkspacePatch,
) -> Result<Response> {
    // Moving a directory is a human decision. Unreachable through HTTP today (the only
    // non-`User` header form already 403s further out); it guards internal callers holding
    // a real `ActorId::AiCodex`.
    if !matches!(actor.to_actor_id(), ActorId::User) {
        return Err(CalmError::Forbidden(
            "track workspace changes are user-only".into(),
        ));
    }

    // There is no `managed → managed`: a managed path is derived from the area and track ids.
    if requested.kind != TrackWorkspaceKind::Attached {
        return Err(CalmError::BadRequest(
            "track workspace: only `attached` is a target — pointing a track at a repository \
             you already have. There is no `managed` target: a managed workspace's path is \
             derived from the track, so re-allocating one would produce the same directory."
                .into(),
        ));
    }

    // The system area's launchpad path is kernel-maintained and is the documented exception
    // to the freeze latch; a user PATCH must not touch it.
    let area = s.repo.area_get(track.area_id.as_str()).await?;
    if area.as_ref().is_none_or(|c| c.kind == AreaKind::System) {
        return Err(CalmError::Forbidden(format!(
            "track {} belongs to the system area; its workspace is kernel-maintained",
            track.id
        )));
    }

    // Validate the target BEFORE any write, so a bad path fails here with git's own words
    // rather than as a worker's `spawn-failed`.
    let new_path = normalize_path(&requested.path);
    crate::workspace_materialize::validate_attached_workspace(std::path::Path::new(&new_path))?;

    let workspace_root = s.workspace_root.clone();
    let track_id = track.id.to_string();
    let area_id = track.area_id.as_str().to_string();

    // Serialize classification through the move with normal task starts and
    // direct Track recovery. Release before restarting the planner, which uses
    // the OperationRuntime itself.
    let mut operation_guard = Some(s.operation_runtime.lock_for_track_delete().await);
    let mut track_guard =
        Some(crate::per_card_lock::lock_key(&s.track_delete_locks, &track_id).await);

    let fence_conflict = FolderConflictSlot::default();
    let fence_track_id = track_id.clone();
    let fence_area_id = area_id.clone();
    let fence_path = new_path.clone();
    let fence_claim = FolderClaim::Enforce {
        attach: requested.attach_folder,
        allow_cross_area_cwd: None,
        conflict: fence_conflict.clone(),
    };
    let fence = crate::db::write_in_tx_typed(s.repo.as_ref(), move |tx| {
        let track_id = fence_track_id.clone();
        let area_id = fence_area_id.clone();
        let new_path = fence_path.clone();
        let claim = fence_claim.clone();
        Box::pin(async move {
            // Authoritative re-read. The route's unlocked read answered 404
            // and scoped the event; every decision below comes from here.
            crate::operation::terminal_disposal::require_safe_tx(
                tx,
                &crate::operation::terminal_disposal::Scope::Track(track_id.clone()),
            )
            .await?;
            let old_workspace = crate::db::sqlite::track_workspace_read_tx(tx, &track_id).await?;
            if old_workspace.kind != TrackWorkspaceKind::Managed {
                return Err(CalmError::Conflict(format!(
                    "track {track_id} already has an attached workspace ({}); an attached \
                     repository belongs to you, and the server never moves, initializes or \
                     deletes one — so it is also never re-pointed away from",
                    old_workspace.path
                )));
            }
            if let Some(frozen_at) = old_workspace.frozen_at {
                return Err(CalmError::Conflict(format!(
                    "track {track_id} workspace was frozen at {frozen_at}; a workspace is a \
                     default that can be changed only before any work happens in it"
                )));
            }
            // The one predicate that does not enumerate writers: it asks the disk. Repeated after
            // the commit because SQLite isolates none of it.
            let verdict = workspace_pristine(std::path::Path::new(&old_workspace.path));
            if !verdict.is_pristine() {
                return Err(CalmError::Conflict(
                    verdict.conflict_message(std::path::Path::new(&old_workspace.path)),
                ));
            }
            // Fail fast on the claim rules WITHOUT minting: this transaction commits (it is also
            // the fence), so a row written here would survive a later refusal.
            enforce_folder_claim_tx(
                tx,
                &claim,
                &area_id,
                &new_path,
                FolderClaimIntent::Repoint,
                FolderClaimPass::ScanOnly,
            )
            .await?;
            // THE FENCE. Every active runtime of this track, not just the planner harness.
            let runtime_ids: Vec<String> = sqlx::query_scalar(
                "SELECT id FROM worker_sessions WHERE track_id=?1 \
                 AND state IN ('starting','running','idle','turn_pending') ORDER BY id",
            )
            .bind(&track_id)
            .fetch_all(&mut **tx)
            .await?;
            for runtime_id in &runtime_ids {
                crate::db::sqlite::session_mark_superseded_runtime_tx(tx, runtime_id)
                    .await
                    .map_err(|e| CalmError::Internal(format!("workspace repoint fence: {e}")))?;
            }
            Ok(RepointFence {
                old_workspace,
                superseded_runtime_ids: runtime_ids,
            })
        })
    })
    .await;
    let fence = match fence {
        Ok(fence) => fence,
        Err(error) => return folder_conflict_response(&fence_conflict, error),
    };

    // The in-memory half of the fence: `maybe_issue_turn` consults no durable state, so an
    // observation enqueued before the commit would still become a turn.
    // A shutdown failure must NOT abort this function: the registry entry is kept for the
    // restart to supersede, and the pre-move re-check catches an in-flight turn.
    for runtime_id in &fence.superseded_runtime_ids {
        let Some(harness) = w.harness.get(runtime_id) else {
            continue;
        };
        let outcome = shutdown_fenced_harness(&harness, &track_id).await;
        match outcome {
            Ok(()) => {
                let _ = w.harness.remove(runtime_id);
            }
            Err(error) => tracing::error!(
                track_id,
                runtime_id,
                error = %error,
                "workspace repoint: shutting the fenced planner harness down failed. \
                 Continuing: the database fence already refuses new turns, the \
                 pre-move re-check catches anything an in-flight turn writes, and \
                 the registry entry is left for the restart to supersede."
            ),
        }
    }

    let old_path = std::path::PathBuf::from(&fence.old_workspace.path);

    // Deterministic race window for the timing test. No-op in production.
    wait_at_workspace_repoint_race_hook(&track_id).await;

    let verdict = workspace_pristine(&old_path);
    if let PristineVerdict::Dirty { .. } = &verdict {
        drop(track_guard.take());
        drop(operation_guard.take());
        restart_planner_harness_at(s, actor, track, &fence.old_workspace.path).await;
        return Err(CalmError::Conflict(verdict.conflict_message(&old_path)));
    }

    let new_workspace = TrackWorkspace {
        kind: TrackWorkspaceKind::Attached,
        path: new_path.clone(),
        // Frozen, one-way: `attached → *` is not a legal transition, and an unfrozen attached
        // row is exactly what a PATCH that forgot to check `kind` would relocate.
        frozen_at: Some(crate::model::now_ms()),
    };
    let scope = EventScope::Track {
        track: track.id.clone(),
        area: track.area_id.clone(),
    };
    let actor_id = actor.to_actor_id();
    let write_conflict = FolderConflictSlot::default();
    let write_claim = FolderClaim::Enforce {
        attach: requested.attach_folder,
        allow_cross_area_cwd: None,
        conflict: write_conflict.clone(),
    };
    let write_track_id = track_id.clone();
    let write_area_id = area_id.clone();
    let write_workspace = new_workspace.clone();
    let written =
        write_with_actor_events_typed(s.repo.as_ref(), None, &s.events, &s.write, move |tx| {
            let scope = scope.clone();
            let track_id = write_track_id.clone();
            let area_id = write_area_id.clone();
            let workspace = write_workspace.clone();
            let claim = write_claim.clone();
            let actor_id = actor_id.clone();
            Box::pin(async move {
                // Authoritative, and atomic with the workspace write because
                // they share this `BEGIN IMMEDIATE`.
                enforce_folder_claim_tx(
                    tx,
                    &claim,
                    &area_id,
                    &workspace.path,
                    FolderClaimIntent::Repoint,
                    FolderClaimPass::Authoritative,
                )
                .await?;
                crate::operation::terminal_disposal::require_safe_tx(
                    tx,
                    &crate::operation::terminal_disposal::Scope::Track(track_id.clone()),
                )
                .await?;
                crate::db::sqlite::track_workspace_write_tx(tx, &track_id, &workspace).await?;
                let track = track_get_tx(tx, &TrackId::from(track_id)).await?;
                let events = vec![(
                    actor_id,
                    scope,
                    Event::TrackUpdated(crate::event::TrackUpdatedPayload::new(
                        track.clone(),
                        None,
                    )),
                )];
                Ok((track, events))
            })
        })
        .await;
    let (updated, _ids) = match written {
        Ok(written) => written,
        Err(error) => {
            // Nothing moved and nothing was written — put the harness back
            // where it was and report.
            drop(track_guard.take());
            drop(operation_guard.take());
            restart_planner_harness_at(s, actor, track, &fence.old_workspace.path).await;
            return folder_conflict_response(&write_conflict, error);
        }
    };

    let decision = workspace_recycle::recycle_track_workspace(
        &workspace_root,
        area.as_ref().map(|c| c.kind),
        &track_id,
        // The OLD workspace value, read inside the fence transaction. The row
        // now says `attached`, so re-reading it here would make guard 1 refuse
        // and leave the directory behind forever.
        &fence.old_workspace,
        crate::model::now_ms(),
    );
    match decision {
        // Never materialized, or already reclaimed. Nothing to move.
        Ok(workspace_recycle::RecycleDecision::Refused(
            workspace_recycle::RecycleRefusal::PathMissing,
        ))
        | Ok(workspace_recycle::RecycleDecision::Trashed { .. }) => {}
        // The row has already moved, so this is a leak, not a failure of the re-point. Loud,
        // and not a 500.
        Ok(workspace_recycle::RecycleDecision::Refused(refusal)) => {
            tracing::error!(
                track_id,
                path = %fence.old_workspace.path,
                reason = refusal.tag(),
                "workspace repoint: the track now points at the user's repository, but its old \
                 managed directory could not be reclaimed and is leaked on disk"
            );
        }
        Err(error) => {
            tracing::error!(
                track_id,
                path = %fence.old_workspace.path,
                error = %error,
                "workspace repoint: the track now points at the user's repository, but moving \
                 its old managed directory to the trash failed; it is leaked on disk"
            );
        }
    }
    workspace_recycle::gc_trash_best_effort(&workspace_root, crate::model::now_ms());

    // `force_new_thread` is the only mechanism that re-reads `cwd`: a resumed codex thread
    // keeps the cwd it was minted with.
    drop(track_guard.take());
    drop(operation_guard.take());
    restart_planner_harness_at(s, actor, &updated, &updated.workspace.path).await;

    Ok(Json(updated).into_response())
}

/// Reach the re-point with a caller HTTP cannot produce. `fixtures`-only.
#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub async fn repoint_track_workspace_for_test(
    s: &RouteState,
    w: &WorkerState,
    actor: &Actor,
    track: &Track,
    requested: &TrackWorkspacePatch,
) -> Result<Response> {
    repoint_track_workspace(s, w, actor, track, requested).await
}

/// Render a parked [`FolderConflict`] as the structured 409 the create route returns, so
/// the FE keeps `folder_id` / `area_id` / `conflict_kind`.
fn folder_conflict_response(slot: &FolderConflictSlot, error: CalmError) -> Result<Response> {
    match slot.take() {
        Some(body) => Ok((StatusCode::CONFLICT, Json(body)).into_response()),
        None => Err(error),
    }
}

/// Re-open the track's planner harness thread at `cwd`. Best effort: the workspace has
/// already moved, so the caller must not be told the whole operation failed.
async fn restart_planner_harness_at(s: &RouteState, actor: &Actor, track: &Track, cwd: &str) {
    // Same resolution the dispatcher uses (`resolve_planner_card`): the role
    // cache, not a `cards.kind` guess.
    let cards = match s.repo.cards_by_track(track.id.as_str()).await {
        Ok(cards) => cards,
        Err(error) => {
            tracing::warn!(track_id = %track.id, error = %error, "workspace repoint: planner card lookup failed");
            return;
        }
    };
    let planner_card_id = cards.into_iter().find_map(|card| {
        (s.write.verify_role(&card.id) == Some(CardRole::Planner)).then(|| card.id.to_string())
    });
    let Some(planner_card_id) = planner_card_id else {
        // No planner card on this track, so no harness to re-anchor.
        return;
    };
    let request = PlannerHarnessStartOperationPayload {
        actor: actor.to_actor_id(),
        track_id: track.id.to_string(),
        planner_card_id: CardId::from(planner_card_id.clone()),
        report_card_id: None,
        sort: None,
        cwd: cwd.to_string(),
        goal: None,
        // The transcript is NOT reset: harness items are persisted per card,
        // so re-opening the thread costs the agent its in-thread context, not
        // the user's history.
        reset_harness_items: false,
        force_new_thread: true,
        profile: Default::default(),
        create_card: None,
        first_message: None,
        create_request_sha256: None,
        // Not a conversation create; nothing to brief.
        opening_briefing: None,
    };
    let hash = match stable_payload_hash(
        &serde_json::json!({"actor": actor.as_str(), "request": &request}),
    ) {
        Ok(hash) => hash,
        Err(error) => {
            tracing::warn!(track_id = %track.id, error = %error, "workspace repoint: payload hash failed");
            return;
        }
    };
    let payload = match serde_json::to_value(&request) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::warn!(track_id = %track.id, error = %error, "workspace repoint: payload encode failed");
            return;
        }
    };
    match s
        .operation_runtime
        .submit(
            "planner-harness-start",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: None,
                payload_hash: hash,
            },
            payload,
        )
        .await
    {
        Ok(op) => match s.operation_runtime.wait(&op).await {
            Ok(result)
                if matches!(
                    result.outcome,
                    OperationOutcome::Succeeded { .. }
                        | OperationOutcome::SucceededViaCollision { .. }
                ) => {}
            other => tracing::warn!(
                track_id = %track.id,
                cwd,
                outcome = ?other.map(|r| r.outcome),
                "workspace repoint: planner harness restart did not succeed; the workspace is \
                 correct but the planner agent is inert"
            ),
        },
        Err(error) => tracing::warn!(
            track_id = %track.id,
            cwd,
            error = %error,
            "workspace repoint: planner harness restart submission failed"
        ),
    }
}

#[utoipa::path(
    patch,
    path = "/api/tracks/{id}",
    tag = "tracks",
    params(("id" = String, Path, description = "Track id")),
    request_body = TrackPatch,
    responses(
        (status = 200, description = "Track updated", body = Track),
        (status = 400, description = "Unsupported workspace change", body = ErrorBody),
        (status = 403, description = "Workspace change refused (system area)", body = ErrorBody),
        (status = 404, description = "Track not found", body = ErrorBody),
        (status = 409, description = "Workspace is frozen, attached, or no longer empty", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn update_track(
    State(s): State<RouteState>,
    State(w): State<WorkerState>,
    actor: Actor,
    Path(id): Path<String>,
    Json(p): Json<TrackPatch>,
) -> Result<Response> {
    // Track rows are immutable wrt their parent area, so reading area_id outside the txn is safe.
    let existing = s
        .repo
        .track_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {id}")))?;

    // A workspace change is a filesystem move bracketed by two transactions, so it does
    // not compose with the row patch; the combination is a 400.
    if let Some(workspace) = p.workspace.as_ref() {
        // Destructured rather than enumerated so adding a field to `TrackPatch` is a compile error here.
        let TrackPatch {
            workspace: _,
            title,
            sort,
            archived_at,
            pinned_at,
            lifecycle,
            task_budget,
            require_task_gates,
            planner_task_ceiling,
            automation_policy,
            tree_task_budget,
            claude_permissions_policy,
        } = &p;
        let mixes_other_fields = title.is_some()
            || sort.is_some()
            || archived_at.is_some()
            || pinned_at.is_some()
            || lifecycle.is_some()
            || task_budget.is_some()
            || require_task_gates.is_some()
            || planner_task_ceiling.is_some()
            || automation_policy.is_some()
            || tree_task_budget.is_some()
            || claude_permissions_policy.is_some();
        if mixes_other_fields {
            return Err(CalmError::BadRequest(
                "track workspace changes must be sent on their own; a workspace re-point moves \
                 directories on disk and cannot share a transaction with row edits"
                    .into(),
            ));
        }
        return repoint_track_workspace(&s, &w, &actor, &existing, workspace).await;
    }

    // The guard fires on mentioning `lifecycle`, not on changing it: accepting a no-op
    // write would advertise an editable field.
    if existing.purpose.as_deref() == Some(AREA_CHAT_PURPOSE) && p.lifecycle.is_some() {
        return Err(CalmError::Forbidden(
            "area chat track lifecycle cannot be changed".into(),
        ));
    }
    let scope = EventScope::Track {
        track: existing.id.clone(),
        area: existing.area_id.clone(),
    };
    let actor_id = actor.to_actor_id();

    // Track-level automation controls are human decisions; a Planner could otherwise
    // raise its own ceiling.
    if (p.planner_task_ceiling.is_some()
        || p.automation_policy.is_some()
        || p.tree_task_budget.is_some()
        || p.claude_permissions_policy.is_some())
        && !matches!(actor_id, ActorId::User)
    {
        return Err(CalmError::Forbidden(
            "automation_policy, planner_task_ceiling, tree_task_budget and \
             claude_permissions_policy are user-only"
                .into(),
        ));
    }

    // Lifecycle preflight: the same snapshot is checked again after BEGIN IMMEDIATE, and only
    // that in-tx check can authorize the row write. Same-state requests are an idempotent
    // silent success for authorized actors.
    let mut p = p;
    let lifecycle_change = if let Some(to) = p.lifecycle {
        validate_transition(existing.lifecycle, to, &actor_id)
            .map_err(|e| CalmError::Forbidden(format!("track lifecycle: {e}")))?;
        if existing.lifecycle == to {
            // Idempotent no-op for lifecycle; drop it from the patch
            // so the row write below is a true no-op when no other
            // field is set.
            p.lifecycle = None;
            None
        } else {
            Some((existing.lifecycle, to))
        }
    } else {
        None
    };

    // `Some(None)` clears back to the kernel default; 0 is a legal "hold new dispatches" budget.
    if let Some(Some(budget)) = p.task_budget
        && budget < 0
    {
        return Err(CalmError::BadRequest(format!(
            "task_budget must be >= 0 (got {budget}); pass null to reset to the kernel default"
        )));
    }
    if let Some(Some(ceiling)) = p.planner_task_ceiling
        && ceiling < 0
    {
        return Err(CalmError::BadRequest(format!(
            "planner_task_ceiling must be >= 0 (got {ceiling}); pass null to reset to the kernel default"
        )));
    }
    // 0 is legal; the root-only rule is enforced inside `track_update_tx`.
    if let Some(Some(budget)) = p.tree_task_budget
        && !(0..=MAX_TREE_TASK_BUDGET).contains(&budget)
    {
        return Err(CalmError::BadRequest(format!(
            "tree_task_budget must be between 0 and {MAX_TREE_TASK_BUDGET} (got {budget}); pass null to reset to the kernel default"
        )));
    }
    if let Some(Some(policy)) = &p.automation_policy
        && !matches!(policy.as_str(), "auto-declare" | "declare-and-wait")
    {
        return Err(CalmError::BadRequest(format!(
            "automation_policy must be auto-declare or declare-and-wait (got {policy}); pass null to reset to the kernel default"
        )));
    }
    validate_policy_patch(&mut p)?;

    // An entirely empty patch is the idempotent retry path: nothing to write or emit.
    let patch_has_other_changes = p.title.is_some()
        || p.sort.is_some()
        || p.archived_at.is_some()
        || p.pinned_at.is_some()
        || p.task_budget.is_some()
        || p.require_task_gates.is_some()
        || p.planner_task_ceiling.is_some()
        || p.automation_policy.is_some()
        || p.tree_task_budget.is_some()
        || p.claude_permissions_policy.is_some();
    if lifecycle_change.is_none() && !patch_has_other_changes {
        return Ok(Json(existing).into_response());
    }

    // A lifecycle change emits both `TrackLifecycleChanged` and `TrackUpdated` from the
    // same txn; both land or neither does.
    let area_id_for_event = existing.area_id.clone();
    let track_id_for_event = existing.id.clone();
    // Every admission policy rebuilds its affected projection before the scheduler sees
    // TrackUpdated, so no pending row admitted by the old policy can race a later claim.
    let projection_policy_changed = p.planner_task_ceiling.is_some()
        || p.automation_policy.is_some()
        || p.require_task_gates.is_some()
        || p.tree_task_budget.is_some();
    let tree_budget_changed = p.tree_task_budget.is_some();
    wait_at_track_lifecycle_patch_race_hook(id.as_str()).await;
    let p_for_tx = p.clone();
    let (track, _ids) =
        write_with_actor_events_typed(s.repo.as_ref(), None, &s.events, &s.write, move |tx| {
            let scope = scope.clone();
            Box::pin(async move {
                if let Some((expected_from, to)) = lifecycle_change {
                    validate_transition_snapshot_in_tx(
                        tx,
                        &track_id_for_event,
                        expected_from,
                        to,
                        &actor_id,
                    )
                    .await?;
                }
                let track = track_update_tx(tx, &id, p_for_tx).await?;
                let projections = if projection_policy_changed {
                    if tree_budget_changed {
                        tasks_rebuild_tree_tx(tx, &id).await?
                    } else {
                        vec![(track.clone(), tasks_rebuild_tx(tx, &id).await?)]
                    }
                } else {
                    Vec::new()
                };
                let mut events: Vec<(ActorId, EventScope, Event)> = Vec::new();
                if let Some((from, to)) = lifecycle_change {
                    events.push((
                        actor_id.clone(),
                        scope.clone(),
                        Event::TrackLifecycleChanged {
                            id: track_id_for_event.clone(),
                            area_id: area_id_for_event.clone(),
                            from,
                            to,
                            agent_message: None,
                        },
                    ));
                }
                events.push((
                    actor_id.clone(),
                    scope.clone(),
                    Event::TrackUpdated(crate::event::TrackUpdatedPayload::new(
                        track.clone(),
                        None,
                    )),
                ));
                for (projected_track, projection) in projections {
                    if !projection.changed_keys.is_empty() {
                        events.push((
                            actor_id.clone(),
                            EventScope::Track {
                                track: projected_track.id.clone(),
                                area: projected_track.area_id.clone(),
                            },
                            Event::PlanUpdated {
                                track_id: projected_track.id,
                                changed_keys: projection.changed_keys,
                                agent_message: None,
                            },
                        ));
                    }
                    events.extend(projection.kernel_events);
                }
                Ok((track, events))
            })
        })
        .await?;
    Ok(Json(track).into_response())
}

async fn snapshot_track_deletion(s: &RouteState, track: &Track) -> Result<TrackDeletePlan> {
    let cards = s.repo.cards_by_track(track.id.as_str()).await?;
    let mut terminals = Vec::new();
    for card in &cards {
        if let Some(terminal) = s.repo.terminal_get_by_card(card.id.as_str()).await? {
            terminals.push(terminal);
        }
    }
    Ok(TrackDeletePlan {
        track_id: track.id.clone(),
        area_id: track.area_id.clone(),
        cards,
        terminals,
    })
}

async fn teardown_track_deletion(
    s: &RouteState,
    w: &WorkerState,
    cs: &CodexShellState,
    plan: &TrackDeletePlan,
) -> Result<Vec<String>> {
    wait_at_track_delete_teardown_hook(plan.track_id.as_str()).await;
    crate::operation::terminal_disposal::require_safe(
        s.repo.as_ref(),
        crate::operation::terminal_disposal::Scope::Track(plan.track_id.to_string()),
        w.daemon.proc_supervisor_sock.as_deref(),
    )
    .await?;
    let mut seals =
        crate::shared_codex_appserver::DeletionThreadSeals::new(cs.shared_codex_appserver.clone());
    for card in &plan.cards {
        if let Some(thread_id) = quiesce_shared_card_active_turn(s.repo.as_ref(), cs, card).await? {
            seals.seal(thread_id);
        }
    }
    for terminal in &plan.terminals {
        quiesce_terminal_artifacts_for_deletion(
            Some(w.terminal_renderer.as_ref()),
            w.daemon.proc_supervisor_sock.as_deref(),
            terminal,
        )
        .await?;
    }
    for thread_id in w
        .harness
        .shutdown_track(&plan.track_id, cs.shared_codex_appserver.clone())
        .await?
    {
        seals.seal(thread_id);
    }
    Ok(seals.retain())
}

async fn surviving_root_before_leaf_removal(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    track_id: &TrackId,
) -> Result<Option<String>> {
    match track_tree_term(tx, track_id.as_str()).await?.term {
        TrackTreeTerm::Share(share) if share.root_id != track_id.as_str() => {
            Ok(Some(share.root_id))
        }
        TrackTreeTerm::Share(_) => Ok(None),
        TrackTreeTerm::RootUnresolved => Err(CalmError::Conflict(format!(
            "track {} belongs to an unresolved tree; repair the tree before deleting it",
            track_id.as_str()
        ))),
    }
}

async fn preflight_track_deletion_reprojection(
    pool: &sqlx::SqlitePool,
    track_id: &TrackId,
) -> Result<()> {
    // BEGIN IMMEDIATE even though read-only: it prevents a writer from changing the tree
    // between the root lookup and survivor validation.
    let mut tx = begin_immediate_tx(pool).await?;
    if let Some(root_id) = surviving_root_before_leaf_removal(&mut tx, track_id).await? {
        let members: Vec<(String, i64)> = sqlx::query_as(TRACK_TREE_MEMBERS_SQL)
            .bind(&root_id)
            .bind(MAX_TRACK_TREE_DEPTH + 1)
            .fetch_all(&mut *tx)
            .await?;
        for (member_id, _) in members {
            if member_id != track_id.as_str() {
                validate_task_rebuild_source_tx(&mut tx, &member_id).await?;
            }
        }
    }
    tx.rollback().await?;
    Ok(())
}

#[allow(deprecated)]
async fn finish_track_deletion(
    s: &RouteState,
    plan: TrackDeletePlan,
    actor: ActorId,
) -> Result<Vec<WorkspaceTrackSweep>> {
    let write_for_tx = s.write.clone();
    let track_id = plan.track_id.clone();
    let area_id = plan.area_id.clone();
    let terminals = plan.terminals;
    let scope = EventScope::Track {
        track: track_id.clone(),
        area: area_id.clone(),
    };
    let (sweeps, _ids) =
        write_with_actor_events_typed(s.repo.as_ref(), None, &s.events, &s.write, move |tx| {
            Box::pin(async move {
                // A leaf deletion changes N in the root budget's B/N split: resolve the root while the
                // leaf still exists, then rebuild every survivor. An unresolved tree must fail closed.
                crate::operation::terminal_disposal::require_safe_tx(
                    tx,
                    &crate::operation::terminal_disposal::Scope::Track(track_id.to_string()),
                )
                .await?;
                let surviving_root = surviving_root_before_leaf_removal(tx, &track_id).await?;
                for terminal in &terminals {
                    match terminal_delete_tx(tx, &terminal.id)
                        .await
                        .map_err(CalmError::from)
                    {
                        Ok(()) => {}
                        Err(CalmError::NotFound(_)) => {}
                        Err(e) => return Err(e),
                    }
                }
                overlay_delete_card_overlays_by_track_tx(tx, track_id.as_str()).await?;
                overlay_delete_by_entity_tx(tx, "track", track_id.as_str()).await?;
                overlay_delete_by_entity_tx(tx, "view", track_id.as_str()).await?;
                let release = release_workspace_leases_for_track_tx(tx, track_id.as_str()).await?;
                let mut events = release.events;
                track_delete_tx(tx, track_id.as_str(), write_for_tx.area_cache()).await?;
                events.push((
                    actor.clone(),
                    scope,
                    Event::TrackDeleted {
                        id: track_id,
                        area_id,
                    },
                ));
                if let Some(root_id) = surviving_root {
                    for (projected_track, projection) in
                        tasks_rebuild_tree_after_member_removal_tx(tx, &root_id).await?
                    {
                        if !projection.changed_keys.is_empty() {
                            events.push((
                                actor.clone(),
                                EventScope::Track {
                                    track: projected_track.id.clone(),
                                    area: projected_track.area_id.clone(),
                                },
                                Event::PlanUpdated {
                                    track_id: projected_track.id,
                                    changed_keys: projection.changed_keys,
                                    agent_message: None,
                                },
                            ));
                        }
                        events.extend(projection.kernel_events);
                    }
                }
                Ok((release.sweep.into_iter().collect::<Vec<_>>(), events))
            })
        })
        .await?;
    Ok(sweeps)
}

/// Reclaim this track's managed workspace between teardown and the row delete, so a
/// failure here leaves the track and its directory intact and retryable. A refusal
/// (guard not satisfied) is not an error.
fn recycle_track_workspace_for_delete(
    s: &RouteState,
    track: &Track,
    area_kind: Option<AreaKind>,
) -> Result<workspace_recycle::RecycleDecision> {
    let decision = workspace_recycle::recycle_track_workspace(
        &s.workspace_root,
        area_kind,
        track.id.as_str(),
        &track.workspace,
        crate::model::now_ms(),
    )?;
    workspace_recycle::gc_trash_best_effort(&s.workspace_root, crate::model::now_ms());
    Ok(decision)
}

impl PreparedTrackDeletion {
    async fn quiesce(
        self,
        route: &RouteState,
        worker: &WorkerState,
        codex: &CodexShellState,
    ) -> Result<QuiescedTrackDeletion> {
        let sealed_thread_ids = teardown_track_deletion(route, worker, codex, &self.plan).await?;
        Ok(QuiescedTrackDeletion {
            prepared: self,
            sealed_thread_ids,
        })
    }
}

impl QuiescedTrackDeletion {
    fn recycle(self, route: &RouteState) -> Result<RecycledTrackDeletion> {
        let Self {
            prepared,
            sealed_thread_ids,
        } = self;
        let recycled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            recycle_track_workspace_for_delete(route, &prepared.track, prepared.area_kind)
        }));
        match recycled {
            Ok(Ok(decision)) => Ok(RecycledTrackDeletion {
                prepared,
                decision,
                sealed_thread_ids,
            }),
            Ok(Err(error)) => {
                if workspace_recycle::workspace_allows_runtime_recovery(&prepared.track) {
                    for thread_id in &sealed_thread_ids {
                        prepared
                            .turn_daemon
                            .unseal_turn_thread_after_rollback(thread_id);
                    }
                }
                Err(error)
            }
            Err(_) => {
                if workspace_recycle::workspace_allows_runtime_recovery(&prepared.track) {
                    for thread_id in &sealed_thread_ids {
                        prepared
                            .turn_daemon
                            .unseal_turn_thread_after_rollback(thread_id);
                    }
                }
                Err(CalmError::Internal(format!(
                    "track deletion saga for {} panicked during workspace recycle",
                    prepared.track.id
                )))
            }
        }
    }
}

impl RecycledTrackDeletion {
    #[allow(deprecated)]
    async fn commit(self, route: &RouteState, actor: ActorId) -> Result<()> {
        let Self {
            prepared,
            decision,
            sealed_thread_ids,
        } = self;
        let track = &prepared.track;
        if wait_at_track_delete_commit_hook(track.id.as_str()).await {
            panic!("fixture: panic track deletion after recycle");
        }
        // Snapshotted before `finish_track_deletion` consumes the plan; used only on the committed arm.
        let deleted_card_ids: HashSet<String> = prepared
            .plan
            .cards
            .iter()
            .map(|card| card.id.to_string())
            .collect();
        // The Terminal cards whose generated hook settings file goes with the committed delete
        // (kept on rollback: the card still exists).
        let terminal_card_ids: Vec<String> = prepared
            .plan
            .terminals
            .iter()
            .map(|terminal| terminal.card_id.to_string())
            .collect();
        let sweeps = match finish_track_deletion(route, prepared.plan, actor).await {
            Ok(sweeps) => sweeps,
            Err(error) => {
                // Another deletion boundary may have won between teardown and this transaction. A
                // missing row is committed deletion, not rollback: never resurrect its cache entry or workspace.
                let track_survived = route.repo.track_get(track.id.as_str()).await?.is_some();
                if !track_survived {
                    return Err(error);
                }
                // `track_delete_tx` updates the process-local cache before commit; a rollback cannot
                // undo the cache or filesystem, so compensate here.
                if let Err(restore_error) = workspace_recycle::restore_recycled_workspace(&decision)
                {
                    return Err(CalmError::Internal(format!(
                        "track deletion rolled back ({error}), but workspace compensation failed: {restore_error}"
                    )));
                }
                route
                    .write
                    .remember_track(track.id.clone(), track.area_id.clone());
                for thread_id in &sealed_thread_ids {
                    prepared
                        .turn_daemon
                        .unseal_turn_thread_after_rollback(thread_id);
                }
                return Err(error);
            }
        };
        // The row delete has COMMITTED, so the daemon's in-memory `thread_id -> card_id`
        // attribution is stale. Post-commit and infallible.
        prepared
            .turn_daemon
            .forget_threads_for_deleted_cards(&deleted_card_ids)
            .await;
        // Same committed arm; the error arm keeps both entries because its Cards still exist.
        prepared
            .turn_daemon
            .forget_turn_state_for_deleted_threads(&sealed_thread_ids);
        // The transient plugin-result ring is process memory keyed by track; the row is gone.
        // An area-cascade delete never reaches this arm and relies on the ring's TTL.
        route
            .mcp_context
            .plugin_results
            .forget_track(track.id.as_str());
        // Post-commit, best effort: remove `<terminal-hooks>/<card_id>.json` for every deleted
        // Terminal card.
        for card_id in &terminal_card_ids {
            route.terminal_renderer.remove_hook_settings(card_id);
        }
        // This sweep is post-commit. A failure here must not restore the
        // workspace: the track row is already gone and the trash path is now
        // authoritative.
        sweep_workspace_worktrees_for_tracks_repo(route.repo.as_ref(), &route.events, sweeps)
            .await?;
        Ok(())
    }
}

/// The workspace move cannot be rolled back by dropping the transaction future, so once it
/// succeeds the remaining saga state and lock move to an owned task; Axum may cancel a
/// request future when the client disconnects.
async fn run_recycled_track_deletion(
    route: &RouteState,
    actor: ActorId,
    deletion: RecycledTrackDeletion,
) -> Result<()> {
    let recovery_track = deletion.prepared.track.clone();
    let recovery_decision = deletion.decision.clone();
    let recovery_thread_ids = deletion.sealed_thread_ids.clone();
    let recovery_turn_daemon = deletion.prepared.turn_daemon.clone();
    match std::panic::AssertUnwindSafe(deletion.commit(route, actor))
        .catch_unwind()
        .await
    {
        Ok(result) => result,
        Err(_) => {
            if route
                .repo
                .track_get(recovery_track.id.as_str())
                .await?
                .is_some()
            {
                workspace_recycle::restore_recycled_workspace(&recovery_decision)?;
                route
                    .write
                    .remember_track(recovery_track.id.clone(), recovery_track.area_id.clone());
                for thread_id in &recovery_thread_ids {
                    recovery_turn_daemon.unseal_turn_thread_after_rollback(thread_id);
                }
            }
            Err(CalmError::Internal(format!(
                "track deletion saga for {} panicked",
                recovery_track.id
            )))
        }
    }
}

#[allow(deprecated)]
async fn finish_prepared_track_deletion_owned(
    route: RouteState,
    worker: WorkerState,
    codex: CodexShellState,
    actor: ActorId,
    prepared: PreparedTrackDeletion,
) -> Result<()> {
    let track_id = prepared.track.id.clone();
    let recovery_track_ids = HashSet::from([track_id.clone()]);
    let task_track_id = track_id.clone();
    tokio::spawn(async move {
        let workflow = std::panic::AssertUnwindSafe(async {
            let recycled = prepared
                .quiesce(&route, &worker, &codex)
                .await?
                .recycle(&route)?;
            run_recycled_track_deletion(&route, actor, recycled).await
        })
        .catch_unwind()
        .await;
        let result = match workflow {
            Ok(result) => result,
            Err(_) => Err(CalmError::Internal(format!(
                "track deletion saga for {task_track_id} panicked before recycle"
            ))),
        };
        let recovery = crate::harness::HarnessRecoveryContext::new(
            worker.repo.clone(),
            route.events.clone(),
            route.write.role_cache().clone(),
            route.write.area_cache().clone(),
            codex.shared_codex_appserver.clone(),
            worker.harness.clone(),
            route.track_delete_locks.clone(),
        );
        if result.is_err()
            && let Err(error) =
                crate::harness::recover_harnesses_for_tracks(&recovery, &recovery_track_ids).await
        {
            tracing::error!(
                track_id = %task_track_id,
                error = %error,
                "aborted track deletion could not recover its planner harness"
            );
        }
        result
    })
    .await
    .map_err(|error| {
        CalmError::Internal(format!(
            "owned deletion task for track {} failed: {error}",
            track_id.as_str()
        ))
    })?
}

#[utoipa::path(
    delete,
    path = "/api/tracks/{id}",
    tag = "tracks",
    params(("id" = String, Path, description = "Track id")),
    responses(
        (status = 204, description = "Track deleted"),
        (status = 404, description = "Track not found", body = ErrorBody),
        (status = 403, description = "Track belongs to the system area and cannot be deleted via REST", body = ErrorBody),
        (status = 409, description = "Track has a descendant or active forge action", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
#[allow(deprecated)]
pub(crate) async fn delete_track(
    State(s): State<RouteState>,
    State(w): State<WorkerState>,
    State(cs): State<CodexShellState>,
    actor: Actor,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    // One process owns this track's move + transaction + compensation at a time.
    // Lock order: operation drive → track delete.
    let operation_guard = s.operation_runtime.lock_for_track_delete().await;
    let delete_guard = crate::per_card_lock::lock_key(&s.track_delete_locks, &id).await;
    // Eager teardown for every terminal under the track: `terminals.card_id` is
    // `ON DELETE RESTRICT`, so a missed cleanup aborts the delete transaction.
    let track = s
        .repo
        .track_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {id}")))?;
    let track_id = track.id.clone();

    // System scaffolding is kernel-owned and not user-deletable. Deleting a system-area track
    // row would strand its managed directory forever: reclaiming it needs the row that names it.
    let owning_area = s.repo.area_get(track.area_id.as_str()).await?;
    if owning_area
        .as_ref()
        .is_some_and(|c| c.kind == AreaKind::System)
    {
        return Err(CalmError::Forbidden(format!(
            "track {id} belongs to the system area and cannot be deleted via the public API"
        )));
    }

    // Defensive TOCTOU guard only: this non-transactional read happens before the teardown
    // tx, so a forge-action can still become in-flight before the sweep.
    let pool = w.repo.sqlite_pool().ok_or_else(|| {
        CalmError::Internal("delete_track forge-action fence requires sqlite-backed repo".into())
    })?;
    let mut preflight = begin_immediate_tx(&pool).await?;
    crate::db::sqlite::track_require_candidate_verification_settled_tx(
        &mut preflight,
        track_id.as_str(),
    )
    .await?;
    preflight.rollback().await?;
    if track_has_active_forge_action(&pool, track_id.as_str()).await? {
        return Err(CalmError::Conflict(format!(
            "track {id} has an in-flight forge-action; retry after it settles"
        )));
    }

    // Experience-only preflight: the in-transaction guard in `track_delete_tx` is the
    // correctness boundary; a child created after this read makes the final delete return
    // Conflict, which is safe and retryable.
    if let Some(child_id) =
        sqlx::query_scalar::<_, String>("SELECT id FROM tracks WHERE parent_track_id=?1 LIMIT 1")
            .bind(track_id.as_str())
            .fetch_optional(&pool)
            .await?
    {
        return Err(CalmError::Conflict(format!(
            "track {id} has child track {child_id}; cancel it if needed, then delete that child track first"
        )));
    }

    // Validate every surviving report source before teardown has any external effect.
    preflight_track_deletion_reprojection(&pool, &track_id).await?;

    let prepared = PreparedTrackDeletion {
        plan: snapshot_track_deletion(&s, &track).await?,
        turn_daemon: cs.shared_codex_appserver.clone(),
        track,
        area_kind: owning_area.map(|area| area.kind),
        _operation_guard: operation_guard,
        _delete_guard: delete_guard,
    };
    finish_prepared_track_deletion_owned(s, w, cs, actor.to_actor_id(), prepared).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// A report link from another track that targets this track.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct TrackBacklink {
    pub src_track_id: String,
    pub src_track_title: String,
    pub src_block_id: String,
    pub dst_block_id: Option<String>,
    pub label: String,
    pub quote: report_backlinks::BacklinkQuote,
    pub updated_at: i64,
}

/// A bounded page of report backlinks.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct TrackBacklinksResponse {
    pub backlinks: Vec<TrackBacklink>,
    pub truncated: bool,
    pub skipped_sources: usize,
}

impl From<report_backlinks::Backlink> for TrackBacklink {
    fn from(value: report_backlinks::Backlink) -> Self {
        Self {
            src_track_id: value.src_track_id,
            src_track_title: value.src_track_title,
            src_block_id: value.src_block_id,
            dst_block_id: value.dst_block_id,
            label: value.label,
            quote: value.quote,
            updated_at: value.updated_at,
        }
    }
}

impl From<report_backlinks::BacklinkPage> for TrackBacklinksResponse {
    fn from(value: report_backlinks::BacklinkPage) -> Self {
        Self {
            backlinks: value
                .backlinks
                .into_iter()
                .map(TrackBacklink::from)
                .collect(),
            truncated: value.truncated,
            skipped_sources: value.skipped_sources,
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/tracks/{id}/backlinks",
    tag = "tracks",
    params(("id" = String, Path, description = "Track id")),
    responses(
        (status = 200, description = "Report links from tracks in the same area", body = TrackBacklinksResponse),
        (status = 404, description = "Track not found", body = ErrorBody),
    ),
)]
pub(crate) async fn get_track_backlinks(
    State(s): State<RouteState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    let page =
        report_backlinks::backlinks_for_track(s.repo.as_ref(), &id, s.task_budget_default).await?;
    Ok(Json(TrackBacklinksResponse::from(page)))
}

/// Request body for `POST /api/tracks/:id/report`. No `author` field: it is pinned
/// server-side to `EditAuthor::User` so a User cannot forge a Planner edit;
/// `schemaVersion` is server-managed too.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateTrackReportBody {
    /// Expected document revision from the latest report read. Use zero
    /// for a document that has never been persisted through the CRDT path.
    pub if_doc_rev: u64,
    /// One-line summary the track-list sidebars surface. Empty string
    /// is a valid value; the caller must commit.
    pub summary: String,
    /// Markdown source. Sections are derived at render time by
    /// splitting at H1 (`^# `) headings; the kernel does not interpret
    /// the structure.
    pub body: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrackReportReadResponse {
    pub schema_version: u32,
    pub doc_rev: u64,
    pub summary: String,
    pub body: String,
    pub blocks: Vec<calm_types::track_report::ReportBlock>,
    pub task_diagnostics: Vec<crate::db::sqlite::BlockVerdict>,
}

#[utoipa::path(
    get,
    path = "/api/tracks/{id}/report",
    tag = "tracks",
    params(("id" = String, Path, description = "Track id")),
    responses(
        (status = 200, description = "Current report with derived task diagnostics", body = TrackReportReadResponse),
        (status = 401, description = "Missing or invalid session", body = ErrorBody),
        (status = 404, description = "Track not found", body = ErrorBody)
    ),
)]
pub(crate) async fn get_track_report(
    State(s): State<RouteState>,
    _principal: Principal,
    Path(id): Path<String>,
) -> Result<Response> {
    let (_, report_card, _) = resolve_report_for_track(s.repo.as_ref(), &id).await?;
    let snapshot = load_report_read_snapshot(
        s.repo.as_ref(),
        report_card.id.as_str(),
        s.task_budget_default,
    )
    .await?;
    Ok((
        StatusCode::OK,
        Json(TrackReportReadResponse {
            schema_version: snapshot.schema_version,
            doc_rev: snapshot.doc_rev,
            summary: snapshot.summary,
            body: snapshot.body,
            blocks: snapshot.blocks,
            task_diagnostics: snapshot.task_diagnostics,
        }),
    )
        .into_response())
}

/// `POST /api/tracks/:id/report` — user-driven track-report edit; only `ActorId::User`
/// is allowed (403 otherwise). The response is the CRDT-projected payload, not the
/// request body verbatim.
#[utoipa::path(
    post,
    path = "/api/tracks/{id}/report",
    tag = "tracks",
    params(("id" = String, Path, description = "Track id")),
    request_body = UpdateTrackReportBody,
    responses(
        (status = 200, description = "Updated track-report payload", body = TrackReportPayload),
        (status = 401, description = "Missing or invalid session", body = ErrorBody),
        (status = 403, description = "Non-user actor (worker / plugin / planner) rejected", body = ErrorBody),
        (status = 409, description = "Report document revision conflict", body = ErrorBody),
        (status = 404, description = "Track not found", body = ErrorBody),
        (status = 500, description = "Internal error (incl. missing report-card invariant)", body = ErrorBody),
    ),
)]
pub(crate) async fn update_track_report(
    State(s): State<RouteState>,
    // `Principal` asserts the session middleware has run; not read today (single-user),
    // held for the multi-user split.
    _principal: Principal,
    actor: Actor,
    Path(id): Path<String>,
    Json(body): Json<UpdateTrackReportBody>,
) -> Result<Response> {
    // Direct string check, NOT `to_actor_id()`: its defensive fallback maps unknown headers
    // to `ActorId::User`, which is right for attribution but wrong for gating.
    super::track_report_blocks::require_rest_user_actor(&actor)?;

    // 404 on missing track; 500 on missing report card (invariant: one report row per track).
    let target = track_report::ReportEditTarget::resolve(s.repo.as_ref(), &id).await?;

    // `schemaVersion` is always the current constant; the field is not on the wire shape.
    let if_doc_rev = body.if_doc_rev;
    let next = TrackReportPayload::new(body.summary, body.body);

    // The author is fixed inside `rest_user_replace`; this handler cannot name one.
    let updated = track_report::write::rest_user_replace(
        s.repo.as_ref(),
        &s.events,
        &s.write,
        target,
        next,
        if_doc_rev,
    )
    .await?;

    // Project the persisted payload out of the updated card row so the response matches
    // what the next reader will see.
    let payload: TrackReportPayload = serde_json::from_value(updated.payload).map_err(|e| {
        CalmError::Internal(format!(
            "track-report edit: re-deserialize projected payload: {e}",
        ))
    })?;
    Ok((StatusCode::OK, Json(payload)).into_response())
}

#[cfg(test)]
mod tests {
    use super::{
        planner_harness_layout_payload, prepare_fork_report, prepare_initial_report_payload,
        prepare_template_report,
    };
    use crate::db::prelude::*;
    use crate::db::sqlite::SqlxRepo;
    use crate::model::{NewArea, NewCard, NewTrack};
    use crate::routes::theme::RequestTheme;
    use crate::templates::TemplateRoster;
    use crate::track_report::write::{InitialReportTarget, structural_init_report_tx};
    use crate::track_report::{ReportBlock, TrackReportPayload};
    use crate::track_report_doc::ReportDoc;
    use serde_json::json;

    /// Every built-in recipe instantiates, and its declarations are the tasks it advertises.
    /// The only place "the declarations are actually produced" is observable: recipe tasks
    /// are all `ready: false`, so the `tasks` table stays empty.
    #[test]
    fn every_recipe_instantiates_and_declares_its_tasks() {
        let roster = TemplateRoster::builtin();
        for template in roster.entries() {
            let key = template.key();
            let compiled = prepare_template_report(roster, key).unwrap_or_else(|error| {
                panic!("`{key}` must instantiate: {error}");
            });
            let payload = compiled.payload;
            let declarations = compiled.declarations;
            assert!(
                payload.blocks.as_ref().is_some_and(|b| !b.is_empty()),
                "`{key}`: no blocks"
            );
            let declared: Vec<&str> = declarations
                .iter()
                .map(|declaration| declaration.key.as_str())
                .collect();
            let fenced: Vec<String> =
                crate::templates::template_task_payloads_from_body(&template.recipe().body)
                    .iter()
                    .filter_map(|task| task.get("key").and_then(|k| k.as_str()).map(str::to_string))
                    .collect();
            assert_eq!(
                declared,
                fenced.iter().map(String::as_str).collect::<Vec<_>>(),
                "`{key}`: declarations must be the recipe's task keys, in order"
            );
        }
    }

    /// Task examples stay in startup context, not in the report or picker.
    #[test]
    fn named_source_task_examples_remain_in_context_only() {
        let task = calm_types::report_blocks::render_fence(
            "task",
            &serde_json::json!({
                "key": "example", "kind": "codex", "goal": "Read the code",
                "acceptance": "Evidence explains the result", "ready": false, "declared_by": "spec"
            }),
        );
        let body = format!("before\n{task}---\n");
        let compiled = prepare_initial_report_payload(
            "example",
            TrackReportPayload::new("Example", body.clone()),
        )
        .unwrap();
        assert!(compiled.task_block_payloads().unwrap().is_empty());
        assert!(compiled.declarations.is_empty());
        assert!(!compiled.payload.body.contains("```neige-block task"));
        let markup: Vec<_> = pulldown_cmark::Parser::new(&compiled.payload.body).collect();
        assert_eq!(
            markup,
            vec![
                pulldown_cmark::Event::Start(pulldown_cmark::Tag::Paragraph),
                pulldown_cmark::Event::Text("before".into()),
                pulldown_cmark::Event::End(pulldown_cmark::TagEnd::Paragraph),
                pulldown_cmark::Event::Rule,
            ],
            "removing a task must not turn its prose neighbors into a Setext heading"
        );
        let context = serde_json::to_value(compiled.template_context.unwrap()).unwrap();
        assert_eq!(context["body"], body);
        assert_eq!(context["title"], "Example");
    }

    #[test]
    fn task_elision_cannot_repair_a_misplaced_source_contract() {
        let task = calm_types::report_blocks::render_fence(
            "task",
            &serde_json::json!({
                "key": "example", "kind": "codex", "goal": "Inspect",
                "ready": true, "declared_by": "user", "released_by_user": true,
            }),
        );
        let body = format!("{task}{}", TrackReportPayload::initial().body);
        assert!(
            prepare_initial_report_payload("misplaced", TrackReportPayload::new("Example", body))
                .is_err()
        );
    }

    /// A recipe whose body does not parse is refused, not silently thinned: `split_body` treats
    /// a malformed fence as prose, so without the check an indented opener would drop a task.
    #[test]
    fn a_recipe_that_does_not_parse_is_refused() {
        let good = TrackReportPayload::new(
            "Example",
            calm_types::report_blocks::render_fence(
                "task",
                &serde_json::json!({"key": "example", "kind": "codex",
                "goal": "Read the code", "ready": false, "declared_by": "spec"}),
            ),
        );

        // A: an indented opener. `split_body` demotes it to prose.
        let indented = good
            .body
            .replacen("```neige-block task", " ```neige-block task", 1);
        assert_ne!(indented, good.body, "the fixture did not change the body");
        // `match` rather than `expect_err`: `ReportDoc` is deliberately not `Debug`.
        let error = match prepare_initial_report_payload(
            "small-change",
            TrackReportPayload::new(good.summary.clone(), indented),
        ) {
            Ok(_) => panic!("an indented fence opener must be refused, not demoted to prose"),
            Err(error) => error,
        };
        assert!(
            format!("{error}").contains("small-change"),
            "the error must name the recipe; got {error}"
        );

        // B: a well-formed fence whose payload violates the task schema.
        let broken =
            good.body
                .replacen("\"kind\": \"codex\"", "\"kind\": \"not-a-worker-kind\"", 1);
        assert_ne!(broken, good.body, "the fixture did not change the body");
        if prepare_initial_report_payload(
            "small-change",
            TrackReportPayload::new(good.summary, broken),
        )
        .is_ok()
        {
            panic!("a schema-invalid task payload must be refused");
        }
    }

    #[test]
    fn fork_revalidates_every_fence_payload() {
        let invalid = ReportBlock {
            id: "b_0001".into(),
            kind: "task".into(),
            rev: 4,
            payload: json!({
                "key": "build",
                "kind": "not-a-worker-kind",
                "goal": "build",
                "ready": true,
                "declared_by": "user"
            }),
        };
        let error = prepare_fork_report("summary".into(), vec![invalid], "source", "target")
            .err()
            .expect("invalid copied task must abort fork");
        assert!(error.to_string().contains("invalid forked report block"));
    }

    /// The `KIND_PROSE` arm `continue`s past the loop's `validate_payload`, so the fence
    /// check has to happen inside that arm.
    #[test]
    fn fork_rejects_malformed_neige_fence_in_a_prose_block() {
        let prose = ReportBlock {
            id: "b_0002".into(),
            kind: "prose".into(),
            rev: 1,
            payload: json!({"markdown": "# A\n```neige-block app\nnot json\n```\n"}),
        };
        let error = prepare_fork_report("summary".into(), vec![prose], "source", "target")
            .err()
            .expect("malformed prose fence must abort the fork");
        assert!(
            matches!(&error, crate::error::CalmError::BadRequest(_)),
            "must be a 400, got: {error:?}"
        );
        let rendered = error.to_string();
        assert!(
            rendered.contains("invalid forked report block b_0002"),
            "error must name the offending block: {rendered}"
        );
        assert!(
            rendered.contains("neige-block"),
            "error must name the malformed fence: {rendered}"
        );
    }

    /// The scope fence for the check above: well-formed fences still fork.
    #[test]
    fn fork_keeps_prose_blocks_with_well_formed_fences() {
        let prose = ReportBlock {
            id: "b_0003".into(),
            kind: "prose".into(),
            rev: 1,
            payload: json!({"markdown": "# A\n\nplain prose, no fence\n"}),
        };
        prepare_fork_report("summary".into(), vec![prose], "source", "target")
            .expect("well-formed prose must fork");
    }

    /// The task block's `refs` point at the prose block of the same snapshot, so it can only
    /// resolve if the payload cache already holds this write.
    #[tokio::test]
    async fn structural_door_writes_cache_crdt_and_projection_together() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let area = repo
            .area_create(NewArea {
                name: "fork-helper".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                area_id: area.id,
                title: "fork helper".into(),
                sort: None,
                cwd: "/tmp/fork-helper".into(),
                template_id: None,
                plugin_scope: None,
                template_input: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let report = repo
            .card_create(NewCard {
                track_id: track.id.clone(),
                kind: "track-report".into(),
                sort: Some(-1.0),
                payload: serde_json::to_value(TrackReportPayload::initial()).unwrap(),
                title: None,
            })
            .await
            .unwrap();
        let blocks = vec![
            ReportBlock {
                id: "b_1234".into(),
                kind: "prose".into(),
                rev: 1,
                payload: json!({"markdown": "projection target"}),
            },
            ReportBlock {
                id: "b_abcd".into(),
                kind: "task".into(),
                rev: 1,
                payload: json!({
                    "key": "projected",
                    "kind": "codex",
                    "goal": "project the fork",
                    "refs": [format!("neige://wave/{}#b_1234", track.id)],
                    "no_gate_reason": "covered by helper behavior test",
                    "ready": true,
                    "released_by_user": true,
                    "declared_by": "spec"
                }),
            },
        ];
        let mut doc = ReportDoc::from_blocks_exact("forked", &blocks).unwrap();
        let (summary, body) = doc.project().unwrap();
        let mut payload = TrackReportPayload::new(summary, body);
        payload.blocks = Some(blocks.clone());
        let payload_value = serde_json::to_value(&payload).unwrap();
        let (declarations, diagnostics) =
            calm_types::report_blocks::tasks::project_task_declarations(&blocks);

        let pool = repo.sqlite_pool().unwrap();
        let mut tx = pool.begin().await.unwrap();
        let (updated, projection) = structural_init_report_tx(
            &mut tx,
            InitialReportTarget {
                report_card_id: report.id.as_str(),
                track_id: track.id.as_str(),
                payload: &payload,
                doc: &mut doc,
                declarations: &declarations,
                diagnostics: &diagnostics,
            },
        )
        .await
        .unwrap();

        assert_eq!(updated.payload, payload_value);
        let persisted: (String, bool) =
            sqlx::query_as("SELECT json(payload),body_crdt IS NOT NULL FROM cards WHERE id=?1")
                .bind(report.id.as_str())
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&persisted.0).unwrap(),
            payload_value
        );
        assert!(persisted.1, "fork helper omitted CRDT bytes");
        assert!(
            projection
                .diagnostics
                .iter()
                .flat_map(|verdict| &verdict.diagnostics)
                .all(|diagnostic| diagnostic.code != "reference_missing"),
            "fork projection diagnostics: {:?}",
            projection.diagnostics
        );
        let task_key: String =
            sqlx::query_scalar("SELECT key FROM tasks WHERE track_id=?1 AND key='projected'")
                .bind(track.id.as_str())
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(task_key, "projected");
        tx.rollback().await.unwrap();
    }

    /// Pins the full-write assumption the events retention pruner's keep-latest `overlay.set`
    /// carve-out depends on: every kernel-emitted `view/layout` `overlay.set` must carry the
    /// complete positions map.
    #[test]
    fn planner_harness_layout_payload_is_a_full_positions_write() {
        let payload = planner_harness_layout_payload("planner-1", "report-1");
        let positions = payload
            .get("positions")
            .and_then(|v| v.as_object())
            .expect("layout overlay.set payload must carry a full positions object");
        assert!(positions.contains_key("planner-1"));
        assert!(positions.contains_key("report-1"));
    }

    /// The create-time `template_input` binding matrix against the owning plugin Manifest.
    mod template_input_binding {
        use super::super::validate_template_input_binding;
        use crate::error::CalmError;
        use crate::plugin_host::manifest::Manifest;
        use crate::plugin_host::template_input::TemplateInputOwner;
        use serde_json::{Value, json};

        /// The three `owner` axis values, as short constructors, so each test
        /// below reads as one cell of the matrix.
        fn owned(plugin: &Manifest) -> TemplateInputOwner<'_> {
            TemplateInputOwner::Plugin(plugin)
        }

        fn plugin(input_schema: Option<Value>) -> Manifest {
            let mut v = json!({
                "manifest_version": 2,
                "id": "dev.neige.git-forge",
                "version": "1.0.0",
                "min_kernel_version": "0.0.1",
                "display_name": "Git Forge",
                "entrypoint": { "command": "bin/x" },
                "templates": [{ "id": "issue-development" }]
            });
            if let Some(schema) = input_schema {
                v["input_schema"] = schema;
            }
            Manifest::parse(&v.to_string()).expect("test plugin manifest")
        }

        fn schema(required: Value) -> Value {
            json!({
                "type": "object",
                "properties": {
                    "issue_url": { "type": "string" },
                    "merge_policy": {
                        "type": "string",
                        "enum": ["hold-for-ratify", "auto-merge"]
                    }
                },
                "required": required,
                "additionalProperties": false
            })
        }

        /// The route prefix is part of every 400 body; asserted on every arm so the wrapper
        /// cannot silently stop wrapping.
        const ROUTE_PREFIX: &str = "track create: ";

        fn expect_bad_request(owner: TemplateInputOwner<'_>, input: Option<&Value>, needle: &str) {
            match validate_template_input_binding(owner, input) {
                Err(CalmError::BadRequest(message)) => {
                    assert!(
                        message.starts_with(ROUTE_PREFIX),
                        "400 body `{message}` must keep the route prefix `{ROUTE_PREFIX}`"
                    );
                    assert!(message.contains(needle), "message `{message}` ∌ `{needle}`");
                }
                other => panic!("expected BadRequest containing `{needle}`, got {other:?}"),
            }
        }

        #[test]
        fn input_without_template_id_is_rejected() {
            expect_bad_request(
                TemplateInputOwner::NoTemplateId,
                Some(&json!({ "x": 1 })),
                "requires `template_id`",
            );
        }

        /// The other cause of "no owning Manifest": the roster admits the id but no running and
        /// trusted plugin declares it.
        #[test]
        fn input_with_a_template_whose_owner_is_not_running_names_that_cause() {
            let message = match validate_template_input_binding(
                TemplateInputOwner::NoBoundPlugin,
                Some(&json!({ "x": 1 })),
            ) {
                Err(CalmError::BadRequest(message)) => message,
                other => panic!("expected BadRequest, got {other:?}"),
            };
            assert!(message.starts_with(ROUTE_PREFIX), "{message}");
            assert!(message.contains("running and trusted"), "{message}");
            // `NoBoundPlugin` also covers a stopped/untrusted owner whose Manifest still declares
            // this template, so the clause must stay scoped to running and trusted.
            assert!(
                message.contains("no running and trusted plugin declares this template"),
                "the cause clause must be scoped to running ∧ trusted, not to all \
                 plugins — a stopped owner still declares the template: {message}"
            );
            // The discriminating half: it must NOT tell the caller to supply a
            // `template_id` they already supplied.
            assert!(
                !message.contains("requires `template_id`"),
                "a stopped owner must not be reported as a missing template_id: {message}"
            );
        }

        #[test]
        fn no_template_no_input_is_ok() {
            validate_template_input_binding(TemplateInputOwner::NoTemplateId, None)
                .expect("plain track create unchanged");
            // Same for an admitted template with no live owner: nothing to
            // validate, and nothing to refuse.
            validate_template_input_binding(TemplateInputOwner::NoBoundPlugin, None)
                .expect("unowned template without input is not an error");
        }

        #[test]
        fn input_against_schema_less_plugin_is_rejected_fail_closed() {
            let p = plugin(None);
            expect_bad_request(owned(&p), Some(&json!({ "x": 1 })), "does not declare");
            expect_bad_request(owned(&p), Some(&json!({ "x": 1 })), "plugin");
        }

        #[test]
        fn schema_less_binding_without_input_stays_valid() {
            let p = plugin(None);
            validate_template_input_binding(owned(&p), None).expect("bound create unchanged");
        }

        #[test]
        fn missing_input_with_required_schema_is_rejected() {
            let p = plugin(Some(schema(json!(["issue_url"]))));
            expect_bad_request(owned(&p), None, "requires `template_input`");
            expect_bad_request(owned(&p), None, "issue_url");
        }

        #[test]
        fn missing_input_with_no_required_fields_is_ok() {
            let p = plugin(Some(schema(json!([]))));
            validate_template_input_binding(owned(&p), None).expect("optional input omitted");
        }

        #[test]
        fn input_is_validated_against_the_plugin_schema() {
            let p = plugin(Some(schema(json!(["issue_url"]))));
            validate_template_input_binding(
                owned(&p),
                Some(&json!({ "issue_url": "u", "merge_policy": "auto-merge" })),
            )
            .expect("conforming input accepted");
            // missing required / extra key / enum still 400.
            expect_bad_request(
                owned(&p),
                Some(&json!({ "merge_policy": "auto-merge" })),
                "template_input.issue_url",
            );
            expect_bad_request(
                owned(&p),
                Some(&json!({ "issue_url": "u", "ghost": true })),
                "template_input.ghost",
            );
            expect_bad_request(
                owned(&p),
                Some(&json!({ "issue_url": "u", "merge_policy": "yolo" })),
                "template_input.merge_policy",
            );
        }
    }

    /// `admit_template` itself, asserted by data-pointer identity: the fixture is a freshly
    /// allocated `String` with identical bytes, so an equality assertion would discriminate nothing.
    mod admission {
        use std::path::Path;
        use std::sync::Arc;

        use axum::extract::FromRef;

        use crate::card_role_cache::CardRoleCache;
        use crate::db::sqlite::SqlxRepo;
        use crate::event::EventBus;
        use crate::plugin_host::{PluginHost, PluginRegistry};
        use crate::routes::tracks::admit_template;
        use crate::state::{AppState, CodexClient, DaemonClient, RouteState, WriteContext};
        use crate::track_area_cache::TrackAreaCache;
        use calm_truth::db::Repo;

        async fn route_state() -> RouteState {
            let repo = Arc::new(
                SqlxRepo::open("sqlite::memory:")
                    .await
                    .expect("open in-memory sqlite"),
            );
            let repo_dyn: Arc<dyn Repo> = repo.clone();
            let events = EventBus::new();
            let roles = CardRoleCache::new();
            let tracks = TrackAreaCache::new();
            let state = AppState::from_parts(
                repo_dyn.clone(),
                events.clone(),
                Arc::new(DaemonClient {
                    data_dir: std::env::temp_dir().join("calm-admit-template-test"),
                    proc_supervisor_sock: None,
                }),
                Arc::new(PluginHost::new_full(
                    Arc::new(PluginRegistry::empty()),
                    repo_dyn,
                    Path::new("").to_path_buf(),
                    std::env::temp_dir().join("calm-admit-template-plugin-test"),
                    Vec::new(),
                    events,
                    WriteContext::new(roles.clone(), tracks.clone()),
                )),
                Arc::new(CodexClient::new_stub()),
                Some(roles),
                Some(tracks),
            );
            RouteState::from_ref(&state)
        }

        #[tokio::test]
        async fn admission_key_is_the_rosters_own_borrow() {
            let s = route_state().await;
            for template in s.templates.entries() {
                let caller_spelling = String::from(template.key());
                assert_ne!(
                    caller_spelling.as_ptr(),
                    template.key().as_ptr(),
                    "the fixture must not accidentally be the roster's own buffer"
                );
                let admission = admit_template(&s, caller_spelling.as_str())
                    .await
                    .unwrap_or_else(|| panic!("`{}` must be admitted", template.key()));
                assert_eq!(
                    admission.key(),
                    template.key(),
                    "`{}`: admitted key changed spelling",
                    template.key()
                );
                assert!(
                    std::ptr::eq(admission.key().as_ptr(), template.key().as_ptr()),
                    "`{}`: `TemplateAdmission::key` must be the roster's own \
                     &'static str, not a value derived from the caller's string",
                    template.key()
                );
            }
            assert!(
                admit_template(&s, "missing-template").await.is_none(),
                "a non-roster id must not be admitted"
            );
        }
    }
}

#[cfg(test)]
mod report_boundaries_tests;
