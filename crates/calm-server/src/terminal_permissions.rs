//! #1704 S1 — `claude_permissions` on `calm.terminal.open`.
//!
//! The Planner declares what Claude may do in a terminal without a dialog:
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
//! prompt still reaches the Planner as a `permission_request` signal. The
//! floor therefore never widens a scope and never makes anything impossible;
//! a Planner `deny` on the same rule still wins. `Edit(...)` rules are
//! anchored with `//` (an absolute path) because a single leading slash
//! anchors at the settings file's own directory. No `defaultMode`,
//! `bypassPermissions`, `additionalDirectories` or `Read(...)` rule is ever
//! written; a terminal opened without a scope gets the hooks-only file.
use crate::error::{CalmError, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Bash prefixes that always prompt when a scope is declared, rendered as
/// `ask` rules (together with `Edit(//<cwd>/.git/**)`).
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

/// The `claude_permissions` argument of `calm.terminal.open`, as declared.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ClaudePermissionsScope {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edit: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bash: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deny: Option<Vec<String>>,
}

/// Exactly Claude Code's `permissions` block: the one value written to the
/// settings file, stamped on the card and echoed by the open.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveClaudePermissions {
    pub allow: Vec<String>,
    pub ask: Vec<String>,
    pub deny: Vec<String>,
}

/// The keys a declared scope may carry, in the schema's order.
const SCOPE_KEYS: [&str; 3] = ["edit", "bash", "deny"];

/// Parse the tool argument into a scope, enforcing exactly the advertised
/// shape with a named reason: an object (not an array, string or null) whose
/// keys are among `edit`, `bash`, `deny`, each an array of strings (`null` is
/// a wrong type, not an absent key). The serde derive on
/// [`ClaudePermissionsScope`] is for storage and the hash view only: a derive
/// alone would also accept a JSON array through `visit_seq` and read
/// `deny: null` as absent.
pub fn parse_scope(value: &Value) -> std::result::Result<ClaudePermissionsScope, String> {
    let Some(object) = value.as_object() else {
        return Err("claude_permissions: must be an object".into());
    };
    if let Some(unknown) = object
        .keys()
        .find(|key| !SCOPE_KEYS.contains(&key.as_str()))
    {
        return Err(format!("claude_permissions: unknown key '{unknown}'"));
    }
    let list = |key: &str| -> std::result::Result<Option<Vec<String>>, String> {
        let Some(value) = object.get(key) else {
            return Ok(None);
        };
        value
            .as_array()
            .and_then(|entries| {
                entries
                    .iter()
                    .map(|entry| entry.as_str().map(str::to_owned))
                    .collect::<Option<Vec<String>>>()
            })
            .map(Some)
            .ok_or_else(|| format!("claude_permissions.{key}: must be an array of strings"))
    };
    Ok(ClaudePermissionsScope {
        edit: list("edit")?,
        bash: list("bash")?,
        deny: list("deny")?,
    })
}

/// Validate a declared scope; `Ok` is the trimmed scope (whitespace-trimmed
/// entries, an empty `deny` dropped), `Err` names the offending key or entry
/// (`claude_permissions.bash[2]: ...`) for `invalid_params`.
pub fn validate_scope(
    scope: &ClaudePermissionsScope,
) -> std::result::Result<ClaudePermissionsScope, String> {
    let edit = list(
        "edit",
        scope.edit.as_deref(),
        CLAUDE_PERMISSIONS_EDIT_MAX,
        true,
    )?;
    let bash = list(
        "bash",
        scope.bash.as_deref(),
        CLAUDE_PERMISSIONS_BASH_MAX,
        true,
    )?;
    let deny = list(
        "deny",
        scope.deny.as_deref(),
        CLAUDE_PERMISSIONS_DENY_MAX,
        false,
    )?;
    if edit.is_none() && bash.is_none() && deny.is_none() {
        return Err("claude_permissions declares nothing; omit the argument".into());
    }
    for (index, glob) in edit.iter().flatten().enumerate() {
        edit_entry(index, glob)?;
    }
    for (index, prefix) in bash.iter().flatten().enumerate() {
        command_entry("bash", index, prefix)?;
        if CLAUDE_PERMISSIONS_FLOOR_BASH.contains(&prefix.as_str()) {
            return Err(format!(
                "claude_permissions.bash[{index}] '{prefix}': floor command, always asks; \
                 put it in deny or omit it"
            ));
        }
    }
    for (index, prefix) in deny.iter().flatten().enumerate() {
        command_entry("deny", index, prefix)?;
    }
    if let (Some(deny), Some(bash)) = (&deny, &bash) {
        for (index, prefix) in deny.iter().enumerate() {
            if let Some(other) = bash.iter().position(|allowed| allowed == prefix) {
                return Err(format!(
                    "claude_permissions.deny[{index}]: also in bash[{other}]"
                ));
            }
        }
    }
    Ok(ClaudePermissionsScope { edit, bash, deny })
}

/// Shared list checks: cap, per-entry emptiness / length / control
/// characters, duplicates. Entries come back trimmed.
fn list(
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
            return Err(format!("claude_permissions.{name}: empty; omit the key"));
        }
        return Ok(None);
    }
    if entries.len() > max {
        return Err(format!(
            "claude_permissions.{name}: {} entries, max {max}",
            entries.len()
        ));
    }
    let mut trimmed: Vec<String> = Vec::with_capacity(entries.len());
    for (index, raw) in entries.iter().enumerate() {
        let entry = raw.trim();
        if entry.is_empty() {
            return Err(format!("claude_permissions.{name}[{index}]: empty"));
        }
        if entry.chars().count() > CLAUDE_PERMISSIONS_ENTRY_MAX_CHARS {
            return Err(format!(
                "claude_permissions.{name}[{index}]: longer than {CLAUDE_PERMISSIONS_ENTRY_MAX_CHARS}"
            ));
        }
        if entry.chars().any(char::is_control) {
            return Err(format!(
                "claude_permissions.{name}[{index}]: control character"
            ));
        }
        if let Some(first) = trimmed.iter().position(|seen| seen == entry) {
            return Err(format!(
                "claude_permissions.{name}[{index}]: duplicate of {name}[{first}]"
            ));
        }
        trimmed.push(entry.to_owned());
    }
    Ok(Some(trimmed))
}

/// An `edit` glob: relative to the cwd, no `.`/`..`/empty segment, no rule
/// syntax, not under `.git` (which is always `ask`).
fn edit_entry(index: usize, glob: &str) -> std::result::Result<(), String> {
    let at = format!("claude_permissions.edit[{index}]");
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
fn command_entry(name: &str, index: usize, prefix: &str) -> std::result::Result<(), String> {
    let at = format!("claude_permissions.{name}[{index}]");
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
        allow.push(format!("Bash({prefix} *)"));
    }
    let ask = CLAUDE_PERMISSIONS_FLOOR_BASH
        .iter()
        .map(|prefix| format!("Bash({prefix} *)"))
        .chain(std::iter::once(format!("Edit(//{root}/.git/**)")))
        .collect();
    let deny = scope
        .deny
        .iter()
        .flatten()
        .map(|prefix| format!("Bash({prefix} *)"))
        .collect();
    Ok(EffectiveClaudePermissions { allow, ask, deny })
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
mod tests {
    use super::*;
    use serde_json::json;

    fn strings(values: &[&str]) -> Option<Vec<String>> {
        Some(values.iter().map(|value| (*value).to_owned()).collect())
    }

    /// The scope of the round-19 ledger task.
    fn round19() -> ClaudePermissionsScope {
        ClaudePermissionsScope {
            edit: strings(&["**"]),
            bash: strings(&[
                "python3 -m unittest",
                "git status",
                "git diff",
                "git log",
                "git show",
                "git add",
                "git commit",
            ]),
            deny: strings(&["git push"]),
        }
    }

    #[test]
    fn round19_scope_renders_the_documented_block() {
        let block = render_claude_permissions("/workspaces/ledger", &round19()).unwrap();
        assert_eq!(
            block,
            EffectiveClaudePermissions {
                allow: vec![
                    "Edit(//workspaces/ledger/**)".into(),
                    "Bash(python3 -m unittest *)".into(),
                    "Bash(git status *)".into(),
                    "Bash(git diff *)".into(),
                    "Bash(git log *)".into(),
                    "Bash(git show *)".into(),
                    "Bash(git add *)".into(),
                    "Bash(git commit *)".into(),
                ],
                ask: vec![
                    "Bash(git push *)".into(),
                    "Bash(git reset --hard *)".into(),
                    "Bash(rm -rf *)".into(),
                    "Bash(curl *)".into(),
                    "Bash(wget *)".into(),
                    "Bash(pip install *)".into(),
                    "Bash(npm install *)".into(),
                    "Edit(//workspaces/ledger/.git/**)".into(),
                ],
                deny: vec!["Bash(git push *)".into()],
            }
        );
        // The wire shape is exactly Claude Code's `permissions` block.
        assert_eq!(
            serde_json::to_value(&block).unwrap(),
            json!({
                "allow": block.allow,
                "ask": block.ask,
                "deny": block.deny,
            })
        );
        // A trailing slash on the cwd changes nothing.
        assert_eq!(
            render_claude_permissions("/workspaces/ledger/", &round19()).unwrap(),
            block
        );
    }

    #[test]
    fn floor_is_ask_and_a_planner_deny_on_the_same_rule_keeps_both() {
        let scope = ClaudePermissionsScope {
            deny: strings(&["git push", "rm -rf"]),
            ..Default::default()
        };
        let block = render_claude_permissions("/w", &scope).unwrap();
        assert_eq!(
            block.deny,
            vec!["Bash(git push *)".to_owned(), "Bash(rm -rf *)".into()]
        );
        for rule in ["Bash(git push *)", "Bash(rm -rf *)"] {
            assert!(block.ask.iter().any(|r| r == rule), "{rule} stays asked");
        }
        assert_eq!(block.ask.len(), CLAUDE_PERMISSIONS_FLOOR_BASH.len() + 1);
        assert_eq!(block.allow, Vec::<String>::new());
        // Nothing from the floor ever lands in deny, whatever the scope.
        let block = render_claude_permissions("/w", &round19()).unwrap();
        for prefix in CLAUDE_PERMISSIONS_FLOOR_BASH {
            let rule = format!("Bash({prefix} *)");
            assert!(block.ask.contains(&rule), "{rule}");
            assert_eq!(block.deny.contains(&rule), prefix == "git push", "{rule}");
        }
        assert!(block.ask.contains(&"Edit(//w/.git/**)".to_owned()));
        assert!(block.deny.iter().all(|rule| rule.starts_with("Bash(")));
    }

    #[test]
    fn cwd_with_glob_or_rule_characters_is_refused() {
        for cwd in [
            "/tmp/a[1]",
            "/x*",
            "/a?b",
            "/a]b",
            "/a\\b",
            "/a\u{7}b",
            "x",
            "",
            "/",
            "//",
        ] {
            let err = render_claude_permissions(cwd, &round19()).unwrap_err();
            assert!(
                matches!(err, CalmError::BadRequest(ref m) if m.starts_with("claude_permissions: cwd")),
                "{cwd:?}: {err:?}"
            );
        }
        let block = render_claude_permissions("/a b/(c)", &round19()).unwrap();
        assert_eq!(block.allow[0], "Edit(//a b/(c)/**)");
        assert_eq!(block.ask[7], "Edit(//a b/(c)/.git/**)");
    }

    #[test]
    fn validate_scope_reasons() {
        let long = "x".repeat(CLAUDE_PERMISSIONS_ENTRY_MAX_CHARS + 1);
        let many_edit: Vec<String> = (0..=CLAUDE_PERMISSIONS_EDIT_MAX)
            .map(|i| format!("d{i}/**"))
            .collect();
        let many_bash: Vec<String> = (0..=CLAUDE_PERMISSIONS_BASH_MAX)
            .map(|i| format!("cmd{i}"))
            .collect();
        let many_deny: Vec<String> = (0..=CLAUDE_PERMISSIONS_DENY_MAX)
            .map(|i| format!("cmd{i}"))
            .collect();
        let cases: Vec<(serde_json::Value, &str)> = vec![
            (
                json!({}),
                "claude_permissions declares nothing; omit the argument",
            ),
            (
                json!({"deny": []}),
                "claude_permissions declares nothing; omit the argument",
            ),
            (
                json!({"edit": []}),
                "claude_permissions.edit: empty; omit the key",
            ),
            (
                json!({"bash": []}),
                "claude_permissions.bash: empty; omit the key",
            ),
            (
                json!({"edit": many_edit}),
                "claude_permissions.edit: 17 entries, max 16",
            ),
            (
                json!({"bash": many_bash}),
                "claude_permissions.bash: 33 entries, max 32",
            ),
            (
                json!({"deny": many_deny}),
                "claude_permissions.deny: 33 entries, max 32",
            ),
            (
                json!({"bash": ["git status", "git diff", "  "]}),
                "claude_permissions.bash[2]: empty",
            ),
            (
                json!({"edit": [long]}),
                "claude_permissions.edit[0]: longer than 200",
            ),
            (
                json!({"deny": ["git push", "git\tpull"]}),
                "claude_permissions.deny[1]: control character",
            ),
            (
                json!({"bash": ["git\nstatus"]}),
                "claude_permissions.bash[0]: control character",
            ),
            (
                json!({"edit": ["/etc/**"]}),
                "claude_permissions.edit[0]: must be relative to the terminal cwd",
            ),
            (
                json!({"edit": ["//etc/**"]}),
                "claude_permissions.edit[0]: must be relative to the terminal cwd",
            ),
            (
                json!({"edit": ["~/x"]}),
                "claude_permissions.edit[0]: must be relative to the terminal cwd",
            ),
            (
                json!({"edit": ["src/../x"]}),
                "claude_permissions.edit[0]: '.', '..' or empty path segment",
            ),
            (
                json!({"edit": ["src/./x"]}),
                "claude_permissions.edit[0]: '.', '..' or empty path segment",
            ),
            (
                json!({"edit": ["src//x"]}),
                "claude_permissions.edit[0]: '.', '..' or empty path segment",
            ),
            (
                json!({"edit": ["src/"]}),
                "claude_permissions.edit[0]: '.', '..' or empty path segment",
            ),
            (
                json!({"edit": [".."]}),
                "claude_permissions.edit[0]: '.', '..' or empty path segment",
            ),
            (
                json!({"edit": ["src\\x"]}),
                "claude_permissions.edit[0]: backslash, parentheses and '!' are not allowed",
            ),
            (
                json!({"edit": ["a(b)"]}),
                "claude_permissions.edit[0]: backslash, parentheses and '!' are not allowed",
            ),
            (
                json!({"edit": ["!src/**"]}),
                "claude_permissions.edit[0]: backslash, parentheses and '!' are not allowed",
            ),
            (
                json!({"edit": ["**", ".git/config"]}),
                "claude_permissions.edit[1]: .git is always ask",
            ),
            (
                json!({"edit": [".git"]}),
                "claude_permissions.edit[0]: .git is always ask",
            ),
            (
                json!({"bash": ["git *"]}),
                "claude_permissions.bash[0]: '*' and parentheses are not allowed; a trailing wildcard is implied",
            ),
            (
                json!({"deny": ["Bash(git push)"]}),
                "claude_permissions.deny[0]: '*' and parentheses are not allowed; a trailing wildcard is implied",
            ),
            (
                json!({"bash": ["git status && git diff"]}),
                "claude_permissions.bash[0]: shell operator; one command per entry",
            ),
            (
                json!({"bash": ["git status; ls"]}),
                "claude_permissions.bash[0]: shell operator; one command per entry",
            ),
            (
                json!({"deny": ["cat x | sh"]}),
                "claude_permissions.deny[0]: shell operator; one command per entry",
            ),
            (
                json!({"bash": ["echo $HOME"]}),
                "claude_permissions.bash[0]: substitution or redirection",
            ),
            (
                json!({"bash": ["echo `id`"]}),
                "claude_permissions.bash[0]: substitution or redirection",
            ),
            (
                json!({"deny": ["cat < x"]}),
                "claude_permissions.deny[0]: substitution or redirection",
            ),
            (
                json!({"bash": ["echo > x"]}),
                "claude_permissions.bash[0]: substitution or redirection",
            ),
            (
                json!({"bash": ["-v"]}),
                "claude_permissions.bash[0]: must start with a command word, single spaces",
            ),
            (
                json!({"bash": ["git  status"]}),
                "claude_permissions.bash[0]: must start with a command word, single spaces",
            ),
            (
                json!({"bash": ["timeout 5 python3"]}),
                "claude_permissions.bash[0]: 'timeout' is stripped before matching; name the wrapped command",
            ),
            (
                json!({"deny": ["xargs rm"]}),
                "claude_permissions.deny[0]: 'xargs' is stripped before matching; name the wrapped command",
            ),
            (
                json!({"bash": ["git status", "git push"]}),
                "claude_permissions.bash[1] 'git push': floor command, always asks; put it in deny or omit it",
            ),
            (
                json!({"bash": [" rm -rf "]}),
                "claude_permissions.bash[0] 'rm -rf': floor command, always asks; put it in deny or omit it",
            ),
            (
                json!({"bash": ["git status", "git diff", "git log", "git diff"]}),
                "claude_permissions.bash[3]: duplicate of bash[1]",
            ),
            (
                json!({"edit": ["**", " ** "]}),
                "claude_permissions.edit[1]: duplicate of edit[0]",
            ),
            (
                json!({"deny": ["git push", "git push"]}),
                "claude_permissions.deny[1]: duplicate of deny[0]",
            ),
            (
                json!({"bash": ["git status", "git commit"], "deny": ["git commit"]}),
                "claude_permissions.deny[0]: also in bash[1]",
            ),
        ];
        for (input, reason) in cases {
            let scope = parse_scope(&input).unwrap();
            let err = validate_scope(&scope).unwrap_err();
            assert_eq!(err, reason, "{input}");
        }
        // Shape rows: `parse_scope` refuses what the schema does not
        // advertise, before any entry is looked at.
        let shape_cases: Vec<(serde_json::Value, &str)> = vec![
            (
                json!([["**"], null, []]),
                "claude_permissions: must be an object",
            ),
            (json!("**"), "claude_permissions: must be an object"),
            (json!(null), "claude_permissions: must be an object"),
            (json!(7), "claude_permissions: must be an object"),
            (
                json!({"edit": ["**"], "deny": null}),
                "claude_permissions.deny: must be an array of strings",
            ),
            (
                json!({"bash": "git status"}),
                "claude_permissions.bash: must be an array of strings",
            ),
            (
                json!({"edit": [1]}),
                "claude_permissions.edit: must be an array of strings",
            ),
            (
                json!({"edit": [["**"]]}),
                "claude_permissions.edit: must be an array of strings",
            ),
            (
                json!({"edit": ["**"], "allow": ["x"]}),
                "claude_permissions: unknown key 'allow'",
            ),
            (json!({"ask": []}), "claude_permissions: unknown key 'ask'"),
        ];
        for (input, reason) in shape_cases {
            assert_eq!(parse_scope(&input).unwrap_err(), reason, "{input}");
        }
        // The storage derive is a separate contract: it also refuses unknown
        // keys, and round-trips what `parse_scope` accepted.
        let err = serde_json::from_value::<ClaudePermissionsScope>(json!({"allow": ["x"]}))
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("unknown field `allow`, expected one of `edit`, `bash`, `deny`"),
            "{err}"
        );
        let parsed = parse_scope(&json!({"edit": ["**"], "deny": []})).unwrap();
        assert_eq!(
            parsed,
            ClaudePermissionsScope {
                edit: strings(&["**"]),
                bash: None,
                deny: strings(&[]),
            }
        );
        assert_eq!(
            serde_json::from_value::<ClaudePermissionsScope>(
                serde_json::to_value(&parsed).unwrap()
            )
            .unwrap(),
            parsed
        );

        // The round-19 scope is accepted; entries come back trimmed, an
        // empty deny is dropped, token-prefixes of floor commands pass.
        let mut untrimmed = round19();
        untrimmed.edit = strings(&[" ** "]);
        assert_eq!(validate_scope(&untrimmed).unwrap(), round19());
        let accepted = ClaudePermissionsScope {
            edit: strings(&["src/**", "tests/**/*.py", "a b/c"]),
            bash: strings(&["git", "rm", "pip", "npm", "python3 -c", "sh"]),
            deny: strings(&[]),
        };
        assert_eq!(
            validate_scope(&accepted).unwrap(),
            ClaudePermissionsScope {
                deny: None,
                ..accepted.clone()
            }
        );
        assert_eq!(
            serde_json::to_value(validate_scope(&accepted).unwrap()).unwrap(),
            json!({"edit": ["src/**", "tests/**/*.py", "a b/c"],
                   "bash": ["git", "rm", "pip", "npm", "python3 -c", "sh"]}),
            "absent lists stay absent on the wire"
        );
    }
}
