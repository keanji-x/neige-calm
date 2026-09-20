//! `/api/settings` — app-global key/value settings. `null` and `""` both delete a key;
//! empty rows are never stored.

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

/// A malformed row (manual DB edit or older binary) fails closed to the boot-resolved default.
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

/// Wire-shape: a flat string map of key -> value; `BTreeMap` for deterministic ordering.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SettingsBag {
    pub settings: BTreeMap<String, String>,
}

/// Request body for `PUT /api/settings`. `null` and `""` both clear a key.
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
    // Validate every typed key before writing the first row, so an earlier unrelated key
    // cannot land before a later typed value returns 400.
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
        // Skip empty keys silently rather than persisting them.
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
                s.repo.settings_delete(key).await?;
            }
        }
    }
    if proxy_changed {
        cs.shared_codex_appserver.mark_needs_respawn();
    }
    if task_budget_changed {
        // Raising the default can release already-pending work without another domain event
        // to poke the scheduler. Lowering is harmless: the sweep never cancels in-flight work.
        let scheduler = worker.dispatcher.scheduler();
        tokio::spawn(async move { scheduler.sweep_all().await });
    }
    let rows = s.repo.settings_get_all().await?;
    Ok(Json(settings_bag(rows, s.task_budget_default)))
}

/// Snapshot of the first-class settings the kernel consumes; unknown keys are ignored here.
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
            // The route strips empty values, but guard anyway so a manual SQL edit can't sneak a `""` proxy in.
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

/// Pulls the snapshot in one shot; bound on `RepoRead` so route handlers can call it.
pub async fn load_settings(repo: &dyn crate::db::RepoRead) -> Result<Settings> {
    let pairs = repo.settings_get_all().await?;
    Ok(Settings::from_pairs(pairs))
}
