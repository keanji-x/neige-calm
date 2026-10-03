//! A Claude conversation's half of a rewind (#1923): where the session is cut, and whether the
//! pinned CLI accepts that cut, both decided before anything is removed.

use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::process::Command;
use uuid::Uuid;

use super::protocol::client_line_uuid;
use super::session::ClaudePlannerSessionParams;
use super::spawn::{self, ResumeTruncation};
use crate::db::TranscriptRow;
use crate::error::{CalmError, Result};

/// The dry run loads the session and exits without a model call (~3 s on the pinned CLI).
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
/// The longest CLI reason a refusal carries.
const MAX_REASON_CHARS: usize = 500;

/// The cut that removes the turn whose prompt line is `prompt_client_id`: keep the session up to the
/// chain entry the previous turn ended on. `previous_turn` is `None` when the turn is the first.
pub(crate) fn truncation(
    previous_turn: Option<&[TranscriptRow]>,
    prompt_client_id: Option<&str>,
) -> Result<ResumeTruncation> {
    let Some(previous_turn) = previous_turn else {
        return Err(CalmError::Conflict(
            "the first message of a Claude conversation cannot be edited: Claude has nothing \
             before it to go back to"
                .into(),
        ));
    };
    let at = anchor(previous_turn).ok_or_else(|| {
        CalmError::Conflict(
            "the previous reply did not record where it ended, so this turn cannot be removed"
                .into(),
        )
    })?;
    let drops_turn = prompt_client_id
        .and_then(|client_id| client_line_uuid(client_id).ok())
        .ok_or_else(|| {
            CalmError::Conflict(
                "this turn's message carries no id Claude knows it by, so it cannot be removed"
                    .into(),
            )
        })?;
    Ok(ResumeTruncation { at, drops_turn })
}

/// The last chain entry of a turn: its outcome's `lastRecordUuid`, or else the record uuid of its
/// last `<uuid>:<n>` reasoning or message item. The fallback is legacy only: rows from before the
/// outcome carried `lastRecordUuid` named items by record uuid, whereas items are now named by API
/// `message.id` (`msg_…:<n>`, not a uuid, so no anchor), and every newer outcome carries it.
pub(crate) fn anchor(turn_rows: &[TranscriptRow]) -> Option<Uuid> {
    let recorded = turn_rows
        .iter()
        .rev()
        .filter(|row| row.method == "turn/completed")
        .find_map(|row| {
            let params: Value = serde_json::from_str(&row.params).ok()?;
            Uuid::try_parse(params.get("lastRecordUuid")?.as_str()?).ok()
        });
    recorded.or_else(|| {
        turn_rows
            .iter()
            .rev()
            .filter(|row| matches!(row.item_type.as_deref(), Some("reasoning" | "agentMessage")))
            .find_map(|row| {
                let (uuid, index) = row.item_uuid.as_deref()?.rsplit_once(':')?;
                index.parse::<u32>().ok()?;
                Uuid::try_parse(uuid).ok()
            })
    })
}

/// Run the dry run with a turn spawn's binary, cwd and environment: exit 0 means the cut applies;
/// anything else is a refusal carrying the CLI's own words. Stdin is closed, so no prompt is read.
pub(crate) async fn check(
    params: &ClaudePlannerSessionParams,
    thread: &str,
    cut: &ResumeTruncation,
) -> Result<()> {
    let thread = Uuid::try_parse(thread).map_err(|_| {
        CalmError::BadRequest(format!("claude planner thread {thread} is not a UUID"))
    })?;
    let config = params.host.configured()?;
    let env = spawn::session_env(params, &config.config_dir)?;
    run(
        &config.claude_binary,
        spawn::truncation_check_argv(thread, cut),
        env,
        &params.cwd,
    )
    .await
}

async fn run(
    binary: &Path,
    args: Vec<OsString>,
    env: Vec<(String, OsString)>,
    cwd: &Path,
) -> Result<()> {
    let child = Command::new(binary)
        .args(args)
        .env_clear()
        .envs(env)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            CalmError::Internal(format!(
                "the Claude rewind check could not start {}: {error}",
                binary.display()
            ))
        })?;
    let output = tokio::time::timeout(CHECK_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| {
            CalmError::ServiceUnavailable(
                "Claude did not finish checking the edit in time; nothing was changed".into(),
            )
        })??;
    if output.status.success() {
        return Ok(());
    }
    Err(CalmError::Conflict(format!(
        "Claude refused to remove this turn: {}",
        refusal_reason(&output.stdout, &output.stderr, output.status)
    )))
}

/// The `errors` of the CLI's result record, else its last stderr line, else the exit status.
fn refusal_reason(stdout: &[u8], stderr: &[u8], status: std::process::ExitStatus) -> String {
    let from_result = String::from_utf8_lossy(stdout)
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|record| record.get("type").and_then(Value::as_str) == Some("result"))
        .find_map(|record| {
            let errors = record
                .get("errors")?
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("; ");
            (!errors.trim().is_empty()).then_some(errors)
        });
    let from_stderr = || {
        String::from_utf8_lossy(stderr)
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .map(str::to_string)
    };
    let reason = from_result
        .or_else(from_stderr)
        .unwrap_or_else(|| format!("claude exited ({status})"));
    reason.trim().chars().take(MAX_REASON_CHARS).collect()
}
