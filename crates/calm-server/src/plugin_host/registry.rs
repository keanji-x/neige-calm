//! In-memory map of `plugin_id → Manifest`, loaded from disk on boot.
//!
//! The registry is the single source of truth for "what plugins does the
//! kernel know about". Slice B's process supervisor consults it on every
//! spawn; Slice D's `/api/plugins/views` endpoint walks it to synthesize the
//! card-kind catalog.
//!
//! Concurrency: `Arc<RwLock<HashMap<...>>>`. Reads dominate (every callback
//! routes through it), writes happen only on install/uninstall/reload —
//! `RwLock` is the right shape.
//!
//! Slice A does **not** spawn anything from here. We only parse + cache.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use thiserror::Error;

use super::manifest::{Manifest, ManifestError};

/// Filename the loader looks for inside each plugin subdirectory.
const MANIFEST_FILENAME: &str = "manifest.json";

/// What `load_from_dir` returns as a side-channel summary alongside the
/// registry — useful for tests and for the `tracing` lines the kernel writes
/// at boot.
#[derive(Debug, Default, Clone)]
pub struct LoadReport {
    /// Absolute paths of subdirectories we successfully loaded.
    pub loaded: Vec<PathBuf>,
    /// Per-directory failure reason. We log + carry on rather than aborting
    /// the entire boot on one broken plugin.
    pub skipped: Vec<(PathBuf, String)>,
}

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// We refuse to load if two manifests claim the same id — they'd race for
    /// the same `plugins` row, the same token, the same kv namespace.
    #[error("duplicate plugin id `{id}` between {first:?} and {second:?}")]
    DuplicateId {
        id: String,
        first: PathBuf,
        second: PathBuf,
    },
}

#[derive(Default)]
struct Inner {
    manifests: HashMap<String, Manifest>,
    /// Where each manifest was loaded from. Useful for hot-reload and for
    /// surfacing in REST responses later (Slice D).
    install_paths: HashMap<String, PathBuf>,
}

pub struct PluginRegistry {
    inner: Arc<RwLock<Inner>>,
}

impl std::fmt::Debug for PluginRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Hand-rolled Debug because `RwLock` doesn't print its guarded value.
        // We surface just the cached set of ids — enough for test panics and
        // `tracing::Display=true` log lines, no manifest payloads dumped.
        let inner = self.inner.read().unwrap();
        f.debug_struct("PluginRegistry")
            .field("len", &inner.manifests.len())
            .field("ids", &inner.manifests.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl PluginRegistry {
    /// Empty registry — handy for tests.
    pub fn empty() -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner::default())),
        }
    }

    /// Walk `dir` one level deep, treating each subdirectory as a candidate
    /// plugin. Loads `<subdir>/manifest.json` for each; on parse or validation
    /// failure, logs a warning via `tracing::warn!` and skips that plugin —
    /// the rest of the directory still loads.
    ///
    /// If `dir` doesn't exist, returns an empty registry without erroring.
    /// Fresh installs hit this path; creating the directory is the caller's
    /// (state.rs's) job.
    pub fn load_from_dir(dir: &Path) -> Result<(Self, LoadReport), RegistryError> {
        let registry = Self::empty();
        let mut report = LoadReport::default();

        if !dir.exists() {
            tracing::debug!(
                plugins_dir = %dir.display(),
                "plugins dir missing — starting with empty registry"
            );
            return Ok((registry, report));
        }

        let entries = std::fs::read_dir(dir)?;
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(error = %e, "skipping unreadable plugin dir entry");
                    continue;
                }
            };
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "stat failed");
                    report
                        .skipped
                        .push((path.clone(), format!("stat failed: {e}")));
                    continue;
                }
            };
            if !file_type.is_dir() {
                // Stray files at the root (a stray README, a leftover tarball)
                // are silently ignored — they don't claim to be plugins.
                continue;
            }
            let manifest_path = path.join(MANIFEST_FILENAME);
            if !manifest_path.exists() {
                tracing::warn!(
                    path = %manifest_path.display(),
                    "no manifest.json — skipping"
                );
                report
                    .skipped
                    .push((path.clone(), "no manifest.json".to_string()));
                continue;
            }
            match load_one(&manifest_path) {
                Ok(manifest) => {
                    let id = manifest.id.clone();
                    let mut inner = registry.inner.write().unwrap();
                    if let Some(prev) = inner.install_paths.get(&id) {
                        return Err(RegistryError::DuplicateId {
                            id,
                            first: prev.clone(),
                            second: path.clone(),
                        });
                    }
                    inner.install_paths.insert(id.clone(), path.clone());
                    inner.manifests.insert(id, manifest);
                    report.loaded.push(path);
                }
                Err(e) => {
                    tracing::warn!(
                        path = %manifest_path.display(),
                        error = %e,
                        "manifest load failed — skipping plugin"
                    );
                    report.skipped.push((path, e.to_string()));
                }
            }
        }

        Ok((registry, report))
    }

    /// Look up a manifest by id.
    pub fn get(&self, id: &str) -> Option<Manifest> {
        self.inner.read().unwrap().manifests.get(id).cloned()
    }

    /// Snapshot the current set of manifests. Returns clones — callers that
    /// want zero-copy can hold the `Arc` themselves via `inner`. We keep that
    /// path private until a measured use case forces the issue.
    pub fn list(&self) -> Vec<Manifest> {
        self.inner
            .read()
            .unwrap()
            .manifests
            .values()
            .cloned()
            .collect()
    }

    /// Where the plugin's files live on disk. `None` if we synthesized this
    /// manifest in-memory (test path) rather than loading it from disk.
    pub fn install_path(&self, id: &str) -> Option<PathBuf> {
        self.inner.read().unwrap().install_paths.get(id).cloned()
    }

    /// Install or overwrite a manifest. Used by Slice D's `/api/plugins/install`
    /// after the file copy completes, and by tests that want to seed entries
    /// without touching the filesystem.
    pub fn insert(&self, manifest: Manifest, install_path: Option<PathBuf>) {
        let mut inner = self.inner.write().unwrap();
        let id = manifest.id.clone();
        if let Some(p) = install_path {
            inner.install_paths.insert(id.clone(), p);
        }
        inner.manifests.insert(id, manifest);
    }

    /// #1164 §2.7(2)(3) — replace ONLY the `exposes_tools` field of an
    /// already-registered manifest, under a single write lock.
    ///
    /// Two properties are load-bearing and must not be "simplified" into a
    /// `get` → mutate → [`insert`](Self::insert) sequence:
    ///
    /// 1. **Field-level, single lock.** `insert` swaps the WHOLE `Manifest`.
    ///    A read-modify-write would race a concurrent `/reload` and roll the
    ///    entire manifest back — url, `tools_allow`, permissions, views,
    ///    workflows — with nothing to notice until the next reload. Confining
    ///    the write to one field caps the worst case at "stale tool list".
    ///
    /// 2. **No-op when `id` is absent.** Uninstall removes the registry entry
    ///    while an in-flight spawn may still be running (§5 R12: there is no
    ///    per-plugin lifecycle lock). If materialization inserted, it would
    ///    RESURRECT the uninstalled manifest into the registry. Returning
    ///    `false` instead is what neutralizes that race.
    ///
    /// Returns whether the entry existed (and was therefore updated).
    pub fn set_exposes_tools(&self, id: &str, tools: Vec<super::manifest::ExposedTool>) -> bool {
        let mut inner = self.inner.write().unwrap();
        match inner.manifests.get_mut(id) {
            Some(manifest) => {
                manifest.exposes_tools = tools;
                true
            }
            None => false,
        }
    }

    /// Remove a manifest. Returns the previous entry, if any.
    pub fn remove(&self, id: &str) -> Option<Manifest> {
        let mut inner = self.inner.write().unwrap();
        inner.install_paths.remove(id);
        inner.manifests.remove(id)
    }

    /// Count of currently registered manifests. Mostly used in tests/logging.
    pub fn len(&self) -> usize {
        self.inner.read().unwrap().manifests.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::empty()
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// One-shot read + parse + validate. Errors carry the file path's failure
/// reason; callers log it.
#[derive(Debug, Error)]
enum LoadOneError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Manifest(#[from] ManifestError),
}

fn load_one(manifest_path: &Path) -> Result<Manifest, LoadOneError> {
    let text = std::fs::read_to_string(manifest_path)?;
    let m = Manifest::parse(&text)?;
    Ok(m)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const VALID: &str = r#"{
        "manifest_version": 1,
        "id": "test.valid",
        "version": "0.1.0",
        "min_kernel_version": "0.1.0",
        "display_name": "Valid",
        "entrypoint": { "command": "bin/run" },
        "views": [{ "view_id": "main", "title": "Main", "scope": "card" }]
    }"#;

    const SECOND_VALID: &str = r#"{
        "manifest_version": 1,
        "id": "test.second",
        "version": "0.2.0",
        "min_kernel_version": "0.1.0",
        "display_name": "Second",
        "entrypoint": { "command": "bin/run" }
    }"#;

    const BROKEN: &str = r#"{ "manifest_version": 1, "id": "BAD ID", "version": "0.1.0" }"#;

    fn write_plugin(root: &Path, id: &str, contents: &str) -> PathBuf {
        let dir = root.join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("manifest.json"), contents).unwrap();
        dir
    }

    #[test]
    fn missing_dir_yields_empty_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let nonexistent = tmp.path().join("does-not-exist");
        let (reg, report) = PluginRegistry::load_from_dir(&nonexistent).unwrap();
        assert!(reg.is_empty());
        assert!(report.loaded.is_empty());
        assert!(report.skipped.is_empty());
    }

    #[test]
    fn loads_two_skips_one_broken_and_one_no_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(tmp.path(), "test.valid", VALID);
        write_plugin(tmp.path(), "test.second", SECOND_VALID);
        write_plugin(tmp.path(), "broken", BROKEN);
        // A subdir with no manifest at all.
        fs::create_dir_all(tmp.path().join("no-manifest")).unwrap();
        // A stray file at root (not a dir) — must be ignored silently.
        fs::write(tmp.path().join("README.txt"), "ignore me").unwrap();

        let (reg, report) = PluginRegistry::load_from_dir(tmp.path()).unwrap();
        assert_eq!(reg.len(), 2, "expected two loaded, got {}", reg.len());
        assert!(reg.get("test.valid").is_some());
        assert!(reg.get("test.second").is_some());
        assert!(reg.get("broken").is_none());

        // Both broken and no-manifest should appear in `skipped`.
        assert_eq!(report.loaded.len(), 2);
        assert_eq!(report.skipped.len(), 2);
    }

    #[test]
    fn duplicate_id_errors() {
        let tmp = tempfile::tempdir().unwrap();
        // Two subdirs both claiming id="test.valid".
        write_plugin(tmp.path(), "a", VALID);
        write_plugin(tmp.path(), "b", VALID);
        let err = PluginRegistry::load_from_dir(tmp.path()).unwrap_err();
        assert!(matches!(err, RegistryError::DuplicateId { .. }));
    }

    #[test]
    fn insert_and_remove_in_memory() {
        let reg = PluginRegistry::empty();
        let m = Manifest::parse(VALID).unwrap();
        reg.insert(m.clone(), Some(PathBuf::from("/tmp/fake")));
        assert_eq!(reg.len(), 1);
        assert_eq!(
            reg.install_path("test.valid"),
            Some(PathBuf::from("/tmp/fake"))
        );
        let prev = reg.remove("test.valid").expect("had entry");
        assert_eq!(prev.id, m.id);
        assert!(reg.is_empty());
        assert!(reg.install_path("test.valid").is_none());
    }

    // -----------------------------------------------------------------
    // #1164 §2.7(2)(3) — `set_exposes_tools`
    // -----------------------------------------------------------------

    fn tool(name: &str) -> super::super::manifest::ExposedTool {
        super::super::manifest::ExposedTool {
            name: name.to_string(),
            description: None,
            kind: None,
            input_schema: None,
            annotations: None,
        }
    }

    #[test]
    fn set_exposes_tools_replaces_only_that_field() {
        let reg = PluginRegistry::empty();
        reg.insert(Manifest::parse(VALID).unwrap(), Some("/tmp/fake".into()));

        assert!(reg.set_exposes_tools("test.valid", vec![tool("a"), tool("b")]));

        let after = reg.get("test.valid").expect("still registered");
        assert_eq!(
            after
                .exposes_tools
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        // Everything else survives — this is the whole reason the mutator is
        // field-level instead of `get` → mutate → `insert`.
        assert_eq!(after.display_name, "Valid");
        assert_eq!(after.version, "0.1.0");
        assert_eq!(after.views.len(), 1);
        assert_eq!(
            reg.install_path("test.valid"),
            Some(PathBuf::from("/tmp/fake"))
        );
    }

    /// §2.7(3): the no-op is what stops an in-flight spawn from resurrecting a
    /// manifest that uninstall already removed (risk R12).
    #[test]
    fn set_exposes_tools_is_a_noop_for_an_absent_id() {
        let reg = PluginRegistry::empty();
        reg.insert(Manifest::parse(VALID).unwrap(), None);

        // Uninstall removed it while a spawn was on the wire.
        reg.remove("test.valid");
        assert!(reg.is_empty());

        assert!(
            !reg.set_exposes_tools("test.valid", vec![tool("a")]),
            "must report that nothing was updated"
        );
        assert!(
            reg.get("test.valid").is_none(),
            "an uninstalled manifest must NOT be resurrected into the registry"
        );
        assert!(reg.is_empty());
        assert!(reg.install_path("test.valid").is_none());
    }

    #[test]
    fn list_returns_all() {
        let reg = PluginRegistry::empty();
        reg.insert(Manifest::parse(VALID).unwrap(), None);
        reg.insert(Manifest::parse(SECOND_VALID).unwrap(), None);
        let mut ids: Vec<String> = reg.list().into_iter().map(|m| m.id).collect();
        ids.sort();
        assert_eq!(
            ids,
            vec!["test.second".to_string(), "test.valid".to_string()]
        );
    }
}
