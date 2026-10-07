//! GitHub-only summaries, using the operator's CLI identity without executing repository code.
use crate::{
    error::{CalmError, Result},
    plugin_host::child_process::{inherited_env, run_bounded},
};
use serde::Deserialize;
use std::{sync::OnceLock, time::Duration};
use tokio::{process::Command, sync::Semaphore};
use utoipa::IntoParams;

const CAP: usize = 256 * 1024;
const TIMEOUT: Duration = Duration::from_secs(12);

pub use calm_types::github_preview::{
    GitHubPreview, GitHubPreviewKind as ReferenceKind, GitHubPreviewState as PreviewState,
    GitHubPullChanges as PullChanges,
};

#[derive(Debug, Deserialize, IntoParams)]
pub struct PreviewQuery {
    pub owner: String,
    pub repo: String,
    pub kind: ReferenceKind,
    pub number: u64,
}

impl PreviewQuery {
    fn endpoint(&self) -> Result<String> {
        fn segment(value: &str, max: usize) -> bool {
            !value.is_empty()
                && value.len() <= max
                && value != "."
                && value != ".."
                && value
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
        }
        if !segment(&self.owner, 39)
            || !segment(&self.repo, 100)
            || self.number == 0
            || self.number > 9_007_199_254_740_991
        {
            return Err(CalmError::BadRequest(
                "Invalid GitHub issue or pull request reference".into(),
            ));
        }
        let collection = match self.kind {
            ReferenceKind::Issue => "issues",
            ReferenceKind::Pull => "pulls",
        };
        Ok(format!(
            "repos/{}/{}/{collection}/{}",
            self.owner, self.repo, self.number
        ))
    }
}

fn command(endpoint: &str) -> Command {
    let mut command = Command::new("gh");
    github_environment(&mut command);
    command.current_dir("/").args([
        "api",
        "--hostname",
        "github.com",
        "--method",
        "GET",
        endpoint,
    ]);
    command
}

fn github_environment(command: &mut Command) {
    command.env_clear().envs(inherited_env(&["LANG", "LC_ALL"]));
    // Only REST credentials: no SSH identity, repository config, daemon/worker secrets, or GH_HOST override.
    for key in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1");
}

fn unavailable() -> CalmError {
    CalmError::ServiceUnavailable("GitHub preview unavailable. Check server GitHub sign-in, repository access, and rate limits.".into())
}

pub async fn read(query: PreviewQuery) -> Result<GitHubPreview> {
    let endpoint = query.endpoint()?;
    static READS: OnceLock<Semaphore> = OnceLock::new();
    let _permit = READS
        .get_or_init(|| Semaphore::new(4))
        .try_acquire()
        .map_err(|_| unavailable())?;
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    let raw = read_raw(command(&endpoint), deadline).await?;
    // GitHub also exposes PRs through /issues. Resolve their authoritative merged/draft state.
    if query.kind == ReferenceKind::Issue && raw.pull_request.is_some() {
        let pull_query = PreviewQuery {
            kind: ReferenceKind::Pull,
            ..query
        };
        let raw = read_raw(command(&pull_query.endpoint()?), deadline).await?;
        return summarize(raw, &pull_query);
    }
    summarize(raw, &query)
}

async fn read_raw(command: Command, deadline: tokio::time::Instant) -> Result<RawPreview> {
    let output = run_bounded(command, deadline, CAP)
        .await
        .map_err(|_| unavailable())?;
    if !output.status.success() {
        return Err(unavailable());
    }
    serde_json::from_slice(&output.stdout).map_err(|_| unavailable())
}

#[derive(Deserialize)]
struct User {
    login: String,
}
#[derive(Deserialize)]
struct Label {
    name: String,
}
#[derive(Deserialize)]
struct RawPreview {
    number: u64,
    title: String,
    state: String,
    user: User,
    labels: Vec<Label>,
    body: Option<String>,
    pull_request: Option<serde_json::Value>,
    #[serde(flatten)]
    pull_fields: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct RawPull {
    merged: bool,
    draft: bool,
    #[serde(flatten)]
    changes: PullChanges,
}

fn summarize(raw: RawPreview, query: &PreviewQuery) -> Result<GitHubPreview> {
    if raw.number != query.number {
        return Err(unavailable());
    }
    let closed = match raw.state.as_str() {
        "open" => false,
        "closed" => true,
        _ => return Err(unavailable()),
    };
    let (state, changes) = if query.kind == ReferenceKind::Pull {
        let pull: RawPull = serde_json::from_value(serde_json::Value::Object(raw.pull_fields))
            .map_err(|_| unavailable())?;
        let state = if pull.merged {
            PreviewState::Merged
        } else if closed {
            PreviewState::Closed
        } else if pull.draft {
            PreviewState::Draft
        } else {
            PreviewState::Open
        };
        (state, Some(pull.changes))
    } else {
        (
            if closed {
                PreviewState::Closed
            } else {
                PreviewState::Open
            },
            None,
        )
    };
    Ok(GitHubPreview {
        kind: query.kind,
        number: raw.number,
        title: raw.title.chars().take(250).collect(),
        state,
        author: raw.user.login.chars().take(80).collect(),
        labels: raw
            .labels
            .into_iter()
            .take(12)
            .map(|l| l.name.chars().take(80).collect())
            .collect(),
        excerpt: raw.body.unwrap_or_default().chars().take(500).collect(),
        changes,
    })
}

#[cfg(test)]
mod tests;
