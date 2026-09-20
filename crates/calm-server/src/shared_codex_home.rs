//! Shared Codex home: seeds and maintains the shared CODEX_HOME with toml_edit
//! round-trip config edits.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::mcp_server::McpShimConfig;
use crate::mcp_server::wiring::daemon_shim_env;
use toml_edit::DocumentMut;

/// The only `[mcp_servers.*]` keys that may legitimately exist in the shared CODEX_HOME's
/// config.toml; `seed_from` strips host `mcp_servers`, so this is a const, not config.
pub const EXPECTED_MCP_SERVERS: &[&str] = &["calm"];

/// Layout: <data_dir>/codex-home/ — the shared daemon's single CODEX_HOME, no per-card subdir.
pub struct SharedCodexHome {
    home: PathBuf,
    #[allow(dead_code)]
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

    /// Boot-time seed: if the home does not exist, mkdir and import operator identity and
    /// model config from the host `~/.codex/`. Never overwrites existing files.
    pub fn seed(&self) -> io::Result<()> {
        let host = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex"));
        self.seed_from(host.as_deref())
    }

    /// Explicit two-file sanitized import (never a recursive copy): `auth.json`, and
    /// `config.toml` with the `mcp_servers` and `hooks` tables (executable vectors) stripped.
    pub fn seed_from(&self, host_codex_dir: Option<&Path>) -> io::Result<()> {
        fs::create_dir_all(&self.home)?;

        let Some(host_codex_dir) = host_codex_dir else {
            return Ok(());
        };
        if !host_codex_dir.exists() {
            return Ok(());
        }

        let src_auth = host_codex_dir.join("auth.json");
        let dst_auth = self.home.join("auth.json");
        if src_auth.exists() && !dst_auth.exists() {
            fs::copy(src_auth, dst_auth)?;
        }

        let src_cfg = host_codex_dir.join("config.toml");
        let dst_cfg = self.home.join("config.toml");
        if src_cfg.exists() && !dst_cfg.exists() {
            let lock_path = self.home.join(".config.lock");
            let _lock = ConfigLock::acquire(&lock_path)?;
            // Re-check under the lock: a concurrent writer may have created it.
            if !dst_cfg.exists() {
                let text = fs::read_to_string(&src_cfg)?;
                match text.parse::<DocumentMut>() {
                    Ok(mut doc) => {
                        let table = doc.as_table_mut();
                        let stripped_mcp = table.remove("mcp_servers").is_some();
                        let stripped_hooks = table.remove("hooks").is_some();
                        if stripped_mcp || stripped_hooks {
                            tracing::info!(
                                host = %host_codex_dir.display(),
                                stripped_mcp,
                                stripped_hooks,
                                "seed: stripped host mcp_servers/hooks tables from imported config.toml"
                            );
                        }
                        write_config_0600(&dst_cfg, doc.to_string().as_bytes())?;
                    }
                    Err(e) => {
                        tracing::warn!(
                            host = %host_codex_dir.display(),
                            error = %e,
                            "host ~/.codex/config.toml is not valid TOML; skipping config import"
                        );
                    }
                }
            }
        }

        Ok(())
    }

    /// The `model` / `model_reasoning_effort` defaults in the shared CODEX_HOME `config.toml`.
    /// Dormant-only source for `GET /api/models`: whenever a daemon connection exists,
    /// `config/read` is the source. Missing file is `Ok(default)`; unparseable TOML is an `Err`.
    pub fn read_default_model_settings(&self) -> io::Result<ConfigTomlModelDefaults> {
        let cfg_path = self.home.join("config.toml");
        let text = match fs::read_to_string(&cfg_path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(ConfigTomlModelDefaults::default());
            }
            Err(e) => return Err(e),
        };
        let doc: DocumentMut = text.parse().map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "shared CODEX_HOME config.toml at {} is not valid TOML: {e}",
                    cfg_path.display()
                ),
            )
        })?;
        Ok(ConfigTomlModelDefaults {
            model: top_level_str(&doc, "model"),
            reasoning_effort: top_level_str(&doc, "model_reasoning_effort"),
        })
    }

    /// Boot guard: errors if `mcp_servers` has any key outside `expected` OR any `hooks` table
    /// exists. Subset policy, not exact-set. Enumerates via `as_table_like()` so inline tables /
    /// dotted keys cannot evade it; also deletes a leaked `<home>/.env` instead of refusing.
    pub fn verify_expected_mcp_servers(&self, expected: &[&str]) -> io::Result<()> {
        if !self.home.exists() {
            return Ok(());
        }
        let lock_path = self.home.join(".config.lock");
        let _lock = ConfigLock::acquire(&lock_path)?;
        // A `<home>/.env` created while the daemon runs would be injected into the daemon's own
        // process env by codex arg0 `load_dotenv` at the next spawn, bypassing the spawn allow-list.
        // It is derived state, so the guard DELETES it instead of refusing.
        if self.remove_leaked_env_file()? {
            tracing::warn!(
                home = %self.home.display(),
                "launch guard deleted leaked CODEX_HOME/.env before spawn"
            );
        }
        let cfg_path = self.home.join("config.toml");
        let text = match fs::read_to_string(&cfg_path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let doc: DocumentMut = text.parse().map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "shared CODEX_HOME config.toml at {} is not valid TOML: {e}",
                    cfg_path.display()
                ),
            )
        })?;

        let mut offenders: Vec<String> = Vec::new();
        if let Some(item) = doc.get("mcp_servers") {
            match item.as_table_like() {
                Some(table) => {
                    for (key, _) in table.iter() {
                        if !expected.contains(&key) {
                            offenders.push(format!("mcp_servers.{key}"));
                        }
                    }
                }
                None => offenders.push("mcp_servers".to_string()),
            }
        }
        if doc.get("hooks").is_some() {
            offenders.push("hooks".to_string());
        }

        if offenders.is_empty() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "shared CODEX_HOME config.toml at {} contains unexpected entries: {}",
                    cfg_path.display(),
                    offenders.join(", ")
                ),
            ))
        }
    }

    /// One-time boot repair for polluted homes: removes unexpected `[mcp_servers.*]` keys and
    /// the `hooks` table, and deletes a leaked `CODEX_HOME/.env`. Returns what it removed.
    pub fn sanitize_unexpected_mcp_servers(&self, expected: &[&str]) -> io::Result<Vec<String>> {
        let mut removed: Vec<String> = Vec::new();

        if self.remove_leaked_env_file()? {
            removed.push(".env".to_string());
        }

        if !self.home.exists() {
            return Ok(removed);
        }
        let lock_path = self.home.join(".config.lock");
        let _lock = ConfigLock::acquire(&lock_path)?;
        let cfg_path = self.home.join("config.toml");
        let text = match fs::read_to_string(&cfg_path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(removed),
            Err(e) => return Err(e),
        };
        let mut doc: DocumentMut = text.parse().map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "shared CODEX_HOME config.toml at {} is not valid TOML: {e}",
                    cfg_path.display()
                ),
            )
        })?;

        if let Some(item) = doc.get_mut("mcp_servers") {
            match item.as_table_like_mut() {
                Some(table) => {
                    let unexpected: Vec<String> = table
                        .iter()
                        .map(|(key, _)| key.to_string())
                        .filter(|key| !expected.contains(&key.as_str()))
                        .collect();
                    for key in unexpected {
                        table.remove(&key);
                        removed.push(format!("mcp_servers.{key}"));
                    }
                }
                None => {
                    doc.as_table_mut().remove("mcp_servers");
                    removed.push("mcp_servers".to_string());
                }
            }
        }
        if doc.as_table_mut().remove("hooks").is_some() {
            removed.push("hooks".to_string());
        }

        let new_text = doc.to_string();
        if new_text != text {
            write_config_0600(&cfg_path, new_text.as_bytes())?;
        }

        Ok(removed)
    }

    /// Delete a leaked `<home>/.env` if present (codex arg0 `load_dotenv` would inject it into
    /// the daemon's process env, bypassing the spawn allow-list). Returns whether a file was removed.
    fn remove_leaked_env_file(&self) -> io::Result<bool> {
        match fs::remove_file(self.home.join(".env")) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// toml_edit round-trip idempotent writer for `<home>/config.toml`.
    pub fn ensure_config_for_cwd(&self, cwd: &Path) -> io::Result<()> {
        self.ensure_config(Some(cwd), None)
    }

    /// Ensure the shared CODEX_HOME has the daemon-level MCP shim config.
    pub fn ensure_daemon_mcp_config(
        &self,
        shim: &McpShimConfig,
        daemon_token: &str,
    ) -> io::Result<()> {
        self.ensure_config(None, Some((shim, daemon_token)))
    }

    fn ensure_config(
        &self,
        cwd: Option<&Path>,
        mcp_block: Option<(&McpShimConfig, &str)>,
    ) -> io::Result<()> {
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

        if let Some(cwd) = cwd {
            let cwd_str = cwd.to_string_lossy().into_owned();
            let projects = doc["projects"].or_insert(toml_edit::table());
            if let Some(projects_table) = projects.as_table_mut() {
                projects_table.set_implicit(true);
                let project = projects_table.entry(&cwd_str).or_insert(toml_edit::table());
                if let Some(project_table) = project.as_table_mut() {
                    project_table["trust_level"] = toml_edit::value("trusted");
                }
            }
        }

        if let Some((shim, daemon_token)) = mcp_block {
            let mcp_servers = doc["mcp_servers"].or_insert(toml_edit::table());
            if let Some(mcp_servers_table) = mcp_servers.as_table_mut() {
                mcp_servers_table.set_implicit(true);
                let calm = mcp_servers_table
                    .entry("calm")
                    .or_insert(toml_edit::table());
                if let Some(calm_table) = calm.as_table_mut() {
                    calm_table["command"] =
                        toml_edit::value(shim.shim_bin.to_string_lossy().to_string());
                    calm_table["args"] = toml_edit::value(toml_edit::Array::new());
                    let env = calm_table.entry("env").or_insert(toml_edit::table());
                    if let Some(env_table) = env.as_table_mut() {
                        for (key, value) in daemon_shim_env(&shim.socket_path, daemon_token) {
                            env_table[key] = toml_edit::value(value);
                        }
                    }
                }
            }
        }

        let new_text = doc.to_string();
        if new_text != text {
            write_config_0600(&cfg_path, new_text.as_bytes())?;
        } else if cfg_path.exists() {
            // Unchanged content: the atomic writer didn't run, but a pre-existing config may carry
            // loose perms; tighten in place.
            fs::set_permissions(&cfg_path, fs::Permissions::from_mode(0o600))?;
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

/// Write `<home>/config.toml` atomically with 0600 perms (it can carry the daemon MCP token):
/// 0600 sibling temp, fsync, `rename(2)`. All callers hold `ConfigLock`, so the fixed temp
/// name cannot collide.
fn write_config_0600(cfg_path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp_path = cfg_path.with_extension("toml.tmp");
    let write_and_rename = || -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp_path)?;
        // `mode(0o600)` only applies on create; enforce on a leftover temp
        // from a crashed earlier write too.
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp_path, cfg_path)
    };
    // On ANY pre-rename failure remove the temp file: it can carry the daemon MCP token.
    if let Err(e) = write_and_rename() {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }
    if let Some(parent) = cfg_path.parent()
        && let Ok(dir) = fs::File::open(parent)
    {
        let _ = dir.sync_all();
    }
    Ok(())
}

struct ConfigLock {
    file: fs::File,
}

impl ConfigLock {
    fn acquire(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
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

/// Model defaults read out of the shared CODEX_HOME `config.toml`; both optional because
/// "not configured" is `null` on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConfigTomlModelDefaults {
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
}

fn top_level_str(doc: &DocumentMut, key: &str) -> Option<String> {
    doc.get(key)
        .and_then(|item| item.as_str())
        .map(str::to_string)
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
