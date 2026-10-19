//! The actual Dispatcher qualifies persisted event inputs. Its declaration is
//! installed only after the original production recovery boundary completes.
use super::*;
use crate::auth::LegacyPrivateExecution;
use crate::conversation_inputs::InputSource;
use crate::db::sqlite::conversation::{self, CurrentInputCarrier};
use crate::db::write_in_tx_typed;
use crate::error::{CalmError, Result};
use calm_truth::db::sqlite::event_inputs::{self, EventInputFact};
use calm_types::conversation::{
    AcceptedInputRef, PendingInput, PrivateHistoryPayload, SubmissionRenderInput,
    SubmissionRenderSegment, SystemObservation,
};
use sqlx::{Sqlite, Transaction};
use std::sync::Weak;

#[derive(Clone)]
pub(super) struct Producer {
    dispatcher: Weak<Dispatcher>,
    execution: LegacyPrivateExecution,
}
impl Producer {
    fn current_owner(&self) -> Result<String> {
        let dispatcher = self.dispatcher.upgrade().ok_or(CalmError::Unauthorized)?;
        if dispatcher.semaphore.is_closed() {
            return Err(CalmError::Unauthorized);
        }
        self.execution.current_owner()
    }
}

#[derive(Clone)]
struct EventBinding {
    producer: Producer,
    database: Arc<String>,
    track: TrackId,
    card: String,
    worker: String,
    event: EventInputFact,
}
impl EventBinding {
    async fn authorize_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        carrier: &CurrentInputCarrier,
    ) -> Result<String> {
        self.producer.current_owner()?;
        if self.card != carrier.card_id() || self.worker != carrier.worker_session_id() {
            return Err(CalmError::Forbidden(
                "event source belongs to another input carrier".into(),
            ));
        }
        let database: String =
            sqlx::query_scalar("SELECT id FROM database_identity WHERE singleton=1")
                .fetch_one(&mut **tx)
                .await?;
        let (track, area): (String,String) = sqlx::query_as("SELECT c.track_id,t.area_id FROM cards c JOIN tracks t ON t.id=c.track_id WHERE c.id=?1")
            .bind(&self.card).fetch_one(&mut **tx).await?;
        let actual = event_inputs::event_input_tx(tx, self.event.envelope.id).await?;
        if database != *self.database
            || track != self.track.as_str()
            || actual.fingerprint != self.event.fingerprint
            || actual.envelope.scope.track_id() != Some(&self.track)
            || actual.envelope.scope.area_id().map(|id| id.as_str()) != Some(area.as_str())
        {
            return Err(CalmError::Forbidden(
                "event source facts or containment changed".into(),
            ));
        }
        let owner = crate::organization_access::require_legacy_event_execution_tx(
            tx,
            &area,
            &self.producer.execution,
        )
        .await?;
        self.producer.current_owner()?;
        Ok(owner)
    }
}

/// Nonserializable and private to this real producer. Neither Actor nor a stored
/// observation/envelope can construct an execution source in another caller.
#[derive(Clone)]
pub(crate) struct DispatcherInputSource {
    binding: EventBinding,
    reference: AcceptedInputRef,
    observation: SystemObservation,
}
impl DispatcherInputSource {
    pub(crate) fn require_repo(&self, repo: &dyn crate::db::RepoEventWrite) -> Result<()> {
        let dispatcher = self
            .binding
            .producer
            .dispatcher
            .upgrade()
            .ok_or(CalmError::Unauthorized)?;
        self.check_current()?;
        if !std::ptr::addr_eq(
            Arc::as_ptr(&dispatcher.inner.repo),
            repo as *const dyn crate::db::RepoEventWrite,
        ) {
            return Err(CalmError::Forbidden(
                "event source belongs to another installed repository context".into(),
            ));
        }
        Ok(())
    }
    pub(crate) fn check_current(&self) -> Result<String> {
        self.binding.producer.current_owner()
    }
    pub(crate) async fn authorize_resource_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        carrier: &CurrentInputCarrier,
    ) -> Result<String> {
        let owner = self.binding.authorize_tx(tx, carrier).await?;
        let expected = conversation::event_system_observation_id_tx(
            tx,
            carrier,
            &self.binding.event,
            &self.observation,
        )
        .await?;
        if self.reference
            != (AcceptedInputRef::System {
                observation_id: expected,
            })
        {
            return Err(CalmError::Forbidden(
                "accepted event/content binding changed; original input remains held".into(),
            ));
        }
        Ok(owner)
    }
    pub(crate) async fn authorizes_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        carrier: &CurrentInputCarrier,
        input: &SubmissionRenderInput,
    ) -> Result<bool> {
        self.authorize_resource_tx(tx, carrier).await?;
        Ok(input.input_ref() == self.reference
            && matches!(input,
            SubmissionRenderInput::System { payload: PrivateHistoryPayload::Present { value }, .. } if value == &self.observation))
    }
}

impl Dispatcher {
    /// One actual boot composition binds this exact public instance. Construction
    /// alone and from_parts never enable automatic private execution.
    pub(crate) async fn activate_legacy_execution(
        self: &Arc<Self>,
        execution: LegacyPrivateExecution,
    ) -> Result<()> {
        execution.current_owner()?;
        if self.semaphore.is_closed() {
            return Err(CalmError::Unauthorized);
        }
        *self
            .inner
            .canonical_producer
            .write()
            .expect("Dispatcher producer") = Some(Producer {
            dispatcher: Arc::downgrade(self),
            execution,
        });
        // The existing reconcile owner retries held input after native recovery;
        // its declaration is never reconstructed from a row or a readiness flag.
        self.inner.reconcile_canonical_inputs().await
    }

    /// Real service retirement revokes sources even when old background Inner or
    /// invocation tasks remain alive. Closing the existing admission semaphore
    /// also prevents new event handlers from starting.
    pub fn retire(&self) {
        self.semaphore.close();
        self.handle.abort();
        self.reconcile_handle.abort();
        if let Some(handle) = &self.reaper_handle {
            handle.abort();
        }
        if let Some(handle) = &self.liveness_feeder_handle {
            handle.abort();
        }
    }
}

struct QualifiedEvent {
    fact: EventInputFact,
    observation: SystemObservation,
}
struct StoredSystem {
    reference: AcceptedInputRef,
    observation: SystemObservation,
    envelope: i64,
}

impl Inner {
    fn producer(&self) -> Result<Producer> {
        let producer = self
            .canonical_producer
            .read()
            .expect("Dispatcher producer")
            .clone()
            .ok_or(CalmError::Unauthorized)?;
        producer.current_owner()?;
        Ok(producer)
    }

    async fn qualify(
        &self,
        track: &TrackId,
        fact: EventInputFact,
    ) -> Result<Option<QualifiedEvent>> {
        if fact.envelope.scope.track_id() != Some(track) {
            return Err(CalmError::Forbidden("event Track scope changed".into()));
        }
        let Some(observation) =
            qualification::qualified_observation(self.repo.as_ref(), track, &fact.envelope).await?
        else {
            return Ok(None);
        };
        let observation = SystemObservation::try_from(observation)
            .map_err(|error| CalmError::Forbidden(error.into()))?;
        Ok(Some(QualifiedEvent { fact, observation }))
    }

    pub(super) async fn reconcile_canonical_inputs(self: &Arc<Self>) -> Result<()> {
        if self
            .canonical_producer
            .read()
            .expect("Dispatcher producer")
            .is_none()
        {
            return Ok(());
        }
        self.producer()?;
        for runtime in self
            .repo
            .session_projection_recover_harnesses_on_boot()
            .await?
        {
            let card = CardId::from(runtime.card_id.clone());
            if self.write.verify_role(&card) != Some(CardRole::Planner)
                || !crate::conversation_inputs::has_canonical_storage(
                    self.repo.as_ref(),
                    card.as_str(),
                )
                .await?
            {
                continue;
            }
            let track = self
                .repo
                .card_track_id(card.as_str())
                .await?
                .ok_or_else(|| CalmError::Conflict("event target lost its Track".into()))?;
            let track = TrackId::from(track);
            let _guard = self.acquire_push_lock(&track).await;
            if let Err(error) = self.accept_canonical_events(&card, None).await {
                tracing::warn!(%card, %error, "Dispatcher canonical input remains held");
            }
        }
        Ok(())
    }

    pub(super) async fn accept_canonical_events(
        self: &Arc<Self>,
        card: &CardId,
        incoming: Option<(&Event, i64)>,
    ) -> Result<()> {
        let producer = self.producer()?;
        let pool = self
            .repo
            .sqlite_db()
            .ok_or_else(|| CalmError::Internal("event admission requires SQLite".into()))?;
        let card_name = card.to_string();
        let (worker, track, floor) = write_in_tx_typed(self.repo.as_ref(), {
            let card = card_name.clone();
            move |tx| {
                Box::pin(async move {
                    let carrier = conversation::current_input_content_carrier_tx(tx, &card).await?;
                    let track: String =
                        sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?1")
                            .bind(&card)
                            .fetch_one(&mut **tx)
                            .await?;
                    let floor = carrier.snapshot()["push_watermark"]
                        .as_i64()
                        .ok_or_else(|| CalmError::Conflict("missing event watermark".into()))?;
                    Ok((
                        carrier.worker_session_id().to_owned(),
                        TrackId::from(track),
                        floor,
                    ))
                })
            }
        })
        .await?;
        if let Some((event, id)) = incoming {
            if id <= 0 {
                return Err(CalmError::Forbidden(
                    "event input requires a real persisted envelope".into(),
                ));
            }
            let actual = event_inputs::track_prefix(
                pool.read_pool(),
                track.as_str(),
                PLANNER_CATCH_UP_KINDS,
                id - 1,
                Some(id),
            )
            .await?;
            if actual.len() != 1
                || serde_json::to_value(&actual[0].envelope.event)? != serde_json::to_value(event)?
            {
                return Err(CalmError::Forbidden(
                    "incoming event does not match its stored envelope".into(),
                ));
            }
        }
        let through = incoming.map(|(_, id)| id);
        let mut qualified = vec![];
        for fact in event_inputs::track_prefix(
            pool.read_pool(),
            track.as_str(),
            PLANNER_CATCH_UP_KINDS,
            floor,
            through,
        )
        .await?
        {
            if let Some(input) = self.qualify(&track, fact).await? {
                qualified.push(input);
            }
        }
        let database = self.repo.database_id();
        let (floor, stored) = write_in_tx_typed(self.repo.as_ref(), {
            let (card, worker, track, database, producer) = (card_name.clone(), worker.clone(), track.clone(), database.clone(), producer.clone());
            move |tx| Box::pin(async move {
                let mut carrier = conversation::current_input_content_carrier_tx(tx, &card).await?;
                if carrier.worker_session_id() != worker { return Err(CalmError::Conflict("event target changed".into())); }
                for input in qualified {
                    let binding = EventBinding { producer: producer.clone(), database: database.clone(), track: track.clone(), card: card.clone(), worker: worker.clone(), event: input.fact };
                    binding.authorize_tx(tx, &carrier).await?;
                    if binding.event.envelope.id > carrier.snapshot()["push_watermark"].as_i64().unwrap_or(0) {
                        conversation::accept_pending_system_tx(tx, &mut carrier, input.observation, &binding.event).await?;
                    }
                }
                let mut stored = vec![];
                for input in &carrier.checkpoint().pending {
                    if let PendingInput::System { observation_id, observation, envelope_id: Some(envelope), .. } = input {
                        stored.push(StoredSystem { reference: AcceptedInputRef::System { observation_id: observation_id.clone() }, observation: observation.clone(), envelope: *envelope });
                    }
                }
                if carrier.phase()? == crate::harness::HarnessPhaseTag::IssuingTurn
                    && let Some(active) = &carrier.checkpoint().active
                    && let [id] = active.submission_ids.as_slice()
                    && carrier.checkpoint().attempt(id).is_none() {
                    let plan = conversation::submission_plan_tx(tx, id).await?;
                    for segment in plan.plan.segments() {
                        if let SubmissionRenderSegment::Input { input: SubmissionRenderInput::System { observation_id, payload: PrivateHistoryPayload::Present { value } }, .. } = segment {
                            let envelope: Option<i64> = sqlx::query_scalar("SELECT envelope_id FROM conversation_submission_inputs WHERE submission_id=?1 AND observation_id=?2 AND effective=1")
                                .bind(id.as_str()).bind(observation_id.as_str()).fetch_one(&mut **tx).await?;
                            let envelope = envelope.ok_or_else(|| CalmError::Conflict("System claim lost its original envelope".into()))?;
                            stored.push(StoredSystem { reference: AcceptedInputRef::System { observation_id: observation_id.clone() }, observation: value.clone(), envelope });
                        }
                    }
                }
                Ok((carrier.snapshot()["push_watermark"].as_i64().unwrap_or(0), stored))
            })
        }).await?;
        self.push_cursor.bump(card.clone(), floor);
        // Dedup never bypasses fresh producer admission of already accepted work.
        for stored in stored {
            let facts = event_inputs::track_prefix(
                pool.read_pool(),
                track.as_str(),
                PLANNER_CATCH_UP_KINDS,
                stored.envelope - 1,
                Some(stored.envelope),
            )
            .await?;
            let fact = facts.into_iter().next().ok_or_else(|| {
                CalmError::Conflict("accepted System source event is unavailable".into())
            })?;
            let Some(qualified) = self.qualify(&track, fact).await? else {
                return Err(CalmError::Forbidden(
                    "accepted event no longer qualifies this producer".into(),
                ));
            };
            let source = DispatcherInputSource {
                binding: EventBinding {
                    producer: producer.clone(),
                    database: database.clone(),
                    track: track.clone(),
                    card: card_name.clone(),
                    worker: worker.clone(),
                    event: qualified.fact,
                },
                reference: stored.reference.clone(),
                observation: stored.observation,
            };
            write_in_tx_typed(self.repo.as_ref(), {
                let source = source.clone();
                let card = card_name.clone();
                move |tx| {
                    Box::pin(async move {
                        let carrier =
                            conversation::current_input_content_carrier_tx(tx, &card).await?;
                        source.authorize_resource_tx(tx, &carrier).await?;
                        Ok(())
                    })
                }
            })
            .await?;
            let wake = self
                .planner_wake
                .read()
                .expect("Planner wake context")
                .clone();
            let source = InputSource::Dispatcher(Box::new(source));
            let handle = self.harness.get(&worker);
            let card = card.clone();
            // Durable acceptance, not mailbox/native progress, acknowledges this
            // event. Invocation state retains only weak producer dependencies.
            tokio::spawn(async move {
                let result = if let Some(handle) = handle {
                    handle
                        .observe_canonical_source(stored.reference, source)
                        .await
                } else if let Some(wake) = wake {
                    wake.wake_canonical(&card, source).await
                } else {
                    Ok(())
                };
                if let Err(error) = result {
                    tracing::warn!(%card, %error, "accepted System input remains held by its original lifecycle");
                }
            });
        }
        Ok(())
    }
}
