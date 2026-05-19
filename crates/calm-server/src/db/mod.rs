//! Storage contract.
//!
//! `Repo` is the interface every persistence backend implements. The kernel
//! is generic over it: REST handlers, terminal lifecycle, plugin host all
//! consume `Arc<dyn Repo>`. Concrete impls:
//!
//!   * `SqlxRepo` (sqlite.rs)  — production backend, owned by track A
//!   * `MockRepo` (this file)  — in-memory; lets tracks B/C/D `cargo run`
//!     end-to-end before track A lands
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
    async fn plugin_update_manifest(
        &self,
        id: &str,
        manifest: serde_json::Value,
    ) -> Result<Plugin>;
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
    async fn plugin_kv_get(
        &self,
        plugin_id: &str,
        key: &str,
    ) -> Result<Option<serde_json::Value>>;
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
}

// =============================================================================
// MockRepo — in-memory backend (HashMap-of-Mutex). Adequate for dev/tests;
// not concurrent-safe in any serious sense. SqlxRepo replaces this in main.
// =============================================================================

use std::collections::HashMap;
use std::sync::Mutex;

use crate::error::CalmError;

#[derive(Default)]
struct MockState {
    coves: HashMap<String, Cove>,
    waves: HashMap<String, Wave>,
    cards: HashMap<String, Card>,
    overlays: HashMap<String, Overlay>,
    terminals: HashMap<String, Terminal>,
    /// `id → Plugin`. Slice D's REST handlers and Slice A's tests are the
    /// primary users; Slice C's callback dispatcher will join through here
    /// for permission lookups.
    plugins: HashMap<String, Plugin>,
    /// `plugin_id → (hashed_token, expires_at_ms)`. One token per plugin.
    plugin_tokens: HashMap<String, (String, i64)>,
    /// `(plugin_id, key) → value`. Tuple-keyed to mirror the sqlite composite PK.
    plugin_kv: HashMap<(String, String), serde_json::Value>,
}

pub struct MockRepo {
    s: Mutex<MockState>,
}

impl MockRepo {
    pub fn new() -> Self {
        Self {
            s: Mutex::new(MockState::default()),
        }
    }

    fn next_sort(items: impl Iterator<Item = f64>) -> f64 {
        items.fold(0.0_f64, |acc, x| acc.max(x)) + 1.0
    }
}

impl Default for MockRepo {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Repo for MockRepo {
    // ---- coves
    async fn coves_list(&self) -> Result<Vec<Cove>> {
        let s = self.s.lock().unwrap();
        let mut v: Vec<Cove> = s.coves.values().cloned().collect();
        v.sort_by(|a, b| a.sort.partial_cmp(&b.sort).unwrap_or(std::cmp::Ordering::Equal));
        Ok(v)
    }
    async fn cove_get(&self, id: &str) -> Result<Option<Cove>> {
        Ok(self.s.lock().unwrap().coves.get(id).cloned())
    }
    async fn cove_create(&self, p: NewCove) -> Result<Cove> {
        let mut s = self.s.lock().unwrap();
        let sort = p
            .sort
            .unwrap_or_else(|| Self::next_sort(s.coves.values().map(|c| c.sort)));
        let now = now_ms();
        let c = Cove {
            id: new_id(),
            name: p.name,
            color: p.color,
            sort,
            created_at: now,
            updated_at: now,
        };
        s.coves.insert(c.id.clone(), c.clone());
        Ok(c)
    }
    async fn cove_update(&self, id: &str, p: CovePatch) -> Result<Cove> {
        let mut s = self.s.lock().unwrap();
        let c = s
            .coves
            .get_mut(id)
            .ok_or_else(|| CalmError::NotFound(format!("cove {id}")))?;
        if let Some(v) = p.name {
            c.name = v;
        }
        if let Some(v) = p.color {
            c.color = v;
        }
        if let Some(v) = p.sort {
            c.sort = v;
        }
        c.updated_at = now_ms();
        Ok(c.clone())
    }
    async fn cove_delete(&self, id: &str) -> Result<()> {
        let mut s = self.s.lock().unwrap();
        if s.coves.remove(id).is_none() {
            return Err(CalmError::NotFound(format!("cove {id}")));
        }
        // cascade
        let waves: Vec<String> = s
            .waves
            .values()
            .filter(|w| w.cove_id == id)
            .map(|w| w.id.clone())
            .collect();
        for w in &waves {
            let cards: Vec<String> = s
                .cards
                .values()
                .filter(|c| &c.wave_id == w)
                .map(|c| c.id.clone())
                .collect();
            for cid in cards {
                s.cards.remove(&cid);
            }
            s.waves.remove(w);
        }
        Ok(())
    }

    // ---- waves
    async fn waves_by_cove(&self, cove_id: &str) -> Result<Vec<Wave>> {
        let s = self.s.lock().unwrap();
        let mut v: Vec<Wave> = s
            .waves
            .values()
            .filter(|w| w.cove_id == cove_id)
            .cloned()
            .collect();
        v.sort_by(|a, b| a.sort.partial_cmp(&b.sort).unwrap_or(std::cmp::Ordering::Equal));
        Ok(v)
    }
    async fn wave_get(&self, id: &str) -> Result<Option<Wave>> {
        Ok(self.s.lock().unwrap().waves.get(id).cloned())
    }
    async fn wave_detail(&self, id: &str) -> Result<Option<WaveDetail>> {
        let s = self.s.lock().unwrap();
        let Some(wave) = s.waves.get(id).cloned() else {
            return Ok(None);
        };
        let mut cards: Vec<Card> = s
            .cards
            .values()
            .filter(|c| c.wave_id == id)
            .cloned()
            .collect();
        cards.sort_by(|a, b| a.sort.partial_cmp(&b.sort).unwrap_or(std::cmp::Ordering::Equal));
        let card_ids: std::collections::HashSet<String> =
            cards.iter().map(|c| c.id.clone()).collect();
        let overlays: Vec<Overlay> = s
            .overlays
            .values()
            .filter(|o| {
                (o.entity_kind == "wave" && o.entity_id == id)
                    || (o.entity_kind == "card" && card_ids.contains(&o.entity_id))
            })
            .cloned()
            .collect();
        Ok(Some(WaveDetail {
            wave,
            cards,
            overlays,
        }))
    }
    async fn wave_create(&self, p: NewWave) -> Result<Wave> {
        let mut s = self.s.lock().unwrap();
        if !s.coves.contains_key(&p.cove_id) {
            return Err(CalmError::NotFound(format!("cove {}", p.cove_id)));
        }
        let sort = p.sort.unwrap_or_else(|| {
            Self::next_sort(s.waves.values().filter(|w| w.cove_id == p.cove_id).map(|w| w.sort))
        });
        let now = now_ms();
        let w = Wave {
            id: new_id(),
            cove_id: p.cove_id,
            title: p.title,
            sort,
            archived_at: None,
            created_at: now,
            updated_at: now,
        };
        s.waves.insert(w.id.clone(), w.clone());
        Ok(w)
    }
    async fn wave_update(&self, id: &str, p: WavePatch) -> Result<Wave> {
        let mut s = self.s.lock().unwrap();
        let w = s
            .waves
            .get_mut(id)
            .ok_or_else(|| CalmError::NotFound(format!("wave {id}")))?;
        if let Some(v) = p.title {
            w.title = v;
        }
        if let Some(v) = p.sort {
            w.sort = v;
        }
        if let Some(v) = p.archived_at {
            w.archived_at = v;
        }
        w.updated_at = now_ms();
        Ok(w.clone())
    }
    async fn wave_delete(&self, id: &str) -> Result<()> {
        let mut s = self.s.lock().unwrap();
        if s.waves.remove(id).is_none() {
            return Err(CalmError::NotFound(format!("wave {id}")));
        }
        let cards: Vec<String> = s
            .cards
            .values()
            .filter(|c| c.wave_id == id)
            .map(|c| c.id.clone())
            .collect();
        for cid in cards {
            s.cards.remove(&cid);
        }
        Ok(())
    }

    // ---- cards
    async fn cards_by_wave(&self, wave_id: &str) -> Result<Vec<Card>> {
        let s = self.s.lock().unwrap();
        let mut v: Vec<Card> = s
            .cards
            .values()
            .filter(|c| c.wave_id == wave_id)
            .cloned()
            .collect();
        v.sort_by(|a, b| a.sort.partial_cmp(&b.sort).unwrap_or(std::cmp::Ordering::Equal));
        Ok(v)
    }
    async fn card_get(&self, id: &str) -> Result<Option<Card>> {
        Ok(self.s.lock().unwrap().cards.get(id).cloned())
    }
    async fn card_create(&self, p: NewCard) -> Result<Card> {
        let mut s = self.s.lock().unwrap();
        if !s.waves.contains_key(&p.wave_id) {
            return Err(CalmError::NotFound(format!("wave {}", p.wave_id)));
        }
        let sort = p.sort.unwrap_or_else(|| {
            Self::next_sort(s.cards.values().filter(|c| c.wave_id == p.wave_id).map(|c| c.sort))
        });
        let now = now_ms();
        let c = Card {
            id: new_id(),
            wave_id: p.wave_id,
            kind: p.kind,
            sort,
            payload: p.payload,
            created_at: now,
            updated_at: now,
        };
        s.cards.insert(c.id.clone(), c.clone());
        Ok(c)
    }
    async fn card_update(&self, id: &str, p: CardPatch) -> Result<Card> {
        let mut s = self.s.lock().unwrap();
        let c = s
            .cards
            .get_mut(id)
            .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
        if let Some(v) = p.kind {
            c.kind = v;
        }
        if let Some(v) = p.sort {
            c.sort = v;
        }
        if let Some(v) = p.payload {
            c.payload = v;
        }
        c.updated_at = now_ms();
        Ok(c.clone())
    }
    async fn card_delete(&self, id: &str) -> Result<()> {
        let mut s = self.s.lock().unwrap();
        if s.cards.remove(id).is_none() {
            return Err(CalmError::NotFound(format!("card {id}")));
        }
        Ok(())
    }

    // ---- overlays
    async fn overlay_upsert(&self, p: NewOverlay) -> Result<Overlay> {
        let mut s = self.s.lock().unwrap();
        let now = now_ms();
        // find existing
        let existing = s.overlays.values_mut().find(|o| {
            o.plugin_id == p.plugin_id
                && o.entity_kind == p.entity_kind
                && o.entity_id == p.entity_id
                && o.kind == p.kind
        });
        if let Some(o) = existing {
            o.payload = p.payload;
            o.updated_at = now;
            return Ok(o.clone());
        }
        let o = Overlay {
            id: new_id(),
            plugin_id: p.plugin_id,
            entity_kind: p.entity_kind,
            entity_id: p.entity_id,
            kind: p.kind,
            payload: p.payload,
            updated_at: now,
        };
        s.overlays.insert(o.id.clone(), o.clone());
        Ok(o)
    }
    async fn overlay_delete(
        &self,
        plugin_id: &str,
        entity_kind: &str,
        entity_id: &str,
        kind: &str,
    ) -> Result<()> {
        let mut s = self.s.lock().unwrap();
        let id_opt = s
            .overlays
            .values()
            .find(|o| {
                o.plugin_id == plugin_id
                    && o.entity_kind == entity_kind
                    && o.entity_id == entity_id
                    && o.kind == kind
            })
            .map(|o| o.id.clone());
        match id_opt {
            Some(id) => {
                s.overlays.remove(&id);
                Ok(())
            }
            None => Err(CalmError::NotFound("overlay".into())),
        }
    }
    async fn overlays_for(&self, entity_kind: &str, entity_id: &str) -> Result<Vec<Overlay>> {
        let s = self.s.lock().unwrap();
        Ok(s.overlays
            .values()
            .filter(|o| o.entity_kind == entity_kind && o.entity_id == entity_id)
            .cloned()
            .collect())
    }

    // ---- terminals
    async fn terminal_create(&self, p: NewTerminal) -> Result<Terminal> {
        let mut s = self.s.lock().unwrap();
        if !s.cards.contains_key(&p.card_id) {
            return Err(CalmError::NotFound(format!("card {}", p.card_id)));
        }
        if s.terminals.values().any(|t| t.card_id == p.card_id) {
            return Err(CalmError::Conflict(format!(
                "terminal already exists for card {}",
                p.card_id
            )));
        }
        let now = now_ms();
        let t = Terminal {
            id: new_id(),
            card_id: p.card_id,
            program: p.program,
            cwd: p.cwd,
            env: p.env,
            daemon_handle: None,
            created_at: now,
        };
        s.terminals.insert(t.id.clone(), t.clone());
        Ok(t)
    }
    async fn terminal_get(&self, id: &str) -> Result<Option<Terminal>> {
        Ok(self.s.lock().unwrap().terminals.get(id).cloned())
    }
    async fn terminal_get_by_card(&self, card_id: &str) -> Result<Option<Terminal>> {
        Ok(self
            .s
            .lock()
            .unwrap()
            .terminals
            .values()
            .find(|t| t.card_id == card_id)
            .cloned())
    }
    async fn terminal_set_handle(&self, id: &str, handle: Option<&str>) -> Result<()> {
        let mut s = self.s.lock().unwrap();
        let t = s
            .terminals
            .get_mut(id)
            .ok_or_else(|| CalmError::NotFound(format!("terminal {id}")))?;
        t.daemon_handle = handle.map(|s| s.to_string());
        Ok(())
    }

    // ---- plugins
    async fn plugins_list(&self) -> Result<Vec<Plugin>> {
        self.plugins_list_all().await
    }
    async fn plugins_list_all(&self) -> Result<Vec<Plugin>> {
        let s = self.s.lock().unwrap();
        let mut v: Vec<Plugin> = s.plugins.values().cloned().collect();
        // Stable ordering for tests / UI consistency. Id is the natural key.
        v.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(v)
    }
    async fn plugin_get_by_id(&self, id: &str) -> Result<Option<Plugin>> {
        Ok(self.s.lock().unwrap().plugins.get(id).cloned())
    }
    async fn plugin_install(&self, p: NewPlugin) -> Result<Plugin> {
        let mut s = self.s.lock().unwrap();
        let now = now_ms();
        // Preserve `installed_at` on update; only freshly-inserted rows get
        // a new value. Sqlite mirror does the same via `COALESCE`.
        let installed_at = s
            .plugins
            .get(&p.id)
            .map(|prev| prev.installed_at)
            .unwrap_or(now);
        let plug = Plugin {
            id: p.id.clone(),
            version: p.version,
            install_path: p.install_path,
            manifest: p.manifest,
            enabled: p.enabled,
            user_config: p.user_config,
            installed_at,
            updated_at: now,
        };
        s.plugins.insert(p.id, plug.clone());
        Ok(plug)
    }
    async fn plugin_update_enabled(&self, id: &str, enabled: bool) -> Result<Plugin> {
        let mut s = self.s.lock().unwrap();
        let plug = s
            .plugins
            .get_mut(id)
            .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))?;
        plug.enabled = enabled;
        plug.updated_at = now_ms();
        Ok(plug.clone())
    }
    async fn plugin_update_user_config(
        &self,
        id: &str,
        user_config: serde_json::Value,
    ) -> Result<Plugin> {
        let mut s = self.s.lock().unwrap();
        let plug = s
            .plugins
            .get_mut(id)
            .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))?;
        plug.user_config = user_config;
        plug.updated_at = now_ms();
        Ok(plug.clone())
    }
    async fn plugin_update_manifest(
        &self,
        id: &str,
        manifest: serde_json::Value,
    ) -> Result<Plugin> {
        let mut s = self.s.lock().unwrap();
        let plug = s
            .plugins
            .get_mut(id)
            .ok_or_else(|| CalmError::NotFound(format!("plugin {id}")))?;
        plug.manifest = manifest;
        plug.updated_at = now_ms();
        Ok(plug.clone())
    }
    async fn plugin_delete(&self, id: &str) -> Result<()> {
        let mut s = self.s.lock().unwrap();
        if s.plugins.remove(id).is_none() {
            return Err(CalmError::NotFound(format!("plugin {id}")));
        }
        // Cascade: token and kv for the deleted plugin go with it. Matches the
        // sqlite ON DELETE CASCADE on the same FK columns.
        s.plugin_tokens.remove(id);
        s.plugin_kv.retain(|(pid, _), _| pid != id);
        Ok(())
    }

    async fn overlays_clear_by_plugin(&self, plugin_id: &str) -> Result<()> {
        let mut s = self.s.lock().unwrap();
        s.overlays.retain(|_, o| o.plugin_id != plugin_id);
        Ok(())
    }

    async fn plugin_kv_clear(&self, plugin_id: &str) -> Result<()> {
        let mut s = self.s.lock().unwrap();
        s.plugin_kv.retain(|(pid, _), _| pid != plugin_id);
        Ok(())
    }

    // ---- plugin tokens
    async fn plugin_token_set(
        &self,
        plugin_id: &str,
        hashed_token: &str,
        expires_at: i64,
    ) -> Result<()> {
        let mut s = self.s.lock().unwrap();
        s.plugin_tokens.insert(
            plugin_id.to_string(),
            (hashed_token.to_string(), expires_at),
        );
        Ok(())
    }
    async fn plugin_token_get(&self, plugin_id: &str) -> Result<Option<(String, i64)>> {
        Ok(self.s.lock().unwrap().plugin_tokens.get(plugin_id).cloned())
    }
    async fn plugin_token_delete(&self, plugin_id: &str) -> Result<()> {
        // Idempotent: deleting a missing token is not an error. Slice H's
        // rotation path benefits from being able to call this unconditionally.
        self.s.lock().unwrap().plugin_tokens.remove(plugin_id);
        Ok(())
    }

    // ---- plugin kv
    async fn plugin_kv_get(
        &self,
        plugin_id: &str,
        key: &str,
    ) -> Result<Option<serde_json::Value>> {
        Ok(self
            .s
            .lock()
            .unwrap()
            .plugin_kv
            .get(&(plugin_id.to_string(), key.to_string()))
            .cloned())
    }
    async fn plugin_kv_set(
        &self,
        plugin_id: &str,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<()> {
        self.s
            .lock()
            .unwrap()
            .plugin_kv
            .insert((plugin_id.to_string(), key.to_string()), value.clone());
        Ok(())
    }
    async fn plugin_kv_list(
        &self,
        plugin_id: &str,
        prefix: &str,
    ) -> Result<Vec<(String, serde_json::Value)>> {
        let s = self.s.lock().unwrap();
        let mut v: Vec<(String, serde_json::Value)> = s
            .plugin_kv
            .iter()
            .filter(|((pid, k), _)| pid == plugin_id && k.starts_with(prefix))
            .map(|((_, k), v)| (k.clone(), v.clone()))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(v)
    }
    async fn plugin_kv_delete(&self, plugin_id: &str, key: &str) -> Result<()> {
        // Same idempotency reasoning as token_delete: callers (Slice C) lean
        // on being able to delete unconditionally.
        self.s
            .lock()
            .unwrap()
            .plugin_kv
            .remove(&(plugin_id.to_string(), key.to_string()));
        Ok(())
    }
}
