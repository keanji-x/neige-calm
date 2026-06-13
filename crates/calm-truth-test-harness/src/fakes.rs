//! Fake calm-exec implementations for #679 PR5 full-loop tests.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use calm_exec::{
    AgentReactor, DecisionIntent, DecisionSink, ObservationSink, SpawnCtx, WorkerProvider,
};
use calm_truth::db::sqlite::{SqlxRepo, session_insert_tx, wave_update_tx};
use calm_truth::db::{RepoEventWrite, RepoRead, write_in_tx_typed};
use calm_truth::decision_gate::{
    DecisionGate, GateDecision, PermissiveGate, WriteTx, commit_decision,
};
use calm_truth::error::{Result as TruthResult, TruthError};
use calm_truth::event::{Event, EventBus, EventScope};
use calm_truth::ids::{ActorId, CardId};
use calm_truth::model::{WavePatch, now_ms};
use calm_truth::session_repo::SessionRepo;
use calm_truth::state::WriteContext;
use calm_truth::{test_helpers, wave_lifecycle};
use calm_types::error::CoreError;
use calm_types::ids::{CoveId, WaveId};
use calm_types::model::WaveLifecycle;
use calm_types::observation::Observation;
use calm_types::worker::{
    ExitEvidence, ExitInterpretation, ExitSource, Liveness, Principal, SessionMode, WorkerSession,
    WorkerSessionId,
};
use serde_json::json;

#[derive(Debug, Default)]
pub struct FakeProvider {
    probe_script: Mutex<VecDeque<Liveness>>,
    probe_calls: AtomicUsize,
}

impl FakeProvider {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_probe_script<I>(self, script: I) -> Self
    where
        I: IntoIterator<Item = Liveness>,
    {
        *self.probe_script.lock().expect("probe script lock") =
            script.into_iter().collect::<VecDeque<_>>();
        self
    }

    pub fn probe_call_count(&self) -> usize {
        self.probe_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl WorkerProvider for FakeProvider {
    fn kind(&self) -> &'static str {
        "fake"
    }

    fn session_mode(&self) -> SessionMode {
        SessionMode::Ephemeral
    }

    async fn probe_liveness(
        &self,
        _session: &WorkerSession,
        _ctx: &SpawnCtx,
    ) -> Result<Liveness, CoreError> {
        self.probe_calls.fetch_add(1, Ordering::SeqCst);
        self.probe_script
            .lock()
            .expect("probe script lock")
            .pop_front()
            .ok_or_else(|| CoreError::Internal("fake probe script exhausted".into()))
    }

    async fn interpret_exit(
        &self,
        _session: &WorkerSession,
        evidence: &ExitEvidence,
        _ctx: &SpawnCtx,
    ) -> Result<ExitInterpretation, CoreError> {
        if evidence.exit_code == Some(0) && !evidence.signal_killed {
            return Ok(ExitInterpretation::Completed);
        }
        Ok(ExitInterpretation::Failed {
            reason: match (evidence.exit_code, evidence.signal_killed) {
                (Some(code), false) => format!("fake worker exited with code {code}"),
                (_, true) => "fake worker was signal-killed".into(),
                (None, false) => "fake worker exited without a code".into(),
            },
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObsMatcher {
    TaskCompleted,
    TaskFailed,
    Any,
}

impl ObsMatcher {
    fn matches(&self, observation: &Observation) -> bool {
        matches!(
            (self, observation),
            (Self::TaskCompleted, Observation::TaskCompleted { .. })
                | (Self::TaskFailed, Observation::TaskFailed { .. })
                | (Self::Any, _)
        )
    }
}

#[derive(Debug)]
pub struct FakeRoot {
    session_id: WorkerSessionId,
    wave_id: WaveId,
    cove_id: CoveId,
    script: Vec<(ObsMatcher, Vec<DecisionIntent>)>,
}

impl FakeRoot {
    pub fn for_wave(session_id: WorkerSessionId, wave_id: WaveId, cove_id: CoveId) -> Self {
        Self {
            session_id,
            wave_id,
            cove_id,
            script: Vec::new(),
        }
    }

    pub fn on<I>(mut self, matcher: ObsMatcher, intents: I) -> Self
    where
        I: IntoIterator<Item = DecisionIntent>,
    {
        self.script.push((matcher, intents.into_iter().collect()));
        self
    }
}

#[async_trait]
impl AgentReactor for FakeRoot {
    fn principal(&self) -> Principal {
        Principal::Agent {
            session_id: self.session_id.clone(),
            wave_id: self.wave_id.clone(),
            cove_id: self.cove_id.clone(),
        }
    }

    async fn react(&self, observation: &Observation) -> Result<Vec<DecisionIntent>, CoreError> {
        Ok(self
            .script
            .iter()
            .find_map(|(matcher, intents)| matcher.matches(observation).then(|| intents.clone()))
            .unwrap_or_default())
    }
}

#[derive(Debug, Default)]
pub struct RecordingDecisionSink {
    committed: Mutex<Vec<(Principal, DecisionIntent)>>,
}

impl RecordingDecisionSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn committed(&self) -> Vec<(Principal, DecisionIntent)> {
        self.committed.lock().expect("committed lock").clone()
    }
}

#[async_trait]
impl DecisionSink for RecordingDecisionSink {
    async fn commit(&self, principal: &Principal, intent: DecisionIntent) -> Result<(), CoreError> {
        self.committed
            .lock()
            .expect("committed lock")
            .push((principal.clone(), intent));
        Ok(())
    }
}

pub struct GatedDecisionSink<G> {
    repo: Arc<SqlxRepo>,
    bus: EventBus,
    write: WriteContext,
    gate: Arc<G>,
}

impl<G> GatedDecisionSink<G>
where
    G: DecisionGate + 'static,
{
    pub fn new(repo: Arc<SqlxRepo>, bus: EventBus, write: WriteContext, gate: Arc<G>) -> Self {
        Self {
            repo,
            bus,
            write,
            gate,
        }
    }
}

#[async_trait]
impl<G> DecisionSink for GatedDecisionSink<G>
where
    G: DecisionGate + 'static,
{
    async fn commit(&self, principal: &Principal, intent: DecisionIntent) -> Result<(), CoreError> {
        let DecisionIntent::LifecycleTransition {
            wave_id,
            to,
            agent_message,
        } = intent
        else {
            return Err(CoreError::Internal(
                "GatedDecisionSink only supports lifecycle transitions in PR5".into(),
            ));
        };

        let wave = self
            .repo
            .wave_get(wave_id.as_str())
            .await
            .map_err(truth_to_core)?
            .ok_or_else(|| CoreError::NotFound(format!("wave {wave_id}")))?;
        let from = wave.lifecycle;
        let event = Event::WaveLifecycleChanged {
            id: wave_id.clone(),
            cove_id: wave.cove_id.clone(),
            from,
            to,
            agent_message: agent_message.clone(),
        };
        let actor = actor_for_principal(principal);
        let transition_actor = actor.clone();
        let scope = EventScope::Wave {
            wave: wave_id.clone(),
            cove: wave.cove_id,
        };
        let wave_id_for_tx = wave_id.clone();

        commit_decision(
            self.repo.as_ref(),
            Arc::clone(&self.gate),
            actor,
            scope,
            None,
            &self.bus,
            &self.write,
            event,
            move |tx| {
                Box::pin(async move {
                    wave_lifecycle::validate_transition(from, to, &transition_actor)
                        .map_err(|e| TruthError::Forbidden(e.to_string()))?;
                    wave_update_tx(
                        tx,
                        wave_id_for_tx.as_str(),
                        WavePatch {
                            lifecycle: Some(to),
                            ..Default::default()
                        },
                    )
                    .await
                })
            },
        )
        .await
        .map(|_| ())
        .map_err(truth_to_core)
    }
}

fn actor_for_principal(principal: &Principal) -> ActorId {
    match principal {
        Principal::User => ActorId::User,
        Principal::Kernel => ActorId::Kernel,
        Principal::Agent { session_id, .. } => ActorId::AiSpec(CardId::from(session_id.as_str())),
    }
}

fn truth_to_core(err: TruthError) -> CoreError {
    match err {
        TruthError::Core(err) => err,
        TruthError::Forbidden(message) => CoreError::Forbidden(message),
        TruthError::Db(err) => CoreError::Internal(format!("database error: {err}")),
        TruthError::Io(err) => CoreError::Io(err),
        TruthError::Serde(err) => CoreError::Serde(err),
        TruthError::Internal(message) => CoreError::Internal(message),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeliveredObservation {
    pub session_id: WorkerSessionId,
    pub observation: Observation,
    pub envelope_id: Option<i64>,
}

#[derive(Debug, Default)]
pub struct FakeObservationSink {
    delivered: Mutex<Vec<DeliveredObservation>>,
    seen_envelopes: Mutex<HashSet<i64>>,
}

impl FakeObservationSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn delivered(&self) -> Vec<DeliveredObservation> {
        self.delivered.lock().expect("delivered lock").clone()
    }
}

#[async_trait]
impl ObservationSink for FakeObservationSink {
    async fn deliver(
        &self,
        session: &WorkerSessionId,
        observation: Observation,
        envelope_id: Option<i64>,
    ) -> Result<(), CoreError> {
        if let Some(id) = envelope_id
            && !self
                .seen_envelopes
                .lock()
                .expect("seen envelopes lock")
                .insert(id)
        {
            return Ok(());
        }
        self.delivered
            .lock()
            .expect("delivered lock")
            .push(DeliveredObservation {
                session_id: session.clone(),
                observation,
                envelope_id,
            });
        Ok(())
    }
}

#[derive(Debug)]
struct RootOnlyGate;

#[async_trait]
impl DecisionGate for RootOnlyGate {
    async fn decide<T>(
        &self,
        tx: &mut T,
        actor: &ActorId,
        scope: &EventScope,
        _event: &Event,
    ) -> TruthResult<GateDecision>
    where
        T: WriteTx + ?Sized + Send,
    {
        let EventScope::Wave { wave, .. } = scope else {
            return Ok(GateDecision::Deny(
                "root-only gate requires wave scope".into(),
            ));
        };
        // PR5 fake-only: actor_for_principal stores the session id in AiSpec's CardId slot.
        let caller_session_id = match actor {
            ActorId::AiSpec(card_id) => WorkerSessionId::from(card_id.as_str()),
            _ => {
                return Ok(GateDecision::Deny(format!(
                    "actor {actor} is not an agent session"
                )));
            }
        };
        let root = tx.read_wave_root_session_id(wave).await?;
        if root.as_ref() == Some(&caller_session_id) {
            Ok(GateDecision::Allow)
        } else {
            Ok(GateDecision::Deny(format!(
                "session {} is not wave root",
                caller_session_id
            )))
        }
    }
}

pub async fn full_loop_dispatch_to_lifecycle_done() {
    let (repo, wave_id) = crate::seeded_repo().await;
    let repo = Arc::new(repo);
    let wave = repo
        .wave_get(wave_id.as_str())
        .await
        .expect("wave get")
        .expect("wave exists");
    let cove_id = wave.cove_id;
    let root_sid = WorkerSessionId::from("ws-full-loop-root");
    let root_session = crate::session(root_sid.as_str(), wave_id.clone());

    write_in_tx_typed(repo.as_ref(), move |tx| {
        Box::pin(async move { session_insert_tx(tx, root_session).await })
    })
    .await
    .expect("seed root session");
    test_helpers::set_wave_root_session_for_test(repo.as_ref(), &wave_id, Some(&root_sid))
        .await
        .expect("set wave root session");
    set_lifecycle(repo.as_ref(), wave_id.clone(), WaveLifecycle::Planning).await;
    set_lifecycle(repo.as_ref(), wave_id.clone(), WaveLifecycle::Dispatching).await;
    set_lifecycle(repo.as_ref(), wave_id.clone(), WaveLifecycle::Working).await;
    set_lifecycle(repo.as_ref(), wave_id.clone(), WaveLifecycle::Reviewing).await;

    assert_eq!(
        wave_lifecycle(repo.as_ref(), &wave_id).await,
        WaveLifecycle::Reviewing
    );

    let exit_evidence = ExitEvidence {
        exit_code: Some(0),
        signal_killed: false,
        observed_at_ms: now_ms(),
        source: ExitSource::Probe,
    };
    let provider = FakeProvider::new().with_probe_script([
        Liveness::Idle,
        Liveness::Exited {
            evidence: exit_evidence.clone(),
        },
    ]);
    let root = FakeRoot::for_wave(root_sid.clone(), wave_id.clone(), cove_id.clone()).on(
        ObsMatcher::TaskCompleted,
        [DecisionIntent::LifecycleTransition {
            wave_id: wave_id.clone(),
            to: WaveLifecycle::Done,
            agent_message: Some("converged".into()),
        }],
    );
    let sink = GatedDecisionSink::new(
        Arc::clone(&repo),
        EventBus::new(),
        crate::write_context(),
        Arc::new(PermissiveGate),
    );

    let session = repo
        .session_get(&root_sid)
        .await
        .expect("session get")
        .expect("root session");
    let verdict = provider
        .interpret_exit(
            &session,
            &exit_evidence,
            &SpawnCtx::new(exit_evidence.observed_at_ms),
        )
        .await
        .expect("interpret exit");
    assert_eq!(verdict, ExitInterpretation::Completed);

    let observation = Observation::TaskCompleted {
        idempotency_key: "t-1".into(),
        result: json!({}),
    };
    let intents = root.react(&observation).await.expect("root reacts");
    assert_eq!(
        intents,
        vec![DecisionIntent::LifecycleTransition {
            wave_id: wave_id.clone(),
            to: WaveLifecycle::Done,
            agent_message: Some("converged".into()),
        }]
    );

    for intent in intents {
        sink.commit(&root.principal(), intent)
            .await
            .expect("gated decision commit");
    }

    assert_eq!(
        wave_lifecycle(repo.as_ref(), &wave_id).await,
        WaveLifecycle::Done
    );
    let events = repo
        .events_since(0, None)
        .await
        .expect("events since")
        .into_iter()
        .map(|(_, _, _, event)| event)
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 1);
    assert!(
        matches!(
            &events[0],
            Event::WaveLifecycleChanged {
                id,
                to: WaveLifecycle::Done,
                ..
            } if id == &wave_id
        ),
        "expected exactly one Done lifecycle event, got {events:?}"
    );
}

pub async fn full_loop_cross_principal_denied() {
    let (repo, wave_id) = crate::seeded_repo().await;
    let repo = Arc::new(repo);
    let wave = repo
        .wave_get(wave_id.as_str())
        .await
        .expect("wave get")
        .expect("wave exists");
    let cove_id = wave.cove_id;
    let root_sid = WorkerSessionId::from("ws-cross-principal-root");
    let non_root_sid = WorkerSessionId::from("ws-cross-principal-not-root");
    let root_session = crate::session(root_sid.as_str(), wave_id.clone());
    let non_root_session = crate::session(non_root_sid.as_str(), wave_id.clone());

    write_in_tx_typed(repo.as_ref(), move |tx| {
        Box::pin(async move {
            session_insert_tx(tx, root_session).await?;
            session_insert_tx(tx, non_root_session).await
        })
    })
    .await
    .expect("seed sessions");
    test_helpers::set_wave_root_session_for_test(repo.as_ref(), &wave_id, Some(&root_sid))
        .await
        .expect("set wave root session");
    set_lifecycle(repo.as_ref(), wave_id.clone(), WaveLifecycle::Planning).await;
    set_lifecycle(repo.as_ref(), wave_id.clone(), WaveLifecycle::Dispatching).await;
    set_lifecycle(repo.as_ref(), wave_id.clone(), WaveLifecycle::Working).await;
    set_lifecycle(repo.as_ref(), wave_id.clone(), WaveLifecycle::Reviewing).await;

    let before_events = repo.events_since(0, None).await.expect("events").len();
    let sink = GatedDecisionSink::new(
        Arc::clone(&repo),
        EventBus::new(),
        crate::write_context(),
        Arc::new(RootOnlyGate),
    );
    let non_root = Principal::Agent {
        session_id: non_root_sid,
        wave_id: wave_id.clone(),
        cove_id,
    };

    let denied = sink
        .commit(
            &non_root,
            DecisionIntent::LifecycleTransition {
                wave_id: wave_id.clone(),
                to: WaveLifecycle::Done,
                agent_message: Some("converged".into()),
            },
        )
        .await;
    assert!(
        matches!(denied, Err(CoreError::Forbidden(ref reason)) if reason == "session ws-cross-principal-not-root is not wave root"),
        "expected forbidden root gate denial, got {denied:?}"
    );
    assert_eq!(
        wave_lifecycle(repo.as_ref(), &wave_id).await,
        WaveLifecycle::Reviewing
    );
    assert_eq!(
        repo.events_since(0, None).await.expect("events").len(),
        before_events
    );
}

pub async fn fake_provider_contract() {
    let wave_id = WaveId::from("fake-provider-wave");
    let session = crate::session("fake-provider-session", wave_id);
    let exit_evidence = ExitEvidence {
        exit_code: Some(0),
        signal_killed: false,
        observed_at_ms: 123,
        source: ExitSource::Probe,
    };
    let provider = FakeProvider::new().with_probe_script([
        Liveness::Idle,
        Liveness::Exited {
            evidence: exit_evidence.clone(),
        },
    ]);
    let ctx = SpawnCtx::new(123);

    assert_eq!(provider.kind(), "fake");
    assert_eq!(provider.session_mode(), SessionMode::Ephemeral);
    assert_eq!(
        provider
            .probe_liveness(&session, &ctx)
            .await
            .expect("first probe"),
        Liveness::Idle
    );
    assert_eq!(
        provider
            .probe_liveness(&session, &ctx)
            .await
            .expect("second probe"),
        Liveness::Exited {
            evidence: exit_evidence.clone(),
        }
    );
    assert_eq!(provider.probe_call_count(), 2);
    assert_eq!(
        provider
            .interpret_exit(&session, &exit_evidence, &ctx)
            .await
            .expect("exit 0"),
        ExitInterpretation::Completed
    );
    assert!(
        matches!(
            provider
                .interpret_exit(
                    &session,
                    &ExitEvidence {
                        exit_code: Some(2),
                        signal_killed: false,
                        observed_at_ms: 124,
                        source: ExitSource::Probe,
                    },
                    &ctx,
                )
                .await,
            Ok(ExitInterpretation::Failed { .. })
        ),
        "nonzero exit must fail"
    );
    assert!(
        matches!(
            provider
                .interpret_exit(
                    &session,
                    &ExitEvidence {
                        exit_code: None,
                        signal_killed: true,
                        observed_at_ms: 125,
                        source: ExitSource::Probe,
                    },
                    &ctx,
                )
                .await,
            Ok(ExitInterpretation::Failed { .. })
        ),
        "signal exit must fail"
    );
    assert!(
        provider.resume(&session, &ctx).await.is_err(),
        "default resume must error for fake"
    );
}

pub async fn fake_root_contract() {
    let wave_id = WaveId::from("fake-root-wave");
    let cove_id = CoveId::from("fake-root-cove");
    let session_id = WorkerSessionId::from("fake-root-session");
    let done = DecisionIntent::LifecycleTransition {
        wave_id: wave_id.clone(),
        to: WaveLifecycle::Done,
        agent_message: Some("first".into()),
    };
    let fallback = DecisionIntent::LifecycleTransition {
        wave_id: wave_id.clone(),
        to: WaveLifecycle::Failed,
        agent_message: Some("fallback".into()),
    };
    let root = FakeRoot::for_wave(session_id.clone(), wave_id.clone(), cove_id.clone())
        .on(ObsMatcher::TaskCompleted, [done.clone()])
        .on(ObsMatcher::Any, [fallback]);

    assert_eq!(
        root.principal(),
        Principal::Agent {
            session_id,
            wave_id,
            cove_id,
        }
    );
    assert_eq!(
        root.react(&Observation::TaskCompleted {
            idempotency_key: "t-1".into(),
            result: json!({}),
        })
        .await
        .expect("matched reaction"),
        vec![done],
        "first matching script entry must win"
    );

    let empty = FakeRoot::for_wave(
        WorkerSessionId::from("empty-root-session"),
        WaveId::from("empty-wave"),
        CoveId::from("empty-cove"),
    );
    assert!(
        empty
            .react(&Observation::WaveGoal { text: "go".into() })
            .await
            .expect("no match reaction")
            .is_empty()
    );
}

pub async fn fake_observation_sink_contract() {
    let sink = FakeObservationSink::new();
    let session = WorkerSessionId::from("fake-observation-session");
    let observation = Observation::TaskCompleted {
        idempotency_key: "t-1".into(),
        result: json!({}),
    };

    sink.deliver(&session, observation.clone(), Some(7))
        .await
        .expect("first delivery");
    sink.deliver(&session, observation.clone(), Some(7))
        .await
        .expect("duplicate delivery");
    sink.deliver(&session, observation.clone(), None)
        .await
        .expect("synthetic delivery one");
    sink.deliver(&session, observation, None)
        .await
        .expect("synthetic delivery two");

    let delivered = sink.delivered();
    assert_eq!(delivered.len(), 3);
    assert_eq!(delivered[0].envelope_id, Some(7));
    assert_eq!(delivered[1].envelope_id, None);
    assert_eq!(delivered[2].envelope_id, None);
}

async fn set_lifecycle(repo: &SqlxRepo, wave_id: WaveId, lifecycle: WaveLifecycle) {
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            wave_update_tx(
                tx,
                wave_id.as_str(),
                WavePatch {
                    lifecycle: Some(lifecycle),
                    ..Default::default()
                },
            )
            .await
        })
    })
    .await
    .expect("set wave lifecycle");
}

async fn wave_lifecycle(repo: &SqlxRepo, wave_id: &WaveId) -> WaveLifecycle {
    repo.wave_get(wave_id.as_str())
        .await
        .expect("wave get")
        .expect("wave exists")
        .lifecycle
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_for_principal_maps_principal_identity() {
        let wave_id = WaveId::from("actor-map-wave");
        let cove_id = CoveId::from("actor-map-cove");
        let root = Principal::Agent {
            session_id: WorkerSessionId::from("actor-map-root"),
            wave_id: wave_id.clone(),
            cove_id: cove_id.clone(),
        };
        let non_root = Principal::Agent {
            session_id: WorkerSessionId::from("actor-map-non-root"),
            wave_id,
            cove_id,
        };

        assert_eq!(
            actor_for_principal(&root),
            ActorId::AiSpec(CardId::from("actor-map-root"))
        );
        assert_eq!(
            actor_for_principal(&non_root),
            ActorId::AiSpec(CardId::from("actor-map-non-root"))
        );
        assert_eq!(actor_for_principal(&Principal::Kernel), ActorId::Kernel);
    }
}
