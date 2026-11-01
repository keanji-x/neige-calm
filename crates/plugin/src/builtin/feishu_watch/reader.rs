//! Only declared read-only CLI commands run, with no inherited environment or shell.
use super::{
    MAX_STATE_BYTES,
    model::{Config, Message, Source, timestamp},
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadError {
    Incomplete,
    Malformed,
    Invocation,
    TimedOut,
    OutputLimit,
    IdentityMismatch,
}
#[async_trait]
pub trait Reader: Send + Sync {
    async fn verify_identity(&self, config: &Config) -> Result<(), ReadError>;
    async fn read(
        &self,
        config: &Config,
        source: &Source,
        since_ms: i64,
        until_ms: i64,
    ) -> Result<Vec<Message>, ReadError>;
}
pub struct CliReader;
#[async_trait]
impl Reader for CliReader {
    async fn verify_identity(&self, config: &Config) -> Result<(), ReadError> {
        let bytes = run_cli(
            config,
            &[
                "whoami".into(),
                "--as".into(),
                "user".into(),
                "--profile".into(),
                config.profile.clone(),
            ],
        )
        .await?;
        parse_identity(&bytes, config)
    }
    async fn read(
        &self,
        config: &Config,
        source: &Source,
        since_ms: i64,
        until_ms: i64,
    ) -> Result<Vec<Message>, ReadError> {
        parse_output(&run_cli(config, &argv(config, source, since_ms, until_ms)?).await?)
    }
}
pub fn parse_identity(bytes: &[u8], config: &Config) -> Result<(), ReadError> {
    let v: Value = serde_json::from_slice(bytes).map_err(|_| ReadError::Malformed)?;
    if v.get("identity").and_then(Value::as_str) != Some("user")
        || v.get("available") != Some(&Value::Bool(true))
        || v.pointer("/onBehalfOf/openId").and_then(Value::as_str) != Some(&config.self_open_id)
    {
        return Err(ReadError::IdentityMismatch);
    }
    Ok(())
}

async fn run_cli(config: &Config, args: &[String]) -> Result<Vec<u8>, ReadError> {
    config.validate().map_err(|_| ReadError::Malformed)?;
    let parent = config.cli_path.parent().ok_or(ReadError::Malformed)?;
    let path = std::env::join_paths([
        parent,
        std::path::Path::new("/usr/bin"),
        std::path::Path::new("/bin"),
    ])
    .map_err(|_| ReadError::Malformed)?;
    let mut command = tokio::process::Command::new(&config.cli_path);
    command
        .args(args)
        .env_clear()
        .env("HOME", &config.home)
        .env("PATH", path)
        .env("LANG", "C.UTF-8")
        .env("LARKSUITE_CLI_NO_UPDATE_NOTIFIER", "1")
        .env("LARKSUITE_CLI_NO_SKILLS_NOTIFIER", "1")
        .current_dir(&config.home);
    let output = crate::child_process::run_bounded(
        command,
        tokio::time::Instant::now() + Duration::from_secs(60),
        MAX_STATE_BYTES,
    )
    .await
    .map_err(|error| match error {
        crate::child_process::BoundedRunError::TimedOut => ReadError::TimedOut,
        crate::child_process::BoundedRunError::Oversized => ReadError::OutputLimit,
        _ => ReadError::Invocation,
    })?;
    if !output.status.success() {
        return Err(ReadError::Invocation);
    }
    Ok(output.stdout)
}

pub fn argv(
    config: &Config,
    source: &Source,
    since_ms: i64,
    until_ms: i64,
) -> Result<Vec<String>, ReadError> {
    let mut args = vec!["im".into()];
    let mut add = |values: &[&str]| args.extend(values.iter().map(|s| s.to_string()));
    match source {
        Source::Group { chat_id } => add(&["+chat-messages-list", "--chat-id", chat_id]),
        Source::IncomingP2p => add(&["+messages-search", "--query", "", "--chat-type", "p2p"]),
        Source::AtMe => add(&["+messages-search", "--query", "", "--is-at-me"]),
        Source::Thread { thread_id } => add(&["+threads-messages-list", "--thread", thread_id]),
    }
    add(&[
        "--as",
        "user",
        "--page-all",
        "--page-size",
        "50",
        "--no-reactions",
        "--format",
        "json",
        "--profile",
        &config.profile,
    ]);
    // Search's documented hard maximum is 40 pages; list commands permit 1000.
    if matches!(source, Source::Group { .. } | Source::Thread { .. }) {
        add(&["--order", "asc"]);
    }
    let page_limit = if matches!(source, Source::IncomingP2p | Source::AtMe) {
        "40"
    } else {
        "1000"
    };
    add(&["--page-limit", page_limit]);
    if !matches!(source, Source::Thread { .. }) {
        let start = chrono::DateTime::from_timestamp_millis(since_ms)
            .ok_or(ReadError::Malformed)?
            .to_rfc3339();
        let end = chrono::DateTime::from_timestamp_millis(until_ms)
            .ok_or(ReadError::Malformed)?
            .to_rfc3339();
        add(&["--start", &start, "--end", &end]);
    }
    Ok(args)
}
pub fn parse_output(bytes: &[u8]) -> Result<Vec<Message>, ReadError> {
    if bytes.len() > MAX_STATE_BYTES {
        return Err(ReadError::OutputLimit);
    }
    let v: Value = serde_json::from_slice(bytes).map_err(|_| ReadError::Malformed)?;
    if v.get("ok") != Some(&Value::Bool(true))
        || v.get("identity").and_then(Value::as_str) != Some("user")
    {
        return Err(ReadError::Malformed);
    }
    if v.pointer("/meta/pagination/complete") != Some(&Value::Bool(true)) {
        return Err(ReadError::Incomplete);
    }
    let messages = v
        .pointer("/data/messages")
        .and_then(Value::as_array)
        .ok_or(ReadError::Malformed)?;
    messages
        .iter()
        .map(|raw| {
            if serde_json::to_vec(raw)
                .map_err(|_| ReadError::Malformed)?
                .len()
                > 32 * 1024
            {
                return Err(ReadError::OutputLimit);
            }
            let id = raw
                .get("message_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or(ReadError::Malformed)?
                .to_string();
            let at_ms = timestamp(raw.get("create_time").ok_or(ReadError::Malformed)?)
                .map_err(|_| ReadError::Malformed)?;
            let sender_id = raw
                .pointer("/sender/id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or(ReadError::Malformed)?
                .to_string();
            Ok(Message {
                id,
                at_ms,
                sender_id,
                raw: raw.clone(),
            })
        })
        .collect()
}
