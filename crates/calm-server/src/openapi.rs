//! OpenAPI document aggregator: every route's `#[utoipa::path]` and every wire model's `ToSchema` registered here so
//! `GET /api/openapi.json` is one self-contained spec. WebSocket endpoints and plugin-host internal types are not included.

use crate::error::ErrorBody;
use crate::harness::HarnessPhaseTag;
use crate::model::{
    Area, AreaFolder, AreaKind, AreaPatch, AreaResolve, Card, CardPatch, CardRuntimeView,
    FolderConflict, FolderConflictKind, HarnessInputPresentation, HarnessInputSegment, HarnessItem,
    NewArea, NewAreaFolder, NewCard, NewOverlay, NewTrack, Overlay, Plugin, Terminal, Track,
    TrackConversationSummary, TrackDetail, TrackPatch, TrackWorkspacePatch,
};
use crate::report_backlinks::BacklinkQuote;
use crate::routes::area_folders::ResolveQuery;
use crate::routes::cards::{CreateCardBody, HarnessItemsQuery, ViaToolCall};
use crate::routes::claude_cards::NewClaudeCardBody;
use crate::routes::codex_cards::NewCodexCardBody;
use crate::routes::fs::{
    DirEntry, GitChangedFile, GitDiffResponse, GitStatusResponse, ListdirResponse, ReadFileResponse,
};
use crate::routes::models::{
    CatalogModel, DefaultSource, ModelDefaults, ModelSource, ModelsResponse, ReasoningEffortOption,
};
use crate::routes::overlays::{OverlayDeleteBody, OverlayQuery};
use crate::routes::planner_cards::{
    GetPlannerRunResponse, InterruptPlannerCardResponse, PlannerRunTokenUsage,
    ResetPlannerCardResponse,
};
use crate::routes::planner_input::{
    DeletePlannerInputBody, EditPlannerInputBody, PlannerInputMutationResponse,
    PlannerInputStaleBody, PlannerSteerConflictBody, PlannerSteerRefusedBody, PlannerSteerResponse,
    SteerPlannerInputBody,
};
use crate::routes::planner_input_send::{SendPlannerInputRequest, SendPlannerInputResponse};
use crate::routes::plugins::{
    InstallBody, InstallSource, PluginDetail, PluginListItem, ToolCallBody, ViewCatalogEntry,
    ViewSizeWire,
};
use crate::routes::settings::{SettingsBag, SettingsPutBody};
use crate::routes::terminal_cards::NewTerminalCardBody;
use crate::routes::threads::ThreadCardResolution;
use crate::routes::today::{TodayLaunchpad, TodayLaunchpadResolved};
use crate::routes::today_summary::TodaySummaryStarted;
use crate::routes::track_report_blocks::{
    CreateReportBlockBody, DeleteReportBlockBody, MoveReportBlockBody, ReportBlockWriteResponse,
    UpdateReportBlockBody,
};
use crate::routes::tracks::{
    CreateTrackRequest, TrackBacklink, TrackBacklinksResponse, TrackFsCatQuery, TrackFsLsQuery,
    TracksWindowQuery, UpdateTrackReportBody,
};
use crate::routes::version::VersionInfo;
use crate::track_fs_dto::{
    TrackFsCardMeta, TrackFsHookEvent, TrackFsRunDetail, TrackFsRunEventRef, TrackFsRunEvents,
    TrackFsRunIndexEntry, TrackFsRunStatus, TrackFsRunVerdict, TrackFsRunVerdictSummary,
};
use crate::track_fs_view::{TrackFsContent, TrackFsEntry};
use axum::http::{Method, StatusCode};
use utoipa::openapi::path::{Operation, ParameterIn};
use utoipa::openapi::security::{ApiKey, ApiKeyValue, SecurityRequirement, SecurityScheme};
use utoipa::openapi::{ContentBuilder, Ref, ResponseBuilder};
use utoipa::{Modify, OpenApi, ToSchema};

#[derive(OpenApi)]
#[openapi(
    modifiers(&DeclaredResponses),
    info(
        title = "calm-server",
        version = env!("CARGO_PKG_VERSION"),
        description = "Wire-format contract between calm-server (Rust) and web-calm (TS). Source of truth for generated TypeScript types.",
    ),
    paths(
        crate::auth::login_handler,
        crate::builtin_plugins::calendar::routes::list,
        crate::builtin_plugins::calendar::routes::read,
        crate::builtin_plugins::calendar::routes::create,
        crate::builtin_plugins::calendar::routes::update,
        crate::mobile_access::routes::status,
        crate::mobile_access::routes::enable,
        crate::mobile_access::routes::disable,
        crate::mobile_access::routes::tailnet_login,
        crate::mobile_access::routes::tailnet_logout,
        crate::mobile_access::enrollment_routes::create,
        crate::mobile_access::enrollment_routes::cancel,
        crate::mobile_access::enrollment_routes::status,
        crate::mobile_access::enrollment_routes::claim,
        crate::mobile_access::enrollment_routes::redeem,
        crate::mobile_access::routes::create,
        crate::mobile_access::routes::approve,
        crate::mobile_access::routes::revoke,
        crate::mobile_access::routes::claim,
        crate::mobile_access::routes::redeem,
        crate::routes::areas::list_areas,
        crate::routes::areas::create_area,
        crate::routes::areas::get_or_create_system_area,
        crate::routes::areas::update_area,
        crate::routes::areas::delete_area,
        crate::routes::area_folders::list_folders,
        crate::routes::area_folders::create_folder,
        crate::routes::area_folders::delete_folder,
        crate::routes::area_folders::resolve_path,
        crate::mentions::list_mentions,
        crate::routes::task_recovery::get_attempts,
        crate::routes::track_templates::list_track_templates,
        crate::routes::track_templates::get_track_template,
        crate::routes::track_templates::get_template_plugin_guides,
        crate::routes::track_recipes::list_recipes,
        crate::routes::track_recipes::get_recipe,
        crate::routes::track_recipes::create_recipe,
        crate::routes::track_recipes::update_recipe,
        crate::routes::track_recipes::delete_recipe,
        crate::routes::track_conversations::list_track_conversations,
        crate::routes::track_conversations::create_track_conversation,
        crate::routes::tracks::list_tracks_by_area,
        crate::routes::tracks::list_tracks_window,
        crate::routes::tracks::get_track_detail,
        crate::routes::tracks::create_track,
        crate::routes::tracks::update_track,
        crate::routes::tracks::delete_track,
        crate::routes::tracks::get_track_backlinks,
        crate::routes::activity_dismissals::dismiss_activity_item,
        crate::routes::track_asks::answer_ask,
        crate::routes::tracks::update_track_report,
        crate::routes::tracks::get_track_report,
        crate::routes::track_report_blocks::create_block,
        crate::routes::track_report_blocks::update_block,
        crate::routes::track_report_blocks::delete_block,
        crate::routes::track_report_blocks::move_block,
        crate::routes::track_report_series::get_report_series,
        crate::routes::track_previews::list_track_previews,
        crate::routes::track_sources::list_track_sources,
        crate::routes::track_sources::get_track_source,
        crate::routes::tracks::list_track_files,
        crate::routes::tracks::cat_track_file,
        crate::routes::daily_planner::resolve_daily_track,
        crate::routes::daily_planner::report_changes,
        crate::routes::daily_planner::report_edits,
        crate::routes::today::ensure_today_launchpad,
        crate::routes::today::resolve_today_launchpad,
        crate::routes::today::reset_today_launchpad_report,
        crate::routes::today_summary::write_today_summary,
        crate::routes::cards::list_cards_by_track,
        crate::routes::cards::create_card,
        crate::routes::cards::update_card,
        crate::routes::cards::get_harness_items,
        crate::routes::harness_live::get_harness_live,
        crate::routes::planner_input_send::send_planner_input,
        crate::routes::planner_cards::interrupt_planner_card,
        crate::routes::planner_compact::compact_planner_card,
        crate::routes::planner_cards::get_planner_run,
        crate::routes::planner_cards::reset_planner_card,
        crate::routes::planner_cards::restart_planner_card,
        // This list is hand-maintained: omitting a handler is not a compile error and not a drift failure — the endpoint simply never reaches either generated client.
        crate::routes::planner_input::edit_planner_input,
        crate::routes::planner_input::delete_planner_input,
        crate::routes::planner_input::steer_planner_input,
        crate::routes::planner_model::set_planner_model,
        crate::routes::cards::delete_card,
        crate::routes::overlays::list_overlays,
        crate::routes::overlays::upsert_overlay,
        crate::routes::overlays::delete_overlay,
        crate::routes::terminal_cards::create_terminal_card,
        crate::routes::terminal::get_terminal_for_card,
        crate::routes::codex_cards::create_codex_card,
        crate::routes::threads::resolve_card_for_thread,
        crate::routes::claude_cards::create_claude_card,
        crate::routes::claude_cards::restart_claude_card,
        crate::routes::fs::listdir,
        crate::routes::fs::readfile,
        crate::routes::fs::readfile_raw,
        crate::routes::fs::read_track_workspace_file,
        crate::routes::fs::read_track_workspace_file_raw,
        crate::routes::fs::gitstatus,
        crate::routes::fs::gitdiff,
        crate::routes::settings::get_settings,
        crate::routes::settings::put_settings,
        crate::routes::plugins::list_plugins,
        crate::routes::plugins::get_plugin_detail,
        crate::routes::plugins::install_plugin,
        crate::routes::plugins::check_mcp_connection,
        crate::routes::plugins::uninstall_plugin,
        crate::routes::plugins::enable_plugin,
        crate::routes::plugins::disable_plugin,
        crate::routes::plugins::patch_plugin_config,
        crate::routes::plugins::reload_plugin,
        crate::routes::plugins::rotate_plugin_token,
        crate::routes::plugins::tail_plugin_log,
        crate::routes::plugins::list_plugin_views,
        crate::routes::plugins::get_plugin_view_html,
        crate::routes::plugins::plugin_tool_call,
        crate::routes::models::list_models,
        crate::routes::agent_providers::list_agent_providers,
        crate::planner_attachments::routes::upload_planner_attachment,
        crate::planner_attachments::routes::read_planner_attachment,
        crate::routes::version::get_version,
    ),
    components(schemas(
        crate::mobile_access::MobileStatus,
        calm_types::mobile_access::MobileProvider,
        calm_types::tailnet::TailnetStatus,
        calm_types::tailnet::TailnetPhase,
        calm_types::tailnet::TailnetNodeState,
        calm_types::tailnet::TailnetLogin,
        calm_types::enrollment::EnrollmentCreated,
        calm_types::enrollment::EnrollmentClaim,
        calm_types::enrollment::EnrollmentClaimed,
        calm_types::enrollment::EnrollmentRedeem,
        calm_types::enrollment::EnrollmentRedeemed,
        calm_types::enrollment::EnrollmentCleanup,
        calm_types::mobile_access::PendingPair,
        calm_types::mobile_access::PairedDevice,
        calm_types::mobile_access::PairingCreated,
        calm_types::mobile_access::PairingClaim,
        calm_types::mobile_access::PairingClaimed,
        calm_types::mobile_access::PairingRedeem,
        crate::mobile_access::routes::MobileAction,
        Area,
        AreaKind,
        NewArea,
        AreaPatch,
        AreaFolder,
        NewAreaFolder,
        AreaResolve,
        FolderConflict,
        FolderConflictKind,
        ResolveQuery,
        Track,
        NewTrack,
        CreateTrackRequest,
        TrackPatch,
        TrackWorkspacePatch,
        TodayLaunchpad,
        TodayLaunchpadResolved,
        crate::routes::today::TodayLaunchpadReportReset,
        TodaySummaryStarted,
        TracksWindowQuery,
        TrackFsLsQuery,
        TrackFsCatQuery,
        TrackFsEntry,
        TrackFsContent,
        TrackBacklink,
        BacklinkQuote,
        TrackBacklinksResponse,
        crate::routes::track_templates::TrackTemplate,
        crate::routes::track_templates::TrackTemplateDetail,
        crate::routes::track_templates::TemplatePluginGuide,
        calm_types::model::TrackRecipe,
        crate::routes::track_recipes::CreateRecipeBody,
        crate::routes::track_recipes::UpdateRecipeBody,
        crate::routes::track_templates::TrackTemplateTask,
        TrackFsCardMeta,
        TrackFsRunStatus,
        TrackFsRunVerdictSummary,
        TrackFsRunVerdict,
        TrackFsRunIndexEntry,
        TrackFsRunEventRef,
        TrackFsRunEvents,
        TrackFsRunDetail,
        TrackFsHookEvent,
        UpdateTrackReportBody,
        CreateReportBlockBody,
        UpdateReportBlockBody,
        DeleteReportBlockBody,
        MoveReportBlockBody,
        ReportBlockWriteResponse,
        crate::routes::track_report_series::ReportSeriesDetail,
        crate::routes::track_report_series::ReportSeriesEntry,
        crate::routes::track_report_series::ReportSeriesResolved,
        crate::routes::track_report_series::ReportSeriesRevConflict,
        crate::routes::track_previews::TrackPreview,
        crate::routes::track_previews::TrackPreviews,
        calm_types::report_sources::SourceProvenance,
        calm_types::report_sources::SourceOrigin,
        calm_types::report_sources::SourceQuote,
        calm_types::report_sources::TrackSourceSummary,
        calm_types::report_sources::TrackSourceDetail,
        calm_types::report_sources::TrackSourceList,
        calm_types::mentions::MentionCandidates,
        calm_types::mentions::TagMention,
        calm_types::mentions::TrackMention,
        calm_types::mentions::BlockMention,
        TrackDetail,
        Card,
        CardRuntimeView,
        NewCard,
        CardPatch,
        HarnessInputPresentation,
        HarnessInputSegment,
        calm_types::model::HarnessInputOrigin,
        calm_types::planner_attachment::AttachmentId,
        calm_types::planner_attachment::PlannerAttachment,
        calm_types::planner_attachment::UploadAttachmentResponse,
        HarnessItem,
        HarnessItemsQuery,
        calm_types::harness::HarnessLiveReplies,
        calm_types::harness::HarnessLiveReply,
        SendPlannerInputRequest,
        SendPlannerInputResponse,
        TrackConversationSummary,
        crate::routes::track_conversations::NewTrackConversationBody,
        crate::side_conversation::SideConversation,
        InterruptPlannerCardResponse,
        crate::routes::planner_compact::CompactPlannerResponse,
        GetPlannerRunResponse,
        PlannerRunTokenUsage,
        EditPlannerInputBody,
        DeletePlannerInputBody,
        PlannerInputMutationResponse,
        PlannerInputStaleBody,
        SteerPlannerInputBody,
        PlannerSteerResponse,
        PlannerSteerRefusedBody,
        PlannerSteerConflictBody,
        HarnessPhaseTag,
        ResetPlannerCardResponse,
        crate::track_report::TrackReportPayload,
        crate::routes::activity_dismissals::DismissActivityItemRequest,
        crate::routes::track_asks::AnswerAskRequest,
        Overlay,
        NewOverlay,
        Terminal,
        Plugin,
        CreateCardBody,
        ViaToolCall,
        NewTerminalCardBody,
        NewCodexCardBody,
        ThreadCardResolution,
        NewClaudeCardBody,
        DirEntry,
        ListdirResponse,
        ReadFileResponse,
        GitChangedFile,
        GitStatusResponse,
        GitDiffResponse,
        SettingsBag,
        SettingsPutBody,
        OverlayQuery,
        OverlayDeleteBody,
        InstallBody,
        InstallSource,
        PluginDetail,
        PluginListItem,
        ToolCallBody,
        ViewCatalogEntry,
        ViewSizeWire,
        VersionInfo,
        crate::routes::planner_model::SetPlannerModelBody,
        crate::routes::planner_model::SetPlannerModelResponse,
        CatalogModel,
        ReasoningEffortOption,
        ModelDefaults,
        DefaultSource,
        ModelSource,
        ModelsResponse,
        crate::agent_providers::ProviderAvailability,
        crate::agent_providers::ProviderStatus,
        crate::routes::theme::RequestTheme,
        ErrorBody,
    )),
    tags(
        (name = "auth", description = "Owner password login"),
        (name = "areas", description = "Area CRUD"),
        (name = "area_folders", description = "Area ↔ folder mapping: claim filesystem paths for an area, resolve a cwd to its owning area"),
        (name = "tracks", description = "Track CRUD + composite detail"),
        (name = "cards", description = "Card CRUD"),
        (name = "overlays", description = "Plugin-rendered overlays attached to tracks/cards"),
        (name = "terminals", description = "PTY-backed terminal cards"),
        (name = "codex", description = "Codex (OpenAI) agent cards — hook-driven event stream"),
        (name = "threads", description = "Internal codex thread resolution"),
        (name = "claude", description = "Claude worker cards — hook-driven event stream"),
        (name = "fs", description = "Read-only host filesystem helpers (directory listing for path pickers)"),
        (name = "settings", description = "App-global settings (HTTP proxy override, etc.)"),
        (name = "plugins", description = "Plugin lifecycle, config, MCP fan-out"),
        (name = "models", description = "Codex model catalog and the default model/reasoning-effort this installation follows"),
        (name = "agent_providers", description = "Whether each Planner provider (Codex, Claude) can run right now, and why not"),
        (name = "version", description = "Kernel, REST, sync, and MCP protocol versions"),
    ),
)]
pub struct ApiDoc;

/// Adds the answers an operation gives because of what it declares, so no annotation lists them by
/// hand: a JSON request body brings `JsonBody`'s rejections ([`crate::extract::JSON_BODY_REJECTIONS`]),
/// a path or query parameter brings `Path`'s and `Query`'s ([`crate::extract::PARAM_REJECTION`]),
/// and requiring the session brings `require_session`'s refusals ([`crate::auth::NO_SESSION`], and
/// [`crate::auth::CROSS_ORIGIN_WRITE`] on a write). The session is required of every operation that
/// does not declare `security(())`. A status the annotation already describes keeps its own
/// description. `tests/cases/openapi_statuses.rs` pins each premise: a JSON request body is
/// declared exactly where the handler takes `JsonBody`, a path or query parameter exactly where it
/// takes `Path` or `Query`, and `security(())` exactly where the router does not apply
/// `require_session`.
struct DeclaredResponses;

impl Modify for DeclaredResponses {
    fn modify(&self, doc: &mut utoipa::openapi::OpenApi) {
        doc.components
            .get_or_insert_with(Default::default)
            .add_security_scheme(
                crate::auth::SESSION_SCHEME,
                SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::new(
                    crate::auth::SESSION_COOKIE,
                ))),
            );
        let required = [SecurityRequirement::new(
            crate::auth::SESSION_SCHEME,
            Vec::<String>::new(),
        )];
        doc.security = Some(required.to_vec());
        for item in doc.paths.paths.values_mut() {
            for (method, operation) in [
                (Method::GET, &mut item.get),
                (Method::PUT, &mut item.put),
                (Method::POST, &mut item.post),
                (Method::DELETE, &mut item.delete),
                (Method::OPTIONS, &mut item.options),
                (Method::HEAD, &mut item.head),
                (Method::PATCH, &mut item.patch),
                (Method::TRACE, &mut item.trace),
            ] {
                let Some(operation) = operation else { continue };
                let json_body = operation
                    .request_body
                    .as_ref()
                    .is_some_and(|body| body.content.contains_key("application/json"));
                let params = operation.parameters.as_ref().is_some_and(|parameters| {
                    parameters.iter().any(|parameter| {
                        matches!(
                            parameter.parameter_in,
                            ParameterIn::Path | ParameterIn::Query
                        )
                    })
                });
                if json_body {
                    for answer in crate::extract::JSON_BODY_REJECTIONS {
                        // One 400 answers both; say so where both can give it.
                        let answer = match answer {
                            (StatusCode::BAD_REQUEST, _) if params => (
                                StatusCode::BAD_REQUEST,
                                "`bad_request`: the body is not parseable JSON, or a path or query \
                                 parameter does not parse.",
                            ),
                            answer => answer,
                        };
                        add_error_response(operation, answer);
                    }
                }
                if params {
                    add_error_response(operation, crate::extract::PARAM_REJECTION);
                }
                // An operation's own `security` replaces the document's; `security(())` is the one
                // empty requirement, which lets a request through without the session.
                let session = operation.security.as_ref().is_none_or(|anyof| {
                    !anyof.is_empty() && !anyof.contains(&SecurityRequirement::default())
                });
                if session {
                    add_error_response(operation, crate::auth::NO_SESSION);
                    if crate::auth::checks_origin(&method) {
                        add_error_response(operation, crate::auth::CROSS_ORIGIN_WRITE);
                    }
                }
            }
        }
    }
}

/// An `ErrorBody` answer under `status`, unless the operation already describes that status.
fn add_error_response(operation: &mut Operation, (status, description): (StatusCode, &str)) {
    operation
        .responses
        .responses
        .entry(status.as_u16().to_string())
        .or_insert_with(|| {
            ResponseBuilder::new()
                .description(description)
                .content(
                    "application/json",
                    ContentBuilder::new()
                        .schema(Some(Ref::from_schema_name(ErrorBody::name())))
                        .build(),
                )
                .build()
                .into()
        });
}
