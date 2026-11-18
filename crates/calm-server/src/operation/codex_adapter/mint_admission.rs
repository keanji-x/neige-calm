//! First native mint is admitted by its original operation and prepared session.
use super::*;
use crate::harness::native_admission::NativePromptAdmission;
use crate::operation::task_launch::TaskLaunch;

pub(super) fn admission<'a>(
    ctx: &'a SpawnCtx,
    operation: &'a Operation,
    card: &'a str,
    worker_session: &'a str,
    task: Option<&'a TaskLaunch>,
) -> NativePromptAdmission<'a> {
    // Kernel starts carry a live database claim, not an HTTP session issuer.
    // The async check below revalidates that owner at each actual ready boundary.
    NativePromptAdmission::new(
        || Ok(()),
        move || {
            let operation = operation.clone();
            let card = card.to_owned();
            let worker_session = worker_session.to_owned();
            let task = task.cloned();
            Box::pin(async move {
                write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
                    Box::pin(async move {
                        crate::operation::repo_sqlite::require_live_claim_tx(tx, &operation)
                            .await?;
                        let current = session_projection_active_for_card_tx(tx, &card)
                            .await?
                            .ok_or_else(|| {
                                CalmError::Conflict("native start owner ended".into())
                            })?;
                        if current.id != worker_session
                            || current.agent_provider != Some(AgentProvider::Codex)
                            || TxOutput::non_empty_string(current.thread_id.as_deref()).is_some()
                        {
                            return Err(CalmError::Conflict(
                                "native start no longer owns its prepared session".into(),
                            ));
                        }
                        if let Some(task) = task {
                            TaskLaunch::authorize_tx(tx, task.task_id()).await?;
                        }
                        Ok(())
                    })
                })
                .await
            })
        },
    )
}
