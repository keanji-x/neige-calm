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
//! `http_proxy` / `https_proxy` are the only keys the kernel actively reads
//! today (see `routes::codex_cards::create_codex_card`). The schema is intentionally
//! open: any string key/value pair is allowed, so future settings can land
//! without a wire-level migration.

use crate::error::{ErrorBody, Result};
use crate::state::{AppState, CodexShellState, RouteState};
use axum::{Json, Router, extract::State, routing::get};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/settings", get(get_settings).put(put_settings))
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
    let mut map = BTreeMap::new();
    for (k, v) in rows {
        map.insert(k, v);
    }
    Ok(Json(SettingsBag { settings: map }))
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
    Json(p): Json<SettingsPutBody>,
) -> Result<Json<SettingsBag>> {
    let before = load_settings(s.repo.as_ref()).await?;
    let mut proxy_changed = false;
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
    let rows = s.repo.settings_get_all().await?;
    let mut map = BTreeMap::new();
    for (k, v) in rows {
        map.insert(k, v);
    }
    Ok(Json(SettingsBag { settings: map }))
}

/// Internal helper: snapshot the settings table into a typed `Settings`
/// struct the codex spawn path consumes. Unknown keys are kept in the
/// `other` bag for forward-compat but no callsite reads from there yet.
#[derive(Debug, Default, Clone)]
pub struct Settings {
    pub http_proxy: Option<String>,
    pub https_proxy: Option<String>,
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
