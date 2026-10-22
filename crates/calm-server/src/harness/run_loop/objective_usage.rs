//! Transient turn endpoints only: never part of a runtime snapshot.
use super::*;
use calm_truth::objective_host::{TurnUsage, record_turn_usage};

#[derive(Default)]
pub(super) struct TurnMeter {
    start: Option<(Option<i64>, Instant)>,
    maintenance: bool,
    settled: Option<String>,
    pending: Option<(String, TurnUsage)>,
}

impl TurnMeter {
    fn settle(&mut self, turn: &str) {
        self.start = None;
        self.pending = None;
        self.settled = Some(turn.to_owned());
    }

    fn observe(
        &mut self,
        phase: HarnessPhaseTag,
        turn: Option<&str>,
        reading: Option<i64>,
    ) -> Option<(String, TurnUsage)> {
        match phase {
            HarnessPhaseTag::IssuingTurn => {
                if self.start.is_none() {
                    self.start = Some((reading, Instant::now()));
                    self.maintenance = false;
                }
            }
            HarnessPhaseTag::TurnRunning => {
                // A recovered in-flight turn has no visible initial endpoint.
                self.start.get_or_insert((None, Instant::now()));
                self.maintenance = false;
            }
            HarnessPhaseTag::TurnCompleted => {
                // The committed END owns the identity. Running is optional, e.g.
                // when END arrives before the scheduler publishes the native ACK.
                if let Some((pending, usage)) = &self.pending
                    && Some(pending.as_str()) == turn
                {
                    return Some((pending.clone(), *usage));
                }
                let start = self.start.take();
                if !self.maintenance
                    && let Some(turn) = turn
                    && self.settled.as_deref() != Some(turn)
                {
                    let (initial, at) = start.unwrap_or((None, Instant::now()));
                    let context_growth = match (initial, reading) {
                        (Some(initial), Some(end)) => end.saturating_sub(initial).max(0) as u64,
                        _ => 0,
                    };
                    self.pending = Some((
                        turn.to_owned(),
                        TurnUsage {
                            context_growth,
                            secs: at.elapsed().as_secs(),
                        },
                    ));
                    return self.pending.clone();
                }
            }
            HarnessPhaseTag::Idle => self.start = None,
            HarnessPhaseTag::Compacting => {
                self.start = None;
                self.maintenance = true;
            }
            _ => {}
        }
        None
    }
}

pub(super) async fn observe_canonical(
    inner: &Inner,
    header: calm_types::harness::snapshot::HarnessRuntimeHeader,
) {
    // failed_end preserves the wedge and Failed session; END grants no write authority.
    let failed_end =
        header.phase == HarnessPhaseTag::TurnCompleted && header.wedged_reason.is_some();
    let phase = header.phase;
    let turn = header.last_turn_id.clone();
    let reading = crate::harness::token_usage::planner_context_usage_from_header(header)
        .map(|r| r.used_tokens);
    observe(inner, phase, turn.as_deref(), reading, failed_end).await;
}

pub(super) async fn observe(
    inner: &Inner,
    phase: HarnessPhaseTag,
    turn: Option<&str>,
    reading: Option<i64>,
    failed_end: bool,
) {
    let mut meter = inner.objective_usage.lock().await;
    // Failed executions remain Failed: do not send their END through usage authorization.
    if failed_end {
        if let Some(turn) = turn {
            meter.settle(turn);
        }
        return;
    }
    let usage = meter.observe(phase, turn, reading);
    let Some((turn, usage)) = usage else { return };
    let Some(db) = inner.repo.sqlite_db() else {
        return;
    };
    if let Err(error) = record_turn_usage(
        &db,
        &inner.track_id,
        inner.worker_session_id.clone().into(),
        turn.clone(),
        usage,
    )
    .await
    {
        tracing::warn!(track = %inner.track_id, %error, "objective turn usage recording failed");
    } else {
        // Recorded, Duplicate and capability-disabled Skipped all settle the identity.
        meter.settle(&turn);
    }
}

#[cfg(all(test, feature = "fixtures"))]
pub(super) async fn exercise_objective_usage(inner: &Arc<Inner>) {
    use calm_types::track_capability::TrackCapability;
    let db = inner.repo.sqlite_db().unwrap();
    let mut tx = crate::db::sqlite::begin_immediate_tx(&db).await.unwrap();
    crate::db::sqlite::track_capabilities::stamp_tx(
        &mut tx,
        &inner.track_id,
        &[TrackCapability::Objectives],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    async fn checkpoint(
        inner: &Arc<Inner>,
        phase: HarnessPhaseTag,
        turn: &str,
        count: i64,
        invalid: bool,
    ) {
        let usage = Some(TokenUsage {
            used_tokens: count,
            total_tokens: 9000000,
            context_window: Some(200000),
            at_ms: 1,
        });
        if inner.inputs.canonical().is_some() {
            let mut next = canonical::read_current(inner).await.unwrap();
            next.snapshot.header.phase = phase;
            next.snapshot.header.last_turn_id = Some(turn.into());
            next.snapshot.header.token_usage = usage;
            next.snapshot.header.context_usage_invalidated = invalid;
            let card = inner.card_id.to_string();
            let runtime = inner.worker_session_id.clone();
            let next = write_in_tx_typed(inner.repo.as_ref(), move |tx| {
                Box::pin(async move {
                    canonical::written(
                        crate::db::sqlite::conversation::persist_runtime_frame_tx(
                            tx,
                            &card,
                            &runtime,
                            &next.fence,
                            crate::harness::snapshot::canonical_runtime_header_value(
                                &next.snapshot,
                            )?,
                        )
                        .await?,
                    )
                })
            })
            .await
            .unwrap();
            canonical::publish(inner, next).await;
        } else {
            *inner.state.lock().await = match phase {
                HarnessPhaseTag::IssuingTurn => HarnessState::Issuing {
                    since: Instant::now(),
                    kind: IssuingKind::TurnStart,
                },
                HarnessPhaseTag::TurnRunning => {
                    HarnessState::turn_running(turn.into(), Instant::now())
                }
                HarnessPhaseTag::Compacting => HarnessState::Compacting {
                    since: Instant::now(),
                },
                _ => HarnessState::TurnCompleted {
                    last_turn_id: turn.into(),
                },
            };
            *inner.last_turn_id.lock().await = Some(turn.into());
            *inner.token_usage.lock().await = usage;
            *inner.context_usage_invalidation.lock().await =
                invalid.then_some(compact_command::UsageInvalidation { user_turn_id: None });
            persist_snapshot(inner).await.unwrap();
        }
    }
    for (turn, start, end, invalid, expected) in [
        ("direct-end", 30000, 40000, false, 10000),
        ("normal", 40000, 50000, false, 20000),
        ("after-compact", 50000, 20000, true, 20000),
        ("falling", 20000, 10000, false, 20000),
    ] {
        checkpoint(inner, HarnessPhaseTag::IssuingTurn, turn, start, invalid).await;
        if turn != "direct-end" {
            checkpoint(inner, HarnessPhaseTag::TurnRunning, turn, start, invalid).await;
        }
        checkpoint(inner, HarnessPhaseTag::TurnCompleted, turn, end, false).await;
        let (_, state) = calm_truth::objective_host::read_state(db.read_pool(), &inner.track_id)
            .await
            .unwrap();
        assert_eq!(state.usage.context_tokens, expected, "{turn}");
    }
    checkpoint(
        inner,
        HarnessPhaseTag::Compacting,
        "maintenance",
        10000,
        false,
    )
    .await;
    checkpoint(
        inner,
        HarnessPhaseTag::TurnCompleted,
        "maintenance",
        80000,
        true,
    )
    .await;
    let (_, state) = calm_truth::objective_host::read_state(db.read_pool(), &inner.track_id)
        .await
        .unwrap();
    assert_eq!(state.usage.turns, 4, "maintenance must not report");
}

#[cfg(all(test, feature = "fixtures"))]
#[tokio::test]
async fn objective_historical_usage_endpoints_and_maintenance() {
    let fixture = super::completed_commit_tests::Fixture::new().await;
    exercise_objective_usage(&fixture.harness.inner).await;
}

#[cfg(test)]
#[test]
fn objective_completion_without_running_or_initial_reading() {
    {
        let intermediate = HarnessPhaseTag::IssuingInterrupt;
        let mut meter = TurnMeter::default();
        meter.observe(HarnessPhaseTag::IssuingTurn, Some("previous"), None);
        meter.start.as_mut().unwrap().1 = Instant::now() - Duration::from_secs(3);
        assert!(meter.observe(intermediate, Some("actual"), None).is_none());
        let (turn, usage) = meter
            .observe(HarnessPhaseTag::TurnCompleted, Some("actual"), Some(50000))
            .expect("END settles even without Running or a visible initial endpoint");
        assert_eq!(turn, "actual");
        assert_eq!(usage.context_growth, 0);
        assert!(usage.secs >= 3);
    }
    let mut meter = TurnMeter::default();
    let (_, usage) = meter
        .observe(
            HarnessPhaseTag::TurnCompleted,
            Some("recovered"),
            Some(50000),
        )
        .expect("a recovered END still counts a turn");
    assert_eq!(usage.context_growth, 0);
    assert_eq!(usage.secs, 0);
    meter.settle("recovered");
    assert!(
        meter
            .observe(
                HarnessPhaseTag::TurnCompleted,
                Some("recovered"),
                Some(50000)
            )
            .is_none()
    );
    meter.observe(HarnessPhaseTag::IssuingTurn, None, Some(40000));
    assert!(
        meter
            .observe(HarnessPhaseTag::Idle, Some("previous"), Some(50000))
            .is_none()
    );
    meter.observe(HarnessPhaseTag::Compacting, Some("previous"), None);
    for _ in 0..2 {
        assert!(
            meter
                .observe(
                    HarnessPhaseTag::TurnCompleted,
                    Some("maintenance"),
                    Some(50000)
                )
                .is_none()
        );
    }
    meter.observe(HarnessPhaseTag::IssuingTurn, None, Some(40000));
    assert!(
        meter
            .observe(
                HarnessPhaseTag::TurnCompleted,
                Some("next-user"),
                Some(50000)
            )
            .is_some()
    );
}

#[cfg(all(test, feature = "fixtures"))]
pub(super) async fn enable_objectives(inner: &Inner) {
    let db = inner.repo.sqlite_db().unwrap();
    let mut tx = crate::db::sqlite::begin_immediate_tx(&db).await.unwrap();
    crate::db::sqlite::track_capabilities::stamp_tx(
        &mut tx,
        &inner.track_id,
        &[calm_types::track_capability::TrackCapability::Objectives],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

#[cfg(all(test, feature = "fixtures"))]
pub(super) async fn assert_objective_failed_end_skipped(inner: &Arc<Inner>, turn: &str) {
    let db = inner.repo.sqlite_db().unwrap();
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM objective_events WHERE track_id=? AND kind='usage'",
    )
    .bind(inner.track_id.as_str())
    .fetch_one(db.read_pool())
    .await
    .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        inner.objective_usage.lock().await.settled.as_deref(),
        Some(turn),
        "failed END must explicitly settle without attempting an unauthorized write"
    );
    // A failed runtime has no execution authority. The next normal user turn
    // uses a fresh active runtime, as required by the existing lifecycle.
    let next = super::completed_commit_tests::Fixture::new().await;
    enable_objectives(&next.harness.inner).await;
    next.enqueue(vec![QueueEntry::user_message(
        "next user".into(),
        None,
        vec![],
    )])
    .await;
    next.issue().await;
    let next_inner = &next.harness.inner;
    let next_turn = next_inner.last_turn_id.lock().await.clone().unwrap();
    historical_started(next_inner, &next_turn).await;
    on_notification(
        next_inner,
        &next_inner.live_claim.writer(),
        crate::harness::codex_events::planner_event(crate::codex_appserver::Notification::parse(
            "turn/completed".into(),
            serde_json::json!({"threadId":"thread-commit-wake",
                "turn":{"id":next_turn,"status":"completed"}}),
        )),
    )
    .await
    .unwrap();
    let (_, state) = calm_truth::objective_host::read_state(next.repo.pool(), &next_inner.track_id)
        .await
        .unwrap();
    assert_eq!(
        state.usage.turns, 1,
        "the next normal user END still counts"
    );
}

#[cfg(all(test, feature = "fixtures"))]
#[tokio::test]
async fn objective_historical_system_error_late_end_is_explicitly_skipped() {
    use serde_json::json;
    let logs = ObjectiveUsageLogs::install();
    let f = super::completed_commit_tests::Fixture::new().await;
    let inner = &f.harness.inner;
    enable_objectives(inner).await;
    f.enqueue(vec![QueueEntry::user_message("test".into(), None, vec![])])
        .await;
    f.issue().await;
    let turn = inner.last_turn_id.lock().await.clone().unwrap();
    historical_started(inner, &turn).await;
    for (method, params) in [
        (
            "thread/status/changed",
            json!({"threadId":"thread-commit-wake","status":{"type":"systemError"}}),
        ),
        (
            "turn/completed",
            json!({"threadId":"thread-commit-wake","turn":{"id":turn,"status":"failed"}}),
        ),
    ] {
        on_notification(
            inner,
            &inner.live_claim.writer(),
            crate::harness::codex_events::planner_event(
                crate::codex_appserver::Notification::parse(method.into(), params),
            ),
        )
        .await
        .unwrap();
    }
    assert!(matches!(
        &*inner.state.lock().await,
        HarnessState::Wedged { .. }
    ));
    assert_objective_failed_end_skipped(inner, &turn).await;
    logs.assert_no_recording_error();
}

#[cfg(all(test, feature = "fixtures"))]
#[derive(Clone)]
struct ObjectiveLogBuffer(Arc<std::sync::Mutex<Vec<u8>>>);
#[cfg(all(test, feature = "fixtures"))]
impl std::io::Write for ObjectiveLogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
#[cfg(all(test, feature = "fixtures"))]
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for ObjectiveLogBuffer {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}
#[cfg(all(test, feature = "fixtures"))]
pub(super) struct ObjectiveUsageLogs {
    buffer: ObjectiveLogBuffer,
    _guard: tracing::subscriber::DefaultGuard,
}
#[cfg(all(test, feature = "fixtures"))]
impl ObjectiveUsageLogs {
    pub(super) fn install() -> Self {
        let buffer = ObjectiveLogBuffer(Arc::new(std::sync::Mutex::new(vec![])));
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::WARN)
            .with_writer(buffer.clone())
            .finish();
        Self {
            buffer,
            _guard: tracing::subscriber::set_default(subscriber),
        }
    }
    pub(super) fn assert_no_recording_error(&self) {
        let logs = String::from_utf8(self.buffer.0.lock().unwrap().clone()).unwrap();
        assert!(
            !logs.contains("objective turn usage recording failed"),
            "{logs}"
        );
    }
}

#[cfg(all(test, feature = "fixtures"))]
#[tokio::test]
async fn objective_historical_usage_write_failure_retries_same_endpoint() {
    let f = super::completed_commit_tests::Fixture::new().await;
    let inner = &f.harness.inner;
    enable_objectives(inner).await;
    *inner.token_usage.lock().await = Some(TokenUsage {
        used_tokens: 40000,
        total_tokens: 9000000,
        context_window: Some(200000),
        at_ms: 1,
    });
    f.enqueue(vec![QueueEntry::user_message("test".into(), None, vec![])])
        .await;
    f.issue().await;
    let turn = inner.last_turn_id.lock().await.clone().unwrap();
    historical_started(inner, &turn).await;
    inner.token_usage.lock().await.as_mut().unwrap().used_tokens = 50000;
    sqlx::query(
        "CREATE TRIGGER refuse_objective_usage BEFORE INSERT ON objective_events
         WHEN NEW.kind='usage' BEGIN SELECT RAISE(ABORT, 'usage unavailable'); END",
    )
    .execute(f.repo.db().writer_pool())
    .await
    .unwrap();
    on_notification(
        inner,
        &inner.live_claim.writer(),
        crate::harness::codex_events::planner_event(crate::codex_appserver::Notification::parse(
            "turn/completed".into(),
            serde_json::json!({"threadId":"thread-commit-wake",
                "turn":{"id":turn,"status":"completed"}}),
        )),
    )
    .await
    .unwrap();
    assert!(inner.objective_usage.lock().await.settled.is_none());
    let (_, state) = calm_truth::objective_host::read_state(f.repo.pool(), &inner.track_id)
        .await
        .unwrap();
    assert_eq!(state.usage.turns, 0);
    sqlx::query("DROP TRIGGER refuse_objective_usage")
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
    // A later reading must not change the already observed END's delta.
    inner.token_usage.lock().await.as_mut().unwrap().used_tokens = 99000;
    for _ in 0..2 {
        persist_snapshot(inner).await.unwrap();
    }
    let (_, state) = calm_truth::objective_host::read_state(f.repo.pool(), &inner.track_id)
        .await
        .unwrap();
    assert_eq!(state.usage.turns, 1);
    assert_eq!(state.usage.context_tokens, 10000);
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM objective_events WHERE track_id=? AND kind='usage'",
    )
    .bind(inner.track_id.as_str())
    .fetch_one(f.repo.pool())
    .await
    .unwrap();
    assert_eq!(count, 1);
}

#[cfg(all(test, feature = "fixtures"))]
async fn historical_started(inner: &Arc<Inner>, turn: &str) {
    on_notification(
        inner,
        &inner.live_claim.writer(),
        crate::harness::codex_events::planner_event(crate::codex_appserver::Notification::parse(
            "turn/started".into(),
            serde_json::json!({"threadId":"thread-commit-wake", "turn":{"id":turn}}),
        )),
    )
    .await
    .unwrap();
    assert!(matches!(
        &*inner.state.lock().await,
        HarnessState::TurnRunning { .. }
    ));
}
