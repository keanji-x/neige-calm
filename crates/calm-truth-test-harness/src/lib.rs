//! Scoped T1-T4 truth-layer conformance for #679 PR2.
//!
//! This crate intentionally sits one hop away from `calm-exec` so
//! `cargo tree --depth 2 -p calm-exec --all-targets` keeps `sqlx` out of
//! the `calm-exec` tree while the conformance implementation still exercises
//! real calm-truth SQLite APIs.

pub mod fakes;

use std::sync::Arc;

use async_trait::async_trait;
use calm_exec::{SpawnCtx, WorkerProvider};
use calm_truth::card_role_cache::CardRoleCache;
use calm_truth::db::sqlite::{
    SqlxRepo, append_decision_event_in_tx, begin_immediate_tx, runtime_set_harness_observation_tx,
    runtime_start_tx, session_insert_tx, session_state_transition_tx,
};
use calm_truth::db::{RepoEventWrite, RepoSyncDomainRaw, write_in_tx_typed};
use calm_truth::decision_gate::{
    DecisionGate, GateDecision, PermissiveGate, WriteTx, commit_decision,
};
use calm_truth::error::{Result as TruthResult, TruthError};
use calm_truth::event::{Event, EventBus, EventScope};
use calm_truth::ids::ActorId;
use calm_truth::model::{NewCard, NewCove, NewWave, RequestTheme, new_id, now_ms};
use calm_truth::runtime_repo::{AgentProvider, RunStatus, RuntimeInit, RuntimeKind, RuntimeRepo};
use calm_truth::session_repo::SessionRepo;
use calm_truth::state::WriteContext;
use calm_truth::test_helpers;
use calm_truth::wave_cove_cache::WaveCoveCache;
use calm_types::ids::WaveId;
use calm_types::worker::{
    ExitEvidence, ExitInterpretation, ExitSource, Liveness, LivenessTag, SessionMode,
    WorkerContract, WorkerProviderKind, WorkerSession, WorkerSessionId, WorkerSessionState,
};

async fn seeded_repo() -> (SqlxRepo, WaveId) {
    let repo = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open migrated sqlite repo");
    let cove = repo
        .cove_create(NewCove {
            name: "truth conformance".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .expect("seed cove");
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "truth conformance".into(),
            sort: None,
            cwd: "/tmp".into(),
            attach_folder: true,
            theme: RequestTheme::default_dark(),
        })
        .await
        .expect("seed wave");
    (repo, wave.id)
}

fn write_context() -> WriteContext {
    WriteContext::new(CardRoleCache::new(), WaveCoveCache::new())
}

fn session(id: &str, wave_id: WaveId) -> WorkerSession {
    WorkerSession {
        id: WorkerSessionId::from(id),
        wave_id,
        provider: WorkerProviderKind::Codex,
        mode: SessionMode::Resumable,
        contract: WorkerContract::Planner,
        parent_session_id: None,
        requester_session_id: None,
        state: WorkerSessionState::Starting,
        mcp_token_hash: None,
        thread_id: None,
        agent_session_id: None,
        active_turn_id: None,
        terminal_run_id: None,
        handle_state_json: None,
        liveness: LivenessTag::Unknown,
        liveness_probed_at_ms: None,
        exit_code: None,
        exit_interpretation: None,
        spawn_op_id: None,
        last_activity_ms: None,
        last_thread_status: None,
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    }
}

fn conformance_event(state: &str) -> Event {
    Event::PluginState {
        id: "truth-conformance".into(),
        state: state.into(),
        last_error: None,
    }
}

#[derive(Debug, Default)]
struct DenyGate;

#[async_trait]
impl DecisionGate for DenyGate {
    async fn decide<T>(
        &self,
        _tx: &mut T,
        _actor: &ActorId,
        _scope: &EventScope,
        _event: &Event,
    ) -> TruthResult<GateDecision>
    where
        T: WriteTx + ?Sized + Send,
    {
        Ok(GateDecision::Deny("test gate denied".into()))
    }
}

#[derive(Debug)]
struct DenyOnRoot {
    wave_id: WaveId,
    caller_session_id: WorkerSessionId,
}

#[async_trait]
impl DecisionGate for DenyOnRoot {
    async fn decide<T>(
        &self,
        tx: &mut T,
        _actor: &ActorId,
        _scope: &EventScope,
        _event: &Event,
    ) -> TruthResult<GateDecision>
    where
        T: WriteTx + ?Sized + Send,
    {
        let root = tx.read_wave_root_session_id(&self.wave_id).await?;
        if root.as_ref() == Some(&self.caller_session_id) {
            Ok(GateDecision::Allow)
        } else {
            Ok(GateDecision::Deny(format!(
                "session {} is not wave root",
                self.caller_session_id
            )))
        }
    }
}

pub async fn invariant_t1_decision_write_couples_state_and_event<G>(gate: Arc<G>)
where
    G: DecisionGate + 'static,
{
    let (repo, wave_id) = seeded_repo().await;
    let bus = EventBus::new();
    let write = write_context();
    let state = session("ws-t1", wave_id);
    let actor = ActorId::Kernel;
    let scope = EventScope::System;
    let before = repo.events_since(0, None).await.expect("event count").len();
    let event = conformance_event("t1");

    let (inserted, event_id) = commit_decision(
        &repo,
        Arc::clone(&gate),
        actor.clone(),
        scope.clone(),
        None,
        &bus,
        &write,
        event,
        move |tx| {
            Box::pin(async move {
                let inserted = session_insert_tx(tx, state).await?;
                Ok(inserted)
            })
        },
    )
    .await
    .expect("decision write commits");

    assert_eq!(inserted.id.as_str(), "ws-t1");
    assert!(event_id > 0);
    assert!(
        repo.session_get(&WorkerSessionId::from("ws-t1"))
            .await
            .expect("session get")
            .is_some()
    );
    let after = repo.events_since(0, None).await.expect("event count");
    assert_eq!(after.len(), before + 1);
}

pub async fn invariant_t1_saga_in_tx_decision_write_couples_state_and_event() {
    let (repo, wave_id) = seeded_repo().await;
    let state = session("ws-t1-saga", wave_id);
    let actor = ActorId::Kernel;
    let scope = EventScope::System;
    let before = repo.events_since(0, None).await.expect("event count").len();
    let event = conformance_event("t1-saga");

    let mut tx = begin_immediate_tx(repo.pool())
        .await
        .expect("begin saga tx");
    let inserted = session_insert_tx(&mut tx, state)
        .await
        .expect("insert session in saga tx");
    let event_id =
        append_decision_event_in_tx(&mut tx, &PermissiveGate, &actor, &scope, None, &event)
            .await
            .expect("append decision event in saga tx");
    tx.commit().await.expect("commit saga tx");

    assert_eq!(inserted.id.as_str(), "ws-t1-saga");
    assert!(event_id > 0);
    assert!(
        repo.session_get(&WorkerSessionId::from("ws-t1-saga"))
            .await
            .expect("session get")
            .is_some()
    );
    let after = repo.events_since(0, None).await.expect("event count");
    assert_eq!(after.len(), before + 1);
}

pub async fn invariant_t1_denied_decision_rolls_back_state_and_event() {
    let (repo, wave_id) = seeded_repo().await;
    let bus = EventBus::new();
    let write = write_context();
    let state = session("ws-t1-denied", wave_id);
    let actor = ActorId::Kernel;
    let scope = EventScope::System;
    let before = repo.events_since(0, None).await.expect("event count").len();

    let result = commit_decision(
        &repo,
        Arc::new(DenyGate),
        actor,
        scope,
        None,
        &bus,
        &write,
        conformance_event("t1-denied"),
        move |tx| Box::pin(async move { session_insert_tx(tx, state).await }),
    )
    .await;

    match result {
        Err(TruthError::Forbidden(reason)) => assert_eq!(reason, "test gate denied"),
        other => panic!("expected forbidden deny, got {other:?}"),
    }
    assert!(
        repo.session_get(&WorkerSessionId::from("ws-t1-denied"))
            .await
            .expect("session get")
            .is_none()
    );
    let after = repo.events_since(0, None).await.expect("event count");
    assert_eq!(after.len(), before);
}

pub async fn invariant_t1_gate_can_read_wave_root_inside_tx() {
    let (repo, wave_id) = seeded_repo().await;
    let bus = EventBus::new();
    let write = write_context();
    let actor = ActorId::Kernel;
    let scope = EventScope::System;
    let root_id = WorkerSessionId::from("ws-root");
    let root_state = session(root_id.as_str(), wave_id.clone());

    write_in_tx_typed(&repo, move |tx| {
        Box::pin(async move { session_insert_tx(tx, root_state).await })
    })
    .await
    .expect("seed root session");
    test_helpers::set_wave_root_session_for_test(&repo, &wave_id, Some(&root_id))
        .await
        .expect("set root session");

    let before = repo.events_since(0, None).await.expect("event count").len();
    let denied_state = session("ws-root-denied", wave_id.clone());
    let denied_gate = DenyOnRoot {
        wave_id: wave_id.clone(),
        caller_session_id: WorkerSessionId::from("ws-not-root"),
    };
    let denied = commit_decision(
        &repo,
        Arc::new(denied_gate),
        actor.clone(),
        scope.clone(),
        None,
        &bus,
        &write,
        conformance_event("root-denied"),
        move |tx| Box::pin(async move { session_insert_tx(tx, denied_state).await }),
    )
    .await;
    assert!(
        matches!(denied, Err(TruthError::Forbidden(ref reason)) if reason == "session ws-not-root is not wave root"),
        "expected root gate deny, got {denied:?}"
    );
    assert!(
        repo.session_get(&WorkerSessionId::from("ws-root-denied"))
            .await
            .expect("session get")
            .is_none()
    );
    assert_eq!(
        repo.events_since(0, None).await.expect("event count").len(),
        before
    );

    let allowed_state = session("ws-root-allowed", wave_id.clone());
    let allowed_gate = DenyOnRoot {
        wave_id,
        caller_session_id: root_id,
    };
    let (_inserted, event_id) = commit_decision(
        &repo,
        Arc::new(allowed_gate),
        actor,
        scope,
        None,
        &bus,
        &write,
        conformance_event("root-allowed"),
        move |tx| Box::pin(async move { session_insert_tx(tx, allowed_state).await }),
    )
    .await
    .expect("root caller allowed");
    assert!(event_id > 0);
    assert!(
        repo.session_get(&WorkerSessionId::from("ws-root-allowed"))
            .await
            .expect("session get")
            .is_some()
    );
    assert_eq!(
        repo.events_since(0, None).await.expect("event count").len(),
        before + 1
    );
}

pub async fn invariant_t2_observation_writes_can_skip_events() {
    let (repo, wave_id) = seeded_repo().await;
    let card = repo
        .card_create(NewCard {
            wave_id,
            kind: "plugin:test:worker".into(),
            sort: None,
            payload: Default::default(),
        })
        .await
        .expect("seed card");
    let runtime_id = new_id();

    write_in_tx_typed(&repo, {
        let runtime_id = runtime_id.clone();
        let card_id = card.id.to_string();
        move |tx| {
            Box::pin(async move {
                runtime_start_tx(
                    tx,
                    RuntimeInit {
                        id: runtime_id,
                        card_id,
                        kind: RuntimeKind::SharedSpec,
                        agent_provider: Some(AgentProvider::Codex),
                        status: RunStatus::Idle,
                        terminal_run_id: None,
                        thread_id: None,
                        session_id: None,
                        active_turn_id: None,
                        handle_state_json: None,
                        lease_owner: None,
                        lease_until_ms: None,
                        spawn_op_id: None,
                        now_ms: now_ms(),
                    },
                )
                .await
                .map_err(TruthError::from)
            })
        }
    })
    .await
    .expect("seed idle runtime");

    let before = repo.events_since(0, None).await.expect("events").len();

    write_in_tx_typed(&repo, {
        let runtime_id = runtime_id.clone();
        move |tx| {
            Box::pin(async move {
                runtime_set_harness_observation_tx(
                    tx,
                    &runtime_id,
                    RunStatus::TurnPending,
                    Some("t-1"),
                    Some("turn-1"),
                )
                .await
                .map_err(TruthError::from)
            })
        }
    })
    .await
    .expect("observation write skips status matrix");

    let runtime = repo
        .runtime_get_by_id(&runtime_id)
        .await
        .expect("runtime get")
        .expect("runtime row");
    assert_eq!(runtime.status, RunStatus::TurnPending);
    let after = repo.events_since(0, None).await.expect("events").len();
    assert_eq!(after, before, "observation write must emit no event");
}

pub async fn invariant_t3_state_is_not_fold_events<G>(gate: Arc<G>)
where
    G: DecisionGate + 'static,
{
    let (repo, wave_id) = seeded_repo().await;
    let bus = EventBus::new();
    let write = write_context();
    let session_id = WorkerSessionId::from("ws-t3");
    let state = session(session_id.as_str(), wave_id);
    let actor = ActorId::Kernel;
    let scope = EventScope::System;
    let event = conformance_event("t3");

    let (_inserted, event_id) = commit_decision(
        &repo,
        Arc::clone(&gate),
        actor.clone(),
        scope.clone(),
        None,
        &bus,
        &write,
        event,
        move |tx| {
            Box::pin(async move {
                let mut inserted = session_insert_tx(tx, state).await?;
                inserted =
                    session_state_transition_tx(tx, &inserted.id, WorkerSessionState::Running)
                        .await?;
                Ok(inserted)
            })
        },
    )
    .await
    .expect("decision write commits");

    test_helpers::delete_event_for_test(&repo, event_id)
        .await
        .expect("delete event row");

    let persisted = repo
        .session_get(&session_id)
        .await
        .expect("session survives event deletion")
        .expect("session row exists");
    assert_eq!(persisted.state, WorkerSessionState::Running);
    assert!(
        repo.events_since(0, None)
            .await
            .expect("events after delete")
            .is_empty()
    );
}

pub fn invariant_t4_no_operations_read_api() {
    // #679 PR4 T4 is a doc/grep firewall: operation sagas have no public
    // truth-layer event append entrance that skips DecisionGate. CI enforces
    // that the retired operation-only append symbols do not exist under
    // `crates/`; saga appends must use `append_decision_event(s)_in_tx`.
}

pub async fn provider_conformance<P: WorkerProvider>(p: P) {
    let expected_mode = expected_session_mode(p.kind());
    assert_eq!(p.session_mode(), expected_mode);

    let ctx = SpawnCtx::new(17);
    let session = provider_session(p.kind(), expected_mode);
    let liveness = p
        .probe_liveness(&session, &ctx)
        .await
        .expect("provider probe succeeds");
    match p.kind() {
        "fake" => assert_eq!(
            liveness,
            Liveness::Alive {
                active_turn_id: None
            }
        ),
        "terminal" | "claude" | "codex" => {
            assert_eq!(
                liveness,
                Liveness::Unknown {
                    since_ms: ctx.now_ms
                }
            );
        }
        other => panic!("unexpected provider kind {other}"),
    }

    assert_eq!(
        p.interpret_exit(&session, &exit_evidence(Some(0), false), &ctx)
            .await
            .expect("zero exit interpretation"),
        ExitInterpretation::Completed
    );
    assert_failed(
        p.interpret_exit(&session, &exit_evidence(Some(2), false), &ctx)
            .await
            .expect("nonzero exit interpretation"),
        p.kind(),
    );
    let signal = p
        .interpret_exit(&session, &exit_evidence(None, true), &ctx)
        .await
        .expect("signal exit interpretation");
    if p.kind() == "codex" {
        assert_eq!(signal, ExitInterpretation::PreserveCard);
    } else {
        assert_failed(signal, p.kind());
    }

    let resume = p.resume(&session, &ctx).await;
    match expected_mode {
        SessionMode::Ephemeral => {
            let err = resume.expect_err("ephemeral provider resume must error");
            assert!(
                err.to_string().contains("not resumable"),
                "unexpected ephemeral resume error: {err}"
            );
        }
        SessionMode::Resumable => {
            let err = resume.expect_err("codex resume is a PR8 seam in this PR");
            assert!(
                err.to_string().contains("#679 PR8"),
                "unexpected codex resume seam error: {err}"
            );
        }
    }
}

pub async fn provider_conformance_fake() {
    provider_conformance(FakeProvider::new().with_probe_script([Liveness::Alive {
        active_turn_id: None,
    }]))
    .await;
}

pub async fn t1_decision_write_couples_state_and_event() {
    invariant_t1_decision_write_couples_state_and_event(Arc::new(PermissiveGate)).await;
}

pub async fn t1_saga_in_tx_decision_write_couples_state_and_event() {
    invariant_t1_saga_in_tx_decision_write_couples_state_and_event().await;
}

pub async fn t1_denied_decision_rolls_back_state_and_event() {
    invariant_t1_denied_decision_rolls_back_state_and_event().await;
}

pub async fn t1_gate_can_read_wave_root_inside_tx() {
    invariant_t1_gate_can_read_wave_root_inside_tx().await;
}

pub async fn t2_observation_writes_can_skip_events() {
    invariant_t2_observation_writes_can_skip_events().await;
}

pub async fn t3_state_is_not_fold_events() {
    invariant_t3_state_is_not_fold_events(Arc::new(PermissiveGate)).await;
}

pub fn t4_no_operations_read_api() {
    invariant_t4_no_operations_read_api();
}

pub use fakes::*;

fn expected_session_mode(kind: &str) -> SessionMode {
    match kind {
        "codex" => SessionMode::Resumable,
        "fake" | "terminal" | "claude" => SessionMode::Ephemeral,
        other => panic!("unexpected provider kind {other}"),
    }
}

fn provider_session(kind: &str, mode: SessionMode) -> WorkerSession {
    let provider = match kind {
        "codex" => WorkerProviderKind::Codex,
        "claude" => WorkerProviderKind::Claude,
        "terminal" | "fake" => WorkerProviderKind::Terminal,
        other => panic!("unexpected provider kind {other}"),
    };
    WorkerSession {
        id: WorkerSessionId::from("ws-provider-conformance"),
        wave_id: WaveId("wave-provider-conformance".into()),
        provider,
        mode,
        contract: WorkerContract::Planner,
        parent_session_id: None,
        requester_session_id: None,
        state: WorkerSessionState::Running,
        mcp_token_hash: None,
        thread_id: Some("thread-provider-conformance".into()),
        agent_session_id: None,
        active_turn_id: None,
        terminal_run_id: Some("term-provider-conformance".into()),
        handle_state_json: None,
        liveness: LivenessTag::Unknown,
        liveness_probed_at_ms: None,
        exit_code: None,
        exit_interpretation: None,
        spawn_op_id: None,
        last_activity_ms: None,
        last_thread_status: None,
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    }
}

fn exit_evidence(exit_code: Option<i32>, signal_killed: bool) -> ExitEvidence {
    ExitEvidence {
        exit_code,
        signal_killed,
        observed_at_ms: 17,
        source: ExitSource::AttachReader,
    }
}

fn assert_failed(interpretation: ExitInterpretation, kind: &str) {
    match interpretation {
        ExitInterpretation::Failed { reason } => {
            assert!(
                reason.contains(kind),
                "failure reason should name provider kind `{kind}`: {reason}"
            );
        }
        other => panic!("expected failed interpretation, got {other:?}"),
    }
}
