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

use super::LifecycleGuard;
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

    /// #1196 S0a — **build-time** construction (设计 §2.3「建表期 vs 运行期」).
    ///
    /// Seeding a registry that no [`super::PluginHost`] owns yet is a different
    /// operation from mutating a live one: there is no lifecycle guard to hold
    /// because there is nothing to race with. Those writes go here; the runtime
    /// mutators ([`Self::insert`] / [`Self::remove`] / [`Self::set_exposes_tools`])
    /// are `pub(crate)` and grow a `&LifecycleGuard` parameter in S1.
    ///
    /// The builder is **consuming**: once [`PluginRegistryBuilder::build`] hands
    /// back a `PluginRegistry`, the build-time write path is gone.
    pub fn builder() -> PluginRegistryBuilder {
        PluginRegistryBuilder {
            inner: Inner::default(),
        }
    }

    /// One-shot form of [`Self::builder`] for the common "seed N manifests"
    /// shape.
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

    /// Walk `dir` one level deep, treating each entry that **resolves** to a
    /// directory as a candidate plugin — symlinks included, because an install
    /// from a source outside `plugins_dir` is materialized as one. Loads
    /// `<subdir>/manifest.json` for each; on parse or validation failure, logs
    /// a warning via `tracing::warn!` and skips that plugin — the rest of the
    /// directory still loads.
    ///
    /// Entries that are not directories: a plain file at the root (a stray
    /// README, a leftover tarball) is ignored silently; anything that fails to
    /// stat, and any symlink not resolving to a directory, goes into
    /// [`LoadReport::skipped`] with a reason.
    ///
    /// Entries are visited in sorted file-name order (`OsString` ordering,
    /// i.e. raw byte order on unix) rather than in the order `std::fs::read_dir`
    /// happens to yield. When two entries carry manifests with the same id, only
    /// one is loaded and the other goes into [`LoadReport::skipped`] with a
    /// reason naming the id and the path it lost to; a duplicate id does not
    /// abort the load. The winner is the entry whose directory name equals the
    /// manifest id — the shape every install produces — and, when neither entry
    /// has that shape, the first one in sorted name order. The directory name
    /// alone does not decide it: names are attacker-chosen and the id is read
    /// out of the manifest afterwards, so a low-sorting name would otherwise be
    /// loaded under another plugin's id.
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

        // Collect before walking: the duplicate-id arm below keeps the first
        // entry it sees, and `std::fs::read_dir` yields entries in an order the
        // filesystem chooses. Sorting by file name makes "which duplicate wins"
        // a property of the names on disk instead of of the filesystem.
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
            // `DirEntry::file_type()` does NOT follow symlinks, so it is used
            // here only to tell "this entry is a symlink" apart from "this
            // entry is a plain file" — never to decide directory-ness.
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
            // #1168 — installs whose source lives outside `plugins_dir` are
            // materialized as a symlink (see
            // `plugin_host::lifecycle::materialize_install_tree`). `fs::metadata`
            // follows, so the symlinked tree is seen as the directory it is.
            // It fails on a broken symlink; that is a report, not a silent drop.
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
                // A plain file at the root (a stray README, a leftover tarball)
                // is silently ignored — it never claimed to be a plugin. A
                // symlink that resolves to something other than a directory is
                // an install artifact, so it is reported instead of dropped.
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
                    // Two manifests claiming one id would race for the same
                    // `plugins` row, token and kv namespace, so only one may
                    // load. Refusing to boot is not an option: an operator's
                    // `ln -s plugins/foo plugins/foo.bak` now resolves to a
                    // directory and would take the whole server down.
                    //
                    // Which one wins may not be decided by the directory name
                    // alone: the name is attacker-chosen and the id is read out
                    // of the manifest afterwards, so a name that sorts early
                    // (`.takeover`, `Zed`: `.` and every uppercase letter sort
                    // below the `[a-z0-9]` a legal id must start with) would
                    // otherwise be loaded *under the victim's id* and push the
                    // real install into `skipped`. So the canonical entry —
                    // the one whose directory name equals the manifest id,
                    // which is the shape every install produces — wins over any
                    // entry that does not have that shape. At most one entry in
                    // a directory can bear a given name, so a canonical entry
                    // is never displaced; among entries that are all
                    // non-canonical the sorted-name order decides, and the
                    // loser is reported either way.
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
    /// after the file copy completes.
    ///
    /// #1196 S0a — **runtime** write. Deliberately `pub(in crate::plugin_host)`:
    /// #1196 S1 gives this method a `&LifecycleGuard` parameter, and only
    /// [`super::PluginHost`] can ever hold one. Narrower than `pub(crate)` on
    /// purpose — with `pub(crate)` the routes could (and did) keep calling this
    /// directly, so S1 would not have produced a compile error there and those
    /// three lifecycle writes would have survived as unlocked writes. Every
    /// write from outside this module tree goes through
    /// [`super::PluginHost::registry_insert`]; everything that writes the
    /// registry *before* a [`super::PluginHost`] exists must go through
    /// [`PluginRegistry::builder`] / [`PluginRegistry::from_manifests`] instead
    /// — see the design doc §2.3 ("建表期 vs 运行期"). Do NOT re-widen this to
    /// `pub` / `pub(crate)` and do NOT add a `insert_unlocked` escape hatch: the
    /// signature is the only thing that forces the migration.
    /// #1196 S1 — takes the [`LifecycleGuard`] for the id being written. Only
    /// [`super::PluginHost::try_lock_lifecycle`] /
    /// [`super::PluginHost::await_lifecycle`] can produce one, so the registry
    /// and the live process table are now behind the same lock: an in-flight
    /// spawn's `set_exposes_tools` and a concurrent uninstall's `remove` can no
    /// longer interleave (design §1.2 races 1 and 4).
    ///
    /// **#1196 S1 review P0-2 — the key is read off the guard, like every other
    /// mutator here.** It used to be `manifest.id` with the guard ignored, which
    /// made the guard a bare capability token: holding `A`'s guard was enough to
    /// write `B`'s entry, and `try_lock_lifecycle` is `pub`, so that was
    /// expressible — not merely un-prevented. `remove` and `set_exposes_tools`
    /// already keyed off the guard; `emit_state_under` does too. This was the
    /// one hole.
    ///
    /// The `assert_eq!` is what keeps the key and the stored manifest from ever
    /// disagreeing (an entry filed under `A` whose manifest says `B` would be a
    /// worse bug than the one being fixed). It is a panic rather than a silent
    /// no-op or a `Result` because it is provably unreachable in-tree — both
    /// production call sites (`install`, `reload`) take the guard on exactly the
    /// id they then write, and `reload` re-checks `manifest.id != id` before
    /// getting here — and because a caller that *did* trip it has already lost
    /// the invariant the lock exists to hold; carrying on would corrupt the
    /// registry quietly.
    ///
    /// Residual, stated honestly: a `LifecycleGuard` does not carry the identity
    /// of the [`super::PluginHost`] whose map minted it, so a guard from a second
    /// host over the same id is still accepted. Every process has one host;
    /// closing that needs the guard to carry a host token, which is #1196 S2
    /// material, not this fix.
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

    /// #1164 §2.7(2)(3) — replace ONLY the `exposes_tools` field of an
    /// already-registered manifest, under a single write lock.
    ///
    /// Two properties are load-bearing and must not be "simplified" into a
    /// `get` → mutate → [`insert`](Self::insert) sequence:
    ///
    /// 1. **Field-level, single lock.** `insert` swaps the WHOLE `Manifest`.
    ///    A read-modify-write would race a concurrent `/reload` and roll the
    ///    entire manifest back — url, `tools_allow`, permissions, views,
    ///    templates — with nothing to notice until the next reload. Confining
    ///    the write to one field caps the worst case at "stale tool list".
    ///
    /// 2. **No-op when `id` is absent.** If materialization inserted, it would
    ///    RESURRECT a manifest that `uninstall` removed.
    ///
    ///    Honest status after #1196 S1: this arm no longer neutralizes a *race*.
    ///    The old comment here said "§5 R12: there is no per-plugin lifecycle
    ///    lock" — S1 is that lock, and it contradicts the paragraph that follows
    ///    it, so it is gone. An in-flight spawn holds `id`'s guard from before
    ///    its registry lookup until after this call, and `uninstall` cannot take
    ///    that guard meanwhile, so the entry cannot vanish under a spawn. What
    ///    the `false` arm now covers is the fail-closed residue: an id that was
    ///    never in the registry to begin with, and any future caller reaching
    ///    here outside a spawn's lookup→write span. The caller
    ///    (`spawn_mcp_http`) reads the `false` and abandons the spawn with a
    ///    terminal `Unavailable` rather than inserting a live entry, which is
    ///    the behaviour worth keeping either way.
    ///
    /// Returns whether the entry existed (and was therefore updated).
    ///
    /// #1196 S0a — **runtime** write, `pub(in crate::plugin_host)` for the same
    /// reason as [`Self::insert`].
    /// #1196 S1 — takes the [`LifecycleGuard`], and reads the id **off it**.
    /// Property 2 below stops being a race-neutralizer and becomes a plain
    /// invariant: an uninstall cannot be inside its own critical section while
    /// this one is running.
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
    ///
    /// #1196 S0a — **runtime** write, `pub(in crate::plugin_host)` for the same
    /// reason as [`Self::insert`].
    pub(in crate::plugin_host) fn remove(&self, guard: &LifecycleGuard) -> Option<Manifest> {
        let id = guard.id();
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
// Build-time construction (#1196 S0a)
// ---------------------------------------------------------------------------

/// Consuming builder for a registry that is being *assembled*, before any
/// [`super::PluginHost`] owns it. See [`PluginRegistry::builder`].
///
/// Note there is no `build_ref` / `as_registry` — `build` moves `self`, so a
/// caller cannot keep writing through the builder after the registry is live.
#[derive(Default)]
pub struct PluginRegistryBuilder {
    inner: Inner,
}

impl PluginRegistryBuilder {
    /// Seed one manifest. Last write wins on duplicate ids, matching the
    /// runtime [`PluginRegistry::insert`] this replaces.
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

/// Does this entry have the canonical shape an install produces — a directory
/// whose own name *is* the manifest id (`plugins_dir/<id>`, see
/// `plugin_host::lifecycle::materialize_install_tree`)?
///
/// The comparison is on the raw `OsStr`, so it is byte-exact: a legal id is
/// `^[a-z0-9][a-z0-9.-]{1,63}$` (`manifest::ID_RE`), and an entry named with a
/// different case (`Test.valid` against id `test.valid`) is a *different*
/// directory that merely resembles the canonical one — treating it as canonical
/// would hand it the win, because uppercase sorts below lowercase.
fn dir_name_is_id(path: &Path, id: &str) -> bool {
    path.file_name() == Some(std::ffi::OsStr::new(id))
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

    /// A lifecycle guard for `id`, over a throwaway mutex. These unit tests
    /// exercise the registry in isolation; there is no host and nothing to
    /// race, so the guard is a pure capability token here.
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

    /// Same id as [`VALID`], different everything else. A takeover fixture whose
    /// manifest were byte-identical to the victim's could not tell "the canonical
    /// entry won" apart from "the impostor won but happens to look the same":
    /// with identical content, dropping the winner's `manifests.insert`
    /// altogether is unobservable through `get`.
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

    /// #1168 regression — a plugin installed from a source **outside**
    /// `plugins_dir` is materialized as a symlink (see
    /// `plugin_host::lifecycle::materialize_install_tree`). `DirEntry::file_type()`
    /// does not follow symlinks, so the loader used to see `is_dir() == false`
    /// and silently `continue`, dropping the plugin on every restart.
    #[cfg(unix)]
    #[test]
    fn loads_a_symlinked_plugin_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins_dir = tmp.path().join("plugins");
        fs::create_dir_all(&plugins_dir).unwrap();

        // The real tree lives outside plugins_dir — the `cargo build` +
        // `curl install` shape.
        let outside = tmp.path().join("outside");
        let real = write_plugin(&outside, "test.valid", VALID);

        std::os::unix::fs::symlink(&real, plugins_dir.join("test.valid")).unwrap();

        let (reg, report) = PluginRegistry::load_from_dir(&plugins_dir).unwrap();
        assert!(
            reg.get("test.valid").is_some(),
            "symlinked plugin dir must load; report = {report:?}"
        );
    }

    /// The other half of #1168: a symlink at the plugin root that does not
    /// resolve to a directory is an install artifact, not stray litter — it
    /// must be reported, never silently dropped. (A plain stray file still is
    /// silently ignored; that is pinned by
    /// `loads_two_skips_one_broken_and_one_no_manifest`.)
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
        // The reason, not just the count: without this the test also passes
        // when the loader stats with `symlink_metadata`, i.e. under exactly the
        // defect #1168 removes — the dangling link then falls through the
        // `!is_dir() && is_symlink()` arm and still lands in `skipped`.
        assert!(
            report.skipped[0].1.starts_with("stat failed: "),
            "must ride the metadata-failure arm, got {:?}",
            report.skipped[0].1
        );
    }

    /// The arm the previous test does *not* reach: a symlink that resolves
    /// fine, but to a regular file. `fs::metadata` succeeds here, so this is
    /// the `!metadata.is_dir()` + `entry_type.is_symlink()` branch — still an
    /// install artifact, still reported rather than dropped.
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

        // A genuinely stray plain file next to it stays silent — that is the
        // half of the old behaviour the fix must preserve.
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

    /// A duplicate id must not abort the load. `AppState::new` propagates this
    /// `Result` with `?`, so returning `Err` here means the server does not
    /// boot — and once the loader follows symlinks (#1168), an operator's
    /// ordinary `ln -s plugins/test.valid plugins/test.valid.bak` manufactures
    /// a duplicate. The real directory wins because its name sorts first; the
    /// backup link is reported.
    #[cfg(unix)]
    #[test]
    fn duplicate_id_via_backup_symlink_is_reported_not_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins_dir = tmp.path().join("plugins");
        fs::create_dir_all(&plugins_dir).unwrap();

        let real = write_plugin(&plugins_dir, "test.valid", VALID);
        // "test.valid" < "test.valid.bak" in byte order, so the real dir wins.
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

    /// Which of two same-id entries wins is decided by sorted file-name order,
    /// not by whatever order `read_dir` yields. The loser is created *first* on
    /// disk, so a `read_dir`-order loader could plausibly keep it instead.
    #[test]
    fn duplicate_id_winner_is_decided_by_sorted_name_order() {
        let tmp = tempfile::tempdir().unwrap();
        // Both manifests claim id="test.valid"; only the directory names differ.
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

    /// The sort key is the *directory name*; the id comes out of
    /// `manifest.json`, read afterwards. So "first in sorted name order wins"
    /// on its own is a plugin-takeover primitive: `.` and every uppercase
    /// letter sort below the `[a-z0-9]` a legal id must start with
    /// (`manifest::ID_RE`), so an entry an operator never installed sorts ahead
    /// of every legitimately-installed plugin and would be loaded **under the
    /// victim's id**, pushing the real install into `skipped`. The canonical
    /// entry — directory name == manifest id, the shape
    /// `materialize_install_tree` always produces — must win instead.
    ///
    /// This case is the symlink-to-an-outside-tree construction: the impostor
    /// tree lives outside `plugins_dir` entirely, which #1168's symlink
    /// following is what makes loadable at all.
    ///
    /// Mutation witness: drop the `path_is_canonical && !prev_is_canonical`
    /// displacement arm (leaving plain first-in-sorted-order) and this test
    /// goes red on `install_path("test.valid") == Some(real)`.
    #[cfg(unix)]
    #[test]
    fn canonical_dir_beats_a_low_sorting_impostor_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins_dir = tmp.path().join("plugins");
        fs::create_dir_all(&plugins_dir).unwrap();

        // The impostor tree is outside `plugins_dir` and claims the victim's id.
        let outside = tmp.path().join("evil");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("manifest.json"), IMPOSTOR).unwrap();
        // ".takeover" sorts before "test.valid" ('.' = 0x2e < 't'), and is
        // created first so `read_dir` order cannot be what saves us either.
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
        // `install_path` alone does not settle it: the manifest served under
        // this id must be the *winner's*, not the impostor's left behind by the
        // losing entry's earlier insert. `entrypoint.command` is what the
        // supervisor would go on to execute.
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

    /// Same takeover, uppercase-initial name instead of a leading dot: `T` =
    /// 0x54 sorts below every lowercase letter, so `Test.valid` also precedes
    /// the canonical `test.valid`.
    ///
    /// This case additionally pins that the name==id comparison is
    /// case-*sensitive*: under a case-insensitive comparator both entries would
    /// count as canonical, neither would displace the other, and the
    /// first-in-sorted-order tiebreak would hand the win to `Test.valid`.
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
        // `install_path` alone does not settle it: the manifest served under
        // this id must be the *winner's*, not the impostor's left behind by the
        // losing entry's earlier insert. `entrypoint.command` is what the
        // supervisor would go on to execute.
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

    // -----------------------------------------------------------------
    // #1196 S1 review P0-2 — every mutator is bound to the guard's id
    // -----------------------------------------------------------------

    /// Holding `A`'s guard must not let you write `B`'s entry. `insert` used to
    /// key off `manifest.id` and ignore the guard entirely, so this wrote
    /// `test.second` under `test.valid`'s lock — with `try_lock_lifecycle`
    /// being `pub`, that was expressible from outside the module.
    ///
    /// Mutation witness: restore `let id = manifest.id.clone();` in
    /// [`PluginRegistry::insert`] and this test stops panicking, i.e. goes red
    /// on the missing panic.
    #[test]
    #[should_panic(expected = "registry insert under the wrong lifecycle guard")]
    fn insert_refuses_a_guard_for_another_id() {
        let reg = PluginRegistry::empty();
        // The guard is for `test.valid`; the manifest is `test.second`.
        reg.insert(
            &g("test.valid"),
            Manifest::parse(SECOND_VALID).unwrap(),
            None,
        );
    }

    /// The other half: the id the entry is filed under is the guard's, so a
    /// matching pair still round-trips and `remove`/`set_exposes_tools` — which
    /// have always keyed off the guard — find it.
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

    /// §2.7(3): the no-op is what stops a spawn tail from resurrecting a
    /// manifest that uninstall already removed. #1196 S1 downgraded this from a
    /// race neutralizer to a fail-closed residue — see the method doc — but the
    /// behaviour it pins is unchanged and still read by `spawn_mcp_http`.
    #[test]
    fn set_exposes_tools_is_a_noop_for_an_absent_id() {
        let reg = PluginRegistry::empty();
        reg.insert(&g("test.valid"), Manifest::parse(VALID).unwrap(), None);

        // Uninstall removed it.
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
