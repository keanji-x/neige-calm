//! Issue discussion writes and reads, lowered into kernel-owned operations.
use super::{forge_payload, optional_attempt, required_string, required_u64};
use crate::forge_caller::ForgeCallerScope;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The plugin id the comment marker encodes. Posted markers, and the forge-action payload hashes
/// stored over their probes, carry the pre-#2087 id; a marker over today's id would find neither,
/// so a retry after the rename would be `idempotency_key_reused` and recovery would repost. Only
/// the marker is frozen: `lower_for_caller` still authorizes the caller against `PLUGIN_ID`.
const COMMENT_MARKER_PLUGIN_ID: &str = "dev.neige.git-forge"; // retired-name: rejection input

const COMMENT_PROBE: &str = "out=$(gh issue view \"$1\" --repo \"$2\" --json comments --jq \"$3\") || exit 3; case \"$out\" in true) exit 0 ;; false) exit 1 ;; *) exit 3 ;; esac";

fn issue_number(args: &Value) -> Result<u64, String> {
    let issue = required_u64(args, "issue")?;
    if issue == 0 {
        return Err("required argument `issue` must be positive".into());
    }
    Ok(issue)
}

pub(super) fn nonblank(args: &Value, name: &str) -> Result<String, String> {
    let value = required_string(args, name)?;
    if value.trim().is_empty() {
        return Err(format!("required argument `{name}` must not be blank"));
    }
    Ok(value)
}

pub(super) fn comment(args: &Value, caller: &ForgeCallerScope) -> Result<Value, String> {
    let repo = nonblank(args, "repo")?;
    let issue = issue_number(args)?;
    let body = nonblank(args, "body")?;
    let idem = nonblank(args, "idem")?;
    let marker_caller = ForgeCallerScope {
        plugin_id: COMMENT_MARKER_PLUGIN_ID.into(),
        ..caller.clone()
    };
    let encoded = serde_json::to_vec(&json!([marker_caller, repo, issue, idem, body]))
        .map_err(|e| format!("encode comment identity: {e}"))?;
    let marker = format!("{:x}", Sha256::digest(encoded));
    let posted = format!("{body}\n\n<!-- neige:issue-comment:{marker} -->");
    let literal =
        serde_json::to_string(&posted).map_err(|e| format!("encode comment body: {e}"))?;
    let query = format!("any(.comments[]; .body == {literal})");
    let identity = serde_json::to_string(&json!([repo, issue, idem]))
        .map_err(|e| format!("encode comment operation identity: {e}"))?;
    forge_payload(
        vec![
            "gh".into(),
            "issue".into(),
            "comment".into(),
            issue.to_string(),
            "--repo".into(),
            repo.clone(),
            "--body".into(),
            posted,
        ],
        // Encode the tuple so colons in user-supplied identifiers cannot alias keys.
        format!("gh.issue.comment:{identity}"),
        None,
        // argv is excluded from the kernel's semantic hash. Bind the original body here.
        json!({ "issue_number": issue, "body_sha256": format!("{:x}", Sha256::digest(body.as_bytes())) }),
        Some(
            json!({ "probe_argv": ["sh", "-c", COMMENT_PROBE, "sh", issue.to_string(), repo, query] }),
        ),
        true,
    )
}

pub(super) fn comments(args: &Value) -> Result<Value, String> {
    let repo = nonblank(args, "repo")?;
    let issue = issue_number(args)?;
    let attempt = optional_attempt(args)?;
    let identity = serde_json::to_string(&json!([repo, issue, attempt]))
        .map_err(|e| format!("encode discussion identity: {e}"))?;
    read_payload(
        vec![
            "gh".into(),
            "issue".into(),
            "view".into(),
            issue.to_string(),
            "--repo".into(),
            repo,
            "--json".into(),
            "comments".into(),
            "--jq".into(),
            ".comments".into(),
        ],
        format!("gh.issue.comments:{identity}"),
        issue,
    )
}

/// Both body and discussion reads publish the existing issue-read result and artifact.
pub(super) fn read_payload(
    argv: Vec<String>,
    identity: String,
    issue: u64,
) -> Result<Value, String> {
    forge_payload(
        argv,
        identity,
        Some(super::event_spec("forge.issue.read", [])),
        json!({ "issue_number": issue }),
        None,
        false,
    )
}

#[cfg(test)]
mod tests;
