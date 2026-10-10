//! Fake calm-exec implementations for contract tests.

use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use calm_exec::{
    AgentReactor, DecisionIntent, DecisionSink, ObservationSink, SpawnCtx, WorkerProvider,
};
use calm_types::error::CoreError;
use calm_types::ids::{AreaId, TrackId};
use calm_types::observation::Observation;
use calm_types::runtime::TimestampMs;
use calm_types::worker::{
    DeathVerdict, ExitEvidence, ExitInterpretation, ExitSource, Liveness, Principal, SessionMode,
    WorkerSession, WorkerSessionId,
};
use serde_json::json;

#[derive(Debug)]
pub struct FakeProvider {
    probe_script: Mutex<VecDeque<Liveness>>,
    probe_calls: AtomicUsize,
    session_mode: SessionMode,
    // The verdict `confirm_durable_death` returns; `None` ⇒ trait default (`Unknown`).
    death_verdict: Option<DeathVerdict>,
    death_verdict_calls: AtomicUsize,
    // The value `daemon_connected_at_ms` returns; `None` ⇒ trait default (the reaper treats it as 0).
    daemon_connected_at_ms: Option<TimestampMs>,
}

impl Default for FakeProvider {
    fn default() -> Self {
        Self {
            probe_script: Mutex::default(),
            probe_calls: AtomicUsize::default(),
            session_mode: SessionMode::Ephemeral,
            death_verdict: None,
            death_verdict_calls: AtomicUsize::default(),
            daemon_connected_at_ms: None,
        }
    }
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

    /// Override the reported [`SessionMode`]. A `Resumable` fake stands in for codex, whose torn-down PTY does not
    /// mean the codex thread died — the reaper must NOT converge it.
    pub fn with_session_mode(mut self, mode: SessionMode) -> Self {
        self.session_mode = mode;
        self
    }

    /// Script the [`DeathVerdict`] returned by `confirm_durable_death`; default defers to the trait default (`Unknown`).
    pub fn with_death_verdict(mut self, verdict: DeathVerdict) -> Self {
        self.death_verdict = Some(verdict);
        self
    }

    /// Override the value `daemon_connected_at_ms` reports.
    pub fn with_daemon_connected_at_ms(mut self, ms: TimestampMs) -> Self {
        self.daemon_connected_at_ms = Some(ms);
        self
    }

    pub fn probe_call_count(&self) -> usize {
        self.probe_calls.load(Ordering::SeqCst)
    }

    /// How many times `confirm_durable_death` was consulted — lets a test assert the reaper's pre-gate short-circuited.
    pub fn death_verdict_call_count(&self) -> usize {
        self.death_verdict_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl WorkerProvider for FakeProvider {
    fn kind(&self) -> &'static str {
        "fake"
    }

    fn session_mode(&self) -> SessionMode {
        self.session_mode
    }

    fn tui_input(&self) -> calm_exec::TuiInput {
        calm_exec::TuiInput::KeysOnly
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
        // Mirror the real ephemeral providers: a `Probe`-sourced exit carries the supervisor's `-1` sentinel, which the reason must HIDE.
        if evidence.source == ExitSource::Probe {
            return Ok(ExitInterpretation::Failed {
                reason: "fake worker exited (outcome unknown; observed via supervisor probe)"
                    .into(),
            });
        }
        Ok(ExitInterpretation::Failed {
            reason: match (evidence.exit_code, evidence.signal_killed) {
                (Some(code), false) => format!("fake worker exited with code {code}"),
                (_, true) => "fake worker was signal-killed".into(),
                (None, false) => "fake worker exited without a code".into(),
            },
        })
    }

    async fn confirm_durable_death(
        &self,
        _thread_id: &str,
        _now_ms: TimestampMs,
        _daemon_connected_at_ms: TimestampMs,
        _rebuild_grace_ms: i64,
    ) -> DeathVerdict {
        self.death_verdict_calls.fetch_add(1, Ordering::SeqCst);
        self.death_verdict.unwrap_or(DeathVerdict::Unknown)
    }

    fn daemon_connected_at_ms(&self) -> Option<TimestampMs> {
        self.daemon_connected_at_ms
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
    track_id: TrackId,
    area_id: AreaId,
    script: Vec<(ObsMatcher, Vec<DecisionIntent>)>,
}

impl FakeRoot {
    pub fn for_track(session_id: WorkerSessionId, track_id: TrackId, area_id: AreaId) -> Self {
        Self {
            session_id,
            track_id,
            area_id,
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
            track_id: self.track_id.clone(),
            area_id: self.area_id.clone(),
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

pub async fn fake_provider_contract() {
    let track_id = TrackId::from("fake-provider-track");
    let session = crate::session("fake-provider-session", track_id);
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
    let track_id = TrackId::from("fake-root-track");
    let area_id = AreaId::from("fake-root-area");
    let session_id = WorkerSessionId::from("fake-root-session");
    let done = DecisionIntent::ReportWrite {
        track_id: track_id.clone(),
        summary: Some("done".into()),
        body: None,
        agent_message: Some("first".into()),
    };
    let fallback = DecisionIntent::ReportWrite {
        track_id: track_id.clone(),
        summary: Some("fallback".into()),
        body: None,
        agent_message: Some("fallback".into()),
    };
    let root = FakeRoot::for_track(session_id.clone(), track_id.clone(), area_id.clone())
        .on(ObsMatcher::TaskCompleted, [done.clone()])
        .on(ObsMatcher::Any, [fallback]);

    assert_eq!(
        root.principal(),
        Principal::Agent {
            session_id,
            track_id,
            area_id,
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

    let empty = FakeRoot::for_track(
        WorkerSessionId::from("empty-root-session"),
        TrackId::from("empty-track"),
        AreaId::from("empty-area"),
    );
    assert!(
        empty
            .react(&Observation::TrackGoal { text: "go".into() })
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
