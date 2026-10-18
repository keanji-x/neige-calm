//! Resource stages belong to the original handoff operation, using existing start/recovery owners.
use super::handoff_activation_sql as sql;
use super::handoff_coordinator::{
    CommittedIdentity, CoordinatorInput, Progress, ResourceResume, Resources,
};
use super::handoff_coordinator_adapter::{HandoffCoordinatorAdapter, continuation};
use super::planner_harness_start_adapter::{HarnessInstallation, HarnessProfile};
use super::{AppServerInteractOutcome, Operation, SpawnCtx, TxOutput};
use crate::error::Result;

impl HandoffCoordinatorAdapter {
    pub(super) async fn forward_resources(
        &self,
        ctx: &SpawnCtx,
        op: &Operation,
        output: &mut TxOutput,
        input: &CoordinatorInput,
        identity: CommittedIdentity,
        mut resources: Resources,
    ) -> Result<AppServerInteractOutcome> {
        if matches!(
            resources,
            Resources::NativeIntent | Resources::Unknown { .. }
        ) {
            // A sending intent is not an idempotency receipt, even after explicit retry.
            return Ok(AppServerInteractOutcome::AwaitingRetry);
        }
        let pool = ctx.operation_repo.sqlite_pool();
        if matches!(resources, Resources::Unprepared) {
            let briefing = super::handoff_activation_briefing::render(
                self.start.activation_repo(),
                &identity.plan(input),
            )
            .await?;
            let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
            let facts = sql::handoff_facts(&mut tx, input, &identity, &op.id, false).await?;
            sqlx::query(r#"
        INSERT INTO
        planner_handoff_resources(operation_id,session_id,retired_source_session_id,retired_target_session_id)
        VALUES(?1,?2,?3,?4)
        "#)
                .bind(&op.id).bind(&input.new_session_id).bind(&input.request.source_session_id).bind(&input.request.target_session_id).execute(&mut *tx).await?;
            let mut next = sql::prepare_snapshot(&mut tx, &facts, briefing).await?;
            next.result = serde_json::to_value(Progress::Committed {
                identity: identity.clone(),
                resources: Resources::Prepared,
            })?;
            super::checkpoint_app_server_interact_tx(&mut tx, op, continuation(), output, &next)
                .await?;
            tx.commit().await?;
            *output = next;
            resources = Resources::Prepared;
        }
        let resume = match resources {
            Resources::Prepared => ResourceResume::Prepared,
            Resources::Proven { thread_id } => ResourceResume::Proven { thread_id },
            Resources::Blocked { resume, .. } => resume,
            _ => return Err(super::handoff_cutover::conflict()),
        };
        let expected_thread = match &resume {
            ResourceResume::Prepared => None,
            ResourceResume::Proven { thread_id } => Some(thread_id.clone()),
        };
        if output.output_optional_string("codex_thread_id", "handoff")? != expected_thread {
            return Err(super::handoff_cutover::conflict());
        }
        let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
        let facts = sql::handoff_facts(&mut tx, input, &identity, &op.id, true).await?;
        facts.validate_output(output)?;
        tx.rollback().await?;
        let provider = &identity.configuration.conversation.provider;
        if let Err(error) = self.start.activation_readiness(provider).await {
            self.checkpoint(
                ctx,
                op,
                output,
                &Progress::Committed {
                    identity,
                    resources: Resources::Blocked {
                        reason: error.to_string(),
                        resume,
                    },
                },
            )
            .await?;
            return Ok(AppServerInteractOutcome::AwaitingRetry);
        }
        if expected_thread.is_none() {
            // Final authority validation and intent share the leased checkpoint transaction.
            let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
            sql::handoff_facts(&mut tx, input, &identity, &op.id, true)
                .await?
                .validate_output(output)?;
            let mut next = output.clone();
            next.result = serde_json::to_value(Progress::Committed {
                identity: identity.clone(),
                resources: Resources::NativeIntent,
            })?;
            super::checkpoint_app_server_interact_tx(&mut tx, op, continuation(), output, &next)
                .await?;
            tx.commit().await?;
            *output = next;
            let minted = self
                .start
                .mint_harness_resources(
                    HarnessProfile::Planner,
                    provider,
                    input.request.target_card_id.as_str(),
                    input.request.track_id.as_str(),
                    output.output_string("cwd", "handoff")?,
                    true,
                )
                .await;
            let (thread_id, token_hash) = match minted {
                Ok(result) => result,
                Err(error) => {
                    self.checkpoint(
                        ctx,
                        op,
                        output,
                        &Progress::Committed {
                            identity,
                            resources: Resources::Unknown {
                                reason: error.to_string(),
                            },
                        },
                    )
                    .await?;
                    return Ok(AppServerInteractOutcome::AwaitingRetry);
                }
            };
            let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
            let facts = sql::handoff_facts(&mut tx, input, &identity, &op.id, true).await?;
            let mut next = output.clone();
            sql::bind_resources_tx(
                &mut tx,
                &facts,
                op,
                &mut next,
                &thread_id,
                token_hash.as_deref(),
                &self.start,
                serde_json::to_value(Progress::Committed {
                    identity: identity.clone(),
                    resources: Resources::Proven {
                        thread_id: thread_id.clone(),
                    },
                })?,
            )
            .await?;
            sql::handoff_facts(&mut tx, input, &identity, &op.id, true)
                .await?
                .validate_output(&next)?;
            tx.commit().await?;
            *output = next;
        }
        // Recover the complete current session row; output.snapshot is never the recovery input.
        self.start
            .install_harness(output, ctx, HarnessInstallation::ActivationRecovery)
            .await?;
        let mut tx = crate::db::sqlite::begin_immediate_tx(&pool).await?;
        let facts = sql::handoff_facts(&mut tx, input, &identity, &op.id, true).await?;
        facts.validate_output(output)?;
        sql::validate_resources(&mut tx, &facts, &op.id).await?;
        let mut next = output.clone();
        next.result = serde_json::to_value(Progress::Activated { identity })?;
        super::checkpoint_app_server_interact_tx(&mut tx, op, continuation(), output, &next)
            .await?;
        let released:Option<bool>=sqlx::query_scalar("UPDATE planner_handoff_coordinators SET active=0 WHERE operation_id=?1 AND active=1 RETURNING active")
            .bind(&op.id).fetch_optional(&mut *tx).await?;
        if released != Some(false) {
            return Err(super::handoff_cutover::conflict());
        }
        tx.commit().await?;
        *output = next;
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point(
            "handoff-activation-committed",
            input.request.target_card_id.as_str(),
        )
        .await;
        Ok(AppServerInteractOutcome::NotApplicable)
    }
}
