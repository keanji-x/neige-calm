//! Daily Track lifecycle. Dates belong here; Area/Track structures use their existing writers.

use crate::db::sqlite::track_update_tx;
use crate::db::write_with_actor_events_typed;
use crate::error::{CalmError, Result};
use crate::event::{Event, EventScope, TrackUpdatedPayload};
use crate::ids::ActorId;
use crate::managed_track::{ManagedTrackIdentity, ReportReadScope, ToolPolicy};
use crate::model::{NewTrack, RequestTheme, Track, TrackPatch};
use crate::state::RouteState;
use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use std::time::Duration;

pub const TIME_ZONE: Tz = chrono_tz::Asia::Shanghai;
const OWNER: &str = "daily-planner";

pub fn spawn(state: RouteState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if let Err(error) = reconcile(&state, Utc::now()).await {
                tracing::warn!(%error, "daily Planner reconciliation failed");
            }
        }
    });
}

fn pool(state: &RouteState) -> Result<&sqlx::SqlitePool> {
    state
        .mcp_context
        .sqlite_pool
        .as_ref()
        .ok_or_else(|| CalmError::Internal("daily Planner requires sqlite".into()))
}

async fn ensure_area(state: &RouteState) -> Result<String> {
    let (_, area) = crate::routes::areas::ensure_system_area(state).await?;
    Ok(area.id.to_string())
}

/// No missed empty days are backfilled. Past open days are closed before today is exposed.
pub async fn reconcile(state: &RouteState, now: DateTime<Utc>) -> Result<Track> {
    let date = now.with_timezone(&TIME_ZONE).date_naive();
    let area_id = ensure_area(state).await?;
    reconcile_closed_days(state, date).await?;
    crate::routes::tracks::create_managed_track(
        state.clone(),
        NewTrack {
            area_id: area_id.into(),
            title: date.to_string(),
            sort: Some(
                -(date
                    .and_hms_opt(0, 0, 0)
                    .expect("midnight")
                    .and_utc()
                    .timestamp() as f64),
            ),
            cwd: String::new(),
            template_id: Some(crate::templates::DAILY_PLANNER.into()),
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        ManagedTrackIdentity {
            owner: OWNER.into(),
            identity: date.to_string(),
            report_read_scope: ReportReadScope::Workspace,
            report_time_zone: TIME_ZONE,
            tool_policy: ToolPolicy::Reports,
            kernel_controls_lifecycle: true,
        },
    )
    .await
}

async fn reconcile_closed_days(state: &RouteState, date: NaiveDate) -> Result<()> {
    let rows: Vec<(String, String, Option<i64>)> = sqlx::query_as(concat!(
        "SELECT m.identity,t.id,t.closed_at FROM managed_track_identities m JOIN tracks t ON ",
        "t.id=m.track_id WHERE m.owner=?1 AND ((m.identity<?2 AND t.closed_at IS NULL) OR ",
        "(m.identity=?2 AND t.closed_at IS NOT NULL))",
    ))
    .bind(OWNER)
    .bind(date.to_string())
    .fetch_all(pool(state)?)
    .await?;
    for (day, id, closed) in rows {
        let should_close = day < date.to_string();
        if should_close == closed.is_some() {
            continue;
        }
        let write_id = id.clone();
        write_with_actor_events_typed(
            state.repo.as_ref(),
            None,
            &state.events,
            &state.write,
            move |tx| {
                Box::pin(async move {
                    let current: Option<i64> =
                        sqlx::query_scalar("SELECT closed_at FROM tracks WHERE id=?1")
                            .bind(&write_id)
                            .fetch_one(&mut **tx)
                            .await?;
                    if current.is_some() == should_close {
                        return Err(CalmError::Conflict(
                            "daily lifecycle already reconciled".into(),
                        ));
                    }
                    let track = track_update_tx(
                        tx,
                        &write_id,
                        TrackPatch {
                            closed: Some(should_close),
                            ..Default::default()
                        },
                    )
                    .await?;
                    let scope = EventScope::Track {
                        track: track.id.clone(),
                        area: track.area_id.clone(),
                    };
                    Ok((
                        (),
                        vec![(
                            ActorId::Kernel,
                            scope,
                            Event::TrackUpdated(TrackUpdatedPayload::new(track, None)),
                        )],
                    ))
                })
            },
        )
        .await
        .or_else(|error| match error {
            CalmError::Conflict(ref message) if message == "daily lifecycle already reconciled" => {
                Ok(((), Vec::new()))
            }
            error => Err(error),
        })?;
    }
    Ok(())
}

pub async fn track_for_date(state: &RouteState, date: NaiveDate) -> Result<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT track_id FROM managed_track_identities WHERE owner=?1 AND identity=?2",
    )
    .bind(OWNER)
    .bind(date.to_string())
    .fetch_optional(pool(state)?)
    .await?)
}

#[cfg(test)]
pub(crate) mod tests;
