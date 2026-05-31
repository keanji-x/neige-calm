//! Shared Codex home support for #410 PR1.
//!
//! PR1 seeds and maintains the shared home only, using toml_edit round-trip
//! config edits. Existing card spawn paths keep using the legacy per-card homes
//! until later PRs switch callers.

use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use toml_edit::DocumentMut;

/// Layout: <data_dir>/codex-home/  <- shared, no per-card subdir
/// 共享 daemon 的单一 CODEX_HOME，PR4 之后所有 card 会指向这里。
/// PR1 只 seed + writer，不切换 callers。
pub struct SharedCodexHome {
    home: PathBuf,
    legacy_homes_parent: PathBuf,
}

impl SharedCodexHome {
    pub fn new(home: PathBuf, legacy_homes_parent: PathBuf) -> Self {
        Self {
            home,
            legacy_homes_parent,
        }
    }

    pub fn path(&self) -> &Path {
        &self.home
    }

    pub fn legacy_parent(&self) -> &Path {
        &self.legacy_homes_parent
    }

    /// Boot-time seed。如果 home 不存在：mkdir + 复制 host `~/.codex/`
    /// 模板（如果存在）。不覆盖已有 auth.json。
    pub fn seed(&self) -> io::Result<()> {
        let host = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex"));
        self.seed_from(host.as_deref())
    }

    pub fn seed_from(&self, host_codex_dir: Option<&Path>) -> io::Result<()> {
        fs::create_dir_all(&self.home)?;

        let Some(host_codex_dir) = host_codex_dir else {
            return Ok(());
        };
        if !host_codex_dir.exists() {
            return Ok(());
        }

        // Defense against recursive self-copy: if CALM_DATA_DIR is configured
        // under host ~/.codex, the shared CODEX_HOME can be read back as a
        // source entry and recurse into codex-home/codex-home/...
        if is_nested_or_same(&self.home, host_codex_dir) {
            tracing::warn!(
                host = %host_codex_dir.display(),
                home = %self.home.display(),
                "shared CODEX_HOME is inside host ~/.codex tree; skipping recursive seed to avoid self-copy"
            );
            return Ok(());
        }

        copy_dir_recursive_excluding_top_auth(host_codex_dir, &self.home)?;

        let src_auth = host_codex_dir.join("auth.json");
        let dst_auth = self.home.join("auth.json");
        if src_auth.exists() && !dst_auth.exists() {
            fs::copy(src_auth, dst_auth)?;
        }

        Ok(())
    }

    /// toml_edit round-trip idempotent writer for `<home>/config.toml`.
    pub fn ensure_config_for_cwd(&self, cwd: &Path) -> io::Result<()> {
        fs::create_dir_all(&self.home)?;

        let lock_path = self.home.join(".config.lock");
        let _lock = ConfigLock::acquire(&lock_path)?;

        let cfg_path = self.home.join("config.toml");
        let text = match fs::read_to_string(&cfg_path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        let mut doc: DocumentMut = text.parse().map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("shared CODEX_HOME config.toml is not valid TOML: {e}"),
            )
        })?;

        ensure_top_level_str(&mut doc, "approval_policy", "never");
        ensure_top_level_str(&mut doc, "sandbox_mode", "workspace-write");
        ensure_table_bool(&mut doc, "sandbox_workspace_write", "network_access", true);

        let cwd_str = cwd.to_string_lossy().into_owned();
        let projects = doc["projects"].or_insert(toml_edit::table());
        if let Some(projects_table) = projects.as_table_mut() {
            projects_table.set_implicit(true);
            let project = projects_table.entry(&cwd_str).or_insert(toml_edit::table());
            if let Some(project_table) = project.as_table_mut() {
                project_table["trust_level"] = toml_edit::value("trusted");
            }
        }

        let new_text = doc.to_string();
        if new_text != text {
            fs::write(cfg_path, new_text)?;
        }

        Ok(())
    }

    /// Returns Codex 0.134/0.135 runtime state files, relative to this home.
    pub fn codex_runtime_state_files(&self) -> Vec<PathBuf> {
        [
            "state_5.sqlite",
            "logs_2.sqlite",
            "goals_1.sqlite",
            "memories_1.sqlite",
        ]
        .into_iter()
        .flat_map(|name| {
            [
                PathBuf::from(name),
                PathBuf::from(format!("{name}-wal")),
                PathBuf::from(format!("{name}-shm")),
            ]
        })
        .collect()
    }
}

fn is_nested_or_same(child: &Path, parent: &Path) -> bool {
    if let (Ok(child), Ok(parent)) = (child.canonicalize(), parent.canonicalize()) {
        return child == parent || child.starts_with(parent);
    }

    let child = child.components().collect::<Vec<_>>();
    let parent = parent.components().collect::<Vec<_>>();
    if parent.len() > child.len() {
        return false;
    }
    parent.iter().zip(child.iter()).all(|(a, b)| a == b)
}

struct ConfigLock {
    file: fs::File,
}

impl ConfigLock {
    fn acquire(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(path)?;
        flock(file.as_raw_fd(), libc::LOCK_EX)?;
        Ok(Self { file })
    }
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        let _ = flock(self.file.as_raw_fd(), libc::LOCK_UN);
    }
}

fn flock(fd: i32, operation: i32) -> io::Result<()> {
    // SAFETY: `fd` is owned by an open `std::fs::File` for the duration of
    // this call. `flock(2)` does not retain pointers into Rust memory.
    let rc = unsafe { libc::flock(fd, operation) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn copy_dir_recursive_excluding_top_auth(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        if entry.file_name() == OsStr::new("auth.json") {
            continue;
        }
        copy_entry_recursive(&entry.path(), &dst.join(entry.file_name()))?;
    }
    Ok(())
}

fn copy_entry_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(src)?;
    if meta.is_dir() {
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            copy_entry_recursive(&entry.path(), &dst.join(entry.file_name()))?;
        }
    } else if meta.is_file() && !dst.exists() {
        fs::copy(src, dst)?;
    }
    Ok(())
}

fn ensure_top_level_str(doc: &mut DocumentMut, key: &str, value: &str) {
    if doc.get(key).is_none() {
        doc[key] = toml_edit::value(value);
    }
}

fn ensure_table_bool(doc: &mut DocumentMut, table: &str, key: &str, value: bool) {
    let entry = doc[table].or_insert(toml_edit::table());
    if let Some(table) = entry.as_table_mut()
        && table.get(key).is_none()
    {
        table[key] = toml_edit::value(value);
    }
}
