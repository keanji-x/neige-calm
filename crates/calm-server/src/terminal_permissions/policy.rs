//! The Track tree's Claude permission policy as the ceiling of a `calm.terminal.open`
//! declaration. A list the declaration omits is inherited from the policy, a list it gives must
//! be within the policy; deny = policy.deny ++ declared.deny. The merge never widens; a declared
//! `deny` equal to an inherited allow is a legal narrowing.

use calm_types::claude_permissions::{ClaudePermissionsScope, ClaudePermissionsSource};

#[cfg(feature = "fixtures")]
use std::collections::HashMap;
#[cfg(feature = "fixtures")]
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
#[cfg(feature = "fixtures")]
use tokio::sync::Notify;

/// How many ceiling entries a refusal names before `, … (N more)`.
const CEILING_NAMED_MAX: usize = 6;

/// Equal, or the ceiling is `**`, or the ceiling is `dir/**` and `glob` starts with `dir/`
/// (slash included). Anything else needs equality: the ceiling is written by a user, the
/// declaration by an agent.
pub(crate) fn edit_glob_within(glob: &str, ceiling: &str) -> bool {
    if glob == ceiling || ceiling == "**" {
        return true;
    }
    if ceiling.ends_with("/**") {
        return glob.starts_with(&ceiling[..ceiling.len() - 2]);
    }
    false
}

/// Equal, or `prefix` starts with `ceiling` followed by a space (token boundary: `git` covers
/// `git status`, not `gitk`).
pub(crate) fn bash_prefix_covered(prefix: &str, ceiling: &str) -> bool {
    prefix == ceiling
        || prefix
            .strip_prefix(ceiling)
            .is_some_and(|rest| rest.starts_with(' '))
}

/// The ceiling list a refusal names.
fn ceiling_list(name: &str, entries: Option<&[String]>) -> String {
    match entries {
        None | Some([]) => format!("{name}: none"),
        Some(entries) => {
            let named = entries
                .iter()
                .take(CEILING_NAMED_MAX)
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            if entries.len() > CEILING_NAMED_MAX {
                format!(
                    "{name}: {named}, … ({} more)",
                    entries.len() - CEILING_NAMED_MAX
                )
            } else {
                format!("{name}: {named}")
            }
        }
    }
}

/// The scope to render and its source. Both inputs are validated scopes. `Ok(None)` is the
/// no-policy, no-declaration case; `Err` names the first exceeding declared entry.
pub fn apply_policy(
    policy: Option<&ClaudePermissionsScope>,
    declared: Option<&ClaudePermissionsScope>,
) -> Result<Option<(ClaudePermissionsScope, ClaudePermissionsSource)>, String> {
    let (policy, declared) = match (policy, declared) {
        (None, None) => return Ok(None),
        (None, Some(declared)) => {
            return Ok(Some((declared.clone(), ClaudePermissionsSource::Declared)));
        }
        (Some(policy), None) => {
            return Ok(Some((policy.clone(), ClaudePermissionsSource::TrackPolicy)));
        }
        (Some(policy), Some(declared)) => (policy, declared),
    };
    for (index, glob) in declared.edit.iter().flatten().enumerate() {
        let within = policy
            .edit
            .iter()
            .flatten()
            .any(|ceiling| edit_glob_within(glob, ceiling));
        if !within {
            return Err(format!(
                "claude_permissions.edit[{index}] '{glob}' exceeds the Track policy ({})",
                ceiling_list("edit", policy.edit.as_deref())
            ));
        }
    }
    for (index, prefix) in declared.bash.iter().flatten().enumerate() {
        let covered = policy
            .bash
            .iter()
            .flatten()
            .any(|ceiling| bash_prefix_covered(prefix, ceiling));
        if !covered {
            return Err(format!(
                "claude_permissions.bash[{index}] '{prefix}' exceeds the Track policy ({})",
                ceiling_list("bash", policy.bash.as_deref())
            ));
        }
        let denied = policy
            .deny
            .iter()
            .flatten()
            .any(|ceiling| bash_prefix_covered(prefix, ceiling));
        if denied {
            return Err(format!(
                "claude_permissions.bash[{index}] '{prefix}' is denied by the Track policy ({})",
                ceiling_list("deny", policy.deny.as_deref())
            ));
        }
    }
    let mut deny: Vec<String> = policy.deny.clone().unwrap_or_default();
    for prefix in declared.deny.iter().flatten() {
        if !deny.contains(prefix) {
            deny.push(prefix.clone());
        }
    }
    let merged = ClaudePermissionsScope {
        edit: declared.edit.clone().or_else(|| policy.edit.clone()),
        bash: declared.bash.clone().or_else(|| policy.bash.clone()),
        deny: Some(deny).filter(|deny| !deny.is_empty()),
    };
    Ok(Some((
        merged,
        ClaudePermissionsSource::DeclaredWithinPolicy,
    )))
}

/// Test seam between the handler's ceiling pre-check and the operation submit, so a fixture can
/// PATCH the policy in between; `prepare_tx` must re-read it inside the write transaction.
#[cfg(feature = "fixtures")]
#[derive(Clone)]
pub struct CeilingCheckedHook {
    pub entered: Arc<Notify>,
    pub release: Arc<Notify>,
}

#[cfg(feature = "fixtures")]
fn ceiling_checked_hooks() -> &'static StdMutex<HashMap<String, CeilingCheckedHook>> {
    static HOOKS: OnceLock<StdMutex<HashMap<String, CeilingCheckedHook>>> = OnceLock::new();
    HOOKS.get_or_init(|| StdMutex::new(HashMap::new()))
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn install_ceiling_checked_hook_for_test(track_id: &str, hook: CeilingCheckedHook) {
    ceiling_checked_hooks()
        .lock()
        .expect("ceiling checked hook mutex")
        .insert(track_id.to_string(), hook);
}

/// Park here once per installed hook for `track_id` (a no-op in production).
pub async fn wait_at_ceiling_checked_hook(track_id: &str) {
    #[cfg(feature = "fixtures")]
    {
        let hook = ceiling_checked_hooks()
            .lock()
            .expect("ceiling checked hook mutex")
            .remove(track_id);
        if let Some(hook) = hook {
            hook.entered.notify_one();
            hook.release.notified().await;
        }
    }
    #[cfg(not(feature = "fixtures"))]
    let _ = track_id;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_permissions::{
        parse_scope, parse_scope_named, validate_scope, validate_scope_named,
    };
    use serde_json::{Value, json};

    fn strings(values: &[&str]) -> Option<Vec<String>> {
        Some(values.iter().map(|value| (*value).to_owned()).collect())
    }

    fn scope(value: Value) -> ClaudePermissionsScope {
        validate_scope(&parse_scope(&value).unwrap()).unwrap()
    }

    fn policy() -> ClaudePermissionsScope {
        scope(json!({
            "edit": ["src/**", "tests/**"],
            "bash": ["git", "python3 -m unittest"],
            "deny": ["git rebase"]
        }))
    }

    #[test]
    fn edit_glob_containment_truth_table() {
        for glob in ["src/x.py", "src/**", "a b/c", "**"] {
            assert!(edit_glob_within(glob, "**"), "** admits {glob}");
        }
        for glob in ["src/**", "src/x.py", "src/lib/**", "src/**/*.py", "src/*"] {
            assert!(edit_glob_within(glob, "src/**"), "src/** admits {glob}");
        }
        for glob in ["src2/x", "src", "**", "**/x"] {
            assert!(!edit_glob_within(glob, "src/**"), "src/** refuses {glob}");
        }
        assert!(
            !edit_glob_within("a/b/**", "a/b"),
            "a single path admits only itself"
        );
        assert!(
            !edit_glob_within("a/b", "a/b/**"),
            "a/b does not start with a/b/"
        );
        assert!(edit_glob_within("a/b", "a/b"));
        assert!(!edit_glob_within("src/x.py", "**/*.py"), "equality only");
        assert!(edit_glob_within("**/*.py", "**/*.py"));
    }

    #[test]
    fn bash_prefix_coverage_truth_table() {
        for prefix in ["git status", "git commit", "git"] {
            assert!(bash_prefix_covered(prefix, "git"), "git covers {prefix}");
        }
        for prefix in ["gitk", "git-lfs"] {
            assert!(!bash_prefix_covered(prefix, "git"), "git refuses {prefix}");
        }
        assert!(bash_prefix_covered(
            "python3 -m unittest discover",
            "python3 -m unittest"
        ));
        assert!(bash_prefix_covered(
            "python3 -m unittest",
            "python3 -m unittest"
        ));
        for prefix in ["python3 -m unittest2", "python3"] {
            assert!(
                !bash_prefix_covered(prefix, "python3 -m unittest"),
                "refuses {prefix}"
            );
        }
    }

    #[test]
    fn apply_policy_rows_one_to_four() {
        assert_eq!(apply_policy(None, None), Ok(None));
        let declared = scope(json!({"edit": ["**"], "deny": ["git push"]}));
        assert_eq!(
            apply_policy(None, Some(&declared)),
            Ok(Some((declared.clone(), ClaudePermissionsSource::Declared)))
        );
        assert_eq!(
            apply_policy(Some(&policy()), None),
            Ok(Some((policy(), ClaudePermissionsSource::TrackPolicy)))
        );

        // A deny-only declaration: the policy's allow lists, the deny appended.
        let narrowed = scope(json!({"deny": ["git push"]}));
        assert_eq!(
            apply_policy(Some(&policy()), Some(&narrowed)),
            Ok(Some((
                ClaudePermissionsScope {
                    edit: strings(&["src/**", "tests/**"]),
                    bash: strings(&["git", "python3 -m unittest"]),
                    deny: strings(&["git rebase", "git push"]),
                },
                ClaudePermissionsSource::DeclaredWithinPolicy
            )))
        );
        // A bash-only declaration: `edit` inherited, the given bash kept
        // (checked), the policy deny alone.
        let bash_only = scope(json!({"bash": ["git status", "python3 -m unittest discover"]}));
        assert_eq!(
            apply_policy(Some(&policy()), Some(&bash_only)),
            Ok(Some((
                ClaudePermissionsScope {
                    edit: strings(&["src/**", "tests/**"]),
                    bash: strings(&["git status", "python3 -m unittest discover"]),
                    deny: strings(&["git rebase"]),
                },
                ClaudePermissionsSource::DeclaredWithinPolicy
            )))
        );
        // Every list given and within: the declaration's lists, the deny
        // merged policy-first and deduplicated.
        let full = scope(json!({
            "edit": ["src/lib/**"],
            "bash": ["git status"],
            "deny": ["git push", "git rebase"]
        }));
        assert_eq!(
            apply_policy(Some(&policy()), Some(&full)),
            Ok(Some((
                ClaudePermissionsScope {
                    edit: strings(&["src/lib/**"]),
                    bash: strings(&["git status"]),
                    deny: strings(&["git rebase", "git push"]),
                },
                ClaudePermissionsSource::DeclaredWithinPolicy
            )))
        );
        // A declared deny equal to an inherited allow is a legal narrowing.
        let deny_git = scope(json!({"deny": ["git"]}));
        let (merged, _) = apply_policy(Some(&policy()), Some(&deny_git))
            .unwrap()
            .unwrap();
        assert_eq!(merged.bash, strings(&["git", "python3 -m unittest"]));
        assert_eq!(merged.deny, strings(&["git rebase", "git"]));
        // A policy without a deny and a declaration without one: no deny.
        let bare = scope(json!({"bash": ["git"]}));
        let (merged, _) = apply_policy(Some(&bare), Some(&scope(json!({"bash": ["git log"]}))))
            .unwrap()
            .unwrap();
        assert_eq!(merged.deny, None);
    }

    #[test]
    fn apply_policy_row_five_messages() {
        let cases: Vec<(Value, &str)> = vec![
            (
                json!({"edit": ["**"]}),
                "claude_permissions.edit[0] '**' exceeds the Track policy (edit: src/**, tests/**)",
            ),
            (
                json!({"edit": ["src/**", "docs/**"]}),
                "claude_permissions.edit[1] 'docs/**' exceeds the Track policy (edit: src/**, tests/**)",
            ),
            // (`pip install` itself is a floor command refused first.)
            (
                json!({"bash": ["git status", "git diff", "git log", "pip download"]}),
                "claude_permissions.bash[3] 'pip download' exceeds the Track policy (bash: git, python3 -m unittest)",
            ),
            (
                json!({"bash": ["git status", "git rebase"]}),
                "claude_permissions.bash[1] 'git rebase' is denied by the Track policy (deny: git rebase)",
            ),
            (
                json!({"bash": ["git status", "git rebase -i"]}),
                "claude_permissions.bash[1] 'git rebase -i' is denied by the Track policy (deny: git rebase)",
            ),
            (
                json!({"bash": ["gitk"]}),
                "claude_permissions.bash[0] 'gitk' exceeds the Track policy (bash: git, python3 -m unittest)",
            ),
            // Edit is checked before bash: the first violation wins.
            (
                json!({"edit": ["**"], "bash": ["pip download"]}),
                "claude_permissions.edit[0] '**' exceeds the Track policy (edit: src/**, tests/**)",
            ),
        ];
        for (declared, reason) in cases {
            let declared = scope(declared);
            assert_eq!(
                apply_policy(Some(&policy()), Some(&declared)).unwrap_err(),
                reason,
                "{declared:?}"
            );
        }
        // An absent policy list admits no declared entry of that kind.
        let bash_only_policy = scope(json!({"bash": ["git"]}));
        assert_eq!(
            apply_policy(
                Some(&bash_only_policy),
                Some(&scope(json!({"edit": ["src/x"]})))
            )
            .unwrap_err(),
            "claude_permissions.edit[0] 'src/x' exceeds the Track policy (edit: none)"
        );
        let edit_only_policy = scope(json!({"edit": ["**"]}));
        assert_eq!(
            apply_policy(
                Some(&edit_only_policy),
                Some(&scope(json!({"bash": ["git status"]})))
            )
            .unwrap_err(),
            "claude_permissions.bash[0] 'git status' exceeds the Track policy (bash: none)"
        );
        // The ceiling list is cut after six entries.
        let wide = scope(json!({"bash": ["a", "b", "c", "d", "e", "f", "g", "h"]}));
        assert_eq!(
            apply_policy(Some(&wide), Some(&scope(json!({"bash": ["z"]})))).unwrap_err(),
            "claude_permissions.bash[0] 'z' exceeds the Track policy (bash: a, b, c, d, e, f, … (2 more))"
        );
        let six = scope(json!({"bash": ["a", "b", "c", "d", "e", "f"]}));
        assert_eq!(
            apply_policy(Some(&six), Some(&scope(json!({"bash": ["z"]})))).unwrap_err(),
            "claude_permissions.bash[0] 'z' exceeds the Track policy (bash: a, b, c, d, e, f)"
        );
    }

    #[test]
    fn named_validation_yields_the_s1_reasons_under_both_names() {
        let cases = [
            json!({}),
            json!({"deny": []}),
            json!({"edit": []}),
            json!({"bash": ["git status", "git push"]}),
            json!({"edit": ["/etc/**"]}),
            json!({"edit": ["src/../x"]}),
            json!({"bash": ["git *"]}),
            json!({"bash": ["timeout 5 python3"]}),
            json!({"bash": ["git status", "git commit"], "deny": ["git commit"]}),
            json!({"edit": ["**", " ** "]}),
        ];
        for input in cases {
            let parsed = parse_scope(&input).unwrap();
            let plain = validate_scope(&parsed).unwrap_err();
            let named = validate_scope_named("claude_permissions_policy", &parsed).unwrap_err();
            assert!(plain.starts_with("claude_permissions"), "{plain}");
            assert_eq!(
                named,
                plain.replacen("claude_permissions", "claude_permissions_policy", 1),
                "{input}"
            );
        }
        let shapes = [
            json!([["**"], null, []]),
            json!(null),
            json!({"edit": ["**"], "deny": null}),
            json!({"bash": "git status"}),
            json!({"edit": ["**"], "allow": ["x"]}),
        ];
        for input in shapes {
            let plain = parse_scope(&input).unwrap_err();
            let named = parse_scope_named("claude_permissions_policy", &input).unwrap_err();
            assert_eq!(
                named,
                plain.replacen("claude_permissions", "claude_permissions_policy", 1),
                "{input}"
            );
        }
        // An accepted scope is the same trimmed value under both names.
        let accepted = parse_scope(&json!({"edit": [" src/** "], "deny": []})).unwrap();
        assert_eq!(
            validate_scope_named("claude_permissions_policy", &accepted).unwrap(),
            validate_scope(&accepted).unwrap()
        );
    }
}
