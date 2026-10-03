//! A Track-created timed entry wakes that Track's Planner once when it starts.
use super::{
    PLUGIN_ID,
    model::{Entry, TimedSpan},
    store::{get, put},
};
use crate::db::{write_in_tx_typed, write_with_events_typed};
use crate::error::{CalmError, Result};
use crate::event::{Event, EventScope};
use crate::ids::ActorId;
use crate::mcp_server::registry::AppContext;
use chrono::{DateTime, FixedOffset, Utc};
use sqlx::{Sqlite, Transaction};
use std::sync::Arc;
use std::time::Duration;

const TICK: Duration = Duration::from_secs(30);
/// A missed wake still fires until its end, but never less than two ticks after its start, so
/// an entry shorter than the tick cannot fall between two scans.
const GRACE: Duration = Duration::from_secs(2 * TICK.as_secs());
const CHANGED: &str = "calendar entry changed during wake";

pub(super) fn spawn(ctx: Arc<AppContext>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if let Err(error) = scan(&ctx, Utc::now()).await {
                tracing::warn!(%error, "calendar wake scan failed");
            }
        }
    });
}

/// The start instant (ms) of the last occurrence handled; separate from the entry so a wake
/// never changes the entry version a user edit checks.
fn fired_key(entry_id: &str) -> String {
    format!("fired:{entry_id}")
}

/// One pass at `now`; returns how many wake events were written.
pub(super) async fn scan(ctx: &AppContext, now: DateTime<Utc>) -> Result<usize> {
    let running = match ctx.plugin_host.get() {
        Some(host) => host.running_plugin_ids().await.contains(PLUGIN_ID),
        None => false,
    };
    if !running {
        return Ok(0);
    }
    let mut woken = 0;
    for (key, value) in ctx.repo.plugin_kv_list(PLUGIN_ID, "entry:").await? {
        let result = match serde_json::from_value::<Entry>(value) {
            Ok(entry) => handle(ctx, entry, now).await,
            Err(error) => Err(CalmError::Internal(format!("calendar record: {error}"))),
        };
        match result {
            Ok(true) => woken += 1,
            Ok(false) => {}
            // One bad entry must not stop the others from waking.
            Err(error) => tracing::warn!(%key, %error, "calendar wake skipped an entry"),
        }
    }
    Ok(woken)
}

pub(super) async fn handle(ctx: &AppContext, entry: Entry, now: DateTime<Utc>) -> Result<bool> {
    // Human entries have no source Track and never wake anything.
    let Some(track_id) = entry.source_track_id.clone() else {
        return Ok(false);
    };
    if entry.cancelled {
        return Ok(false);
    }
    let Some(span) = entry.task.schedule.timed_span()? else {
        return Ok(false);
    };
    let start_ms = span.start.timestamp_millis();
    if start_ms > now.timestamp_millis() {
        return Ok(false);
    }
    let key = fired_key(&entry.id);
    let fired = ctx
        .repo
        .plugin_kv_get(PLUGIN_ID, &key)
        .await?
        .map(serde_json::from_value::<i64>)
        .transpose()
        .map_err(|e| CalmError::Internal(format!("calendar wake cursor: {e}")))?;
    if fired.is_some_and(|fired| fired >= start_ms) {
        return Ok(false);
    }
    let track = ctx
        .repo
        .track_get(&track_id)
        .await?
        .filter(|track| track.closed_at.is_none());
    let deadline = span.end.max(span.start + GRACE);
    let (id, version) = (entry.id.clone(), entry.version);
    let written = match track.filter(|_| now < deadline) {
        // Ended, closed or missing: record the occurrence as handled without waking.
        None => {
            write_in_tx_typed(ctx.repo.as_ref(), move |tx| {
                Box::pin(async move { claim(tx, &id, version, start_ms).await.map(|()| false) })
            })
            .await
        }
        Some(track) => {
            let event = Event::TrackWakeRequested {
                track_id: track.id.clone(),
                source: PLUGIN_ID.into(),
                key: entry.id.clone(),
                text: wake_text(&entry, &span, now),
            };
            let scope = EventScope::Track {
                track: track.id,
                area: track.area_id,
            };
            write_with_events_typed(
                ctx.repo.as_ref(),
                ActorId::Kernel,
                None,
                &ctx.events,
                &ctx.write,
                move |tx| {
                    Box::pin(async move {
                        claim(tx, &id, version, start_ms).await?;
                        Ok(((), vec![(scope, event)]))
                    })
                },
            )
            .await
            .map(|_| true)
        }
    };
    match written {
        Err(CalmError::Conflict(message)) if message == CHANGED => Ok(false),
        other => other,
    }
}

/// Every cursor write first proves the entry is the version the scan decided on; a concurrent
/// edit or cancel wins and the next tick decides on the new version.
async fn claim(
    tx: &mut Transaction<'_, Sqlite>,
    entry_id: &str,
    version: i64,
    start_ms: i64,
) -> Result<()> {
    let current = get::<Entry>(tx, &format!("entry:{entry_id}")).await?;
    if current.map(|entry| entry.version) != Some(version) {
        return Err(CalmError::Conflict(CHANGED.into()));
    }
    put(tx, &fired_key(entry_id), &start_ms).await
}

fn wake_text(entry: &Entry, span: &TimedSpan, now: DateTime<Utc>) -> String {
    let local = |at: DateTime<FixedOffset>| at.with_timezone(&span.tz).format("%Y-%m-%d %H:%M");
    let late = (now - span.start.with_timezone(&Utc)).num_minutes();
    let lateness = if late < 1 {
        "on time".to_string()
    } else {
        format!("{late} min late")
    };
    format!(
        "Calendar entry {:?} started at {} {} and ends at {} ({lateness}). \
         Do what this Track scheduled it for.",
        entry.task.title,
        local(span.start),
        span.tz.name(),
        local(span.end),
    )
}
