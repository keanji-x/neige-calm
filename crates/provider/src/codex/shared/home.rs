//! Shared Codex home: seeds and maintains the shared CODEX_HOME with toml_edit
//! round-trip config edits.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use toml_edit::DocumentMut;

/// Application-declared MCP entry. The owner supplies the identifier and credentials;
/// provider only maintains the Codex config representation. Deliberately has no Debug impl.
pub struct McpServerConfig<'a> {
    pub key: &'a str,
    pub command: &'a Path,
    pub env: &'a [(String, String)],
}

/// The operator's own Codex home, as Codex resolves it: a non-empty `CODEX_HOME`, else
/// `~/.codex`.
pub fn host_codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
}

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
    /// model config from [`host_codex_home`]. Never overwrites existing files.
    pub fn seed(&self) -> io::Result<()> {
        self.seed_from(host_codex_home().as_deref())
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
                            "host Codex config.toml is not valid TOML; skipping config import"
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

    /// Refresh the application entry `[mcp_servers.<key>]`: bump its `env.<toolset_key>` generation
    /// to one past its current value, and make its per-tool map `tools` exactly `approved_tools`, each
    /// with Codex's never-ask approval mode. Returns the new generation. An absent or unparseable
    /// generation counts as 0; the counter wraps, which still yields a value unlike the current one,
    /// the only property Codex's entry comparison needs. The entry's `tools` map belongs to this
    /// writer: a tool outside `approved_tools` and any other per-tool key are removed, and an empty
    /// set removes the map. Nothing else in the entry changes. Same lock and atomic writer as every
    /// other edit, so one write carries both. Refuses when the entry has no `command`: a lone `env`
    /// table would leave Codex an entry it cannot start.
    pub fn refresh_mcp_entry(
        &self,
        key: &str,
        toolset_key: &str,
        approved_tools: &BTreeSet<String>,
    ) -> io::Result<u64> {
        let lock_path = self.home.join(".config.lock");
        let _lock = ConfigLock::acquire(&lock_path)?;
        let cfg_path = self.home.join("config.toml");
        let text = fs::read_to_string(&cfg_path)?;
        let mut doc: DocumentMut = text.parse().map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("shared CODEX_HOME config.toml is not valid TOML: {e}"),
            )
        })?;
        let Some(kernel) = doc
            .get_mut("mcp_servers")
            .and_then(|servers| servers.get_mut(key))
            .and_then(toml_edit::Item::as_table_mut)
            .filter(|kernel| kernel.contains_key("command"))
        else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("shared CODEX_HOME config.toml has no [mcp_servers.{key}] entry to update"),
            ));
        };
        let env = kernel.entry("env").or_insert(toml_edit::table());
        let Some(env_table) = env.as_table_mut() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("[mcp_servers.{key}].env is not a table"),
            ));
        };
        let generation = env_table
            .get(toolset_key)
            .and_then(toml_edit::Item::as_str)
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0)
            .wrapping_add(1);
        env_table[toolset_key] = toml_edit::value(generation.to_string());
        write_approved_tools(kernel, approved_tools);
        write_config_0600(&cfg_path, doc.to_string().as_bytes())?;
        Ok(generation)
    }

    /// toml_edit round-trip idempotent writer for `<home>/config.toml`. Writing the application entry
    /// drops its per-tool `tools` map; the next [`Self::refresh_mcp_entry`] writes it again.
    pub fn ensure_config(&self, mcp_block: Option<&McpServerConfig<'_>>) -> io::Result<()> {
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
        ensure_table_bool(
            &mut doc,
            "sandbox_workspace_write",
            "network_access",
            WORKSPACE_WRITE_NETWORK_ACCESS,
        );

        if let Some(shim) = mcp_block {
            let mcp_servers = doc["mcp_servers"].or_insert(toml_edit::table());
            if let Some(mcp_servers_table) = mcp_servers.as_table_mut() {
                mcp_servers_table.set_implicit(true);
                let kernel = mcp_servers_table
                    .entry(shim.key)
                    .or_insert(toml_edit::table());
                if let Some(kernel_table) = kernel.as_table_mut() {
                    kernel_table["command"] =
                        toml_edit::value(shim.command.to_string_lossy().to_string());
                    kernel_table["args"] = toml_edit::value(toml_edit::Array::new());
                    // Approval grants are [`Self::refresh_mcp_entry`]'s alone, and a boot never carries
                    // one across a restart: the plugin behind a granted name may have changed meanwhile.
                    kernel_table.remove("tools");
                    let env = kernel_table.entry("env").or_insert(toml_edit::table());
                    if let Some(env_table) = env.as_table_mut() {
                        for (key, value) in shim.env {
                            env_table[key] = toml_edit::value(value.as_str());
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
}

/// Codex's per-tool `approval_mode` under which it runs an MCP tool call without asking.
const MCP_TOOL_APPROVE: &str = "approve";

/// Make `entry.tools` exactly `approved`, each `{ approval_mode = "approve" }`, editing in place so
/// an unchanged set leaves the table's text as it was.
fn write_approved_tools(entry: &mut toml_edit::Table, approved: &BTreeSet<String>) {
    if approved.is_empty() {
        entry.remove("tools");
        return;
    }
    let item = entry.entry("tools").or_insert(toml_edit::table());
    if !item.is_table() {
        *item = toml_edit::table();
    }
    let Some(tools) = item.as_table_mut() else {
        return;
    };
    tools.set_implicit(true);
    tools.retain(|name, _| approved.contains(name));
    for name in approved {
        let exact = tools
            .get(name)
            .and_then(toml_edit::Item::as_table_like)
            .is_some_and(|tool| {
                tool.len() == 1
                    && tool.get("approval_mode").and_then(toml_edit::Item::as_str)
                        == Some(MCP_TOOL_APPROVE)
            });
        if !exact {
            let mut tool = toml_edit::Table::new();
            tool["approval_mode"] = toml_edit::value(MCP_TOOL_APPROVE);
            tools.insert(name, toml_edit::Item::Table(tool));
        }
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

/// Whether the workspace-write sandbox of neige's Codex threads reaches the network: seeded into
/// the shared home's config and repeated by every Planner turn's explicit sandbox policy.
pub const WORKSPACE_WRITE_NETWORK_ACCESS: bool = true;

fn ensure_table_bool(doc: &mut DocumentMut, table: &str, key: &str, value: bool) {
    let entry = doc[table].or_insert(toml_edit::table());
    if let Some(table) = entry.as_table_mut()
        && table.get(key).is_none()
    {
        table[key] = toml_edit::value(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #2512: Codex reads its home from a non-empty `CODEX_HOME`, else `~/.codex`; the seed
    /// imports from the same place.
    #[test]
    fn seed_imports_from_codex_home_when_it_is_set() {
        let root = tempfile::tempdir().unwrap();
        let codex_home = root.path().join("codex-home");
        let user_home = root.path().join("user");
        fs::create_dir_all(&codex_home).unwrap();
        fs::create_dir_all(user_home.join(".codex")).unwrap();
        fs::write(codex_home.join("auth.json"), "from-codex-home").unwrap();
        fs::write(user_home.join(".codex/auth.json"), "from-home").unwrap();
        let seeded = |name: &str, codex_home_value: &Path| {
            // SAFETY: nextest runs each test in its own process; nothing else reads the env here.
            unsafe {
                std::env::set_var("CODEX_HOME", codex_home_value);
                std::env::set_var("HOME", &user_home);
            }
            let home = SharedCodexHome::new(root.path().join(name), root.path().join("legacy"));
            home.seed().unwrap();
            fs::read_to_string(home.path().join("auth.json")).unwrap()
        };
        assert_eq!(seeded("set", &codex_home), "from-codex-home");
        assert_eq!(seeded("empty", Path::new("")), "from-home");
    }
    fn names(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    /// A home whose `svc` entry carries keys of its own besides the writer's.
    fn home_with_entry(root: &Path) -> SharedCodexHome {
        let home = SharedCodexHome::new(root.join("home"), root.join("legacy"));
        let env = vec![("SVC_TOKEN".to_owned(), "secret".to_owned())];
        home.ensure_config(Some(&McpServerConfig {
            key: "svc",
            command: Path::new("/svc/shim"),
            env: &env,
        }))
        .unwrap();
        let cfg = home.path().join("config.toml");
        let mut text = fs::read_to_string(&cfg).unwrap();
        text.push_str("\n[mcp_servers.svc.tools.stale]\napproval_mode = \"approve\"\n");
        text.push_str(
            "\n[mcp_servers.svc.tools.kept]\napproval_mode = \"prompt\"\nenabled = false\n",
        );
        text.push_str("\n[profiles.other]\nmodel = \"m\"\n");
        let mut doc: DocumentMut = text.parse().unwrap();
        doc["mcp_servers"]["svc"]["startup_timeout_sec"] = toml_edit::value(30);
        fs::write(&cfg, doc.to_string()).unwrap();
        home
    }

    fn doc(home: &SharedCodexHome) -> DocumentMut {
        fs::read_to_string(home.path().join("config.toml"))
            .unwrap()
            .parse()
            .unwrap()
    }

    /// The config text with the writer's two fields taken out.
    fn without_writer_fields(home: &SharedCodexHome) -> String {
        let mut doc = doc(home);
        let entry = doc["mcp_servers"]["svc"].as_table_mut().unwrap();
        entry.remove("tools");
        entry["env"].as_table_mut().unwrap().remove("GEN");
        doc.to_string()
    }

    fn approved(home: &SharedCodexHome) -> BTreeSet<String> {
        let doc = doc(home);
        let Some(tools) = doc["mcp_servers"]["svc"].get("tools") else {
            return BTreeSet::new();
        };
        let tools = tools.as_table().unwrap();
        for (name, tool) in tools.iter() {
            let tool = tool.as_table().unwrap();
            assert_eq!(tool.len(), 1, "{name}: only the approval mode");
            assert_eq!(tool["approval_mode"].as_str(), Some("approve"), "{name}");
        }
        tools.iter().map(|(name, _)| name.to_string()).collect()
    }

    #[test]
    fn the_tools_map_is_exactly_the_approved_set_and_nothing_else_changes() {
        let root = tempfile::tempdir().unwrap();
        let home = home_with_entry(root.path());
        let rest = without_writer_fields(&home);

        let set = names(&["kept", "plugin_a_open", "plugin_b_input"]);
        assert_eq!(home.refresh_mcp_entry("svc", "GEN", &set).unwrap(), 1);
        assert_eq!(approved(&home), set, "stale removed, kept reset, new added");
        assert_eq!(without_writer_fields(&home), rest);
        assert_eq!(
            doc(&home)["mcp_servers"]["svc"]["env"]["GEN"].as_str(),
            Some("1")
        );

        let fewer = names(&["plugin_b_input"]);
        assert_eq!(home.refresh_mcp_entry("svc", "GEN", &fewer).unwrap(), 2);
        assert_eq!(approved(&home), fewer);
        assert_eq!(without_writer_fields(&home), rest);

        assert_eq!(
            home.refresh_mcp_entry("svc", "GEN", &BTreeSet::new())
                .unwrap(),
            3
        );
        assert!(doc(&home)["mcp_servers"]["svc"].get("tools").is_none());
        assert_eq!(without_writer_fields(&home), rest);
        home.verify_expected_mcp_servers(&["svc"]).unwrap();
    }

    #[test]
    fn a_refresh_with_an_unchanged_set_changes_only_the_generation() {
        let root = tempfile::tempdir().unwrap();
        let home = home_with_entry(root.path());
        let set = names(&["plugin_a_open", "plugin_b_input"]);
        home.refresh_mcp_entry("svc", "GEN", &set).unwrap();
        let first = fs::read_to_string(home.path().join("config.toml")).unwrap();
        home.refresh_mcp_entry("svc", "GEN", &set).unwrap();
        let second = fs::read_to_string(home.path().join("config.toml")).unwrap();
        assert_eq!(
            second.replace("GEN = \"2\"", "GEN = \"1\""),
            first,
            "the same set rewrites nothing but the generation"
        );
        assert_eq!(approved(&home), set);
    }

    #[test]
    fn declared_mcp_identity_and_generation_keys_are_authoritative() {
        let root = tempfile::tempdir().unwrap();
        let home = SharedCodexHome::new(root.path().join("home"), root.path().join("legacy"));
        let env = vec![("CUSTOM_GENERATION".to_owned(), "7".to_owned())];
        home.ensure_config(Some(&McpServerConfig {
            key: "custom_service",
            command: Path::new("/custom/shim"),
            env: &env,
        }))
        .unwrap();
        home.verify_expected_mcp_servers(&["custom_service"])
            .unwrap();
        assert_eq!(
            home.refresh_mcp_entry("custom_service", "CUSTOM_GENERATION", &BTreeSet::new())
                .unwrap(),
            8
        );
        let doc: DocumentMut = fs::read_to_string(home.path().join("config.toml"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            doc["mcp_servers"]["custom_service"]["command"].as_str(),
            Some("/custom/shim")
        );
        assert_eq!(
            doc["mcp_servers"]["custom_service"]["env"]["CUSTOM_GENERATION"].as_str(),
            Some("8")
        );
        assert!(
            home.verify_expected_mcp_servers(&["other_service"])
                .is_err()
        );
    }
}
