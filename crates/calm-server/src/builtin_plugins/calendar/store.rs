use super::{PLUGIN_ID, model::*};
use crate::db::write_with_events_typed;
use crate::error::{CalmError, Result};
use crate::event::{Event, EventScope};
use crate::ids::ActorId;
use crate::mcp_server::registry::AppContext;
use crate::model::{new_id, now_ms};
use serde::{Deserialize, Serialize};
use sqlx::{Sqlite, Transaction};

pub struct Access {
    pub track: Option<String>,
    pub actor: ActorId,
    pub scope: EventScope,
    pub creator: String,
}
impl Access {
    fn permits(&self, entry: &Entry) -> bool {
        self.track
            .as_ref()
            .is_none_or(|track| entry.source_track_id.as_ref() == Some(track))
    }
}
#[derive(Serialize, Deserialize)]
struct Receipt {
    request: Draft,
    entry_id: String,
}
pub(super) async fn get<T: serde::de::DeserializeOwned>(
    tx: &mut Transaction<'_, Sqlite>,
    key: &str,
) -> Result<Option<T>> {
    let value: Option<String> =
        sqlx::query_scalar("SELECT value FROM plugin_kv WHERE plugin_id=? AND key=?")
            .bind(PLUGIN_ID)
            .bind(key)
            .fetch_optional(&mut **tx)
            .await?;
    value
        .map(|v| {
            serde_json::from_str(&v)
                .map_err(|e| CalmError::Internal(format!("calendar record: {e}")))
        })
        .transpose()
}
pub(super) async fn put<T: Serialize>(
    tx: &mut Transaction<'_, Sqlite>,
    key: &str,
    value: &T,
) -> Result<()> {
    let value = serde_json::to_string(value).map_err(|e| CalmError::Internal(e.to_string()))?;
    sqlx::query("INSERT INTO plugin_kv(plugin_id,key,value,updated_at) VALUES(?,?,?,?) \
        ON CONFLICT(plugin_id,key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at")
        .bind(PLUGIN_ID).bind(key).bind(value).bind(now_ms()).execute(&mut **tx).await?;
    Ok(())
}
pub async fn list(ctx: &AppContext, access: &Access, window: Window) -> Result<Vec<Listed>> {
    window.validate()?;
    let mut entries = Vec::new();
    for (_, value) in ctx.repo.plugin_kv_list(PLUGIN_ID, "entry:").await? {
        let entry: Entry = serde_json::from_value(value)
            .map_err(|e| CalmError::Internal(format!("calendar record: {e}")))?;
        if access.permits(&entry) && !entry.cancelled && window.contains(&entry.task.schedule)? {
            let occurrences = window.occurrences(&entry.task.schedule)?;
            entries.push(Listed {
                entry,
                occurrences: occurrences.iter().map(TimedSpan::occurrence).collect(),
            });
        }
    }
    entries
        .sort_by(|a, b| (a.entry.created_at, &a.entry.id).cmp(&(b.entry.created_at, &b.entry.id)));
    Ok(entries)
}
pub async fn create(ctx: &AppContext, access: Access, mut request: Create) -> Result<Entry> {
    request.task.resolve_times()?;
    if request.idempotency_key.trim().is_empty() || request.idempotency_key.len() > 200 {
        return Err(invalid("idempotency_key must contain 1 to 200 bytes"));
    }
    // Tuple encoding avoids delimiter collisions and scopes receipts to the authenticated caller.
    let receipt_key = format!(
        "receipt:{}",
        serde_json::to_string(&(&access.creator, &request.idempotency_key)).unwrap()
    );
    let retry_key = receipt_key.clone();
    let retry_task = request.task.clone();
    let result = write_with_events_typed(
        ctx.repo.as_ref(),
        access.actor,
        None,
        &ctx.events,
        &ctx.write,
        move |tx| {
            Box::pin(async move {
                if get::<Receipt>(tx, &receipt_key).await?.is_some() {
                    // Roll back this read-only retry. The event writer deliberately forbids
                    // empty batches; resolve the immutable receipt outside its transaction.
                    return Err(CalmError::Conflict(
                        "calendar creation receipt exists".into(),
                    ));
                }
                let now = now_ms();
                let entry = Entry {
                    id: new_id(),
                    task: request.task,
                    version: 1,
                    cancelled: false,
                    source_track_id: access.track,
                    created_by: access.creator,
                    created_at: now,
                    updated_at: now,
                };
                put(tx, &format!("entry:{}", entry.id), &entry).await?;
                put(
                    tx,
                    &receipt_key,
                    &Receipt {
                        request: entry.task.clone(),
                        entry_id: entry.id.clone(),
                    },
                )
                .await?;
                Ok((
                    entry,
                    vec![(
                        access.scope,
                        Event::PluginDataChanged {
                            plugin_id: PLUGIN_ID.into(),
                        },
                    )],
                ))
            })
        },
    )
    .await;
    match result {
        Ok((entry, _)) => Ok(entry),
        Err(CalmError::Conflict(message)) if message == "calendar creation receipt exists" => {
            let receipt = ctx
                .repo
                .plugin_kv_get(PLUGIN_ID, &retry_key)
                .await?
                .ok_or_else(|| CalmError::Internal("calendar receipt disappeared".into()))?;
            let receipt: Receipt =
                serde_json::from_value(receipt).map_err(|e| CalmError::Internal(e.to_string()))?;
            if receipt.request != retry_task {
                return Err(CalmError::Conflict(
                    "idempotency key already used for different content".into(),
                ));
            }
            let entry = ctx
                .repo
                .plugin_kv_get(PLUGIN_ID, &format!("entry:{}", receipt.entry_id))
                .await?
                .ok_or_else(|| CalmError::Internal("calendar receipt without entry".into()))?;
            serde_json::from_value(entry).map_err(|e| CalmError::Internal(e.to_string()))
        }
        Err(error) => Err(error),
    }
}
/// What a versioned write does to one entry.
pub enum Change {
    /// The REST editor's whole replacement, its stored `cancelled` included.
    Replace(Update),
    /// `neige_calendar_set`: a new task for an entry that is not removed.
    Edit(Draft),
    /// `neige_calendar_rm`: the stored `cancelled` is set, which stops the entry's wakes and
    /// hides it from lists. There is no undo through the tools.
    Remove,
}
pub async fn update(
    ctx: &AppContext,
    access: Access,
    id: String,
    expected_version: i64,
    mut change: Change,
) -> Result<Entry> {
    match &mut change {
        Change::Replace(Update { task, .. }) | Change::Edit(task) => task.resolve_times()?,
        Change::Remove => {}
    }
    let (entry, _) = write_with_events_typed(
        ctx.repo.as_ref(),
        access.actor.clone(),
        None,
        &ctx.events,
        &ctx.write,
        move |tx| {
            Box::pin(async move {
                let key = format!("entry:{id}");
                let mut entry = get::<Entry>(tx, &key)
                    .await?
                    .filter(|entry| access.permits(entry))
                    .ok_or_else(|| CalmError::NotFound("calendar task".into()))?;
                if entry.cancelled && !matches!(change, Change::Replace(_)) {
                    return Err(CalmError::Conflict(
                        "calendar task was removed; a removal has no undo".into(),
                    ));
                }
                if entry.version != expected_version {
                    return Err(CalmError::Conflict(
                        "calendar task changed; reload before editing".into(),
                    ));
                }
                match change {
                    Change::Replace(request) => {
                        entry.task = request.task;
                        entry.cancelled = request.cancelled;
                    }
                    Change::Edit(task) => entry.task = task,
                    Change::Remove => entry.cancelled = true,
                }
                entry.version += 1;
                entry.updated_at = now_ms();
                put(tx, &key, &entry).await?;
                Ok((
                    entry,
                    vec![(
                        access.scope,
                        Event::PluginDataChanged {
                            plugin_id: PLUGIN_ID.into(),
                        },
                    )],
                ))
            })
        },
    )
    .await?;
    Ok(entry)
}
