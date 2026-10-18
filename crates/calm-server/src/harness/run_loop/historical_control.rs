//! Historical public controls use the original loop and snapshot writer.
//! Admission binds a live issuer to that exact current runtime; no credential
//! enters a stored operation and no policy is copied from conversation_inputs.
use super::*;
pub(super) mod stop;
use crate::conversation_inputs::HistoricalHumanSource;

#[derive(Clone)]
pub(super) enum Admission {
    Human(HistoricalHumanSource),
    #[cfg(any(test, feature = "fixtures"))]
    Fixture,
}

impl Admission {
    pub(super) async fn require(&self, inner: &Arc<Inner>) -> Result<()> {
        let (admission, card, worker) = (
            self.clone(),
            inner.card_id.to_string(),
            inner.worker_session_id.clone(),
        );
        write_in_tx_typed(inner.repo.as_ref(), move |tx| {
            Box::pin(async move { admission.require_tx(tx, &card, &worker).await })
        })
        .await
    }

    pub(super) async fn require_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        card: &str,
        worker: &str,
    ) -> Result<()> {
        match self {
            Self::Human(source) => source.require_card_tx(tx, card).await?,
            #[cfg(any(test, feature = "fixtures"))]
            Self::Fixture => return Ok(()),
        }
        let runtime = crate::db::sqlite::session_projection_active_for_card_tx(tx, card).await?;
        if !runtime.is_some_and(|runtime| runtime.id == worker) {
            return Err(CalmError::Conflict(
                "historical control runtime was superseded".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(super) enum SnapshotWrite {
    Kernel(Option<track_vcs::CommitHash>),
    Send(DurableSendBinding),
    Mutation(Admission),
    Stop(stop::PreparedStop),
}

impl SnapshotWrite {
    pub(super) async fn require(&self, inner: &Arc<Inner>) -> Result<()> {
        if matches!(
            self,
            Self::Kernel(_) | Self::Send(DurableSendBinding { source: None, .. })
        ) {
            return Ok(());
        }
        let (write, card, worker) = (
            self.clone(),
            inner.card_id.to_string(),
            inner.worker_session_id.clone(),
        );
        write_in_tx_typed(inner.repo.as_ref(), move |tx| {
            Box::pin(async move { write.require_tx(tx, &card, &worker).await })
        })
        .await
    }

    pub(super) async fn require_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        card: &str,
        worker: &str,
    ) -> Result<()> {
        match self {
            Self::Kernel(_) => Ok(()),
            Self::Send(send) => {
                if let Some(source) = &send.source {
                    source.require_card_tx(tx, card).await?;
                }
                // The original Send writer owns runtime CAS and its typed
                // superseded refusal. A control must not pre-empt that owner.
                Ok(())
            }
            Self::Mutation(admission) => admission.require_tx(tx, card, worker).await,
            Self::Stop(plan) => plan.require_tx(tx).await,
        }
    }
}

pub(super) async fn persist_mutation(inner: &Arc<Inner>, admission: &Admission) -> Result<()> {
    if persist_snapshot_inner(inner, SnapshotWrite::Mutation(admission.clone())).await? {
        Ok(())
    } else {
        Err(CalmError::Conflict(
            "historical control runtime no longer accepts writes".into(),
        ))
    }
}

impl PlannerHarness {
    pub(crate) async fn mutate_pending_entry_admitted(
        &self,
        mutation: QueueMutation,
        actor: ActorId,
        source: HistoricalHumanSource,
    ) -> Result<MutationResult> {
        self.mutate_pending_entry_with_admission(mutation, actor, Admission::Human(source))
            .await
    }

    /// Direct data fixtures do not issue a public credential. Production ingress
    /// has only the required-source method above.
    #[cfg(any(test, feature = "fixtures"))]
    pub async fn mutate_pending_entry(
        &self,
        mutation: QueueMutation,
        actor: ActorId,
    ) -> Result<MutationResult> {
        self.mutate_pending_entry_with_admission(mutation, actor, Admission::Fixture)
            .await
    }

    async fn mutate_pending_entry_with_admission(
        &self,
        mutation: QueueMutation,
        actor: ActorId,
        admission: Admission,
    ) -> Result<MutationResult> {
        self.inner.historical_queue()?;
        admission.require(&self.inner).await?;
        if self.inner.shutting_down.load(Ordering::SeqCst) {
            return Err(CalmError::Conflict(
                "planner harness is shutting down; refusing queue mutation".into(),
            ));
        }
        match &self.inner.observations {
            ObservationIngress::Running(sender) => {
                let (applied, answer) = oneshot::channel();
                sender
                    .try_send(HarnessObservationCommand::Mutate {
                        admission,
                        mutation,
                        actor,
                        applied,
                    })
                    .map_err(map_observation_send_error)?;
                answer.await.map_err(|_| {
                    CalmError::Conflict(
                        "planner harness runtime shut down before the queue mutation was applied"
                            .into(),
                    )
                })?
            }
            #[cfg(feature = "fixtures")]
            ObservationIngress::Unstarted(_) => {
                handle_queue_mutation(&self.inner, &mutation, &actor, &admission).await
            }
        }
    }

    pub(crate) async fn steer_pending_entry_admitted(
        &self,
        entry_id: QueueEntryId,
        if_entry_rev: u32,
        actor: ActorId,
        source: HistoricalHumanSource,
    ) -> Result<SteerResult> {
        self.steer_pending_entry_with_admission(
            entry_id,
            if_entry_rev,
            actor,
            Admission::Human(source),
        )
        .await
    }

    #[cfg(any(test, feature = "fixtures"))]
    pub async fn steer_pending_entry(
        &self,
        entry_id: QueueEntryId,
        if_entry_rev: u32,
        actor: ActorId,
    ) -> Result<SteerResult> {
        self.steer_pending_entry_with_admission(entry_id, if_entry_rev, actor, Admission::Fixture)
            .await
    }

    async fn steer_pending_entry_with_admission(
        &self,
        entry_id: QueueEntryId,
        if_entry_rev: u32,
        actor: ActorId,
        admission: Admission,
    ) -> Result<SteerResult> {
        self.inner.historical_queue()?;
        admission.require(&self.inner).await?;
        if self.inner.shutting_down.load(Ordering::SeqCst) {
            return Err(CalmError::Conflict(
                "planner harness is shutting down; refusing to steer".into(),
            ));
        }
        match &self.inner.observations {
            ObservationIngress::Running(sender) => {
                let (applied, answer) = oneshot::channel();
                sender
                    .try_send(HarnessObservationCommand::Steer {
                        admission,
                        entry_id,
                        if_entry_rev,
                        actor,
                        applied,
                    })
                    .map_err(map_observation_send_error)?;
                answer.await.map_err(|_| {
                    CalmError::Conflict(
                        "planner harness runtime shut down before the steer was answered".into(),
                    )
                })?
            }
            #[cfg(feature = "fixtures")]
            ObservationIngress::Unstarted(_) => {
                handle_steer(&self.inner, &entry_id, if_entry_rev, &actor, &admission).await
            }
        }
    }
}
