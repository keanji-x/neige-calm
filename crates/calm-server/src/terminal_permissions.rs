//! `claude_permissions` on `calm.terminal.open`: validate the Planner's declared scope and
//! render it into Claude Code's `permissions` block. The floor is appended as `ask`, never
//! `deny`: Claude Code evaluates `deny`, then `ask`, then `allow`, so an `ask` rule prompts even
//! when an `allow` rule also matches. `Edit(...)` rules are anchored with `//` (absolute)
//! because a single leading slash anchors at the settings file's own directory.
use crate::error::{CalmError, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod policy;
pub use calm_types::claude_permissions::{
    ClaudePermissionsScope, ClaudePermissionsSource, parse_scope_named,
};
#[cfg(feature = "fixtures")]
pub use policy::{CeilingCheckedHook, install_ceiling_checked_hook_for_test};
pub use policy::{apply_policy, wait_at_ceiling_checked_hook};

/// Bash prefixes rendered as `ask` rules whenever a scope is declared: in their usual spellings
/// they prompt even when a `bash` prefix admits them.
pub const CLAUDE_PERMISSIONS_FLOOR_BASH: [&str; 7] = [
    "git push",
    "git reset --hard",
    "rm -rf",
    "curl",
    "wget",
    "pip install",
    "npm install",
];

/// Caps of the three lists (`edit`, `bash`, `deny`) and of one entry.
pub const CLAUDE_PERMISSIONS_EDIT_MAX: usize = 16;
pub const CLAUDE_PERMISSIONS_BASH_MAX: usize = 32;
pub const CLAUDE_PERMISSIONS_DENY_MAX: usize = 32;
pub const CLAUDE_PERMISSIONS_ENTRY_MAX_CHARS: usize = 200;

/// Command wrappers Claude Code strips before matching a `Bash(...)` rule; a
/// rule naming one of them can never match.
const STRIPPED_WRAPPERS: [&str; 9] = [
    "timeout", "time", "nice", "nohup", "stdbuf", "command", "builtin", "noglob", "xargs",
];

/// Exactly Claude Code's `permissions` block: the one value written to the
/// settings file, stamped on the card and echoed by the open.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveClaudePermissions {
    pub allow: Vec<String>,
    pub ask: Vec<String>,
    pub deny: Vec<String>,
}

/// Parse the tool argument into a scope. The serde derive on [`ClaudePermissionsScope`] is for
/// storage only: a derive alone would also accept a JSON array and read `deny: null` as absent.
pub fn parse_scope(value: &Value) -> std::result::Result<ClaudePermissionsScope, String> {
    parse_scope_named("claude_permissions", value)
}

/// Validate a declared scope; `Ok` is the trimmed scope, `Err` names the offending key or entry.
pub fn validate_scope(
    scope: &ClaudePermissionsScope,
) -> std::result::Result<ClaudePermissionsScope, String> {
    validate_scope_named("claude_permissions", scope)
}

/// [`validate_scope`] with the reasons written under `field`; the Track policy runs the same
/// rules, so a policy never admits what a declaration could not.
pub fn validate_scope_named(
    field: &str,
    scope: &ClaudePermissionsScope,
) -> std::result::Result<ClaudePermissionsScope, String> {
    let edit = list(
        field,
        "edit",
        scope.edit.as_deref(),
        CLAUDE_PERMISSIONS_EDIT_MAX,
        true,
    )?;
    let bash = list(
        field,
        "bash",
        scope.bash.as_deref(),
        CLAUDE_PERMISSIONS_BASH_MAX,
        true,
    )?;
    let deny = list(
        field,
        "deny",
        scope.deny.as_deref(),
        CLAUDE_PERMISSIONS_DENY_MAX,
        false,
    )?;
    if edit.is_none() && bash.is_none() && deny.is_none() {
        return Err(format!("{field} declares nothing; omit the argument"));
    }
    for (index, glob) in edit.iter().flatten().enumerate() {
        edit_entry(field, index, glob)?;
    }
    for (index, prefix) in bash.iter().flatten().enumerate() {
        command_entry(field, "bash", index, prefix)?;
        if CLAUDE_PERMISSIONS_FLOOR_BASH.contains(&prefix.as_str()) {
            return Err(format!(
                "{field}.bash[{index}] '{prefix}': floor command, always asks; \
                 put it in deny or omit it"
            ));
        }
    }
    for (index, prefix) in deny.iter().flatten().enumerate() {
        command_entry(field, "deny", index, prefix)?;
    }
    if let (Some(deny), Some(bash)) = (&deny, &bash) {
        for (index, prefix) in deny.iter().enumerate() {
            if let Some(other) = bash.iter().position(|allowed| allowed == prefix) {
                return Err(format!("{field}.deny[{index}]: also in bash[{other}]"));
            }
        }
    }
    Ok(ClaudePermissionsScope { edit, bash, deny })
}

/// Shared list checks: cap, per-entry emptiness / length / control
/// characters, duplicates. Entries come back trimmed.
fn list(
    field: &str,
    name: &str,
    entries: Option<&[String]>,
    max: usize,
    must_not_be_empty: bool,
) -> std::result::Result<Option<Vec<String>>, String> {
    let Some(entries) = entries else {
        return Ok(None);
    };
    if entries.is_empty() {
        if must_not_be_empty {
            return Err(format!("{field}.{name}: empty; omit the key"));
        }
        return Ok(None);
    }
    if entries.len() > max {
        return Err(format!(
            "{field}.{name}: {} entries, max {max}",
            entries.len()
        ));
    }
    let mut trimmed: Vec<String> = Vec::with_capacity(entries.len());
    for (index, raw) in entries.iter().enumerate() {
        let entry = raw.trim();
        if entry.is_empty() {
            return Err(format!("{field}.{name}[{index}]: empty"));
        }
        if entry.chars().count() > CLAUDE_PERMISSIONS_ENTRY_MAX_CHARS {
            return Err(format!(
                "{field}.{name}[{index}]: longer than {CLAUDE_PERMISSIONS_ENTRY_MAX_CHARS}"
            ));
        }
        if entry.chars().any(char::is_control) {
            return Err(format!("{field}.{name}[{index}]: control character"));
        }
        if let Some(first) = trimmed.iter().position(|seen| seen == entry) {
            return Err(format!(
                "{field}.{name}[{index}]: duplicate of {name}[{first}]"
            ));
        }
        trimmed.push(entry.to_owned());
    }
    Ok(Some(trimmed))
}

/// An `edit` glob: relative to the cwd, no `.`/`..`/empty segment, no rule
/// syntax, not under `.git` (which is always `ask`).
fn edit_entry(field: &str, index: usize, glob: &str) -> std::result::Result<(), String> {
    let at = format!("{field}.edit[{index}]");
    if glob.starts_with('/') || glob.starts_with('~') {
        return Err(format!("{at}: must be relative to the terminal cwd"));
    }
    if glob
        .split('/')
        .any(|segment| matches!(segment, "" | "." | ".."))
    {
        return Err(format!("{at}: '.', '..' or empty path segment"));
    }
    if glob.contains(['\\', '(', ')']) || glob.starts_with('!') {
        return Err(format!(
            "{at}: backslash, parentheses and '!' are not allowed"
        ));
    }
    if glob.split('/').next() == Some(".git") {
        return Err(format!("{at}: .git is always ask"));
    }
    Ok(())
}

/// A `bash` / `deny` prefix: one command word plus single-spaced arguments,
/// no rule syntax, no shell operator, no substitution or redirection, not a
/// wrapper Claude Code strips before matching.
fn command_entry(
    field: &str,
    name: &str,
    index: usize,
    prefix: &str,
) -> std::result::Result<(), String> {
    let at = format!("{field}.{name}[{index}]");
    if prefix.contains(['*', '(', ')']) {
        return Err(format!(
            "{at}: '*' and parentheses are not allowed; a trailing wildcard is implied"
        ));
    }
    if prefix.contains([';', '|', '&', '\n']) {
        return Err(format!("{at}: shell operator; one command per entry"));
    }
    if prefix.contains(['$', '`', '<', '>']) {
        return Err(format!("{at}: substitution or redirection"));
    }
    if prefix.starts_with('-') || prefix.contains("  ") {
        return Err(format!(
            "{at}: must start with a command word, single spaces"
        ));
    }
    let first = prefix.split(' ').next().unwrap_or_default();
    if STRIPPED_WRAPPERS.contains(&first) {
        return Err(format!(
            "{at}: '{first}' is stripped before matching; name the wrapped command"
        ));
    }
    Ok(())
}

/// Render a validated scope for a terminal whose cwd is `cwd`. Rules written by hand are not
/// escaped by Claude Code, so a cwd carrying glob or rule characters is refused.
pub fn render_claude_permissions(
    cwd: &str,
    scope: &ClaudePermissionsScope,
) -> Result<EffectiveClaudePermissions> {
    let root = rule_root(cwd)?;
    let mut allow: Vec<String> = Vec::new();
    for glob in scope.edit.iter().flatten() {
        allow.push(format!("Edit(//{root}/{glob})"));
    }
    for prefix in scope.bash.iter().flatten() {
        allow.extend(bash_rules(prefix, &root));
    }
    let ask = CLAUDE_PERMISSIONS_FLOOR_BASH
        .iter()
        .flat_map(|prefix| bash_rules(prefix, &root))
        .chain(std::iter::once(format!("Edit(//{root}/.git/**)")))
        .collect();
    let deny = scope
        .deny
        .iter()
        .flatten()
        .flat_map(|prefix| bash_rules(prefix, &root))
        .collect();
    Ok(EffectiveClaudePermissions { allow, ask, deny })
}

/// `Bash(<prefix> *)`, and for a `git <rest>` prefix also `Bash(git -C /<root> <rest> *)` —
/// the spelling Claude Code runs from the terminal's cwd. The variant is emitted only when
/// `root` carries no whitespace or quote: with a space in the path the tokens shift and the
/// `allow` variant of `git status` would admit a push the `deny` variant does not match.
fn bash_rules(prefix: &str, root: &str) -> Vec<String> {
    let mut rules = vec![format!("Bash({prefix} *)")];
    let root_is_one_token = !root
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '\'' | '"' | '\\'));
    if let Some(rest) = prefix.strip_prefix("git ")
        && !rest.is_empty()
        && root_is_one_token
    {
        rules.push(format!("Bash(git -C /{root} {rest} *)"));
    }
    rules
}

/// `/workspaces/ledger/` → `workspaces/ledger`, the text after `//` in an
/// absolute `Edit(...)` rule.
fn rule_root(cwd: &str) -> Result<String> {
    if !cwd.starts_with('/') {
        return Err(CalmError::BadRequest(format!(
            "claude_permissions: cwd {cwd:?} is not absolute"
        )));
    }
    let root = cwd.trim_matches('/');
    if root.is_empty() {
        return Err(CalmError::BadRequest(format!(
            "claude_permissions: cwd {cwd:?} is the filesystem root"
        )));
    }
    if root
        .chars()
        .any(|c| matches!(c, '*' | '?' | '[' | ']' | '\\') || c.is_control())
    {
        return Err(CalmError::BadRequest(format!(
            "claude_permissions: cwd {cwd:?} contains glob or rule characters"
        )));
    }
    Ok(root.to_owned())
}

#[cfg(test)]
mod tests;
