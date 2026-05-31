//! Shared Codex home support for #410 PR1.
//!
//! PR1 seeds and maintains the shared home only. Existing card spawn paths keep
//! using the legacy per-card homes until later PRs switch callers.

use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

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

        copy_dir_recursive_excluding_top_auth(host_codex_dir, &self.home)?;

        let src_auth = host_codex_dir.join("auth.json");
        let dst_auth = self.home.join("auth.json");
        if src_auth.exists() && !dst_auth.exists() {
            fs::copy(src_auth, dst_auth)?;
        }

        Ok(())
    }

    /// TOML-aware idempotent writer for `<home>/config.toml`.
    pub fn ensure_config_for_cwd(&self, cwd: &Path) -> io::Result<()> {
        fs::create_dir_all(&self.home)?;

        let lock_path = self.home.join(".config.lock");
        let _lock = ConfigLock::acquire(&lock_path)?;

        let cfg_path = self.home.join("config.toml");
        let mut text = match fs::read_to_string(&cfg_path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        let before = text.clone();

        ensure_top_level_key(&mut text, "approval_policy", r#"approval_policy = "never""#);
        ensure_top_level_key(
            &mut text,
            "sandbox_mode",
            r#"sandbox_mode = "workspace-write""#,
        );
        ensure_table_key(
            &mut text,
            "sandbox_workspace_write",
            "network_access",
            "network_access = true",
        );

        let escaped_cwd = escape_toml_basic_string(&cwd.to_string_lossy());
        ensure_table_key(
            &mut text,
            &format!(r#"projects."{escaped_cwd}""#),
            "trust_level",
            r#"trust_level = "trusted""#,
        );

        if text != before {
            fs::write(cfg_path, text)?;
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

fn escape_toml_basic_string(value: &str) -> String {
    // Note: a cwd containing `]` would produce a TOML header that table-header
    // detection mis-parses; we accept this since (a) such cwds are exotic and
    // (b) it matches the existing `build_codex_config_toml` escape rule.
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn ensure_top_level_key(text: &mut String, key: &str, line: &str) {
    if has_key_in_section(text, None, key) {
        return;
    }
    insert_top_level_line(text, line);
}

fn ensure_table_key(text: &mut String, table: &str, key: &str, line: &str) {
    if has_key_in_section(text, Some(table), key) {
        return;
    }
    if has_bracket_header(text, table) {
        insert_line_at_table_end(text, table, line);
    } else if has_table(text, table) {
        insert_top_level_line(text, &format!("{table}.{line}"));
    } else {
        append_table(text, table, line);
    }
}

fn has_key_in_section(text: &str, target: Option<&str>, key: &str) -> bool {
    let mut section: Option<String> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = table_header_name(line) {
            section = Some(name.to_string());
            continue;
        }
        if line.starts_with('[') {
            section = Some(String::new());
            continue;
        }
        if section.as_deref() == target
            && let Some((lhs, _)) = line.split_once('=')
            && lhs.trim() == key
        {
            return true;
        }
        if let Some(table) = target
            && section.is_none()
            && let Some((lhs, _)) = line.split_once('=')
            && dotted_key_matches(lhs.trim(), table, key)
        {
            return true;
        }
    }
    false
}

/// Returns true if `text` already declares the table `table` either as a
/// bracket header (`[table]`) or via a dotted key (`table.foo = ...`).
/// Both forms are valid TOML and emit the same table; treating them
/// equivalently is the load-bearing invariant the shared writer relies
/// on.
fn has_table(text: &str, table: &str) -> bool {
    has_bracket_header(text, table) || has_dotted_key_under(text, table)
}

fn has_bracket_header(text: &str, table: &str) -> bool {
    text.lines()
        .filter_map(|line| table_header_name(line.trim()))
        .any(|name| name == table)
}

fn has_dotted_key_under(text: &str, table: &str) -> bool {
    if !is_bare_toml_key(table) {
        return false;
    }

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            return false;
        }
        let Some((lhs, _)) = line.split_once('=') else {
            continue;
        };
        if root_dotted_key(lhs.trim()) == Some(table) {
            return true;
        }
    }
    false
}

fn root_dotted_key(lhs: &str) -> Option<&str> {
    let (root, rest) = lhs.split_once('.')?;
    if root.is_empty() || rest.trim().is_empty() {
        return None;
    }
    let root = root.trim();
    if is_bare_toml_key(root) {
        Some(root)
    } else {
        None
    }
}

fn dotted_key_matches(lhs: &str, table: &str, key: &str) -> bool {
    let Some((root, rest)) = lhs.split_once('.') else {
        return false;
    };
    let root = root.trim();
    let key = key.trim();
    is_bare_toml_key(root) && root == table && rest.trim() == key
}

fn is_bare_toml_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn table_header_name(line: &str) -> Option<&str> {
    if line.starts_with("[[") {
        return None;
    }
    line.strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .map(str::trim)
}

fn insert_top_level_line(text: &mut String, line: &str) {
    let insert_at = first_table_byte_offset(text).unwrap_or(text.len());
    let needs_prefix_newline = insert_at > 0 && !text[..insert_at].ends_with('\n');
    let mut addition = String::new();
    if needs_prefix_newline {
        addition.push('\n');
    }
    addition.push_str(line);
    addition.push('\n');
    if insert_at < text.len() {
        addition.push('\n');
    }
    text.insert_str(insert_at, &addition);
}

fn append_table(text: &mut String, table: &str, line: &str) {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if !text.is_empty() {
        text.push('\n');
    }
    text.push('[');
    text.push_str(table);
    text.push_str("]\n");
    text.push_str(line);
    text.push('\n');
}

fn insert_line_at_table_end(text: &mut String, table: &str, line: &str) {
    let mut in_target = false;
    let mut offset = 0usize;
    let mut insert_at = text.len();

    for raw in text.split_inclusive('\n') {
        let trimmed = raw.trim();
        if let Some(name) = table_header_name(trimmed) {
            if in_target {
                insert_at = offset;
                break;
            }
            in_target = name == table;
        }
        offset += raw.len();
    }

    let mut addition = String::new();
    if insert_at > 0 && !text[..insert_at].ends_with('\n') {
        addition.push('\n');
    }
    addition.push_str(line);
    addition.push('\n');
    if insert_at < text.len() {
        addition.push('\n');
    }
    text.insert_str(insert_at, &addition);
}

fn first_table_byte_offset(text: &str) -> Option<usize> {
    let mut offset = 0usize;
    for raw in text.split_inclusive('\n') {
        if table_header_name(raw.trim()).is_some() {
            return Some(offset);
        }
        offset += raw.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::has_table;

    #[test]
    fn has_table_detects_top_level_dotted_key() {
        assert!(has_table("foo.bar = 1\n", "foo"));
    }

    #[test]
    fn has_table_detects_bracket_header() {
        assert!(has_table("[foo]\nbar = 1\n", "foo"));
    }

    #[test]
    fn has_table_ignores_nested_dotted_key_under_other_table() {
        assert!(!has_table("[other]\nfoo.bar = 1\n", "foo"));
    }

    #[test]
    fn has_table_does_not_match_prefix_substring() {
        assert!(!has_table("foobar.x = 1\n", "foo"));
    }

    #[test]
    fn has_table_ignores_dotted_key_inside_comment() {
        assert!(!has_table("# foo.bar = 1\n", "foo"));
    }
}
