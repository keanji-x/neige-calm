//! The actual Dispatcher qualifies persisted event inputs. Its declaration is
//! installed only after the original production recovery boundary completes.
use super::assistant_admission::EventAdmission;
use super::planner_admission::PlannerAdmission;
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
    pub(super) dispatcher: Weak<Dispatcher>,
    pub(super) execution: LegacyPrivateExecution,
}
impl Producer {
    pub(super) fn current_owner(&self) -> Result<String> {
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
    admission: EventAdmission,
    event: EventInputFact,
}
impl EventBinding {
    async fn authorize_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        carrier: &CurrentInputCarrier,
    ) -> Result<String> {
        self.producer.current_owner()?;
        self.admission.authorize_tx(tx).await?;
        if self.event.envelope.id <= self.admission.baseline() {
            return Err(CalmError::Forbidden("event predates Planner tenure".into()));
        }
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
struct EventSourceHandoff {
    sources: Vec<(AcceptedInputRef, InputSource)>,
    handoff: crate::harness::registry::SourceHandoff,
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
        if let EventAdmission::Assistant(admission) = &self.binding.admission {
            let Event::AssistantInputRequested { subject, .. } = &self.binding.event.envelope.event
            else {
                return Err(CalmError::Forbidden("targeted source kind changed".into()));
            };
            conversation::system_assistant_receipt_replay_tx(
                tx,
                carrier.card_id(),
                admission.source,
                subject,
                self.binding.event.envelope.id,
                &self.reference,
            )
            .await?;
        }
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
        self.scheduler.bind_child_dispatcher(Arc::downgrade(self));
        // The existing reconcile owner retries held input after native recovery;
        // its declaration is never reconstructed from a row or a readiness flag.
        self.inner.reconcile_canonical_inputs().await
    }

    /// Scheduler completion waits for activation of the exact creation-time
    /// worker. Qualification is the same durable event-prefix path as catch-up.
    pub(crate) async fn bootstrap_canonical_child(
        &self,
        card: &CardId,
        worker: &str,
    ) -> Result<()> {
        let worker = worker.to_owned();
        let current = self
            .inner
            .repo
            .session_projection_active_for_card(&card.to_string())
            .await?;
        if current.as_ref().map(|row| row.id.as_str()) != Some(worker.as_str()) {
            return Err(CalmError::Conflict(
                "child bootstrap carrier changed".into(),
            ));
        }
        let Some(EventSourceHandoff {
            sources,
            handoff: _handoff,
        }) = self.inner.canonical_event_sources(card, None).await?
        else {
            return Ok(());
        };
        for (reference, source) in sources {
            if let Some(handle) = self.inner.harness.get(&worker) {
                handle.observe_canonical_source(reference, source).await?;
            } else {
                let wake = self
                    .inner
                    .planner_wake
                    .read()
                    .expect("Planner wake context")
                    .clone()
                    .ok_or_else(|| {
                        CalmError::ServiceUnavailable("child activation owner unavailable".into())
                    })?;
                wake.wake_canonical(card, source).await?;
            }
        }
        Ok(())
    }

    /// Real service retirement revokes sources even when old background Inner or
    /// invocation tasks remain alive. Closing the existing admission semaphore
    /// also prevents new event handlers from starting.
    pub fn retire(&self) {
        for handle in self
            .additional_liveness_feeders
            .lock()
            .expect("liveness followers")
            .iter()
        {
            handle.abort();
        }
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
    pub(super) fn producer(&self) -> Result<Producer> {
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
        for (track, card) in PlannerAdmission::reconcile_candidates(self.repo.as_ref()).await? {
            if !crate::conversation_inputs::has_canonical_storage(self.repo.as_ref(), card.as_str())
                .await?
            {
                continue;
            }
            #[cfg(feature = "fixtures")]
            crate::test_seams::pause_point("planner-reconcile-before-push", card.as_str()).await;
            let _guard = self.acquire_push_lock(&track).await;
            let Some(current) = PlannerAdmission::discover(self.repo.as_ref(), &track).await?
            else {
                continue;
            };
            let card = current.card;
            if let Err(error) = self.accept_canonical_events(&card, None).await {
                tracing::warn!(%card, %error, "Dispatcher canonical input remains held");
            }
        }
        let db = self
            .repo
            .sqlite_db()
            .ok_or_else(|| CalmError::Internal("Assistant discovery requires SQLite".into()))?;
        let candidates: Vec<(String, String)> = sqlx::query_as("SELECT track_id,card_id FROM conversation_system_assistant_entries ORDER BY track_id,card_id")
            .fetch_all(db.read_pool()).await?;
        for (track, card) in candidates {
            let _guard = self.acquire_push_lock(&track.into()).await;
            let card = CardId::from(card);
            if let Err(error) = self.accept_canonical_events(&card, None).await {
                tracing::warn!(%card, %error, "Dispatcher Assistant input remains held");
            }
        }
        Ok(())
    }

    pub(super) async fn accept_canonical_events(
        self: &Arc<Self>,
        card: &CardId,
        incoming: Option<(&Event, i64)>,
    ) -> Result<()> {
        let Some(EventSourceHandoff { sources, handoff }) =
            self.canonical_event_sources(card, incoming).await?
        else {
            return Ok(());
        };
        let handoff = Arc::new(handoff);
        for (reference, source) in sources {
            let handoff = handoff.clone();
            let wake = self
                .planner_wake
                .read()
                .expect("Planner wake context")
                .clone();
            let worker = match &source {
                InputSource::Dispatcher(source) => source.binding.worker.clone(),
                _ => unreachable!("event producer only returns Dispatcher sources"),
            };
            let handle = self.harness.get(&worker);
            let card = card.clone();
            tokio::spawn(async move {
                let _handoff = handoff;
                let result = if let Some(handle) = handle {
                    handle.observe_canonical_source(reference, source).await
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

    /// One ordered selection for POST, live through hints and cold reconciliation.
    /// Facts carry fingerprints, never permission to accept or execute.
    pub(super) async fn read_event_prefix_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        admission: &EventAdmission,
        incoming: Option<(&Event, i64)>,
    ) -> Result<Vec<EventInputFact>> {
        if incoming.is_some_and(|(_, id)| id > 0 && id <= admission.baseline()) {
            return Ok(vec![]);
        }
        let carrier =
            conversation::current_input_content_carrier_tx(tx, admission.card().as_str()).await?;
        if admission.worker() != carrier.worker_session_id() {
            return Err(CalmError::Forbidden("event target worker changed".into()));
        }
        let floor = carrier.snapshot()["push_watermark"]
            .as_i64()
            .ok_or_else(|| CalmError::Conflict("missing event watermark".into()))?
            .max(admission.baseline());
        if let Some((event, id)) = incoming {
            if id <= 0 {
                return Err(CalmError::Forbidden(
                    "event input requires a real persisted envelope".into(),
                ));
            }
            let actual = event_inputs::track_prefix_tx(
                tx,
                admission.track().as_str(),
                admission.kinds(),
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
        Ok(event_inputs::track_prefix_tx(
            tx,
            admission.track().as_str(),
            admission.kinds(),
            floor,
            through,
        )
        .await?)
    }

    /// The only event acceptance composition. Captured facts are qualified under
    /// the outer writer and compared with SQL again before the Truth owner runs.
    pub(super) async fn accept_event_prefix_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        producer: &Producer,
        admission: &EventAdmission,
        facts: Vec<EventInputFact>,
    ) -> Result<CurrentInputCarrier> {
        producer.current_owner()?;
        admission.authorize_tx(tx).await?;
        let card = admission.card().to_string();
        let worker = admission.worker().to_owned();
        let mut carrier = conversation::current_input_content_carrier_tx(tx, &card).await?;
        if carrier.worker_session_id() != worker {
            return Err(CalmError::Conflict("event target changed".into()));
        }
        for fact in facts {
            let Some(observation) = admission.qualify_tx(tx, self.repo.as_ref(), &fact).await?
            else {
                continue;
            };
            let observation = SystemObservation::try_from(observation)
                .map_err(|error| CalmError::Forbidden(error.into()))?;
            let input = QualifiedEvent { fact, observation };
            let binding = EventBinding {
                producer: producer.clone(),
                database: self.repo.database_id(),
                track: admission.track().clone(),
                card: card.clone(),
                worker: worker.clone(),
                admission: admission.clone(),
                event: input.fact,
            };
            binding.authorize_tx(tx, &carrier).await?;
            if binding.event.envelope.id
                <= carrier.snapshot()["push_watermark"].as_i64().unwrap_or(0)
            {
                continue;
            }
            match admission {
                EventAdmission::Assistant(_) => {
                    conversation::system_assistant_accept_tx(tx, &card, binding.event.envelope.id)
                        .await?;
                    // The atomic owner reloads privately; our carrier is now stale.
                    carrier = conversation::current_input_content_carrier_tx(tx, &card).await?;
                }
                EventAdmission::Planner(_) => {
                    let mut observation = input.observation.observation().clone();
                    crate::report_read_ledger::delta::attach_tx(
                        tx,
                        &worker,
                        &binding.event.envelope.event,
                        &mut observation,
                    )
                    .await?;
                    let observation = SystemObservation::try_from(observation)
                        .map_err(|error| CalmError::Forbidden(error.into()))?;
                    conversation::accept_pending_system_tx(
                        tx,
                        &mut carrier,
                        observation,
                        &binding.event,
                    )
                    .await?;
                }
            }
        }
        producer.current_owner()?;
        Ok(carrier)
    }

    async fn canonical_event_sources(
        self: &Arc<Self>,
        card: &CardId,
        incoming: Option<(&Event, i64)>,
    ) -> Result<Option<EventSourceHandoff>> {
        let producer = self.producer()?;
        let card_name = card.to_string();
        let incoming = incoming.map(|(event, id)| (event.clone(), id));
        let (admission, facts) = write_in_tx_typed(self.repo.as_ref(), {
            let (inner, card, incoming) = (self.clone(), card_name.clone(), incoming.clone());
            move |tx| {
                Box::pin(async move {
                    let admission = EventAdmission::load_tx(tx, &card).await?;
                    let facts = inner
                        .read_event_prefix_tx(
                            tx,
                            &admission,
                            incoming.as_ref().map(|(event, id)| (event, *id)),
                        )
                        .await?;
                    Ok((admission, facts))
                })
            }
        })
        .await?;
        if incoming
            .as_ref()
            .is_some_and(|(_, id)| *id > 0 && *id <= admission.baseline())
        {
            return Ok(None);
        }
        let worker = admission.worker().to_owned();
        let track = admission.track().clone();
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point("planner-events-before-accept", card.as_str()).await;
        let database = self.repo.database_id();
        let (floor, stored, handoff) = write_in_tx_typed(self.repo.as_ref(), {
            let (inner, admission, producer) = (self.clone(), admission.clone(), producer.clone());
            move |tx| Box::pin(async move {
                let carrier = inner.accept_event_prefix_tx(tx, &producer, &admission, facts).await?;
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
                let handoff = inner.harness.canonical_sources(carrier.worker_session_id(), admission.track()).begin_handoff();
                Ok((carrier.snapshot()["push_watermark"].as_i64().unwrap_or(0), stored, handoff))
            })
        }).await?;
        self.push_cursor.bump(card.clone(), floor);
        // Dedup never bypasses fresh producer admission of already accepted work.
        let mut sources = vec![];
        for stored in stored {
            let reference = stored.reference.clone();
            let source = write_in_tx_typed(self.repo.as_ref(), {
                let (inner, admission, producer) =
                    (self.clone(), admission.clone(), producer.clone());
                let (track, card, worker, database) = (
                    track.clone(),
                    card_name.clone(),
                    worker.clone(),
                    database.clone(),
                );
                move |tx| {
                    Box::pin(async move {
                        let facts = event_inputs::track_prefix_tx(
                            tx,
                            track.as_str(),
                            admission.kinds(),
                            stored.envelope - 1,
                            Some(stored.envelope),
                        )
                        .await?;
                        let fact = facts.into_iter().next().ok_or_else(|| {
                            CalmError::Conflict(
                                "accepted System source event is unavailable".into(),
                            )
                        })?;
                        let Some(observation) =
                            admission.qualify_tx(tx, inner.repo.as_ref(), &fact).await?
                        else {
                            return Err(CalmError::Forbidden(
                                "accepted event no longer qualifies this producer".into(),
                            ));
                        };
                        SystemObservation::try_from(observation)
                            .map_err(|error| CalmError::Forbidden(error.into()))?;
                        let source = DispatcherInputSource {
                            binding: EventBinding {
                                producer,
                                database,
                                track,
                                card: card.clone(),
                                worker,
                                admission,
                                event: fact,
                            },
                            reference: stored.reference,
                            observation: stored.observation,
                        };
                        let carrier =
                            conversation::current_input_content_carrier_tx(tx, &card).await?;
                        source.authorize_resource_tx(tx, &carrier).await?;
                        Ok(source)
                    })
                }
            })
            .await?;
            sources.push((reference, InputSource::Dispatcher(Box::new(source))));
        }
        Ok(Some(EventSourceHandoff { sources, handoff }))
    }
}

impl Dispatcher {
    /// Transactional form of the production catch-up prefix; no second event-kind list.
    pub async fn carrier_quiescence_prefix_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        track: &TrackId,
        after: i64,
    ) -> Result<Vec<EventInputFact>> {
        Ok(
            event_inputs::track_prefix_tx(tx, track.as_str(), &PLANNER_CATCH_UP_KINDS, after, None)
                .await?,
        )
    }
}

impl Dispatcher {
    pub(crate) async fn planner_switch_push_lock(
        &self,
        track: &TrackId,
    ) -> crate::harness::PushLockGuard {
        self.inner.acquire_push_lock(track).await
    }
    pub(crate) async fn planner_switch_pending(
        &self,
        track: &TrackId,
        facts: &[EventInputFact],
        baseline: i64,
    ) -> Result<bool> {
        for fact in facts.iter().filter(|fact| fact.envelope.id > baseline) {
            if self.inner.qualify(track, fact.clone()).await?.is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(all(test, feature = "fixtures"))]
impl Dispatcher {
    pub(crate) async fn accept_event_prefix_for_test(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        card: &str,
        incoming: Option<(&Event, i64)>,
    ) -> Result<CurrentInputCarrier> {
        let admission = EventAdmission::load_tx(tx, card).await?;
        let facts = self
            .inner
            .read_event_prefix_tx(tx, &admission, incoming)
            .await?;
        self.inner
            .accept_event_prefix_tx(tx, &self.inner.producer()?, &admission, facts)
            .await
    }
}
