//! Kernel-owned Planner context and task statistics for native Report live slots.
//! Events wake the serial projector; a boot sweep and 30 s reconciliation cover quiet writes.
use std::{sync::Arc, time::Duration};

use calm_types::{plugin::KERNEL_OVERLAY_PLUGIN_ID, report_blocks::native_view::*};
use tokio::sync::broadcast::error::RecvError;

use crate::{
    db::{
        Repo,
        sqlite::{overlay_upsert_tx, track_get_tx},
        write_with_events_typed,
    },
    error::{CalmError, Result},
    event::{BroadcastEnvelope, Event, EventBus, EventScope},
    harness::token_usage::{persisted_context_snapshot, planner_context_usage},
    ids::{ActorId, TrackId},
    model::NewOverlay,
    state::WriteContext,
};

mod objective_budget;

pub const RUN_STATS_OVERLAY_KIND: &str = "run.stats";

pub const PLANNER_CONTEXT_OVERLAY_KIND: &str = "planner.context";

pub struct TrackPlannerContextProjector {
    repo: Arc<dyn Repo>,
    pool: sqlx::SqlitePool,
    bus: EventBus,
    write: WriteContext,
}

fn known(amount: f64, unit: &str) -> MetricValue {
    MetricValue::Known {
        amount,
        unit: unit.into(),
        placement: Placement::Suffix,
        decimals: 0.0,
        signed: false,
    }
}

fn unknown() -> MetricValue {
    MetricValue::Unknown {
        reason: "No valid context reading".into(),
    }
}

fn metric(id: &str, label: &str, value: MetricValue) -> Metric {
    Metric {
        id: id.into(),
        label: label.into(),
        value,
        detail: String::new(),
        tone: Tone::Neutral,
        emphasis: Emphasis::Normal,
    }
}

fn compact_metric(completed: i64) -> Metric {
    metric(
        "compact.completed",
        "Completed compact tasks",
        known(completed as f64, ""),
    )
}

fn unit(snapshot: Option<&crate::harness::HarnessSnapshot>, completed: i64) -> DataUnit {
    let reading = snapshot.and_then(planner_context_usage);
    let invalidated = snapshot.map(|s| s.context_usage_invalidated);
    DataUnit {
        snapshot: Snapshot {
            id: "planner.context".into(),
            observed_at: snapshot
                .and_then(|s| s.token_usage.as_ref())
                .map(|u| u.at_ms as f64),
            produced_at: None,
        },
        cell: Component::Metrics {
            id: "planner.context".into(),
            title: "Planner context".into(),
            items: vec![
                metric(
                    "percent",
                    "Used",
                    reading
                        .as_ref()
                        .and_then(|r| r.percent)
                        .map(|p| known(p, "%"))
                        .unwrap_or_else(unknown),
                ),
                metric(
                    "used_tokens",
                    "Context tokens",
                    reading
                        .as_ref()
                        .map(|r| known(r.used_tokens as f64, ""))
                        .unwrap_or_else(unknown),
                ),
                metric(
                    "context_window",
                    "Context window",
                    reading
                        .as_ref()
                        .and_then(|r| r.context_window)
                        .map(|w| known(w as f64, ""))
                        .unwrap_or_else(unknown),
                ),
                metric(
                    "invalidated",
                    "Invalidated by compaction",
                    invalidated
                        .map(|value| MetricValue::Text {
                            text: value.to_string(),
                        })
                        .unwrap_or_else(unknown),
                ),
                compact_metric(completed),
            ],
        },
    }
}

impl TrackPlannerContextProjector {
    pub fn new(repo: Arc<dyn Repo>, bus: EventBus, write: WriteContext) -> Option<Self> {
        let pool = repo.sqlite_db()?.read_pool().clone();
        Some(Self {
            repo,
            pool,
            bus,
            write,
        })
    }

    /// Returns true only when a changed data unit was published.
    pub async fn recompute_track(&self, track_id: &str) -> Result<bool> {
        let overlays = self.repo.overlays_for("track", track_id).await?;
        let previous = overlays
            .iter()
            .find(|o| {
                o.plugin_id == KERNEL_OVERLAY_PLUGIN_ID && o.kind == PLANNER_CONTEXT_OVERLAY_KIND
            })
            .map(|o| o.payload.clone());
        let card: Option<String> =
            sqlx::query_scalar("SELECT id FROM cards WHERE track_id = ?1 AND role = 'planner'")
                .bind(track_id)
                .fetch_optional(&self.pool)
                .await?;
        let runtime = if let Some(card) = &card {
            self.repo
                .session_projection_projectable_for_card(card)
                .await?
        } else {
            None
        };
        let snapshot = runtime
            .as_ref()
            .and_then(|s| s.handle_state_json.clone())
            .and_then(persisted_context_snapshot);
        let completed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tasks WHERE track_id = ?1 AND kind = 'compact' AND status = 'done'"
        ).bind(track_id).fetch_one(&self.pool).await?;
        // A sleeping/reaped session can have no projectable row. Keep its last observation,
        // including its timestamp; task completions remain independent fresh facts.
        let next = if runtime.is_none() && card.is_some() {
            previous
                .clone()
                .and_then(|p| serde_json::from_value::<DataUnit>(p).ok())
                .map(|mut previous| {
                    if let Component::Metrics { items, .. } = &mut previous.cell {
                        items.retain(|m| m.id != "compact.completed");
                        items.push(compact_metric(completed));
                    }
                    previous
                })
                .unwrap_or_else(|| unit(None, completed))
        } else {
            unit(snapshot.as_ref(), completed)
        };
        let payload = serde_json::to_value(next)?;
        let context_changed = self
            .publish(
                track_id,
                PLANNER_CONTEXT_OVERLAY_KIND,
                previous.as_ref(),
                payload,
            )
            .await?;
        let stats = self.run_stats(track_id).await?;
        let previous = overlays
            .iter()
            .find(|o| o.plugin_id == KERNEL_OVERLAY_PLUGIN_ID && o.kind == RUN_STATS_OVERLAY_KIND);
        let stats_changed = self
            .publish(
                track_id,
                RUN_STATS_OVERLAY_KIND,
                previous.map(|o| &o.payload),
                serde_json::to_value(stats)?,
            )
            .await?;
        let mut objective_changed = false;
        if let Some(unit) = objective_budget::build(&self.pool, track_id).await? {
            if let Err(error) =
                crate::objective_dispatch::dispatch_track(self.repo.as_ref(), &self.bus, track_id)
                    .await
            {
                tracing::warn!(%track_id, %error, "objective dispatch failed; continuing overlay publication");
            }
            let previous = overlays.iter().find(|o| {
                o.plugin_id == KERNEL_OVERLAY_PLUGIN_ID && o.kind == objective_budget::KIND
            });
            objective_changed = self
                .publish(
                    track_id,
                    objective_budget::KIND,
                    previous.map(|o| &o.payload),
                    serde_json::to_value(unit)?,
                )
                .await?;
        }
        Ok(context_changed || stats_changed || objective_changed)
    }

    async fn run_stats(&self, track_id: &str) -> Result<DataUnit> {
        // All durable attempts count, including historical recovery attempts. Candidate rows
        // survive event retention. A no-change delivery does not create a commit.
        let rows: Vec<(String, String, i64)> = sqlx::query_as(
            "SELECT 'status', status, COUNT(*) FROM tasks WHERE track_id = ?1 GROUP BY status
             UNION ALL SELECT 'kind', kind, COUNT(*) FROM tasks WHERE track_id = ?1 GROUP BY kind
             UNION ALL SELECT 'commits', 'delivered', COUNT(DISTINCT commit_sha)
             FROM tasks JOIN task_candidates ON task_candidates.producer_attempt_id = tasks.id
             WHERE tasks.track_id = ?1 AND task_candidates.track_id = ?1
               AND commit_sha <> base_sha
             ORDER BY 1, 2",
        )
        .bind(track_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(DataUnit {
            snapshot: Snapshot {
                id: RUN_STATS_OVERLAY_KIND.into(),
                observed_at: None,
                produced_at: None,
            },
            cell: Component::Table {
                id: RUN_STATS_OVERLAY_KIND.into(),
                title: "Track task statistics".into(),
                table: serde_json::to_value(InlineTable {
                    columns: [
                        ("dimension", "Group"),
                        ("value", "Category"),
                        ("count", "Count"),
                    ]
                    .into_iter()
                    .map(|(key, label)| TableColumn {
                        key: key.into(),
                        label: label.into(),
                        align: None,
                    })
                    .collect(),
                    rows: rows
                        .into_iter()
                        .map(|(dimension, value, count)| {
                            [
                                ("dimension".into(), TableScalar::Text(TableText(dimension))),
                                ("value".into(), TableScalar::Text(TableText(value))),
                                ("count".into(), TableScalar::Number(count as f64)),
                            ]
                            .into_iter()
                            .collect()
                        })
                        .collect(),
                    caption: None,
                    highlight: None,
                })?,
            },
        })
    }

    async fn publish(
        &self,
        track_id: &str,
        kind: &str,
        previous: Option<&serde_json::Value>,
        payload: serde_json::Value,
    ) -> Result<bool> {
        if previous == Some(&payload) {
            return Ok(false);
        }
        let overlay = NewOverlay {
            plugin_id: KERNEL_OVERLAY_PLUGIN_ID.into(),
            entity_kind: "track".into(),
            entity_id: track_id.into(),
            kind: kind.into(),
            payload,
        };
        let track_id = TrackId::from(track_id.to_string());
        write_with_events_typed(
            self.repo.as_ref(),
            ActorId::Kernel,
            None,
            &self.bus,
            &self.write,
            move |tx| {
                Box::pin(async move {
                    let track = match track_get_tx(tx, &track_id).await.map_err(CalmError::from) {
                        Ok(track) => track,
                        Err(CalmError::NotFound(_)) => return Ok((false, vec![])),
                        Err(error) => return Err(error),
                    };
                    let overlay = overlay_upsert_tx(tx, overlay).await?;
                    Ok((
                        true,
                        vec![(
                            EventScope::Track {
                                track: track.id,
                                area: track.area_id,
                            },
                            Event::OverlaySet(overlay),
                        )],
                    ))
                })
            },
        )
        .await
        .map(|(changed, _)| changed)
    }

    async fn track_for_event(&self, envelope: &BroadcastEnvelope) -> Option<String> {
        match &envelope.event {
            Event::HarnessPhaseChanged { track_id, .. } => Some(track_id.to_string()),
            Event::WorkerSessionStarted { card_id, .. }
            | Event::WorkerSessionStatusChanged { card_id, .. }
            | Event::WorkerSessionSuperseded { card_id, .. } => self
                .repo
                .card_get(card_id)
                .await
                .ok()
                .flatten()
                .map(|c| c.track_id.to_string()),
            Event::TaskCompleted { .. }
            | Event::TaskFailed { .. }
            | Event::TaskDispatched { .. }
            | Event::TaskGateResult { .. }
            | Event::TaskRegateRequested { .. }
            | Event::TaskGitDeliverySettled { .. }
            | Event::PlanUpdated { .. } => envelope.scope.track_id().map(ToString::to_string),
            Event::TrackUpdated(payload) => Some(payload.track.id.to_string()),
            _ => None,
        }
    }

    pub async fn run(self) {
        let mut rx = self.bus.subscribe();
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let tracks = tokio::select! {
                _ = tick.tick() => crate::track_activity::sql::track_ids(&self.pool).await,
                received = rx.recv() => match received {
                    Ok(envelope) => Ok(self.track_for_event(&envelope).await.into_iter().collect()),
                    Err(RecvError::Lagged(_)) => crate::track_activity::sql::track_ids(&self.pool).await,
                    Err(RecvError::Closed) => break,
                }
            };
            match tracks {
                Ok(tracks) => {
                    for track in tracks {
                        if let Err(error) = self.recompute_track(&track).await {
                            tracing::warn!(%track, %error, "track runtime projection failed");
                        }
                    }
                }
                Err(error) => tracing::warn!(%error, "track runtime enumeration failed"),
            }
        }
    }
}

pub fn spawn(repo: Arc<dyn Repo>, bus: EventBus, write: WriteContext) {
    if let Some(projector) = TrackPlannerContextProjector::new(repo, bus, write) {
        tokio::spawn(projector.run());
    }
}
