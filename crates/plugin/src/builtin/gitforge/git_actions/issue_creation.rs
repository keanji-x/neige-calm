//! Issue creation identity and external API evidence belong to gitforge.
use super::issue::nonblank;
use super::{event_spec as event_contract, forge_payload, optional_attempt};
use crate::forge_caller::ForgeCallerScope;
use calm_types::event::FieldSource;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const CREATE: &str = include_str!("issue_create.sh");
const PROBE: &str = include_str!("issue_create_probe.sh");

/// Closed selector grammar prevents API path, flag and hostname injection.
fn repository(args: &Value) -> Result<(String, String, String), String> {
    let input = nonblank(args, "repo")?;
    let parts: Vec<_> = input.split('/').collect();
    let (host, owner, name) = match parts.as_slice() {
        [owner, name] => ("github.com", *owner, *name),
        [host, owner, name] => (*host, *owner, *name),
        _ => return Err("repo must be owner/name or host/owner/name".into()),
    };
    let valid = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            && s.as_bytes()[0].is_ascii_alphanumeric()
    };
    if !valid(host) || !valid(owner) || !valid(name) || host.ends_with('.') {
        return Err("invalid repository selector".into());
    }
    let host = host.to_ascii_lowercase();
    let path = format!(
        "{}/{}",
        owner.to_ascii_lowercase(),
        name.to_ascii_lowercase()
    );
    Ok((
        format!("{host}/{path}"),
        host,
        format!("repos/{path}/issues"),
    ))
}

pub(super) fn create(args: &Value, caller: &ForgeCallerScope) -> Result<Value, String> {
    let (repo, host, endpoint) = repository(args)?;
    let title = nonblank(args, "title")?;
    let body = nonblank(args, "body")?;
    let idem = nonblank(args, "idem")?;
    let request = json!({"repo":repo,"title":title,"body":body});
    let request_sha256 = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&request).unwrap())
    );
    let marker_caller = caller.clone();
    let marker = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&json!([marker_caller, repo, idem, request])).unwrap())
    );
    let posted = format!("{body}\n\n<!-- neige:issue-create:v1:{marker} -->");
    let input = serde_json::to_string(&json!({"title":title,"body":posted})).unwrap();
    let probe: Vec<String> = ["sh", "-c", PROBE, "sh", &host, &endpoint, &title, &posted]
        .into_iter()
        .map(str::to_owned)
        .collect();
    forge_payload(
        vec![
            "sh".into(),
            "-c".into(),
            CREATE.into(),
            "sh".into(),
            host,
            endpoint,
            input,
        ],
        format!(
            "gh.issue.create:{}",
            serde_json::to_string(&json!([repo, idem])).unwrap()
        ),
        Some(event_contract(
            "forge.issue.created",
            [
                (
                    "issue_number",
                    FieldSource::JsonField {
                        path: "/issue_number".into(),
                    },
                ),
                (
                    "issue_url",
                    FieldSource::JsonField {
                        path: "/issue_url".into(),
                    },
                ),
            ],
        )),
        json!({"request_sha256":request_sha256}),
        Some(json!({"probe_argv":probe,"output_probe_argv":probe})),
        true,
    )
}

pub(super) fn search(args: &Value) -> Result<Value, String> {
    let (repo, _, _) = repository(args)?;
    let query = nonblank(args, "query")?;
    let state = match args.get("state") {
        None => "all",
        Some(Value::String(s)) if ["open", "closed", "all"].contains(&s.as_str()) => s,
        _ => return Err("state must be open, closed or all".into()),
    };
    let limit = match args.get("limit") {
        None => 30,
        Some(v) => v
            .as_u64()
            .filter(|n| (1..=100).contains(n))
            .ok_or("limit must be 1..100")?,
    };
    let attempt = optional_attempt(args)?;
    let request = json!([repo, query, state, limit, attempt]);
    forge_payload(
        vec![
            "gh".into(),
            "issue".into(),
            "list".into(),
            "--repo".into(),
            repo,
            "--search".into(),
            query,
            "--state".into(),
            state.into(),
            "--limit".into(),
            limit.to_string(),
            "--json".into(),
            "number,url,state,title,body,labels".into(),
        ],
        format!("gh.issue.search:v1:{request}"),
        Some(event_contract("forge.issue.searched", [])),
        json!({"request":request}),
        None,
        false,
    )
}

#[cfg(test)]
mod tests;
