//! #1704 S1 — `claude_permissions` on `calm.terminal.open`.
//!
//! The Planner declares the permission rules for Claude Code in a terminal:
//! `edit` globs relative to the terminal cwd, `bash` command prefixes and
//! `deny` prefixes. The kernel validates the declaration ([`validate_scope`]),
//! renders it into Claude Code's `permissions` block
//! ([`render_claude_permissions`]) and writes that ONE value
//! ([`EffectiveClaudePermissions`]) to the generated settings file, the card
//! payload and the open result.
//!
//! Whenever a scope is declared the floor is appended as `ask`, never `deny`:
//! Claude Code evaluates `deny`, then `ask`, then `allow` over the merged rule
//! set, so an `ask` rule prompts even when an `allow` rule also matches, and a
//! dialog reaches the Planner as a `permission_request` signal. The floor
//! therefore never widens a scope; a Planner `deny` on the same rule still
//! wins. Every rule matches its usual spelling only (`git -C . push` is not
//! `Bash(git push *)`), and an action no rule matches keeps Claude Code's
//! usual permission behaviour. One exception (#1729): a `git <rest>` prefix
//! in any of the three lists is rendered twice, as `Bash(git <rest> *)` and
//! as `Bash(git -C /<cwd> <rest> *)` for the terminal's own absolute cwd,
//! the spelling Claude Code actually runs — only when that cwd carries no
//! whitespace and no quote character (a space in the path shifts the token
//! boundaries and would let an `allow` variant admit a denied subcommand;
//! such a cwd keeps the bare rule alone and prompts for `-C` spellings); no
//! other prefix, no other directory and not a bare `git`. `Edit(...)` rules are
//! anchored with `//` (an absolute path) because a single leading slash
//! anchors at the settings file's own directory. No `defaultMode`,
//! `bypassPermissions`, `additionalDirectories` or `Read(...)` rule is ever
//! written; a terminal opened without a scope gets the hooks-only file.
//!
//! #1704 S2 — the Track tree's policy (`tracks.claude_permissions_policy`)
//! is the ceiling of every Planner-opened Claude in that tree: [`policy`]
//! holds the containment rules and the merge that produces the ONE scope
//! rendered here.
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

/// Bash prefixes rendered as `ask` rules whenever a scope is declared
/// (together with `Edit(//<cwd>/.git/**)`): in their usual spellings they
/// prompt even when a `bash` prefix admits them; other spellings are not
/// matched.
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

/// Parse the tool argument into a scope, enforcing exactly the advertised
/// shape with a reason under `claude_permissions`: an object (not an array,
/// string or null) whose keys are among `edit`, `bash`, `deny`, each an array
/// of strings (`null` is a wrong type, not an absent key). The serde derive on
/// [`ClaudePermissionsScope`] is for storage and the hash view only: a derive
/// alone would also accept a JSON array through `visit_seq` and read
/// `deny: null` as absent. `parse_scope(v) == parse_scope_named(
/// "claude_permissions", v)`; the Track PATCH runs the same parser under
/// `claude_permissions_policy`.
pub fn parse_scope(value: &Value) -> std::result::Result<ClaudePermissionsScope, String> {
    parse_scope_named("claude_permissions", value)
}

/// Validate a declared scope; `Ok` is the trimmed scope (whitespace-trimmed
/// entries, an empty `deny` dropped), `Err` names the offending key or entry
/// (`claude_permissions.bash[2]: ...`) for `invalid_params`.
/// `validate_scope(s) == validate_scope_named("claude_permissions", s)`.
pub fn validate_scope(
    scope: &ClaudePermissionsScope,
) -> std::result::Result<ClaudePermissionsScope, String> {
    validate_scope_named("claude_permissions", scope)
}

/// [`validate_scope`] with the reasons written under `field`: the same
/// rules for the Track policy (`claude_permissions_policy`, #1704 S2) — a
/// policy therefore never admits what a declaration could not.
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

/// Render a validated scope for a terminal whose working directory is `cwd`
/// (absolute; a trailing `/` is ignored). Rules written by hand are not
/// escaped by Claude Code, so a cwd carrying glob or rule characters is
/// refused rather than rendered into a rule that matches something else.
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

/// The `Bash(...)` rules of one validated prefix: `Bash(<prefix> *)`, and
/// for a `git <rest>` prefix (the literal ASCII `git ` followed by a
/// non-empty rest; a `git\u{a0}status` prefix is one shell token and gets
/// nothing) also `Bash(git -C /<root> <rest> *)` right after it (#1729) —
/// the spelling Claude Code runs from a terminal whose cwd is `/<root>`. The
/// variant is emitted only when `root` carries no whitespace and none of
/// `'`, `"`, `\`: with a space in the path (`/w push`) the tokens shift and
/// the `allow` variant of `git status` would read as `git -C /w push status`,
/// admitting a push the `deny` variant does not match; such a root keeps
/// the bare rule alone (no error, no quoting) and Claude prompts for `-C`
/// spellings there. A bare `git` gets no variant (`Bash(git *)` admits every
/// spelling); a `-C` to any other directory matches no rule. Used for
/// `allow`, the floor's `ask` and `deny` alike, so within one cwd the
/// variant is in exactly the lists its bare rule is in.
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
