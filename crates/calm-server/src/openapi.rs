//! OpenAPI document aggregator. We register every route's
//! `#[utoipa::path]` attribute and every wire model's `ToSchema` derive
//! here so `GET /api/openapi.json` returns a single self-contained spec
//! the frontend consumes to generate TypeScript types.
//!
//! The spec is the source-of-truth contract between `calm-server` and
//! `web-calm` — adding a new public model or route means adding a path
//! entry below alongside the handler annotation. The aggregator does not
//! pull in WebSocket endpoints (those don't roundtrip JSON request/response
//! pairs and aren't part of the wire-types contract) nor any plugin-host
//! internal types.

use crate::error::ErrorBody;
use crate::model::Terminal;
use crate::model::{
    Card, CardPatch, Cove, CoveFolder, CoveKind, CovePatch, CoveResolve, FolderConflict,
    FolderConflictKind, NewCard, NewCove, NewCoveFolder, NewOverlay, NewWave, Overlay, Plugin,
    Wave, WaveDetail, WavePatch,
};
use crate::routes::cards::{CreateCardBody, ViaToolCall};
use crate::routes::codex_cards::NewCodexCardBody;
use crate::routes::cove_folders::ResolveQuery;
use crate::routes::fs::{DirEntry, ListdirResponse};
use crate::routes::overlays::{OverlayDeleteBody, OverlayQuery};
use crate::routes::plugins::{
    InstallBody, InstallSource, PluginDetail, PluginListItem, ToolCallBody, ViewCatalogEntry,
    ViewSizeWire,
};
use crate::routes::settings::{SettingsBag, SettingsPutBody};
use crate::routes::terminal_cards::NewTerminalCardBody;
use crate::routes::version::VersionInfo;
use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "calm-server",
        version = env!("CARGO_PKG_VERSION"),
        description = "Wire-format contract between calm-server (Rust) and web-calm (TS). Source of truth for generated TypeScript types.",
    ),
    paths(
        // ---- coves ----
        crate::routes::coves::list_coves,
        crate::routes::coves::create_cove,
        crate::routes::coves::get_or_create_system_cove,
        crate::routes::coves::update_cove,
        crate::routes::coves::delete_cove,
        // ---- cove_folders (#250 PR 1) ----
        crate::routes::cove_folders::list_folders,
        crate::routes::cove_folders::create_folder,
        crate::routes::cove_folders::delete_folder,
        crate::routes::cove_folders::resolve_path,
        // ---- waves ----
        crate::routes::waves::list_waves_by_cove,
        crate::routes::waves::get_wave_detail,
        crate::routes::waves::create_wave,
        crate::routes::waves::update_wave,
        crate::routes::waves::delete_wave,
        // ---- cards ----
        crate::routes::cards::list_cards_by_wave,
        crate::routes::cards::create_card,
        crate::routes::cards::update_card,
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
        // ---- fs ----
        crate::routes::fs::listdir,
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
        Cove,
        CoveKind,
        NewCove,
        CovePatch,
        CoveFolder,
        NewCoveFolder,
        CoveResolve,
        FolderConflict,
        FolderConflictKind,
        ResolveQuery,
        Wave,
        NewWave,
        WavePatch,
        WaveDetail,
        Card,
        NewCard,
        CardPatch,
        // Issue #229 PR B — wave-report card payload shape (kernel-owned;
        // surfaced in the OpenAPI doc so frontend codegen + external
        // consumers see the v1 contract).
        crate::wave_report::WaveReportPayload,
        Overlay,
        NewOverlay,
        Terminal,
        Plugin,
        // route-local DTOs
        CreateCardBody,
        ViaToolCall,
        NewTerminalCardBody,
        NewCodexCardBody,
        DirEntry,
        ListdirResponse,
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
        // shared error response
        ErrorBody,
    )),
    tags(
        (name = "coves", description = "Cove CRUD"),
        (name = "cove_folders", description = "Cove ↔ folder mapping: claim filesystem paths for a cove, resolve a cwd to its owning cove"),
        (name = "waves", description = "Wave CRUD + composite detail"),
        (name = "cards", description = "Card CRUD"),
        (name = "overlays", description = "Plugin-rendered overlays attached to waves/cards"),
        (name = "terminals", description = "PTY-backed terminal cards"),
        (name = "codex", description = "Codex (OpenAI) agent cards — hook-driven event stream"),
        (name = "fs", description = "Read-only host filesystem helpers (directory listing for path pickers)"),
        (name = "settings", description = "App-global settings (HTTP proxy override, etc.)"),
        (name = "plugins", description = "Plugin lifecycle, config, MCP fan-out"),
        (name = "version", description = "Kernel, REST, sync, and MCP protocol versions"),
    ),
)]
pub struct ApiDoc;
