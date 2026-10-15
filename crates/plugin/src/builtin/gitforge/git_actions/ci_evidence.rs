use super::{
    checks_sanitizer, event_spec as event_contract, forge_payload, optional_attempt,
    required_commit_sha, required_string, required_u64,
};
use serde_json::{Value, json};

/// The evidence read: the shared Actions prelude, then the script.
pub(super) const CI_EVIDENCE_SCRIPT: &str = concat!(
    include_str!("actions_prelude.sh"),
    include_str!("ci_evidence.sh"),
);

/// Bounded failure evidence of the head's Actions runs, stored only in the event's artifact.
pub(super) fn lower_gh_ci_failure_evidence(args: &Value) -> Result<Value, String> {
    // Idempotent read: intentionally probe-free.
    let repo = required_string(args, "repo")?;
    let pr = required_u64(args, "pr")?;
    let head_sha = required_commit_sha(args, "head_sha")?;
    let attempt = optional_attempt(args)?;
    // A JSON array encodes every argument injectively, so replaying by this key alone is safe.
    let arguments = serde_json::to_string(&json!([repo, pr, head_sha, attempt])).unwrap();
    let mut payload = forge_payload(
        vec![
            "sh".into(),
            "-c".into(),
            CI_EVIDENCE_SCRIPT.into(),
            "sh".into(),
            repo,
            head_sha.clone(),
            checks_sanitizer(),
        ],
        format!("gh_ci_failure_evidence:v1:{arguments}"),
        Some(event_contract("forge.ci.evidence.read", [])),
        json!({ "pr_number": pr, "head_sha": head_sha }),
        None,
        false,
    )?;
    payload["replay_any_version"] = json!(true);
    Ok(payload)
}

#[cfg(test)]
mod tests;
