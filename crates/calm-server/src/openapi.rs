//! OpenAPI document aggregator. We register every route's
//! `#[utoipa::path]` attribute and every wire model's `ToSchema` derive
//! here so `GET /api/openapi.json` returns a single self-contained spec
//! the frontend consumes to generate TypeScript types.
//!
//! The document is the source-of-truth contract between `calm-server` and
//! `web-calm` — adding a new public model or route means adding a path
//! entry below alongside the handler annotation. The aggregator does not
//! pull in WebSocket endpoints (those don't roundtrip JSON request/response
//! pairs and aren't part of the wire-types contract) nor any plugin-host
//! internal types.

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
use crate::routes::cards::{
    CreateCardBody, GetPlannerRunResponse, HarnessItemsQuery, InterruptPlannerCardResponse,
    PlannerRunTokenUsage, ResetPlannerCardResponse, SendPlannerInputRequest,
    SendPlannerInputResponse, ViaToolCall,
};
use crate::routes::claude_cards::NewClaudeCardBody;
use crate::routes::codex_cards::NewCodexCardBody;
use crate::routes::fs::{
    DirEntry, GitChangedFile, GitDiffResponse, GitStatusResponse, ListdirResponse, ReadFileResponse,
};
use crate::routes::overlays::{OverlayDeleteBody, OverlayQuery};
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
use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "calm-server",
        version = env!("CARGO_PKG_VERSION"),
        description = "Wire-format contract between calm-server (Rust) and web-calm (TS). Source of truth for generated TypeScript types.",
    ),
    paths(
        // ---- areas ----
        crate::routes::areas::list_areas,
        crate::routes::areas::create_area,
        crate::routes::areas::get_or_create_system_area,
        crate::routes::areas::update_area,
        crate::routes::areas::delete_area,
        // ---- area_folders (#250 PR 1) ----
        crate::routes::area_folders::list_folders,
        crate::routes::area_folders::create_folder,
        crate::routes::area_folders::delete_folder,
        crate::routes::area_folders::resolve_path,
        // ---- tracks ----
        crate::routes::track_templates::list_track_templates,
        crate::routes::track_recipes::list_recipes,
        crate::routes::track_recipes::get_recipe,
        crate::routes::track_recipes::create_recipe,
        crate::routes::track_recipes::update_recipe,
        crate::routes::track_recipes::delete_recipe,
        // ---- track conversations (#1189) ----
        crate::routes::track_conversations::list_track_conversations,
        crate::routes::track_conversations::create_track_conversation,
        crate::routes::tracks::list_tracks_by_area,
        crate::routes::tracks::list_tracks_window,
        crate::routes::tracks::get_track_detail,
        crate::routes::tracks::create_track,
        crate::routes::tracks::update_track,
        crate::routes::tracks::delete_track,
        crate::routes::tracks::get_track_backlinks,
        // Issue #247 PR3 — user-facing track-report edit endpoint
        crate::routes::tracks::update_track_report,
        crate::routes::tracks::get_track_report,
        crate::routes::track_report_blocks::create_block,
        crate::routes::track_report_blocks::update_block,
        crate::routes::track_report_blocks::delete_block,
        crate::routes::track_report_blocks::move_block,
        crate::routes::tracks::list_track_files,
        crate::routes::tracks::cat_track_file,
        crate::routes::today::ensure_today_launchpad,
        crate::routes::today::resolve_today_launchpad,
        crate::routes::today::reset_today_launchpad_report,
        crate::routes::today_summary::write_today_summary,
        // ---- cards ----
        crate::routes::cards::list_cards_by_track,
        crate::routes::cards::create_card,
        crate::routes::cards::update_card,
        crate::routes::cards::get_harness_items,
        crate::routes::cards::send_planner_input,
        crate::routes::cards::ratify_card,
        crate::routes::cards::interrupt_planner_card,
        crate::routes::cards::get_planner_run,
        crate::routes::cards::reset_planner_card,
        crate::routes::cards::delete_card,
        // ---- overlays ----
        crate::routes::overlays::list_overlays,
        crate::routes::overlays::upsert_overlay,
        crate::routes::overlays::delete_overlay,
        // ---- terminals ----
        crate::routes::terminal_cards::create_terminal_card,
        crate::routes::terminal::get_terminal_for_card,
        // ---- codex ----
        crate::routes::codex_cards::create_codex_card,
        crate::routes::threads::resolve_card_for_thread,
        // ---- claude ----
        crate::routes::claude_cards::create_claude_card,
        crate::routes::claude_cards::restart_claude_card,
        // ---- fs ----
        crate::routes::fs::listdir,
        crate::routes::fs::readfile,
        crate::routes::fs::readfile_raw,
        crate::routes::fs::gitstatus,
        crate::routes::fs::gitdiff,
        // ---- settings ----
        crate::routes::settings::get_settings,
        crate::routes::settings::put_settings,
        // ---- plugins ----
        crate::routes::plugins::list_plugins,
        crate::routes::plugins::get_plugin_detail,
        crate::routes::plugins::install_plugin,
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
        // ---- version ----
        crate::routes::version::get_version,
    ),
    components(schemas(
        // domain models
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
        // Issue #247 PR3 — request body for `POST /api/tracks/:id/report`
        UpdateTrackReportBody,
        CreateReportBlockBody,
        UpdateReportBlockBody,
        DeleteReportBlockBody,
        MoveReportBlockBody,
        ReportBlockWriteResponse,
        TrackDetail,
        Card,
        CardRuntimeView,
        NewCard,
        CardPatch,
        HarnessInputPresentation,
        HarnessInputSegment,
        HarnessItem,
        HarnessItemsQuery,
        SendPlannerInputRequest,
        SendPlannerInputResponse,
        TrackConversationSummary,
        crate::routes::track_conversations::NewTrackConversationBody,
        InterruptPlannerCardResponse,
        GetPlannerRunResponse,
        PlannerRunTokenUsage,
        HarnessPhaseTag,
        ResetPlannerCardResponse,
        // Issue #229 PR B — track-report card payload shape (kernel-owned;
        // surfaced in the OpenAPI doc so frontend codegen + external
        // consumers see the v1 contract).
        crate::track_report::TrackReportPayload,
        Overlay,
        NewOverlay,
        Terminal,
        Plugin,
        // route-local DTOs
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
        // #177 — required theme field on card/track creation DTOs
        crate::routes::theme::RequestTheme,
        // shared error response
        ErrorBody,
    )),
    tags(
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
        (name = "version", description = "Kernel, REST, sync, and MCP protocol versions"),
    ),
)]
pub struct ApiDoc;
