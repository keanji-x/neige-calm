//! #1704 S2 — the `claude_permissions_policy` field of `PATCH /api/tracks/:id`.
//!
//! The policy is a scope under S1's own rules (`validate_scope_named`), so a
//! Track can never grant a Claude what a Planner declaration could not; the
//! user-only guard, the "travels alone with `workspace`" rule and the
//! short-circuit exemption live in `update_track` beside the other policy
//! fields, and the tree-root-only rule (a child PATCH is 409) is
//! `track_update_tx`'s, the writer every entry shares.

use crate::error::{CalmError, Result};
use crate::model::TrackPatch;
use crate::terminal_permissions::validate_scope_named;

/// Validate a present policy in place: `Some(Some(scope))` must declare at
/// least one list (400 `declares nothing; send null to clear`) and pass S1's
/// scope rules under the field's name (400 with the S1 reason, e.g.
/// `claude_permissions_policy.bash[0] 'git push': floor command, ...`); the
/// trimmed scope replaces the patch value. `Some(None)` (clear) and `None`
/// (leave alone) pass untouched. Shape errors (an array, a `null` list, an
/// unknown key) never reach here: the `TrackPatch` deserializer refuses them
/// as 422 naming `claude_permissions_policy`.
pub(super) fn validate_policy_patch(patch: &mut TrackPatch) -> Result<()> {
    let Some(Some(scope)) = patch.claude_permissions_policy.as_mut() else {
        return Ok(());
    };
    let declares_nothing = [&scope.edit, &scope.bash, &scope.deny]
        .into_iter()
        .all(|list| list.as_ref().is_none_or(Vec::is_empty));
    if declares_nothing {
        return Err(CalmError::BadRequest(
            "claude_permissions_policy declares nothing; send null to clear the policy".into(),
        ));
    }
    *scope =
        validate_scope_named("claude_permissions_policy", scope).map_err(CalmError::BadRequest)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use calm_types::claude_permissions::ClaudePermissionsScope;
    use serde_json::json;

    fn patch(body: serde_json::Value) -> TrackPatch {
        serde_json::from_value(body).unwrap()
    }

    #[test]
    fn a_present_policy_is_trimmed_or_refused_under_its_own_name() {
        let mut absent = patch(json!({"title": "x"}));
        validate_policy_patch(&mut absent).unwrap();
        assert!(absent.claude_permissions_policy.is_none());
        let mut clear = patch(json!({"claude_permissions_policy": null}));
        validate_policy_patch(&mut clear).unwrap();
        assert_eq!(clear.claude_permissions_policy, Some(None));

        let mut trimmed =
            patch(json!({"claude_permissions_policy": {"edit": [" src/** "], "deny": []}}));
        validate_policy_patch(&mut trimmed).unwrap();
        assert_eq!(
            trimmed.claude_permissions_policy,
            Some(Some(ClaudePermissionsScope {
                edit: Some(vec!["src/**".into()]),
                bash: None,
                deny: None,
            }))
        );

        for (body, reason) in [
            (
                json!({"claude_permissions_policy": {}}),
                "claude_permissions_policy declares nothing; send null to clear the policy",
            ),
            (
                json!({"claude_permissions_policy": {"deny": []}}),
                "claude_permissions_policy declares nothing; send null to clear the policy",
            ),
            (
                json!({"claude_permissions_policy": {"bash": ["git push"]}}),
                "claude_permissions_policy.bash[0] 'git push': floor command, always asks; \
                 put it in deny or omit it",
            ),
            (
                json!({"claude_permissions_policy": {"edit": ["/etc/**"]}}),
                "claude_permissions_policy.edit[0]: must be relative to the terminal cwd",
            ),
        ] {
            let mut p = patch(body.clone());
            let err = validate_policy_patch(&mut p).unwrap_err();
            assert!(
                matches!(&err, CalmError::BadRequest(m) if m == reason),
                "{body}: {err:?}"
            );
        }
        // Shape errors are the deserializer's, under the field name.
        for body in [
            json!({"claude_permissions_policy": ["**"]}),
            json!({"claude_permissions_policy": {"allow": []}}),
            json!({"claude_permissions_policy": {"bash": "git"}}),
        ] {
            let err = serde_json::from_value::<TrackPatch>(body.clone()).unwrap_err();
            assert!(
                err.to_string().contains("claude_permissions_policy"),
                "{body}: {err}"
            );
        }
    }
}
