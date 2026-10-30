//! Boot-bound typed issuance composes allocation, Truth acceptance, then publish/wake.
use super::assistant_admission::{AssistantAdmission, EventAdmission};
use super::canonical_inputs::Producer;
use super::*;
use crate::db::sqlite::{self, conversation};
use crate::error::{CalmError, Result};
use calm_types::system_assistant::{SystemAssistantReceipt, SystemAssistantSource};

#[derive(Clone)]
pub(crate) struct SystemAssistantIssuer {
    producer: Producer,
    source: SystemAssistantSource,
}

type ActorEvents = Vec<(ActorId, crate::event::EventScope, Event)>;
struct Acceptance {
    receipt: SystemAssistantReceipt,
    events: ActorEvents,
    ids: Vec<i64>,
}

pub(crate) struct PreparedSystemInput {
    pub bootstrap: String,
    pub text: String,
    pub events: ActorEvents,
}
pub(crate) enum PreparedSubmission {
    Accepted {
        receipt: SystemAssistantReceipt,
        fresh: bool,
    },
    Retry(TrackId),
}

impl Dispatcher {
    /// Only the producer installed by completed production recovery can bind an issuer.
    pub(crate) fn system_assistant_issuer(
        &self,
        source: SystemAssistantSource,
    ) -> Result<SystemAssistantIssuer> {
        Ok(SystemAssistantIssuer {
            producer: self.inner.producer()?,
            source,
        })
    }
}
impl SystemAssistantIssuer {
    fn context(&self, state: &crate::state::RouteState) -> Result<Arc<Dispatcher>> {
        let dispatcher = self
            .producer
            .dispatcher
            .upgrade()
            .ok_or(CalmError::Unauthorized)?;
        self.producer.current_owner()?;
        if !std::ptr::addr_eq(
            Arc::as_ptr(&dispatcher.inner.repo),
            Arc::as_ptr(&state.repo),
        ) {
            return Err(CalmError::Forbidden(
                "issuer repository context changed".into(),
            ));
        }
        Ok(dispatcher)
    }
    fn validate_input(&self, subject: &str, text: &str) -> Result<()> {
        if subject.is_empty() || subject == self.source.bootstrap_subject() {
            return Err(CalmError::BadRequest(
                "input subject must be fresh and distinct from bootstrap".into(),
            ));
        }
        crate::routes::conversations_shared::validate_first_message(text)
    }
    async fn qualify_namespace_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        track: &TrackId,
    ) -> Result<()> {
        let derived = crate::conversation_keys::derive_track_conversation_keys(
            track.as_str(),
            self.source.namespace(),
        );
        let existing: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM conversation_system_assistant_entries WHERE card_id=?1)",
        )
        .bind(&derived.card_id)
        .fetch_one(&mut **tx)
        .await?;
        if !existing {
            let keys =
                crate::routes::conversations_shared::operation_key_attempts(&derived.operation_key)
                    .collect::<Vec<_>>();
            let historical: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM operations WHERE kind=?1 AND idempotency_key IN (SELECT value FROM json_each(?2))) OR EXISTS(SELECT 1 FROM cards WHERE id=?3)")
                .bind(crate::routes::conversations_shared::PLANNER_HARNESS_START).bind(serde_json::to_string(&keys)?).bind(&derived.card_id).fetch_one(&mut **tx).await?;
            if historical || conversation::assistant_key_used_tx(tx, &derived.card_id).await? {
                return Err(CalmError::IdempotencyKeyExhausted(
                    "historical Assistant namespace requires explicit migration".into(),
                ));
            }
        }
        Ok(())
    }
    async fn publish(
        &self,
        state: &crate::state::RouteState,
        accepted: Acceptance,
        guard: crate::harness::PushLockGuard,
    ) -> Result<SystemAssistantReceipt> {
        let dispatcher = self.context(state)?;
        let fresh = !accepted.events.is_empty();
        sqlite::broadcast_actor_events(&state.events, &accepted.ids, accepted.events);
        drop(guard);
        if fresh {
            let card = CardId::from(accepted.receipt.card_id.clone());
            if let Err(error) = dispatcher.inner.accept_canonical_events(&card, None).await {
                tracing::warn!(%card, %error, "accepted System Assistant work remains held");
            }
        }
        Ok(accepted.receipt)
    }
    /// Exact typed submissions retain their payload contract. HTTP first-accept
    /// preparation below instead returns the permanent ACK before computing text.
    #[cfg(test)]
    pub(crate) async fn submit(
        &self,
        state: &crate::state::RouteState,
        track: TrackId,
        bootstrap: String,
        subject: String,
        text: String,
    ) -> Result<SystemAssistantReceipt> {
        self.validate_input(&subject, &text)?;
        let dispatcher = self.context(state)?;
        let guard = dispatcher.inner.acquire_push_lock(&track).await;
        let issuer = self.clone();
        let route = state.clone();
        let accepted = crate::db::write_in_tx_typed(state.repo.as_ref(), move |tx| {
            Box::pin(async move {
                issuer
                    .accept_tx(tx, &route, track, bootstrap, subject, text)
                    .await
            })
        })
        .await?;
        self.publish(state, accepted, guard).await
    }

    pub(crate) async fn replay(
        &self,
        state: &crate::state::RouteState,
        principal: &crate::auth::Principal,
        subject: &str,
    ) -> Result<Option<SystemAssistantReceipt>> {
        self.context(state)?;
        let issuer = self.clone();
        let principal = principal.clone();
        let subject = subject.to_owned();
        // The event-less owner uses BEGIN IMMEDIATE even for reads: no parked
        // deferred snapshot or lock upgrade, and this closure writes no rows.
        crate::db::write_in_tx_typed(state.repo.as_ref(), move |tx| {
            Box::pin(async move { issuer.replay_tx(tx, &principal, &subject).await })
        })
        .await
    }

    async fn replay_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        principal: &crate::auth::Principal,
        subject: &str,
    ) -> Result<Option<SystemAssistantReceipt>> {
        self.producer.current_owner()?;
        let receipt =
            conversation::system_assistant_subject_receipt_tx(tx, self.source, subject).await?;
        if let Some(receipt) = &receipt {
            crate::track_access::require_detail_access_tx(tx, &receipt.track_id, principal).await?;
        }
        self.producer.current_owner()?;
        Ok(receipt)
    }

    /// Prepare domain state only on a permanent receipt miss, under the very
    /// writer that accepts it. Resolve again before namespace checks or mutation;
    /// a changed target commits no writes and releases the lock before retry.
    pub(crate) async fn submit_prepared<R, P>(
        &self,
        state: &crate::state::RouteState,
        principal: crate::auth::Principal,
        track: TrackId,
        subject: String,
        resolve: R,
        prepare: P,
    ) -> Result<PreparedSubmission>
    where
        R: for<'tx> FnOnce(
                &'tx mut sqlx::Transaction<'_, sqlx::Sqlite>,
            ) -> futures::future::BoxFuture<'tx, Result<TrackId>>
            + Send
            + 'static,
        P: for<'tx> FnOnce(
                &'tx mut sqlx::Transaction<'_, sqlx::Sqlite>,
            )
                -> futures::future::BoxFuture<'tx, Result<PreparedSystemInput>>
            + Send
            + 'static,
    {
        if subject.is_empty() || subject == self.source.bootstrap_subject() {
            return Err(CalmError::BadRequest("invalid input subject".into()));
        }
        let dispatcher = self.context(state)?;
        let guard = dispatcher.inner.acquire_push_lock(&track).await;
        let issuer = self.clone();
        let route = state.clone();
        let outcome = crate::db::write_in_tx_typed(state.repo.as_ref(), move |tx| {
            Box::pin(async move {
                issuer.producer.current_owner()?;
                if let Some(receipt) = issuer.replay_tx(tx, &principal, &subject).await? {
                    return Ok(Ok(Acceptance {
                        receipt,
                        events: vec![],
                        ids: vec![],
                    }));
                }
                let actual = resolve(tx).await?;
                if actual != track {
                    return Ok(Err(actual));
                }
                issuer.qualify_namespace_tx(tx, &track).await?;
                let prepared = prepare(tx).await?;
                crate::track_access::require_detail_access_tx(tx, track.as_str(), &principal)
                    .await?;
                let mut ids = if prepared.events.is_empty() {
                    vec![]
                } else {
                    sqlite::append_actor_events_in_tx(tx, None, &route.write, &prepared.events)
                        .await?
                        .0
                };
                let mut accepted = issuer
                    .accept_tx(
                        tx,
                        &route,
                        track,
                        prepared.bootstrap,
                        subject,
                        prepared.text,
                    )
                    .await?;
                let mut events = prepared.events;
                events.append(&mut accepted.events);
                ids.append(&mut accepted.ids);
                accepted.events = events;
                accepted.ids = ids;
                Ok(Ok(accepted))
            })
        })
        .await?;
        match outcome {
            Err(track) => {
                drop(guard);
                Ok(PreparedSubmission::Retry(track))
            }
            Ok(accepted) => {
                let fresh = !accepted.events.is_empty();
                Ok(PreparedSubmission::Accepted {
                    receipt: self.publish(state, accepted, guard).await?,
                    fresh,
                })
            }
        }
    }

    #[allow(deprecated)]
    async fn accept_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        state: &crate::state::RouteState,
        track: TrackId,
        bootstrap: String,
        subject: String,
        text: String,
    ) -> Result<Acceptance> {
        self.validate_input(&subject, &text)?;
        let dispatcher = self.context(state)?;
        let inner = dispatcher.inner.clone();
        let source = self.source;
        let roles = state.write.role_cache().clone();
        let derived = crate::conversation_keys::derive_track_conversation_keys(
            track.as_str(),
            source.namespace(),
        );
        self.producer.current_owner()?;
        let target = sqlite::track_get_tx(tx, &track).await?;
        if target.closed_at.is_some() || target.purpose.as_deref() == Some(crate::AREA_CHAT_PURPOSE)
        {
            return Err(CalmError::Forbidden(
                "System Assistant Track is not open".into(),
            ));
        }
        crate::organization_access::require_legacy_event_execution_tx(
            tx,
            target.area_id.as_str(),
            &self.producer.execution,
        )
        .await?;
        let existing: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM conversation_system_assistant_entries WHERE card_id=?1)",
        )
        .bind(&derived.card_id)
        .fetch_one(&mut **tx)
        .await?;
        let mut events = vec![];
        let mut ids = vec![];
        if !existing {
            self.qualify_namespace_tx(tx, &track).await?;
            let allocated = sqlite::card_create_with_id_tx(tx, derived.card_id.clone(), crate::model::NewCard {
                    track_id: track.clone(), kind: "codex".into(), sort: None, title: None,
                    payload: serde_json::json!({"schemaVersion":1,"harness_profile":"assistant","assistant_provider":"codex"}),
                }, CardRole::Assistant, false, &roles).await?;
            let floor = calm_truth::db::sqlite::event_inputs::event_creation_floor_tx(tx).await?;
            sqlite::session_start_runtime_tx(
                tx,
                crate::session_projection_repo::WorkerSessionInit::managed_assistant(
                    crate::model::new_id(),
                    allocated.id.to_string(),
                    crate::session_projection_repo::AgentProvider::Codex,
                    crate::session_projection_repo::WorkerSessionState::Idle,
                    None,
                    serde_json::to_value(
                        crate::harness::snapshot::CanonicalHarnessSnapshot::dormant(floor),
                    )?,
                    crate::model::now_ms(),
                ),
            )
            .await?;
            conversation::system_assistant_entry_insert_tx(
                tx,
                &derived.card_id,
                source,
                &bootstrap,
            )
            .await?;
            let scope = crate::event::EventScope::Card {
                card: allocated.id.clone(),
                track: track.clone(),
                area: target.area_id.clone(),
            };
            let event = Event::CardAdded(allocated);
            ids.push(
                sqlite::append_decision_event_in_tx(tx, &ActorId::Kernel, &scope, None, &event)
                    .await?,
            );
            events.push((ActorId::Kernel, scope, event));
        }
        let admission = AssistantAdmission::load_tx(tx, &derived.card_id).await?;
        if admission.source != source || admission.track != track {
            return Err(CalmError::Forbidden("issuer target changed".into()));
        }
        let bootstrap_ack = conversation::system_assistant_receipt_tx(
            tx,
            &derived.card_id,
            source,
            source.bootstrap_subject(),
        )
        .await?;
        if bootstrap_ack.as_ref().is_some_and(|ack| {
            ack.payload_digest != crate::conversation_keys::first_message_digest(&bootstrap)
        }) {
            return Err(CalmError::IdempotencyKeyReused(
                "bootstrap names another frozen instruction".into(),
            ));
        }
        if existing && bootstrap_ack.is_none() {
            return Err(CalmError::Conflict(
                "original bootstrap acceptance is missing".into(),
            ));
        }
        if let Some(original) =
            conversation::system_assistant_receipt_tx(tx, &derived.card_id, source, &subject)
                .await?
        {
            if original.payload_digest != crate::conversation_keys::first_message_digest(&text) {
                return Err(CalmError::IdempotencyKeyReused(
                    "subject names another frozen input".into(),
                ));
            }
            self.producer.current_owner()?;
            return Ok(Acceptance {
                receipt: original,
                events,
                ids,
            });
        }
        let mut inputs = vec![];
        if !existing {
            inputs.push((source.bootstrap_subject().to_owned(), bootstrap));
        }
        inputs.push((subject, text));
        let mut last = None;
        for (subject, text) in inputs {
            let scope = crate::event::EventScope::Card {
                card: admission.card.clone(),
                track: track.clone(),
                area: target.area_id.clone(),
            };
            let event = Event::AssistantInputRequested {
                card_id: admission.card.clone(),
                track_id: track.clone(),
                original_worker: admission.worker.clone(),
                source,
                subject: subject.clone(),
                version: 1,
                text,
            };
            let id =
                sqlite::append_decision_event_in_tx(tx, &ActorId::Kernel, &scope, None, &event)
                    .await?;
            let runtime = EventAdmission::Assistant(admission.clone());
            let facts = inner
                .read_event_prefix_tx(tx, &runtime, Some((&event, id)))
                .await?;
            inner
                .accept_event_prefix_tx(tx, &self.producer, &runtime, facts)
                .await?;
            let receipt =
                conversation::system_assistant_receipt_tx(tx, &derived.card_id, source, &subject)
                    .await?
                    .ok_or_else(|| {
                        CalmError::Conflict("original System acceptance is missing".into())
                    })?;
            events.push((ActorId::Kernel, scope.clone(), event));
            ids.push(id);
            let presentation = Event::HarnessPresentationChanged {
                card_id: admission.card.clone(),
                track_id: track.clone(),
                cursor: calm_types::conversation_display::ConversationCursor::new(
                    &derived.card_id,
                    calm_types::conversation_display::ConversationCursorSource::Presentation,
                    receipt.presentation_sequence,
                ),
            };
            ids.push(
                sqlite::append_decision_event_in_tx(
                    tx,
                    &ActorId::Kernel,
                    &scope,
                    None,
                    &presentation,
                )
                .await?,
            );
            events.push((ActorId::Kernel, scope, presentation));
            last = Some(receipt);
        }
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point("system-assistant-before-commit", &derived.card_id).await;
        self.producer.current_owner()?;
        Ok(Acceptance {
            receipt: last.expect("input is required"),
            events,
            ids,
        })
    }
}
