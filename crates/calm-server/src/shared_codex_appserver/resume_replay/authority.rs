//! Replay reuses its actual lifecycle and cold owner at every new private effect.
use super::*;

impl SharedCodexAppServer {
    pub(super) fn require_replay_locator(&self, thread_id: &str, card_id: &str) -> Result<()> {
        if self.cached_card_for_thread(thread_id).as_deref() != Some(card_id)
            || self.thread_seals.is_sealed(thread_id)
        {
            return Err(CalmError::Conflict("Codex replay locator changed".into()));
        }
        Ok(())
    }

    pub(super) async fn replay_authority(
        &self,
        thread_id: &str,
        card_id: &str,
    ) -> Result<Option<ColdAuthority>> {
        let thread_id = thread_id.to_owned();
        let card_id = card_id.to_owned();
        write_in_tx_typed(self.repo.as_ref(), move |tx| {
            Box::pin(async move {
                crate::conversation_inputs::require_historical_storage_tx(tx, &card_id).await?;
                let shape = crate::db::sqlite::card_execution_shape_tx(tx, &card_id).await?;
                cold_resume_authority_tx(tx, &thread_id, &card_id, &shape).await
            })
        })
        .await
    }

    pub(super) async fn revalidate_replay_owner(
        &self,
        client: &Arc<CodexAppServer>,
        thread_id: &str,
        card_id: &str,
        expected: &ColdAuthority,
    ) -> Result<()> {
        if self.replay_authority(thread_id, card_id).await?.as_ref() != Some(expected) {
            return Err(CalmError::Conflict("Codex replay authority changed".into()));
        }
        // The caller already holds transition/replay locks. Inspect their owner
        // without reacquiring either lock or entering connection recovery.
        let current = self
            .with_replay_plan(client, |plan| {
                !plan.excluded.contains(thread_id)
                    && plan
                        .configs
                        .get(&(thread_id.to_owned(), card_id.to_owned()))
                        .is_none_or(|prepared| &prepared.authority == expected)
            })
            .await?;
        if !current {
            return Err(CalmError::Conflict(
                "Codex replay preparation changed".into(),
            ));
        }
        Ok(())
    }
}

pub(super) async fn cold_resume_authority_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    thread_id: &str,
    card_id: &str,
    shape: &crate::db::sqlite::CardExecutionShape,
) -> Result<Option<ColdAuthority>> {
    let current = session_projection_active_for_card_tx(tx, card_id).await?;
    let Some(runtime) = current else {
        let failed: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM cards c JOIN worker_sessions ws ON ws.id=c.session_id WHERE c.id=?1 \
             AND ws.state='failed' AND json_extract(ws.handle_state_json,'$.mode')=?2)",
        )
        .bind(card_id)
        .bind(calm_types::harness::HARNESS_MODE)
        .fetch_one(&mut **tx)
        .await?;
        return Ok((!failed).then_some(ColdAuthority::NoMcp(None)));
    };
    let stamp = ColdRuntime {
        id: runtime.id.clone(),
        thread: runtime.thread_id.clone(),
    };
    let binding = crate::harness::profile::PlannerBinding::from_shape(
        &shape.kind,
        shape.role,
        &shape.payload,
    );
    let no_mcp = match &binding {
        Some(binding) => {
            binding.profile.mcp_role().is_none() || binding.provider != AgentProvider::Codex
        }
        None => shape.role == CardRole::Planner,
    };
    Ok(Some(
        if runtime.thread_id.as_deref() != Some(thread_id) || no_mcp {
            ColdAuthority::NoMcp(Some(stamp))
        } else {
            ColdAuthority::Mcp {
                runtime: stamp,
                role: shape.role,
            }
        },
    ))
}
