//! `/api/settings` — app-global key/value settings. `null` and `""` both delete a key;
//! empty rows are never stored.

use crate::error::{ErrorBody, Result};
use crate::extract::JsonBody;
use crate::state::{AppState, CodexShellState, RouteState};
use axum::{Json, Router, extract::State, routing::get};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/settings", get(get_settings).put(put_settings))
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
    Ok(Json(SettingsBag {
        settings: rows.into_iter().collect(),
    }))
}

#[utoipa::path(
    put,
    path = "/api/settings",
    tag = "settings",
    request_body = SettingsPutBody,
    responses(
        (status = 200, description = "Settings replaced; returns the resulting bag", body = SettingsBag),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn put_settings(
    State(s): State<RouteState>,
    State(cs): State<CodexShellState>,
    JsonBody(p): JsonBody<SettingsPutBody>,
) -> Result<Json<SettingsBag>> {
    let before = load_settings(s.repo.as_ref()).await?;
    let mut proxy_changed = false;
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
    let rows = s.repo.settings_get_all().await?;
    Ok(Json(SettingsBag {
        settings: rows.into_iter().collect(),
    }))
}

/// Snapshot of the first-class settings the kernel consumes; unknown keys are ignored here.
#[derive(Debug, Default, Clone)]
pub struct Settings {
    pub http_proxy: Option<String>,
    pub https_proxy: Option<String>,
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
