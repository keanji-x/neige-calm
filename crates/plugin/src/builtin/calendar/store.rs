use super::ports::{CommitMode, Storage, Transaction};
use super::{PLUGIN_ID, model::*};
use crate::ports::ErrorFactory;
use calm_types::{
    event::{Event, EventScope},
    ids::ActorId,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
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
pub(super) async fn get<T: serde::de::DeserializeOwned, E: ErrorFactory>(
    tx: &mut dyn Transaction<E>,
    key: &str,
) -> Result<Option<T>, E> {
    tx.read(key)
        .await?
        .map(|value| {
            serde_json::from_str(&value).map_err(|e| E::internal(format!("calendar record: {e}")))
        })
        .transpose()
}
pub(super) async fn put<T: Serialize, E: ErrorFactory>(
    tx: &mut dyn Transaction<E>,
    key: &str,
    value: &T,
) -> Result<(), E> {
    let value = serde_json::to_string(value).map_err(|e| E::internal(e.to_string()))?;
    tx.put(key, &value).await
}
pub async fn list<S: Storage>(
    ctx: &S,
    access: &Access,
    window: Window,
) -> Result<Vec<Listed>, S::Error> {
    window.validate()?;
    let mut entries = Vec::new();
    for (_, value) in ctx.list("entry:").await? {
        let entry: Entry = serde_json::from_value(value)
            .map_err(|e| S::Error::internal(format!("calendar record: {e}")))?;
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
/// One entry by id, a cancelled one included: the list leaves those out, and a client reading back a cancel needs it.
pub async fn read<S: Storage>(ctx: &S, access: &Access, id: &str) -> Result<Entry, S::Error> {
    let entry: Entry = ctx
        .get(&format!("entry:{id}"))
        .await?
        .map(|value| {
            serde_json::from_value(value)
                .map_err(|e| S::Error::internal(format!("calendar record: {e}")))
        })
        .transpose()?
        .filter(|entry: &Entry| access.permits(entry))
        .ok_or_else(|| S::Error::not_found("calendar task".into()))?;
    Ok(entry)
}
pub async fn create<S: Storage>(
    ctx: &S,
    access: Access,
    mut request: Create,
) -> Result<Entry, S::Error> {
    request.task.resolve_times()?;
    if request.idempotency_key.trim().is_empty() || request.idempotency_key.len() > 200 {
        return Err(invalid("idempotency_key must contain 1 to 200 bytes").into());
    }
    // Tuple encoding avoids delimiter collisions and scopes receipts to the authenticated caller.
    let receipt_key = format!(
        "receipt:{}",
        serde_json::to_string(&(&access.creator, &request.idempotency_key)).unwrap()
    );
    let retry_key = receipt_key.clone();
    let retry_task = request.task.clone();
    let result = ctx
        .commit(
            CommitMode::Events(access.actor.clone()),
            Box::new(move |tx| {
                Box::pin(async move {
                    if get::<Receipt, _>(tx, &receipt_key).await?.is_some() {
                        // Roll back this read-only retry. The event writer deliberately forbids
                        // empty batches; resolve the immutable receipt outside its transaction.
                        return Err(S::Error::conflict(
                            "calendar creation receipt exists".into(),
                        ));
                    }
                    let now = tx.now_ms();
                    let entry = Entry {
                        id: tx.new_id(),
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
                        json!(entry),
                        vec![(
                            access.scope,
                            Event::PluginDataChanged {
                                plugin_id: PLUGIN_ID.into(),
                            },
                        )],
                    ))
                })
            }),
        )
        .await;
    match result {
        Ok(value) => serde_json::from_value(value).map_err(|e| S::Error::internal(e.to_string())),
        Err(error) if error.is_conflict("calendar creation receipt exists") => {
            let receipt = ctx
                .get(&retry_key)
                .await?
                .ok_or_else(|| S::Error::internal("calendar receipt disappeared".into()))?;
            let receipt: Receipt =
                serde_json::from_value(receipt).map_err(|e| S::Error::internal(e.to_string()))?;
            if receipt.request != retry_task {
                return Err(S::Error::conflict(
                    "idempotency key already used for different content".into(),
                ));
            }
            let entry = ctx
                .get(&format!("entry:{}", receipt.entry_id))
                .await?
                .ok_or_else(|| S::Error::internal("calendar receipt without entry".into()))?;
            serde_json::from_value(entry).map_err(|e| S::Error::internal(e.to_string()))
        }
        Err(error) => Err(error),
    }
}
/// What a versioned write does to one entry.
pub enum Change {
    /// The REST editor's whole replacement, its stored `cancelled` included.
    Replace(Update),
    /// `plugin_calendar_set`: a new task for an entry that is not removed.
    Edit(Draft),
    /// `plugin_calendar_rm`: the stored `cancelled` is set, which stops the entry's wakes and
    /// hides it from lists. There is no undo through the tools.
    Remove,
}
pub async fn update<S: Storage>(
    ctx: &S,
    access: Access,
    id: String,
    expected_version: i64,
    mut change: Change,
) -> Result<Entry, S::Error> {
    match &mut change {
        Change::Replace(Update { task, .. }) | Change::Edit(task) => task.resolve_times()?,
        Change::Remove => {}
    }
    let entry = ctx
        .commit(
            CommitMode::Events(access.actor.clone()),
            Box::new(move |tx| {
                Box::pin(async move {
                    let key = format!("entry:{id}");
                    let mut entry = get::<Entry, _>(tx, &key)
                        .await?
                        .filter(|entry| access.permits(entry))
                        .ok_or_else(|| S::Error::not_found("calendar task".into()))?;
                    if entry.cancelled && !matches!(change, Change::Replace(_)) {
                        return Err(S::Error::conflict(
                            "calendar task was removed; a removal has no undo".into(),
                        ));
                    }
                    if entry.version != expected_version {
                        return Err(S::Error::conflict(
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
                    entry.updated_at = tx.now_ms();
                    put(tx, &key, &entry).await?;
                    Ok((
                        json!(entry),
                        vec![(
                            access.scope,
                            Event::PluginDataChanged {
                                plugin_id: PLUGIN_ID.into(),
                            },
                        )],
                    ))
                })
            }),
        )
        .await?;
    serde_json::from_value(entry).map_err(|e| S::Error::internal(e.to_string()))
}
