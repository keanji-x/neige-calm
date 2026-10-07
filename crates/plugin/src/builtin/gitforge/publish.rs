//! `plugin_gitforge_publish` (#1830 S3, Planner-only): push `neige/track-<id>` to the checkout's
//! upstream URL and open (or reuse) its PR, only when the branch tip is the commit of a `done`
//! attempt of this track (`docs/architecture/1830-s3-push-pr-reclaim.md` D1–D7).
//!
//! The refusals (`refused: publish-…`, `-32409`, the repository's Conflict convention) are
//! answered before any operation exists. The publish itself is one kernel forge action under
//! `GIT_FORGE_PLUGIN_ID`, run in the track worktree by `PR_PUBLISH_SCRIPT` after the
//! credential split `FORGE_SHELL_PRELUDE`; the Planner waits for it and gets the PR inline.

use super::publish_scripts::{
    PR_PUBLISH_OUTPUT_PROBE_SCRIPT, PR_PUBLISH_PROBE_SCRIPT, PR_PUBLISH_SCRIPT,
};
use crate::{builtin::tools::NativeToolSpec, mcp::RpcError};
use async_trait::async_trait;
use calm_types::{
    event::{FieldSource, ForgeEventSpec},
    forge_action::{PluginForgePayload, ProbeSpec, forge_action_payload},
    forge_git::FORGE_SHELL_PRELUDE,
    model::CardRole,
};
use serde_json::{Value, json};
use std::path::PathBuf;
pub const PUBLISH: &str = "publish";
#[derive(Clone)]
pub struct Candidate {
    pub attempt_id: String,
    pub commit_sha: String,
    pub status: String,
    pub done: bool,
}
pub enum PublishOutcome {
    Succeeded { op_id: String, event: Value },
    Failed { op_id: String, reason: String },
}
#[async_trait]
pub trait Services: Send + Sync {
    async fn destination(&self, card_id: &str) -> Result<(String, Destination), RpcError>;
    async fn candidates(&self, track_id: &str) -> Result<Vec<Candidate>, RpcError>;
    async fn execute(
        &self,
        track_id: &str,
        card_id: &str,
        worktree: PathBuf,
        payload: PluginForgePayload,
    ) -> Result<PublishOutcome, RpcError>;
}
pub fn descriptor() -> NativeToolSpec {
    NativeToolSpec {
        name: PUBLISH.into(),
        description: include_str!("prompts/plugin_gitforge_publish.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["idempotency_key", "title", "body"],
            "properties": {
                "idempotency_key": { "type": "string", "minLength": 1 },
                "title": { "type": "string", "minLength": 1 },
                "body": { "type": "string" }
            }
        }),
        annotations: Some(calm_types::plugin::role_gated_write_annotations()),
        roles: &[CardRole::Planner],
        listed_for: &[CardRole::Planner],
    }
}

/// Refusals carry no tool name: the registry leads each with the served name.
fn required_string(args: &Value, name: &str, non_empty: bool) -> Result<String, RpcError> {
    args.get(name)
        .and_then(Value::as_str)
        .filter(|value| !non_empty || !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| RpcError::invalid_params(format!("missing `{name}`")))
}

fn refused(text: String) -> RpcError {
    RpcError::conflict(format!("refused: {text}"))
}

/// D3: the tip may be published only when a `done` attempt of this track produced it. `facts`
/// are the track's candidates, newest first.
fn check_tip(track_id: &str, tip: &str, facts: &[Candidate]) -> Result<(), RpcError> {
    let at_tip: Vec<&Candidate> = facts.iter().filter(|c| c.commit_sha == tip).collect();
    if at_tip.iter().any(|c| c.done) {
        return Ok(());
    }
    if let Some(candidate) = at_tip.first() {
        return Err(refused(format!(
            "publish-candidate-not-done: {tip} is the candidate of attempt {}, which is {}; only \
             a done attempt's commit can be published.",
            candidate.attempt_id, candidate.status
        )));
    }
    let latest = facts.first().map_or_else(
        || "no attempt has produced a candidate yet".to_string(),
        |c| {
            format!(
                "latest candidate {}, attempt {}, {}",
                c.commit_sha, c.attempt_id, c.status
            )
        },
    );
    Err(refused(format!(
        "publish-not-a-candidate: neige/track-{track_id} is at {tip}, which no attempt of this \
         track produced ({latest}). A commit made after the last attempt is not verified: let a \
         task produce it, or undo it."
    )))
}

/// What the publish pushes and where: the tip of the track branch, the upstream URL (both the
/// push destination and gh's `--repo`, D2) and the base branch — the track branch's own upstream
/// (#2112), never the branch the main checkout is on now.
pub struct Destination {
    pub worktree: PathBuf,
    pub branch: String,
    pub tip: String,
    pub url: String,
    pub base: String,
}

/// `sh -c "<prelude>\n<script>" sh <args…>`.
fn shell_argv(script: &str, args: &[&str]) -> Vec<String> {
    let mut argv = vec![
        "sh".to_string(),
        "-c".to_string(),
        format!("{FORGE_SHELL_PRELUDE}\n{script}"),
        "sh".to_string(),
    ];
    argv.extend(args.iter().map(|arg| arg.to_string()));
    argv
}

/// D5 and D6: the forge payload of one publish. `facts` are the track's candidates, whose
/// commits the script may replace on the remote (#2058 D1); argv is not part of the payload hash,
/// the event fields and the probes are.
fn publish_payload(
    dest: &Destination,
    title: &str,
    body: &str,
    idempotency_key: &str,
    facts: &[Candidate],
) -> PluginForgePayload {
    let (sha, branch, url) = (dest.tip.as_str(), dest.branch.as_str(), dest.url.as_str());
    let own_commits = facts
        .iter()
        .map(|candidate| candidate.commit_sha.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    forge_action_payload(
        // The sha is in the payload (argv and probe), not the key: the same key replays its first
        // publish, and the same key over a moved tip is `idempotency_payload_conflict`.
        format!("track.publish:{idempotency_key}"),
        shell_argv(
            PR_PUBLISH_SCRIPT,
            &[sha, branch, url, &dest.base, title, body, &own_commits],
        ),
        publish_receipt_event(),
        ProbeSpec {
            probe_argv: shell_argv(PR_PUBLISH_PROBE_SCRIPT, &[sha, branch, url]),
            output_probe_argv: Some(shell_argv(
                PR_PUBLISH_OUTPUT_PROBE_SCRIPT,
                &[sha, branch, url],
            )),
        },
    )
}

/// The publish's synchronous receipt: `forge.pr.published` from the script's stdout. `pr_action` and `url` are
/// for the tool result; the event drops them.
fn publish_receipt_event() -> ForgeEventSpec {
    let json_field = |path: &str| FieldSource::JsonField { path: path.into() };
    ForgeEventSpec {
        event_kind: "forge.pr.published".into(),
        fields: [
            ("pr_number".to_string(), json_field("/number")),
            // `created`, `reused` or `recovered`.
            ("pr_action".to_string(), json_field("/pr_action")),
            ("head_sha".to_string(), json_field("/headRefOid")),
            // The PR's web URL.
            ("url".to_string(), json_field("/url")),
        ]
        .into_iter()
        .collect(),
    }
}

pub async fn call<S: Services>(
    services: &S,
    card_id: &str,
    args: Value,
) -> Result<Value, RpcError> {
    let idempotency_key = required_string(&args, "idempotency_key", true)?;
    let title = required_string(&args, "title", true)?;
    let body = required_string(&args, "body", false)?;

    let (track_id, dest) = services.destination(card_id).await?;
    let facts = services.candidates(&track_id).await?;
    check_tip(&track_id, &dest.tip, &facts)?;
    let result = services
        .execute(
            &track_id,
            card_id,
            dest.worktree.clone(),
            publish_payload(&dest, &title, &body, &idempotency_key, &facts),
        )
        .await?;
    let (op_id, event) = match result {
        PublishOutcome::Succeeded { op_id, event } => (op_id, event),
        PublishOutcome::Failed { op_id, reason } => return Err(publish_failed(&op_id, &reason)),
    };
    Ok(json!({
        "ok": true,
        "op_id": op_id,
        "pr_number": event["pr_number"],
        "pr_action": event["pr_action"],
        "head_sha": event["head_sha"],
        "branch": dest.branch,
        "base": dest.base,
        "url": event["url"],
    }))
}

/// A publish that ran and did not land. Its key is spent (operation keys are permanent).
fn publish_failed(op_id: &str, reason: &str) -> RpcError {
    RpcError::conflict(format!(
        "publish-failed: operation {op_id}: {reason}. Nothing more runs under this \
             idempotency_key: fix the cause, then call again with a new one."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #2139: a recovered publish re-reads the PR with the output probe; its stdout, from gh's
    /// indented export, extracts through the real event fields with `pr_action` `recovered`.
    #[test]
    fn the_output_probe_answers_every_event_field() {
        let dest = Destination {
            worktree: PathBuf::from("/nonexistent"),
            branch: "neige/track-t".into(),
            tip: "abc".into(),
            url: "/origin.git".into(),
            base: "main".into(),
        };
        let payload = publish_payload(&dest, "Title", "Body.", "k", &[]);
        let argv = payload.probe.unwrap().output_probe_argv.unwrap();
        // `argv[2]` is the prelude, a newline and the script; the fake gh replaces the prelude.
        let script = argv[2].strip_prefix(FORGE_SHELL_PRELUDE).unwrap();
        let fake_gh = "neige_gh() { printf '{\n  \"headRefOid\": \"abc\",\n  \"number\": 7,\n  \"url\": \"https://github.invalid/pull/7\"\n}\n'; }";
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{fake_gh}{script}"))
            .args(&argv[3..])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
        let fields = publish_receipt_event()
            .extract_payload(0, Some(&stdout))
            .unwrap();
        assert_eq!(fields["pr_action"], json!("recovered"));
        assert_eq!(fields["pr_number"], json!(7));
        assert_eq!(fields["head_sha"], json!("abc"));
        assert_eq!(fields["url"], json!("https://github.invalid/pull/7"));
    }
}

#[cfg(test)]
mod receipt_tests {
    use super::*;

    #[test]
    fn native_publish_uses_a_synchronous_receipt_event() {
        let dest = Destination {
            worktree: PathBuf::from("/nonexistent"),
            branch: "neige/track-t".into(),
            tip: "abc".into(),
            url: "/origin.git".into(),
            base: "main".into(),
        };
        let payload = publish_payload(&dest, "Title", "Body.", "receipt", &[]);
        assert!(!payload.parked);
        assert_eq!(payload.event_spec.unwrap().event_kind, "forge.pr.published");
    }
}
