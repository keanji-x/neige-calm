//! Storage contract.
//!
//! `Repo` is the interface every persistence backend implements. The kernel
//! is generic over it: REST handlers, terminal lifecycle, plugin host all
//! consume `Arc<dyn Repo>`. The only concrete impl is `SqlxRepo`
//! (sqlite.rs) — used both in production (file-backed sqlite) and in
//! tests/dev (`sqlite::memory:`). A second hand-maintained in-memory
//! `MockRepo` used to live here; it was removed in D3 once tests covered
//! cascade semantics directly — running both impls in lockstep had drifted
//! and become a booby trap (see issue #4).
//!
//! ## Conventions
//!
//! * Methods that "get" a missing row return `Ok(None)`. Methods that
//!   "update/delete" a missing row return `Err(CalmError::NotFound(...))`.
//! * Patch fields that are `None` mean "leave alone".
//! * The repo stamps `created_at` / `updated_at` itself via `model::now_ms()`.
//! * The repo allocates ids via `model::new_id()`.
//! * `sort` defaults to "append to end" (current max + 1.0) when `None`.

use crate::error::Result;
use crate::model::*;
use async_trait::async_trait;

pub mod sqlite;

#[async_trait]
pub trait Repo: Send + Sync + 'static {
    // ---- coves
    async fn coves_list(&self) -> Result<Vec<Cove>>;
    async fn cove_get(&self, id: &str) -> Result<Option<Cove>>;
    async fn cove_create(&self, p: NewCove) -> Result<Cove>;
    async fn cove_update(&self, id: &str, p: CovePatch) -> Result<Cove>;
    async fn cove_delete(&self, id: &str) -> Result<()>;

    // ---- waves
    async fn waves_by_cove(&self, cove_id: &str) -> Result<Vec<Wave>>;
    async fn wave_get(&self, id: &str) -> Result<Option<Wave>>;
    async fn wave_detail(&self, id: &str) -> Result<Option<WaveDetail>>;
    async fn wave_create(&self, p: NewWave) -> Result<Wave>;
    async fn wave_update(&self, id: &str, p: WavePatch) -> Result<Wave>;
    async fn wave_delete(&self, id: &str) -> Result<()>;

    // ---- cards
    async fn cards_by_wave(&self, wave_id: &str) -> Result<Vec<Card>>;
    async fn card_get(&self, id: &str) -> Result<Option<Card>>;
    async fn card_create(&self, p: NewCard) -> Result<Card>;
    async fn card_update(&self, id: &str, p: CardPatch) -> Result<Card>;
    async fn card_delete(&self, id: &str) -> Result<()>;

    // ---- overlays
    /// Upserts on the `(plugin_id, entity_kind, entity_id, kind)` unique tuple.
    async fn overlay_upsert(&self, p: NewOverlay) -> Result<Overlay>;
    async fn overlay_delete(
        &self,
        plugin_id: &str,
        entity_kind: &str,
        entity_id: &str,
        kind: &str,
    ) -> Result<()>;
    async fn overlays_for(&self, entity_kind: &str, entity_id: &str) -> Result<Vec<Overlay>>;
    /// List every overlay attached to entities of the given `entity_kind`
    /// (e.g. `"wave"`), regardless of `entity_id`. Used by the sidebar so
    /// wave status indicators stay accurate without per-wave detail fetches.
    async fn overlays_by_kind(&self, entity_kind: &str) -> Result<Vec<Overlay>>;

    // ---- terminals
    async fn terminal_create(&self, p: NewTerminal) -> Result<Terminal>;
    async fn terminal_get(&self, id: &str) -> Result<Option<Terminal>>;
    async fn terminal_get_by_card(&self, card_id: &str) -> Result<Option<Terminal>>;
    async fn terminal_set_handle(&self, id: &str, handle: Option<&str>) -> Result<()>;

    // ---- plugins
    //
    // M3 (Slice A) surface: install / enable / get-by-id / delete / list-all.
    // `plugins_list` is kept as a thin alias around `plugins_list_all` so
    // Slice D's REST handler (and the existing stub) can keep calling it.
    async fn plugins_list(&self) -> Result<Vec<Plugin>>;
    async fn plugins_list_all(&self) -> Result<Vec<Plugin>>;
    async fn plugin_get_by_id(&self, id: &str) -> Result<Option<Plugin>>;
    /// Upsert by id. The repo stamps `installed_at` (preserving the existing
    /// value on update) and `updated_at`. `enabled` defaults to false on the
    /// install row — the user (or Slice D's enable endpoint) flips it later.
    async fn plugin_install(&self, p: NewPlugin) -> Result<Plugin>;
    async fn plugin_update_enabled(&self, id: &str, enabled: bool) -> Result<Plugin>;
    /// Overwrite `user_config` (the opaque JSON blob the PATCH config route
    /// writes). The repo stamps `updated_at`; everything else is preserved.
    async fn plugin_update_user_config(
        &self,
        id: &str,
        user_config: serde_json::Value,
    ) -> Result<Plugin>;
    /// Overwrite the persisted manifest blob. The reload route calls this
    /// after re-reading manifest.json from disk so subsequent `GET
    /// /api/plugins/:id` responses (which read from the DB row, not the
    /// live registry) reflect on-disk reality.
    async fn plugin_update_manifest(&self, id: &str, manifest: serde_json::Value)
    -> Result<Plugin>;
    async fn plugin_delete(&self, id: &str) -> Result<()>;

    /// Drop every overlay owned by a plugin. Slice D's uninstall route fires
    /// this so a deleted plugin's overlays don't render as ghosts. (Design
    /// doc §2.7 calls this out as the default; the alternative — "keep for
    /// forensics" — is what users have to opt into manually.)
    async fn overlays_clear_by_plugin(&self, plugin_id: &str) -> Result<()>;

    /// Drop every KV row owned by a plugin. Called from the uninstall path so
    /// per-plugin KV doesn't outlive the install row.
    async fn plugin_kv_clear(&self, plugin_id: &str) -> Result<()>;

    // ---- per-plugin tokens (Slice H wires the lifecycle; Slice A just owns
    // the storage). Hash is hex-encoded `SHA-256(raw_token)`; expires_at is
    // unix millis (matches the rest of the kernel's `*_at` columns).
    async fn plugin_token_set(
        &self,
        plugin_id: &str,
        hashed_token: &str,
        expires_at: i64,
    ) -> Result<()>;
    async fn plugin_token_get(&self, plugin_id: &str) -> Result<Option<(String, i64)>>;
    async fn plugin_token_delete(&self, plugin_id: &str) -> Result<()>;

    // ---- per-plugin KV store (Slice C will surface to plugins via
    // `neige.kv.*`; Slice A owns the bare CRUD). Values are arbitrary JSON;
    // the kernel does not parse semantics, but it does enforce per-plugin
    // namespacing at this trait layer (no method takes a global key).
    async fn plugin_kv_get(&self, plugin_id: &str, key: &str) -> Result<Option<serde_json::Value>>;
    async fn plugin_kv_set(
        &self,
        plugin_id: &str,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<()>;
    async fn plugin_kv_list(
        &self,
        plugin_id: &str,
        prefix: &str,
    ) -> Result<Vec<(String, serde_json::Value)>>;
    async fn plugin_kv_delete(&self, plugin_id: &str, key: &str) -> Result<()>;

    // ---- app-global settings (Settings page, codex spawn proxy override).
    //
    // Tiny KV. `settings_get_all` returns every key/value pair the kernel
    // owns; the Settings route just hands it back, and `routes::codex`
    // reads the snapshot at spawn time to derive HTTP_PROXY env overrides.
    // `settings_upsert` is per-key INSERT OR REPLACE; an empty string is
    // treated as a delete on the *route* boundary (callers can still
    // upsert an empty value if they have a reason to).
    async fn settings_get_all(&self) -> Result<Vec<(String, String)>>;
    async fn settings_upsert(&self, key: &str, value: &str) -> Result<()>;
    async fn settings_delete(&self, key: &str) -> Result<()>;
}
