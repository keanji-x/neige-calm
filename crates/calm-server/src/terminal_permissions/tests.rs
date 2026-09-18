//! Unit tests of the scope parser, the validator and the renderer.
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
    // The storage derive is a separate contract: lenient on unknown keys
    // (#1704 S2 — a stored row written by a newer binary must still
    // decode; only `parse_scope` refuses them), and it round-trips what
    // `parse_scope` accepted.
    assert_eq!(
        serde_json::from_value::<ClaudePermissionsScope>(json!({"allow": ["x"]})).unwrap(),
        ClaudePermissionsScope::default()
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
        serde_json::from_value::<ClaudePermissionsScope>(serde_json::to_value(&parsed).unwrap())
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
