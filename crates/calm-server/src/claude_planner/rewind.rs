//! A Claude conversation's half of a rewind (#1923): where the session is cut, and whether the
//! pinned CLI accepts that cut, both decided before anything is removed.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use crate::provider_execution::{
    self as execution, KnownEnd, Purpose, StopReason, stderr, stdio::EngineStdio,
};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use uuid::Uuid;
use workspace_runtime::execution::*;

use super::session::ClaudePlannerSessionParams;
use super::spawn::{self, ResumeTruncation};
use crate::db::TranscriptRow;
use crate::error::{CalmError, Result};
use provider::claude::protocol::client_line_uuid;
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
    let engine = execution::engine(params.engine.as_ref())?;
    let pool = params
        .repo
        .sqlite_db()
        .ok_or(CalmError::ExecutionUnavailable)?;
    let owner = &params.host.provider_execution;
    let id = ExecutionId::fresh()?;
    let dir = stderr::create(&params.host.instructions_dir, id.as_str())?;
    let prepared = intent(
        params,
        &id,
        &dir,
        &config.claude_binary,
        spawn::truncation_check_argv(thread, cut),
        env,
    )
    .await;
    let (intent, payload) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            stderr::cleanup(&params.host.instructions_dir, id.as_str());
            return Err(error);
        }
    };
    let (claim, guard) = match owner
        .admit(
            &pool,
            engine,
            &params.worker_session_id,
            intent,
            Purpose::RewindCheck,
        )
        .await
    {
        Ok(admitted) => admitted,
        Err(error) => {
            stderr::cleanup(&params.host.instructions_dir, id.as_str());
            return Err(error);
        }
    };
    let mut cancellation = Cancellation {
        owner: owner.clone(),
        pool: pool.clone(),
        engine: engine.clone(),
        session: params.worker_session_id.clone(),
        claim: claim.clone(),
        armed: true,
    };
    let mut stdout = Vec::new();
    let mut lines = BufReader::new(EngineStdio::new(engine.clone(), id.clone())).lines();
    let mut released = false;
    let read = async {
        execution::release::send(&pool, engine, &params.worker_session_id, &claim, &payload)
            .await?;
        released = true;
        engine.close_stdin(&id).await.map_err(execution::conflict)?;
        let mut ticks = tokio::time::interval(Duration::from_millis(10));
        loop {
            tokio::select! {
                line = lines.next_line() => {
                    let Some(line) = line? else { break };
                    stdout.extend_from_slice(line.as_bytes()); stdout.push(b'\n');
                    if serde_json::from_str::<Value>(&line).ok().is_some_and(|value| value["type"] == "result") {
                        // Keep the exit-status verdict: give a result-producing CLI
                        // the same bounded opportunity to exit as an ordinary turn.
                        let _ = tokio::time::timeout(Duration::from_secs(5), async {
                            loop {
                                if matches!(engine.output(&id, None).await, Ok(ExecutionReply::Observation {
                                    state: ExecutionState::Exited { .. } | ExecutionState::Drained { .. } | ExecutionState::NoChild { .. } | ExecutionState::Revoked, .. })) { break }
                                tokio::time::sleep(Duration::from_millis(10)).await;
                            }
                        }).await;
                        break
                    }
                }
                _ = ticks.tick() => {
                    if matches!(engine.output(&id, None).await, Ok(ExecutionReply::Observation {
                        state: ExecutionState::Exited { .. } | ExecutionState::Drained { .. } | ExecutionState::NoChild { .. } | ExecutionState::Revoked, .. })) { break }
                }
            }
        }
        Ok::<_, CalmError>(())
    };
    let result = tokio::time::timeout(CHECK_TIMEOUT, read).await;
    let (reason, failure) = match result {
        Ok(Ok(())) => (StopReason::Finished, None),
        Ok(Err(error)) => (
            if released {
                StopReason::OutputGap
            } else {
                StopReason::ReleaseRefused
            },
            Some(error),
        ),
        Err(_) => (
            StopReason::Interrupted,
            Some(CalmError::ServiceUnavailable(
                "Claude did not finish checking the edit in time; nothing was changed".into(),
            )),
        ),
    };
    owner
        .stop(
            &pool,
            Some(engine),
            &params.worker_session_id,
            &claim,
            reason,
        )
        .await?;
    tokio::time::timeout(super::session::SETTLE_AFTER_STOP, engine.wait_settled(&id))
        .await
        .map_err(execution::conflict)?
        .map_err(execution::conflict)?;
    let settled = execution::settlement(&pool, &id)
        .await?
        .ok_or_else(|| execution::conflict("not settled"))?;
    let mut failure = failure;
    if failure.is_none() {
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    stdout.extend_from_slice(line.as_bytes());
                    stdout.push(b'\n');
                }
                Ok(None) => break,
                Err(error) => {
                    failure = Some(error.into());
                    break;
                }
            }
        }
    }
    let diagnostic = stderr::diagnostic(&dir);
    tracing::debug!(execution_id=%id.as_str(), capture=?diagnostic.capture, "rewind stderr capture");
    let prepared = if let Some(error) = failure {
        EditPreparation::Failed { error }
    } else {
        verdict(
            &stdout,
            &diagnostic.bytes,
            &settled,
            NativeEditPlan {
                target: NativeEditTarget::TranscriptCut {
                    resume_at: cut.at,
                    drops_turn: cut.drops_turn,
                },
            },
        )
    };
    owner
        .finish(
            &pool,
            Some(engine),
            &params.worker_session_id,
            &claim,
            Some(KnownEnd::RewindCheck),
        )
        .await?;
    cancellation.armed = false;
    drop(guard);
    Ok(prepared)
}

#[cfg(feature = "fixtures")]
impl super::session::ClaudePlannerSession {
    pub async fn prepare_edit_for_test(&self, thread: &str) -> Result<EditPreparation<CalmError>> {
        self.prepare_edit(
            thread,
            NativeEditTarget::TranscriptCut {
                resume_at: Uuid::from_u128(1),
                drops_turn: Uuid::from_u128(2),
            },
        )
        .await
    }
}

struct Cancellation {
    owner: execution::ProviderExecution,
    pool: crate::db::sqlite::SqliteDb,
    engine: crate::execution_boot::ExecutionEngine,
    session: String,
    claim: execution::Claim,
    armed: bool,
}
impl Drop for Cancellation {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let owner = self.owner.clone();
        let pool = self.pool.clone();
        let engine = self.engine.clone();
        let session = self.session.clone();
        let claim = self.claim.clone();
        tokio::spawn(async move {
            if let Err(error) = owner
                .stop(
                    &pool,
                    Some(&engine),
                    &session,
                    &claim,
                    StopReason::DriverLost,
                )
                .await
            {
                tracing::warn!(%error, "rewind cancellation Stop failed; durable sweep will retry");
            }
        });
    }
}

/// The sole rewind intent constructor. Per-execution aliases survive old tombstones.
async fn intent(
    params: &ClaudePlannerSessionParams,
    id: &ExecutionId,
    dir: &Path,
    binary: &Path,
    args: Vec<OsString>,
    env: Vec<(String, OsString)>,
) -> Result<(ExecutionIntent, Vec<u8>)> {
    let text = |value: OsString| {
        value
            .into_string()
            .map_err(|_| CalmError::BadRequest("provider arguments require UTF-8".into()))
    };
    use crate::execution_environment::{PROXY_KEYS, proxy_value_is_secret};
    use crate::operation::stdin_payload::{KeySource, PayloadKeys};
    let mut secrets = std::collections::HashMap::new();
    // Command::envs resolves duplicate names with the last value winning.
    // Resolve that final environment before classifying either side of the payload.
    let env = env
        .into_iter()
        .filter(|(key, _)| key != super::stop::MARKER_KEY)
        .collect::<std::collections::BTreeMap<_, _>>()
        .into_iter()
        .filter_map(|(key, value)| {
            if PROXY_KEYS.contains(&key.as_str()) && proxy_value_is_secret(&key, &value) {
                secrets.insert(key, value);
                None
            } else {
                Some((key, value))
            }
        })
        .collect::<Vec<_>>();
    // Only classified secret keys belong in the read prefix: non-secret proxy
    // values must retain their environment values rather than be unset.
    let present = PROXY_KEYS
        .iter()
        .copied()
        .filter(|key| secrets.contains_key(*key))
        .collect::<Vec<_>>();
    let keys = PayloadKeys::new(&present, KeySource::Kernel)?;
    let payload = keys
        .render(|key| secrets.get(key).cloned())
        .map_err(|_| CalmError::BadRequest("invalid Planner release payload".into()))?;
    let root = tokio::fs::canonicalize(&params.cwd).await?;
    let mut wrapper_args = vec![
        "-c".into(),
        stderr::wrapper(&keys.read_prefix()),
        "neige-rewind".into(),
        text(dir.as_os_str().to_owned())?,
        text(root.as_os_str().to_owned())?,
        text(binary.as_os_str().to_owned())?,
    ];
    wrapper_args.extend(args.into_iter().map(text).collect::<Result<Vec<_>>>()?);
    let environment = env
        .into_iter()
        .map(|(key, value)| Ok((key, text(value)?)))
        .collect::<Result<Vec<_>>>()?;
    Ok((
        ExecutionIntent {
            metadata: ExecutionMetadata {
                consumer: super::execution::CONSUMER.into(),
                id: id.clone(),
                alias: ProcessAlias(execution::sweep::alias(
                    &params.worker_session_id,
                    id.as_str(),
                )),
                owner: OperationRef(format!("worker-session:{}", params.worker_session_id)),
                track: TrackId(params.track_id.clone()),
                source: PrivateSource {
                    root,
                    cwd: workspace::identity::WorkspaceCwd::root(),
                    registration: ObjectRegistrationId::fresh()?,
                    nonce: TransferNonce::fresh()?,
                    directory: ExternalDirectoryId::fresh()?,
                    incarnation: ExternalDirectoryIncarnationId::fresh()?,
                    authority: ExecutionAuthority {
                        grant_id: ExecutionGrantId::fresh()?,
                        authorization_binding: AuthorizationBindingId::fresh()?,
                        authorization_version: 1,
                    },
                },
                program: ExecutionSpecification {
                    program: "/bin/sh".into(),
                    args: wrapper_args,
                    environment,
                    io_mode: calm_session::control::IoMode::Pipe,
                    replay_bytes: 4 * 1024 * 1024,
                },
                specification: SpecificationId::fresh()?,
                ticket_id: LaunchTicketId::fresh()?,
            },
            secrets: ExecutionSecrets {
                ticket: CapabilityToken::fresh()?,
                tokens: ExecutionControlTokens::fresh()?,
            },
        },
        payload,
    ))
}

fn verdict(
    stdout: &[u8],
    stderr: &[u8],
    settlement: &Settlement,
    plan: NativeEditPlan,
) -> EditPreparation<CalmError> {
    match settlement {
        Settlement::Journal(ExecutionState::Drained { status: Some(0) }) => {
            EditPreparation::Prepared { plan }
        }
        Settlement::Journal(ExecutionState::Drained { status }) => EditPreparation::Failed {
            error: CalmError::Conflict(format!(
                "Claude refused to remove this turn: {}",
                refusal_reason(stdout, stderr, *status)
            )),
        },
        _ => EditPreparation::Failed {
            error: execution::conflict(format!("rewind did not run: {settlement:?}")),
        },
    }
}

/// The `errors` of the CLI's result record, else its last stderr line, else the exit status.
fn refusal_reason(stdout: &[u8], stderr: &[u8], status: Option<i32>) -> String {
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
        .unwrap_or_else(|| format!("claude exited ({status:?})"));
    reason.trim().chars().take(MAX_REASON_CHARS).collect()
}

#[cfg(test)]
mod mapping_tests {
    use super::*;
    #[test]
    fn m1_mapping_edit_dry_run_accept_refuse_and_failure_table() {
        let plan = || NativeEditPlan {
            target: NativeEditTarget::TranscriptCut {
                resume_at: Uuid::from_u128(1),
                drops_turn: Uuid::from_u128(2),
            },
        };
        assert!(matches!(
            verdict(
                b"",
                b"",
                &Settlement::Journal(ExecutionState::Drained { status: Some(0) }),
                plan()
            ),
            EditPreparation::Prepared { .. }
        ));
        let refusal = verdict(
            br#"{"type":"result","errors":["native reason"]}"#,
            b"",
            &Settlement::Journal(ExecutionState::Drained { status: Some(1) }),
            plan(),
        );
        assert!(matches!(refusal, EditPreparation::Failed {
            error: CalmError::Conflict(message)
        } if message == "Claude refused to remove this turn: native reason"));
        assert!(matches!(
            verdict(
                b"",
                b"",
                &Settlement::Journal(ExecutionState::Revoked),
                plan()
            ),
            EditPreparation::Failed { .. }
        ));
        assert!(
            matches!(native_edit_target(None, None), Err(CalmError::Conflict(message)) if message == "the first message of a Claude conversation cannot be edited: Claude has nothing before it to go back to")
        );
        assert!(
            matches!(native_edit_target(Some(&[]), None), Err(CalmError::Conflict(message)) if message == "the previous reply did not record where it ended, so this turn cannot be removed")
        );
    }
}
