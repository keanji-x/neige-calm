use super::ports::{CommitMode, Storage, Transaction};
use super::{
    PLUGIN_ID,
    model::{Entry, TimedSpan},
    store::{get, put},
};
use crate::ports::ErrorFactory;
use calm_types::{
    event::{Event, EventScope},
    ids::ActorId,
};
use chrono::{DateTime, FixedOffset, Utc};
use serde_json::json;
use std::{sync::Arc, time::Duration};
const TICK: Duration = Duration::from_secs(30);
/// A missed wake still fires until its end, but never less than two ticks after its start, so
/// an entry shorter than the tick cannot fall between two scans.
const GRACE: Duration = Duration::from_secs(2 * TICK.as_secs());
const CHANGED: &str = "calendar wake decided on changed state";

pub fn spawn<S: Storage + 'static>(ctx: Arc<S>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if let Err(error) = scan(ctx.as_ref(), Utc::now()).await {
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
pub async fn scan<S: Storage>(ctx: &S, now: DateTime<Utc>) -> Result<usize, S::Error> {
    let running = ctx.is_running().await;
    if !running {
        return Ok(0);
    }
    let mut woken = 0;
    for (key, value) in ctx.list("entry:").await? {
        let result = match serde_json::from_value::<Entry>(value) {
            Ok(entry) => handle(ctx, entry, now).await,
            Err(error) => Err(S::Error::internal(format!("calendar record: {error}"))),
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

pub async fn handle<S: Storage>(
    ctx: &S,
    entry: Entry,
    now: DateTime<Utc>,
) -> Result<bool, S::Error> {
    // Human entries have no source Track and never wake anything.
    let Some(track_id) = entry.source_track_id.clone() else {
        return Ok(false);
    };
    if entry.cancelled {
        return Ok(false);
    }
    // Only the latest started occurrence counts; older missed ones are passed over silently.
    let Some(span) = entry.task.schedule.latest_occurrence(now)? else {
        return Ok(false);
    };
    let start_ms = span.start.timestamp_millis();
    let key = fired_key(&entry.id);
    let fired = ctx
        .get(&key)
        .await?
        .map(serde_json::from_value::<i64>)
        .transpose()
        .map_err(|e| S::Error::internal(format!("calendar wake cursor: {e}")))?;
    if fired.is_some_and(|fired| fired >= start_ms) {
        return Ok(false);
    }
    let track = ctx
        .track(&track_id)
        .await?
        .filter(|track| track.closed_at.is_none());
    let deadline = span.end.max(span.start + GRACE);
    let (id, version) = (entry.id.clone(), entry.version);
    let written = match track.filter(|_| now < deadline) {
        // Ended, closed or missing: record the occurrence as handled without waking.
        None => {
            ctx.commit(
                CommitMode::Silent,
                Box::new(move |tx| {
                    Box::pin(async move {
                        claim(tx, &id, version, start_ms, None)
                            .await
                            .map(|()| (json!(false), Vec::new()))
                    })
                }),
            )
            .await
        }
        Some(track) => {
            let event = Event::TrackWakeRequested {
                track_id: track.id.clone(),
                source: PLUGIN_ID.into(),
                key: entry.id.clone(),
                text: wake_text(&entry, &span, now),
            };
            let track_id = track.id.to_string();
            let scope = EventScope::Track {
                track: track.id,
                area: track.area_id,
            };
            ctx.commit(
                CommitMode::Events(ActorId::Kernel),
                Box::new(move |tx| {
                    Box::pin(async move {
                        claim(tx, &id, version, start_ms, Some(&track_id)).await?;
                        Ok((json!(true), vec![(scope, event)]))
                    })
                }),
            )
            .await
        }
    };
    match written {
        Err(error) if error.is_conflict(CHANGED) => Ok(false),
        other => other.map(|value| value.as_bool().expect("calendar mutation returns a bool")),
    }
}

/// Every cursor write first re-proves what the scan read outside the transaction: the entry is
/// still that version, the occurrence is not yet handled and, for a wake, the Track is still open.
/// Anything else changed wins; the next tick decides on the new state.
async fn claim<E: ErrorFactory>(
    tx: &mut dyn Transaction<E>,
    entry_id: &str,
    version: i64,
    start_ms: i64,
    open_track: Option<&str>,
) -> Result<(), E> {
    let current = get::<Entry, _>(tx, &format!("entry:{entry_id}")).await?;
    let fired = get::<i64, _>(tx, &fired_key(entry_id)).await?;
    let closed = match open_track {
        Some(track) => !tx.track_is_open(track).await?,
        None => false,
    };
    if current.map(|entry| entry.version) != Some(version)
        || fired.is_some_and(|fired| fired >= start_ms)
        || closed
    {
        return Err(E::conflict(CHANGED.into()));
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
