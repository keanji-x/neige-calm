//! Transient turn endpoints only: never part of a runtime snapshot.
use super::*;
use calm_truth::objective_host::{TurnUsage, record_turn_usage};

#[derive(Default)]
pub(super) struct TurnMeter {
    start: Option<(Option<i64>, Instant)>,
    maintenance: bool,
    settled: Option<String>,
}

impl TurnMeter {
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
                let start = self.start.take();
                if !self.maintenance
                    && let Some(turn) = turn
                    && self.settled.as_deref() != Some(turn)
                {
                    // Re-publishing a completed frame must not repeatedly request a write.
                    self.settled = Some(turn.to_owned());
                    let (initial, at) = start.unwrap_or((None, Instant::now()));
                    let context_growth = match (initial, reading) {
                        (Some(initial), Some(end)) => end.saturating_sub(initial).max(0) as u64,
                        _ => 0,
                    };
                    return Some((
                        turn.to_owned(),
                        TurnUsage {
                            context_growth,
                            secs: at.elapsed().as_secs(),
                        },
                    ));
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

pub(super) async fn observe(
    inner: &Inner,
    phase: HarnessPhaseTag,
    turn: Option<&str>,
    reading: Option<i64>,
) {
    let usage = inner
        .objective_usage
        .lock()
        .await
        .observe(phase, turn, reading);
    let Some((turn, usage)) = usage else { return };
    let Some(db) = inner.repo.sqlite_db() else {
        return;
    };
    if let Err(error) = record_turn_usage(
        &db,
        &inner.track_id,
        inner.worker_session_id.clone().into(),
        turn,
        usage,
    )
    .await
    {
        tracing::warn!(track = %inner.track_id, %error, "objective turn usage recording failed");
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
    for intermediate in [HarnessPhaseTag::IssuingInterrupt, HarnessPhaseTag::Wedged] {
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
