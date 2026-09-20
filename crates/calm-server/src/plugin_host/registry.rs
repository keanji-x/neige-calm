//! In-memory map of `plugin_id → Manifest`, loaded from disk on boot.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use thiserror::Error;

use super::LifecycleGuard;
use super::manifest::{Manifest, ManifestError};

/// Filename the loader looks for inside each plugin subdirectory.
const MANIFEST_FILENAME: &str = "manifest.json";

/// Side-channel summary returned by `load_from_dir`.
#[derive(Debug, Default, Clone)]
pub struct LoadReport {
    /// Absolute paths of subdirectories we successfully loaded.
    pub loaded: Vec<PathBuf>,
    /// Per-directory failure reason; one broken plugin does not abort boot.
    pub skipped: Vec<(PathBuf, String)>,
}

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Default)]
struct Inner {
    manifests: HashMap<String, Manifest>,
    /// Where each manifest was loaded from.
    install_paths: HashMap<String, PathBuf>,
}

pub struct PluginRegistry {
    inner: Arc<RwLock<Inner>>,
}

impl std::fmt::Debug for PluginRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Hand-rolled Debug: `RwLock` doesn't print its guarded value; ids only, no payloads.
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

    /// **Build-time** construction: seeding a registry no `PluginHost` owns yet needs no lifecycle guard. The builder is consuming, so the build-time write path is gone once it is live.
    pub fn builder() -> PluginRegistryBuilder {
        PluginRegistryBuilder {
            inner: Inner::default(),
        }
    }

    /// One-shot form of `builder` for seeding N manifests.
    pub fn from_manifests<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (Manifest, Option<PathBuf>)>,
    {
        let mut b = Self::builder();
        for (manifest, install_path) in entries {
            b = b.with(manifest, install_path);
        }
        b.build()
    }

    /// Walk `dir` one level deep; each entry that **resolves** to a directory (symlinks included) is a candidate plugin. Parse/validation failures are logged and skipped.
    /// Duplicate ids: the entry whose directory name equals the manifest id wins, else the first in sorted name order — names are attacker-chosen and the id is read from the manifest afterwards.
    /// A missing `dir` yields an empty registry.
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

        // Sort so 'which duplicate wins' is a property of the names on disk, not of the filesystem's `read_dir` order.
        let mut entries: Vec<std::fs::DirEntry> = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            match entry {
                Ok(e) => entries.push(e),
                Err(e) => {
                    tracing::warn!(error = %e, "skipping unreadable plugin dir entry");
                }
            }
        }
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            let path = entry.path();
            // `DirEntry::file_type()` does NOT follow symlinks; used only to tell symlink from plain file, never for directory-ness.
            let entry_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "stat failed");
                    report
                        .skipped
                        .push((path.clone(), format!("stat failed: {e}")));
                    continue;
                }
            };
            // `fs::metadata` follows symlinks (outside-source installs are materialized as one); a broken symlink is reported, not dropped.
            let metadata = match std::fs::metadata(&path) {
                Ok(md) => md,
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "stat failed (unresolvable entry, e.g. a broken symlink)"
                    );
                    report
                        .skipped
                        .push((path.clone(), format!("stat failed: {e}")));
                    continue;
                }
            };
            if !metadata.is_dir() {
                // A plain file at the root is ignored silently; a symlink resolving to a non-directory is an install artifact and is reported.
                if entry_type.is_symlink() {
                    tracing::warn!(
                        path = %path.display(),
                        "plugin root symlink does not resolve to a directory — skipping"
                    );
                    report.skipped.push((
                        path.clone(),
                        "symlink does not resolve to a directory".to_string(),
                    ));
                }
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
                    let path_is_canonical = dir_name_is_id(&path, &id);
                    let mut inner = registry.inner.write().unwrap();
                    // Two manifests claiming one id would race for the same `plugins` row, token and kv namespace, so only one may load; refusing to boot is not an option.
                    // The canonical entry (directory name == manifest id) wins over any non-canonical one, since `.`/uppercase names sort below a legal id and would otherwise load under the victim's id.
                    if let Some(prev) = inner.install_paths.get(&id).cloned() {
                        let prev_is_canonical = dir_name_is_id(&prev, &id);
                        let (winner, loser) = if path_is_canonical && !prev_is_canonical {
                            inner.install_paths.insert(id.clone(), path.clone());
                            inner.manifests.insert(id.clone(), manifest);
                            drop(inner);
                            report.loaded.retain(|loaded| loaded != &prev);
                            report.loaded.push(path.clone());
                            (path, prev)
                        } else {
                            drop(inner);
                            (prev, path)
                        };
                        tracing::warn!(
                            plugin_id = %id,
                            winner = %winner.display(),
                            loser = %loser.display(),
                            "duplicate plugin id — one entry is loaded, the other is skipped"
                        );
                        let reason = format!(
                            "duplicate plugin id `{id}` — already loaded from {}",
                            winner.display()
                        );
                        report.skipped.push((loser, reason));
                        continue;
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

    pub fn get(&self, id: &str) -> Option<Manifest> {
        self.inner.read().unwrap().manifests.get(id).cloned()
    }

    /// Snapshot the current set of manifests, as clones.
    pub fn list(&self) -> Vec<Manifest> {
        self.inner
            .read()
            .unwrap()
            .manifests
            .values()
            .cloned()
            .collect()
    }

    /// `None` if the manifest was synthesized in-memory rather than loaded from disk.
    pub fn install_path(&self, id: &str) -> Option<PathBuf> {
        self.inner.read().unwrap().install_paths.get(id).cloned()
    }

    /// Install or overwrite a manifest. `pub(in crate::plugin_host)` and guard-taking on purpose: only `PluginHost` can hold a guard, so every runtime write is behind the lifecycle lock. Do NOT re-widen or add an unlocked escape hatch.
    /// The key is read off the guard; the `assert_eq!` keeps the key and the stored manifest from disagreeing.
    pub(in crate::plugin_host) fn insert(
        &self,
        guard: &LifecycleGuard,
        manifest: Manifest,
        install_path: Option<PathBuf>,
    ) {
        let id = guard.id();
        assert_eq!(
            id, manifest.id,
            "registry insert under the wrong lifecycle guard: the lock is held \
             for `{id}` but the manifest being written is `{}`",
            manifest.id
        );
        let mut inner = self.inner.write().unwrap();
        if let Some(p) = install_path {
            inner.install_paths.insert(id.to_string(), p);
        }
        inner.manifests.insert(id.to_string(), manifest);
    }

    /// Replace ONLY the `exposes_tools` field, under a single write lock. Not a `get` → mutate → `insert`: that would race a concurrent `/reload` and roll the whole manifest back.
    /// No-op (returns `false`) when `id` is absent, so a spawn tail cannot resurrect an uninstalled manifest.
    pub(in crate::plugin_host) fn set_exposes_tools(
        &self,
        guard: &LifecycleGuard,
        tools: Vec<super::manifest::ExposedTool>,
    ) -> bool {
        let id = guard.id();
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
    pub(in crate::plugin_host) fn remove(&self, guard: &LifecycleGuard) -> Option<Manifest> {
        let id = guard.id();
        let mut inner = self.inner.write().unwrap();
        inner.install_paths.remove(id);
        inner.manifests.remove(id)
    }

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

/// Consuming builder for a registry being assembled before any `PluginHost` owns it; `build` moves `self`.
#[derive(Default)]
pub struct PluginRegistryBuilder {
    inner: Inner,
}

impl PluginRegistryBuilder {
    /// Seed one manifest. Last write wins on duplicate ids.
    #[must_use]
    pub fn with(mut self, manifest: Manifest, install_path: Option<PathBuf>) -> Self {
        let id = manifest.id.clone();
        if let Some(p) = install_path {
            self.inner.install_paths.insert(id.clone(), p);
        }
        self.inner.manifests.insert(id, manifest);
        self
    }

    /// Freeze the accumulated manifests into a live registry.
    pub fn build(self) -> PluginRegistry {
        PluginRegistry {
            inner: Arc::new(RwLock::new(self.inner)),
        }
    }
}

#[derive(Debug, Error)]
enum LoadOneError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Manifest(#[from] ManifestError),
}

/// Does this entry have the canonical shape an install produces — a directory whose own name *is* the manifest id?
/// Byte-exact on the raw `OsStr`: a differently-cased name is a different directory, and uppercase sorts below lowercase.
fn dir_name_is_id(path: &Path, id: &str) -> bool {
    path.file_name() == Some(std::ffi::OsStr::new(id))
}

fn load_one(manifest_path: &Path) -> Result<Manifest, LoadOneError> {
    let text = std::fs::read_to_string(manifest_path)?;
    let m = Manifest::parse(&text)?;
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A guard over a throwaway mutex; nothing to race here.
    fn g(id: &str) -> LifecycleGuard {
        LifecycleGuard::for_test(id)
    }
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

    /// Same id as `VALID`, different everything else, so a takeover test can tell which manifest is served.
    const IMPOSTOR: &str = r#"{
        "manifest_version": 1,
        "id": "test.valid",
        "version": "9.9.9",
        "min_kernel_version": "0.1.0",
        "display_name": "Impostor",
        "entrypoint": { "command": "bin/pwn" },
        "views": [{ "view_id": "main", "title": "Main", "scope": "card" }]
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
        fs::create_dir_all(tmp.path().join("no-manifest")).unwrap();
        fs::write(tmp.path().join("README.txt"), "ignore me").unwrap();

        let (reg, report) = PluginRegistry::load_from_dir(tmp.path()).unwrap();
        assert_eq!(reg.len(), 2, "expected two loaded, got {}", reg.len());
        assert!(reg.get("test.valid").is_some());
        assert!(reg.get("test.second").is_some());
        assert!(reg.get("broken").is_none());

        assert_eq!(report.loaded.len(), 2);
        assert_eq!(report.skipped.len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn loads_a_symlinked_plugin_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins_dir = tmp.path().join("plugins");
        fs::create_dir_all(&plugins_dir).unwrap();

        let outside = tmp.path().join("outside");
        let real = write_plugin(&outside, "test.valid", VALID);

        std::os::unix::fs::symlink(&real, plugins_dir.join("test.valid")).unwrap();

        let (reg, report) = PluginRegistry::load_from_dir(&plugins_dir).unwrap();
        assert!(
            reg.get("test.valid").is_some(),
            "symlinked plugin dir must load; report = {report:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn broken_symlink_lands_in_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins_dir = tmp.path().join("plugins");
        fs::create_dir_all(&plugins_dir).unwrap();

        let dangling = plugins_dir.join("test.gone");
        std::os::unix::fs::symlink(tmp.path().join("no-such-target"), &dangling).unwrap();

        let (reg, report) = PluginRegistry::load_from_dir(&plugins_dir).unwrap();
        assert!(reg.is_empty());
        assert!(report.loaded.is_empty());
        assert_eq!(
            report.skipped.len(),
            1,
            "broken symlink must be reported, not silently dropped; report = {report:?}"
        );
        assert_eq!(report.skipped[0].0, dangling);
        // The reason, not just the count: a `symlink_metadata` loader would also land the link in `skipped`, via the other arm.
        assert!(
            report.skipped[0].1.starts_with("stat failed: "),
            "must ride the metadata-failure arm, got {:?}",
            report.skipped[0].1
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_a_file_lands_in_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins_dir = tmp.path().join("plugins");
        fs::create_dir_all(&plugins_dir).unwrap();

        let target = tmp.path().join("not-a-plugin.tar.gz");
        fs::write(&target, "tarball").unwrap();
        let link = plugins_dir.join("test.file");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        fs::write(plugins_dir.join("README.txt"), "ignore me").unwrap();

        let (reg, report) = PluginRegistry::load_from_dir(&plugins_dir).unwrap();
        assert!(reg.is_empty());
        assert_eq!(
            report.skipped.len(),
            1,
            "exactly the symlink is reported, the stray file stays silent; report = {report:?}"
        );
        assert_eq!(report.skipped[0].0, link);
        assert_eq!(
            report.skipped[0].1, "symlink does not resolve to a directory",
            "must ride the `!is_dir()` + `is_symlink()` arm, not the stat-failure one"
        );
    }

    /// `AppState::new` propagates this `Result` with `?`, so an `Err` here means the server does not boot.
    #[cfg(unix)]
    #[test]
    fn duplicate_id_via_backup_symlink_is_reported_not_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins_dir = tmp.path().join("plugins");
        fs::create_dir_all(&plugins_dir).unwrap();

        let real = write_plugin(&plugins_dir, "test.valid", VALID);
        let backup = plugins_dir.join("test.valid.bak");
        std::os::unix::fs::symlink(&real, &backup).unwrap();

        let (reg, report) = PluginRegistry::load_from_dir(&plugins_dir)
            .expect("a duplicate id must not abort the load (AppState::new uses `?`)");
        assert_eq!(reg.install_path("test.valid"), Some(real.clone()));
        assert_eq!(report.loaded, vec![real.clone()]);
        assert_eq!(report.skipped.len(), 1, "report = {report:?}");
        assert_eq!(report.skipped[0].0, backup);
        let reason = &report.skipped[0].1;
        assert!(
            reason.contains("test.valid") && reason.contains(&real.display().to_string()),
            "reason must name the id and the path it lost to, got {reason:?}"
        );
    }

    /// The loser is created *first* on disk, so a `read_dir`-order loader could plausibly keep it.
    #[test]
    fn duplicate_id_winner_is_decided_by_sorted_name_order() {
        let tmp = tempfile::tempdir().unwrap();
        let loser = write_plugin(tmp.path(), "zzz-later", VALID);
        let winner = write_plugin(tmp.path(), "aaa-earlier", VALID);

        let (reg, report) = PluginRegistry::load_from_dir(tmp.path()).unwrap();
        assert_eq!(
            reg.install_path("test.valid"),
            Some(winner.clone()),
            "the first entry in sorted name order must win; report = {report:?}"
        );
        assert_eq!(report.loaded, vec![winner]);
        assert_eq!(report.skipped.len(), 1, "report = {report:?}");
        assert_eq!(report.skipped[0].0, loser);
    }

    /// `.` sorts below the `[a-z0-9]` a legal id must start with, so a never-installed entry would be loaded under the victim's id unless the canonical entry wins.
    #[cfg(unix)]
    #[test]
    fn canonical_dir_beats_a_low_sorting_impostor_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins_dir = tmp.path().join("plugins");
        fs::create_dir_all(&plugins_dir).unwrap();

        let outside = tmp.path().join("evil");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("manifest.json"), IMPOSTOR).unwrap();
        // Created first so `read_dir` order cannot be what saves us either.
        let impostor = plugins_dir.join(".takeover");
        std::os::unix::fs::symlink(&outside, &impostor).unwrap();

        let real = write_plugin(&plugins_dir, "test.valid", VALID);

        let (reg, report) = PluginRegistry::load_from_dir(&plugins_dir)
            .expect("a duplicate id must not abort the load (AppState::new uses `?`)");
        assert_eq!(
            reg.install_path("test.valid"),
            Some(real.clone()),
            "the canonical `plugins_dir/<id>` entry must win over a lower-sorting \
             impostor claiming the same id; report = {report:?}"
        );
        assert_eq!(report.loaded, vec![real.clone()], "report = {report:?}");
        assert_eq!(report.skipped.len(), 1, "report = {report:?}");
        assert_eq!(report.skipped[0].0, impostor);
        assert_eq!(
            report.skipped[0].1,
            format!(
                "duplicate plugin id `test.valid` — already loaded from {}",
                real.display()
            ),
            "the reason must name the id and the path the impostor lost to"
        );
        // `install_path` alone does not settle it: the manifest served must be the winner's.
        let served = reg.get("test.valid").expect("winner must be retrievable");
        assert_eq!(
            served.version, "0.1.0",
            "impostor manifest served under the victim's id"
        );
        assert_eq!(served.display_name, "Valid");
        assert_eq!(
            served.entrypoint.as_ref().map(|e| e.command.as_str()),
            Some("bin/run"),
            "the supervisor would spawn the impostor's entrypoint"
        );
    }

    /// `T` = 0x54 sorts below every lowercase letter. Also pins that the name==id comparison is case-sensitive.
    #[test]
    fn canonical_dir_beats_an_uppercase_impostor() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins_dir = tmp.path().join("plugins");
        fs::create_dir_all(&plugins_dir).unwrap();

        let impostor = write_plugin(&plugins_dir, "Test.valid", IMPOSTOR);
        let real = write_plugin(&plugins_dir, "test.valid", VALID);

        let (reg, report) = PluginRegistry::load_from_dir(&plugins_dir)
            .expect("a duplicate id must not abort the load (AppState::new uses `?`)");
        assert_eq!(
            reg.install_path("test.valid"),
            Some(real.clone()),
            "the canonical `plugins_dir/<id>` entry must win over an \
             uppercase-initial impostor claiming the same id; report = {report:?}"
        );
        assert_eq!(report.loaded, vec![real.clone()], "report = {report:?}");
        assert_eq!(report.skipped.len(), 1, "report = {report:?}");
        assert_eq!(report.skipped[0].0, impostor);
        assert_eq!(
            report.skipped[0].1,
            format!(
                "duplicate plugin id `test.valid` — already loaded from {}",
                real.display()
            ),
            "the reason must name the id and the path the impostor lost to"
        );
        // `install_path` alone does not settle it: the manifest served must be the winner's.
        let served = reg.get("test.valid").expect("winner must be retrievable");
        assert_eq!(
            served.version, "0.1.0",
            "impostor manifest served under the victim's id"
        );
        assert_eq!(served.display_name, "Valid");
        assert_eq!(
            served.entrypoint.as_ref().map(|e| e.command.as_str()),
            Some("bin/run"),
            "the supervisor would spawn the impostor's entrypoint"
        );
    }

    #[test]
    fn insert_and_remove_in_memory() {
        let reg = PluginRegistry::empty();
        let m = Manifest::parse(VALID).unwrap();
        reg.insert(
            &g("test.valid"),
            m.clone(),
            Some(PathBuf::from("/tmp/fake")),
        );
        assert_eq!(reg.len(), 1);
        assert_eq!(
            reg.install_path("test.valid"),
            Some(PathBuf::from("/tmp/fake"))
        );
        let prev = reg.remove(&g("test.valid")).expect("had entry");
        assert_eq!(prev.id, m.id);
        assert!(reg.is_empty());
        assert!(reg.install_path("test.valid").is_none());
    }

    /// Holding `A`'s guard must not let you write `B`'s entry.
    #[test]
    #[should_panic(expected = "registry insert under the wrong lifecycle guard")]
    fn insert_refuses_a_guard_for_another_id() {
        let reg = PluginRegistry::empty();
        reg.insert(
            &g("test.valid"),
            Manifest::parse(SECOND_VALID).unwrap(),
            None,
        );
    }

    #[test]
    fn insert_files_the_entry_under_the_guards_id() {
        let reg = PluginRegistry::empty();
        reg.insert(
            &g("test.valid"),
            Manifest::parse(VALID).unwrap(),
            Some("/tmp/fake".into()),
        );
        assert!(reg.get("test.valid").is_some());
        assert!(reg.set_exposes_tools(&g("test.valid"), vec![]));
        assert!(reg.remove(&g("test.valid")).is_some());
    }

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
        reg.insert(
            &g("test.valid"),
            Manifest::parse(VALID).unwrap(),
            Some("/tmp/fake".into()),
        );

        assert!(reg.set_exposes_tools(&g("test.valid"), vec![tool("a"), tool("b")]));

        let after = reg.get("test.valid").expect("still registered");
        assert_eq!(
            after
                .exposes_tools
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(after.display_name, "Valid");
        assert_eq!(after.version, "0.1.0");
        assert_eq!(after.views.len(), 1);
        assert_eq!(
            reg.install_path("test.valid"),
            Some(PathBuf::from("/tmp/fake"))
        );
    }

    /// The no-op is what stops a spawn tail from resurrecting a manifest that uninstall already removed.
    #[test]
    fn set_exposes_tools_is_a_noop_for_an_absent_id() {
        let reg = PluginRegistry::empty();
        reg.insert(&g("test.valid"), Manifest::parse(VALID).unwrap(), None);

        reg.remove(&g("test.valid"));
        assert!(reg.is_empty());

        assert!(
            !reg.set_exposes_tools(&g("test.valid"), vec![tool("a")]),
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
        reg.insert(&g("test.valid"), Manifest::parse(VALID).unwrap(), None);
        reg.insert(
            &g("test.second"),
            Manifest::parse(SECOND_VALID).unwrap(),
            None,
        );
        let mut ids: Vec<String> = reg.list().into_iter().map(|m| m.id).collect();
        ids.sort();
        assert_eq!(
            ids,
            vec!["test.second".to_string(), "test.valid".to_string()]
        );
    }
}
