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
use provider::control::{EditPreparation, NativeEditPlan, NativeEditTarget};

/// The dry run loads the session and exits without a model call (~3 s on the pinned CLI).
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
/// The longest CLI reason a refusal carries.
const MAX_REASON_CHARS: usize = 500;

/// Resolve host transcript rows only in the Claude host integration.
pub(crate) fn native_edit_target(
    previous_rows: Option<&[TranscriptRow]>,
    prompt_client_id: Option<&str>,
) -> Result<NativeEditTarget> {
    let previous_anchor = previous_rows.map(anchor);
    let Some(previous_anchor) = previous_anchor else {
        return Err(CalmError::Conflict(
            "the first message of a Claude conversation cannot be edited: Claude has nothing \
             before it to go back to"
                .into(),
        ));
    };
    let at = previous_anchor.ok_or_else(|| {
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
    Ok(NativeEditTarget::TranscriptCut {
        resume_at: at,
        drops_turn,
    })
}

/// The last chain entry of a turn: its outcome's `lastRecordUuid`, or else the record uuid of its
/// last `<uuid>:<n>` reasoning or message item. The fallback is legacy only: rows from before the
/// outcome carried `lastRecordUuid` named items by record uuid, whereas items are now named by API
/// `message.id` (`msg_…:<n>`, not a uuid, so no anchor), and every newer outcome carries it.
pub(super) fn anchor(turn_rows: &[TranscriptRow]) -> Option<Uuid> {
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
) -> Result<EditPreparation<CalmError>> {
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
        NativeEditPlan {
            target: NativeEditTarget::TranscriptCut {
                resume_at: cut.at,
                drops_turn: cut.drops_turn,
            },
        },
    )
    .await
}

async fn run(
    binary: &Path,
    args: Vec<OsString>,
    env: Vec<(String, OsString)>,
    cwd: &Path,
    plan: NativeEditPlan,
) -> Result<EditPreparation<CalmError>> {
    let result = async {
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
            return Ok(EditPreparation::Prepared { plan });
        }
        Ok(EditPreparation::Failed {
            error: CalmError::Conflict(format!(
                "Claude refused to remove this turn: {}",
                refusal_reason(&output.stdout, &output.stderr, output.status)
            )),
        })
    }
    .await;
    Ok(match result {
        Ok(preparation) => preparation,
        Err(error) => EditPreparation::Failed { error },
    })
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

#[cfg(test)]
mod mapping_tests {
    use super::*;
    #[tokio::test]
    async fn m1_mapping_edit_dry_run_accept_refuse_and_failure_table() {
        for (command, expected) in [
            ("exit 0", "prepared"),
            (
                "printf '%s' '{\"type\":\"result\",\"errors\":[\"native reason\"]}'; exit 1",
                "rejected",
            ),
            ("", "failed"),
        ] {
            let target = NativeEditTarget::TranscriptCut {
                resume_at: Uuid::from_u128(1),
                drops_turn: Uuid::from_u128(2),
            };
            let binary = if expected == "failed" {
                Path::new("/no-such-neige-rewind-binary")
            } else {
                Path::new("/bin/sh")
            };
            let prepared = run(
                binary,
                vec!["-c".into(), command.into()],
                vec![],
                Path::new("/tmp"),
                NativeEditPlan {
                    target: target.clone(),
                },
            )
            .await
            .unwrap();
            match (expected, prepared) {
                ("prepared", EditPreparation::Prepared { plan }) => assert_eq!(plan.target, target),
                (
                    "rejected",
                    EditPreparation::Failed {
                        error: CalmError::Conflict(message),
                    },
                ) => assert_eq!(message, "Claude refused to remove this turn: native reason"),
                (
                    "failed",
                    EditPreparation::Failed {
                        error: CalmError::Internal(message),
                    },
                ) => assert!(message.starts_with(
                    "the Claude rewind check could not start /no-such-neige-rewind-binary:"
                )),
                other => panic!("wrong preparation {other:?}"),
            }
        }
        assert!(
            matches!(native_edit_target(None, None), Err(CalmError::Conflict(message)) if message == "the first message of a Claude conversation cannot be edited: Claude has nothing before it to go back to")
        );
        assert!(
            matches!(native_edit_target(Some(&[]), None), Err(CalmError::Conflict(message)) if message == "the previous reply did not record where it ended, so this turn cannot be removed")
        );
    }
}
