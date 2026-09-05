//! `/api/settings` — app-global key/value settings.
//!
//! The Settings page in the UI reads the whole bag with `GET /api/settings`
//! and writes back the full edited bag with `PUT /api/settings`. There's no
//! per-key DELETE / PATCH; the bag is small (a handful of keys at most) and
//! "send the whole form" is simpler than diffing on the client.
//!
//! ## Empty-string semantics
//!
//! On the wire we model values as `Option<String>` so the client can either
//! omit a key entirely or send it explicitly as `null` / `""`. On the
//! write boundary here:
//!
//!   * `null` — delete the key (clear the override).
//!   * `""` (empty string) — delete the key (same as null; an empty proxy
//!     is the same as "use container defaults").
//!   * Non-empty value — upsert.
//!
//! This keeps the codex spawn reader simple: "if the key isn't in the bag,
//! don't override the env." We never store empty rows, so the reader never
//! has to decide whether `""` means "disable" vs "default".
//!
//! ## First-class keys
//!
//! `http_proxy`, `https_proxy`, and `task_budget_default` are the first-class
//! keys the kernel actively reads. The schema is intentionally open: any
//! string key/value pair is allowed, so future settings can land without a
//! wire-level migration.

use crate::error::{CalmError, ErrorBody, Result};
use crate::state::{AppState, CodexShellState, RouteState, WorkerState};
use axum::{Json, Router, extract::State, routing::get};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/settings", get(get_settings).put(put_settings))
}

/// Persisted override for the default number of concurrently admitted tasks
/// per track. A nullable `tracks.task_budget` remains the per-track override.
pub const TASK_BUDGET_DEFAULT_KEY: &str = "task_budget_default";

fn parse_task_budget_default(value: &str) -> Option<i64> {
    value.trim().parse::<i64>().ok().filter(|value| *value > 0)
}

/// A malformed row can only arrive through a manual database edit or an older
/// binary. Fail closed to the boot-resolved deployment default instead of
/// letting it disable scheduling or inflate the budget unpredictably.
pub(crate) fn effective_task_budget_default(value: Option<&str>, fallback: i64) -> i64 {
    value
        .and_then(parse_task_budget_default)
        .unwrap_or(fallback)
}

fn settings_bag(rows: Vec<(String, String)>, task_budget_fallback: i64) -> SettingsBag {
    let mut settings: BTreeMap<_, _> = rows.into_iter().collect();
    let effective = effective_task_budget_default(
        settings.get(TASK_BUDGET_DEFAULT_KEY).map(String::as_str),
        task_budget_fallback,
    );
    settings.insert(TASK_BUDGET_DEFAULT_KEY.into(), effective.to_string());
    SettingsBag { settings }
}

/// Wire-shape: a flat string map of key -> value. We use `BTreeMap` for
/// deterministic ordering in the response so the OpenAPI spec consumers
/// see stable test diffs.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SettingsBag {
    pub settings: BTreeMap<String, String>,
}

/// Request body for `PUT /api/settings`. Values are `Option<String>` so
/// the client can clear a key by sending `null`. Empty strings are also
/// treated as deletes; see module docs for the rationale.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SettingsPutBody {
    #[serde(default)]
    pub settings: BTreeMap<String, Option<String>>,
}

#[utoipa::path(
    get,
    path = "/api/settings",
    tag = "settings",
    responses(
        (status = 200, description = "Current settings map (string→string)", body = SettingsBag),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn get_settings(State(s): State<RouteState>) -> Result<Json<SettingsBag>> {
    let rows = s.repo.settings_get_all().await?;
    Ok(Json(settings_bag(rows, s.task_budget_default)))
}

#[utoipa::path(
    put,
    path = "/api/settings",
    tag = "settings",
    request_body = SettingsPutBody,
    responses(
        (status = 200, description = "Settings replaced; returns the resulting bag", body = SettingsBag),
        (status = 400, description = "Invalid first-class setting", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn put_settings(
    State(s): State<RouteState>,
    State(cs): State<CodexShellState>,
    State(worker): State<WorkerState>,
    Json(p): Json<SettingsPutBody>,
) -> Result<Json<SettingsBag>> {
    // Validate every typed key before writing the first row. The KV endpoint
    // accepts an arbitrary bag, so validation inside the write loop would let
    // an earlier unrelated key land before a later typed value returns 400.
    if let Some(Some(value)) = p.settings.get(TASK_BUDGET_DEFAULT_KEY)
        && !value.is_empty()
        && parse_task_budget_default(value).is_none()
    {
        return Err(CalmError::BadRequest(format!(
            "{TASK_BUDGET_DEFAULT_KEY} must be a positive integer (got {value:?})"
        )));
    }
    let before = load_settings(s.repo.as_ref()).await?;
    let mut proxy_changed = false;
    let mut task_budget_changed = false;
    for (key, maybe_val) in p.settings.iter() {
        // Skip empty keys silently — a malformed JSON object with "" keys
        // shouldn't break the call; we just refuse to persist them.
        if key.is_empty() {
            continue;
        }
        match key.as_str() {
            "http_proxy" | "HTTP_PROXY" => {
                let next = maybe_val.as_deref().filter(|v| !v.is_empty());
                if before.http_proxy.as_deref() != next {
                    proxy_changed = true;
                }
            }
            "https_proxy" | "HTTPS_PROXY" => {
                let next = maybe_val.as_deref().filter(|v| !v.is_empty());
                if before.https_proxy.as_deref() != next {
                    proxy_changed = true;
                }
            }
            TASK_BUDGET_DEFAULT_KEY => {
                let next = maybe_val
                    .as_deref()
                    .filter(|value| !value.is_empty())
                    .and_then(parse_task_budget_default);
                if before.task_budget_default != next {
                    task_budget_changed = true;
                }
            }
            _ => {}
        }
        match maybe_val.as_deref() {
            Some(v) if !v.is_empty() => {
                s.repo.settings_upsert(key, v).await?;
            }
            _ => {
                // None or empty string → clear.
                s.repo.settings_delete(key).await?;
            }
        }
    }
    if proxy_changed {
        cs.shared_codex_appserver.mark_needs_respawn();
    }
    if task_budget_changed {
        // Raising the default can release already-pending work without another
        // domain event to poke the scheduler. Lowering is harmless here: the
        // sweep never cancels in-flight work and its claim transaction applies
        // the new budget before admitting anything else.
        let scheduler = worker.dispatcher.scheduler();
        tokio::spawn(async move { scheduler.sweep_all().await });
    }
    let rows = s.repo.settings_get_all().await?;
    Ok(Json(settings_bag(rows, s.task_budget_default)))
}

/// Internal helper: snapshot the first-class settings the kernel consumes.
/// Unknown keys stay persisted in the wire bag but are ignored here.
#[derive(Debug, Default, Clone)]
pub struct Settings {
    pub http_proxy: Option<String>,
    pub https_proxy: Option<String>,
    pub task_budget_default: Option<i64>,
}

impl Settings {
    pub fn from_pairs(pairs: Vec<(String, String)>) -> Self {
        let mut out = Settings::default();
        for (k, v) in pairs {
            // Empty values should never make it into the table (the route
            // strips them) but guard anyway so a manual SQL edit can't
            // sneak a `""` proxy in.
            if v.is_empty() {
                continue;
            }
            match k.as_str() {
                "http_proxy" | "HTTP_PROXY" => out.http_proxy = Some(v),
                "https_proxy" | "HTTPS_PROXY" => out.https_proxy = Some(v),
                TASK_BUDGET_DEFAULT_KEY => out.task_budget_default = parse_task_budget_default(&v),
                _ => {}
            }
        }
        out
    }
}

/// Async helper used by `routes::codex` — pulls the snapshot in one shot.
/// Bound on the narrow `RepoRead` trait so the helper can be invoked from
/// route handlers via the `AppState::repo` handle (which is a `RouteRepo`,
/// transitively a `RepoRead`).
pub async fn load_settings(repo: &dyn crate::db::RepoRead) -> Result<Settings> {
    let pairs = repo.settings_get_all().await?;
    Ok(Settings::from_pairs(pairs))
}
