//! Claude owns provider completion and per-execution diagnostics.
use crate::{
    error::{CalmError, Result},
    provider_execution::*,
};
use async_trait::async_trait;
use std::{path::PathBuf, sync::Arc};
pub const CONSUMER: &str = "claude-planner";
pub(crate) struct Owner {
    root: PathBuf,
    repo: std::sync::OnceLock<Arc<dyn crate::db::Repo>>,
    registry: std::sync::OnceLock<crate::harness::registry::WeakHarnessRegistry>,
}
impl std::fmt::Debug for Owner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaudeExecutionOwner")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}
impl Owner {
    pub(crate) fn repo(&self) -> Result<&Arc<dyn crate::db::Repo>> {
        self.repo
            .get()
            .ok_or_else(|| conflict("Claude completion repository unavailable"))
    }
    pub(crate) fn bind(&self, repo: Arc<dyn crate::db::Repo>) {
        let _ = self.repo.set(repo);
    }
    pub(crate) fn bind_registry(&self, registry: crate::harness::HarnessRegistry) {
        let _ = self.registry.set(registry.downgrade());
    }
}
pub(crate) fn owner(root: PathBuf) -> (ProviderExecution, Arc<Owner>) {
    let owner = Arc::new(Owner {
        root,
        repo: Default::default(),
        registry: Default::default(),
    });
    (
        ProviderExecution {
            owner: owner.clone(),
            drivers: Default::default(),
        },
        owner,
    )
}

/// A known protocol result is consumed by the same owner entry point as recovery.
pub struct End {
    pub(crate) shared: Arc<super::session::Shared>,
    pub(crate) slot: Arc<super::turn_slot::TurnSlot>,
    pub(crate) translator: provider::claude::translate::TurnTranslator,
    pub(crate) instructions: super::spawn::InstructionsFile,
    pub(crate) event: super::driver::TerminalEvent,
    pub(crate) cause: Option<super::turn_slot::TerminalCause>,
    pub(crate) qualified: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) thread: String,
    pub(crate) turn: String,
    pub(crate) approvals: super::approvals::Approvals,
}
#[async_trait]
impl ProviderExecutionOwner for Owner {
    fn consumer(&self) -> &'static str {
        CONSUMER
    }
    fn alias_prefix(&self) -> crate::provider_execution::AliasPrefix {
        crate::provider_execution::AliasPrefix::planner()
    }
    fn alias(&self, session: &str, id: &str) -> String {
        self.alias_prefix().alias(session, id)
    }
    async fn complete(
        &self,
        session: &str,
        record: &Record,
        evidence: CompletionEvidence,
        known: Option<KnownEnd>,
    ) -> Result<()> {
        let id = execution_id(record)?;
        match &record.purpose {
            Purpose::RewindCheck => match known {
                None | Some(KnownEnd::RewindCheck) => {}
                Some(
                    KnownEnd::ClaudeTurn(_)
                    | KnownEnd::ClaudeRefused
                    | KnownEnd::AcpTurn(_)
                    | KnownEnd::AcpRefused
                    | KnownEnd::AcpCompaction,
                ) => {
                    return Err(conflict("wrong Claude completion purpose"));
                }
            },
            Purpose::Turn { turn_id } | Purpose::Compaction { turn_id } => {
                let terminal = foreground_settled(&evidence);
                match known {
                    Some(KnownEnd::ClaudeTurn(mut end)) => {
                        if end.turn != *turn_id || end.shared.params.worker_session_id != session {
                            return Err(conflict("Claude completion identity changed"));
                        }
                        let event = match end.event {
                            super::driver::TerminalEvent::Exit { .. } => {
                                let diagnostic =
                                    stderr::diagnostic(&stderr::directory(&self.root, id.as_str()));
                                let text = String::from_utf8_lossy(&diagnostic.bytes);
                                let tail = text
                                    .lines()
                                    .rev()
                                    .find(|line| !line.trim().is_empty())
                                    .unwrap_or("");
                                let evidence = match &evidence {
                                    CompletionEvidence::Settled(settlement) => {
                                        format!("{settlement:?}")
                                    }
                                    CompletionEvidence::Unconfirmed => {
                                        "unconfirmed settlement".into()
                                    }
                                };
                                let prefix = if terminal {
                                    "claude exited"
                                } else {
                                    "claude output ended"
                                };
                                super::driver::TerminalEvent::Exit {
                                    detail: format!(
                                        "{prefix} ({evidence}): {tail} ({:?})",
                                        diagnostic.capture
                                    ),
                                }
                            }
                            event => event,
                        };
                        let mut outcome = super::driver::decide(end.cause.as_ref(), &event);
                        if !terminal
                            && matches!(
                                outcome,
                                provider::claude::translate::TurnOutcome::Completed
                            )
                        {
                            outcome = provider::claude::translate::TurnOutcome::Interrupted;
                        }
                        let qualified = terminal
                            && (matches!(record.purpose, Purpose::Turn { .. })
                                || matches!(
                                    outcome,
                                    provider::claude::translate::TurnOutcome::Completed
                                ));
                        let completed = end
                            .translator
                            .turn_completed(&outcome, crate::model::now_ms());
                        if matches!(record.purpose, Purpose::Turn { .. }) {
                            let provider::events::PlannerEventKind::TurnCompleted { turn } =
                                &completed.kind
                            else {
                                unreachable!()
                            };
                            super::outcome_record::record_outcome(
                                &end.shared,
                                &end.thread,
                                turn_id,
                                turn,
                                if terminal {
                                    crate::harness::turn_outcome::Confirmation::ProviderTerminal
                                } else {
                                    crate::harness::turn_outcome::Confirmation::Unconfirmed
                                },
                            )
                            .await?;
                        }
                        end.instructions.remove()?;
                        stderr::cleanup(&self.root, id.as_str());
                        let closes = if matches!(record.purpose, Purpose::Turn { .. }) {
                            end.translator.close_open(crate::model::now_ms())
                        } else {
                            vec![]
                        };
                        #[cfg(feature = "fixtures")]
                        {
                            let pause = end
                                .shared
                                .hooks
                                .lock()
                                .expect("hooks")
                                .before_turn_completed
                                .clone();
                            if let Some(pause) = pause {
                                pause.entered.notify_one();
                                pause.release.notified().await;
                            }
                        }
                        drop(end.approvals);
                        end.shared.finish_turn(
                            turn_id,
                            &end.slot,
                            closes.into_iter().chain([completed]),
                        );
                        end.slot.settled.send_replace(true);
                        end.qualified
                            .store(qualified, std::sync::atomic::Ordering::Release);
                        return Ok(());
                    }
                    Some(KnownEnd::ClaudeRefused) => {}
                    Some(
                        KnownEnd::RewindCheck
                        | KnownEnd::AcpRefused
                        | KnownEnd::AcpTurn(_)
                        | KnownEnd::AcpCompaction,
                    ) => {
                        return Err(conflict("wrong Claude completion purpose"));
                    }
                    None => {
                        let repo = self
                            .repo
                            .get()
                            .ok_or_else(|| conflict("Claude completion repository unavailable"))?;
                        let row = repo
                            .session_get(&calm_types::worker::WorkerSessionId(session.into()))
                            .await?
                            .ok_or_else(|| conflict("Claude completion session missing"))?;
                        let runtime = repo
                            .session_projection_by_id(session)
                            .await?
                            .ok_or_else(|| conflict("Claude completion runtime missing"))?;
                        let thread = crate::harness::effective_runtime_thread_id(&runtime)
                            .ok_or_else(|| conflict("Claude completion thread missing"))?;
                        let status = if matches!(record.purpose, Purpose::Compaction { .. }) {
                            "failed"
                        } else {
                            "interrupted"
                        };
                        let mut turn = serde_json::json!({"id": turn_id, "status": status, "error": {"message": "Claude execution driver was lost"}});
                        if matches!(record.purpose, Purpose::Turn { .. }) {
                            crate::harness::turn_outcome::record(
                                repo.as_ref(),
                                session,
                                row.card_id
                                    .as_ref()
                                    .ok_or_else(|| conflict("Claude completion card missing"))?
                                    .as_str(),
                                row.track_id.as_str(),
                                &thread,
                                turn_id,
                                &turn,
                                crate::harness::turn_outcome::Confirmation::Unconfirmed,
                            )
                            .await?;
                        }
                        if matches!(record.purpose, Purpose::Turn { .. }) {
                            let card = row
                                .card_id
                                .as_ref()
                                .ok_or_else(|| conflict("Claude completion card missing"))?;
                            if let Some(existing) = repo
                                .transcript_rows_of_thread(card.as_str(), &thread)
                                .await?
                                .into_iter()
                                .find(|row| {
                                    row.method == "turn/completed"
                                        && row.turn_id.as_deref() == Some(turn_id)
                                })
                            {
                                turn = serde_json::from_str(&existing.params)?;
                            }
                        }
                        super::spawn::InstructionsFile::remove_for_turn(
                            &self.root, session, turn_id,
                        )?;
                        if let Some(harness) = self
                            .registry
                            .get()
                            .and_then(|registry| registry.get(&session.to_owned()))
                        {
                            harness
                                .complete_provider_execution(&id, turn_id, &record.purpose, turn)
                                .await?;
                        }
                    }
                }
            }
        }
        stderr::cleanup(&self.root, id.as_str());
        Ok(())
    }
}

use super::session::ClaudePlannerSessionParams;
use crate::provider_execution as execution;
use std::{ffi::OsString, path::Path};
use workspace_runtime::execution::*;
/// Shared per-execution intent: credentials only enter the Held release payload. Per-execution aliases survive old tombstones.
pub(super) async fn intent(
    params: &ClaudePlannerSessionParams,
    id: &ExecutionId,
    dir: &Path,
    binary: &Path,
    args: Vec<OsString>,
    env: Vec<(String, OsString)>,
    token: Option<String>,
) -> Result<(ExecutionIntent, Vec<u8>)> {
    use crate::execution_environment::{PROXY_KEYS, proxy_value_is_secret};
    use crate::operation::stdin_payload::{KeySource, PayloadKeys};
    let mut secrets = std::collections::HashMap::new();
    if let Some(token) = token {
        secrets.insert("NEIGE_MCP_TOKEN".to_string(), token.into());
    }
    // Command::envs resolves duplicate names with the last value winning.
    // Resolve that final environment before classifying either side of the payload.
    let env = env
        .into_iter()
        .filter(|(key, _)| key != "NEIGE_MCP_TOKEN")
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
        .chain(["NEIGE_MCP_TOKEN"])
        .filter(|key| secrets.contains_key(*key))
        .collect::<Vec<_>>();
    let keys = PayloadKeys::new(&present, KeySource::Kernel)?;
    let payload = keys
        .render(|key| secrets.get(key).cloned())
        .map_err(|_| CalmError::BadRequest("invalid Planner release payload".into()))?;
    execution::launch::intent(
        params.host.provider_execution.owner.as_ref(),
        &params.worker_session_id,
        &params.track_id,
        &params.cwd,
        id,
        dir,
        binary,
        args,
        env,
        &keys.read_prefix(),
        payload,
    )
    .await
}

// Claude runs as a foreground command. This consumer accepts platform-specific
// settlement without turning an original-group observation into a descendant proof.
fn foreground_settled(evidence: &CompletionEvidence) -> bool {
    matches!(
        evidence,
        CompletionEvidence::Settled(Settlement::Journal(
            ExecutionState::Drained { .. } | ExecutionState::OriginalGroupEmpty { .. }
        ))
    )
}

#[cfg(test)]
mod completion_tests {
    use super::*;

    #[test]
    fn native_macos_foreground_completion_is_terminal() {
        // Protocol success must not become Interrupted just because the host is Mac.
        for status in [Some(0), Some(1), None] {
            assert!(foreground_settled(&CompletionEvidence::Settled(
                Settlement::Journal(ExecutionState::OriginalGroupEmpty { status })
            )));
            assert!(foreground_settled(&CompletionEvidence::Settled(
                Settlement::Journal(ExecutionState::Drained { status })
            )));
        }
    }

    #[test]
    fn foreground_completion_requires_confirmed_settlement() {
        assert!(!foreground_settled(&CompletionEvidence::Unconfirmed));
        for state in [
            ExecutionState::Exited { status: Some(0) },
            ExecutionState::Unknown {
                reason: "lost ownership".into(),
            },
            ExecutionState::NoChild {
                reason: "not launched".into(),
            },
            ExecutionState::Revoked,
        ] {
            assert!(!foreground_settled(&CompletionEvidence::Settled(
                Settlement::Journal(state)
            )));
        }
    }
}
