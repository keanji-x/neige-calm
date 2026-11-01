use super::{
    MAX_STATE_BYTES, Storage,
    model::*,
    reader::{ReadError, Reader},
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use tokio::io::AsyncReadExt;

/// Validate the retained legacy record once; retain every original field as provenance.
pub fn initialize(config: &Config, legacy: Value) -> Result<State, String> {
    config.validate()?;
    if legacy.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err("unsupported legacy schema_version; operator reconciliation required".into());
    }
    if legacy.get("profile").and_then(Value::as_str) != Some(&config.profile)
        || legacy.get("self_open_id").and_then(Value::as_str) != Some(&config.self_open_id)
        || legacy.get("identity").and_then(Value::as_str) != Some("user")
        || legacy.get("enabled") != Some(&json!(true))
    {
        return Err("legacy identity/profile/enabled differs from operator configuration".into());
    }
    let minutes = legacy
        .get("interval_minutes")
        .and_then(Value::as_i64)
        .filter(|n| (1..=1440).contains(n))
        .ok_or("invalid legacy interval_minutes")?;
    let overlap = legacy
        .get("overlap_minutes")
        .and_then(Value::as_i64)
        .filter(|n| (0..=1440).contains(n))
        .ok_or("invalid legacy overlap_minutes")?;
    let configured = legacy
        .get("monitor_sources")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
        .ok_or("missing legacy monitor_sources")?;
    let mut sources = BTreeMap::new();
    for key in configured {
        let key = key.as_str().ok_or("invalid monitor source")?;
        let source = match key {
            "group" => Source::Group {
                chat_id: required_string(legacy.pointer("/group/chat_id"), "group.chat_id")?,
            },
            "incoming_p2p" => Source::IncomingP2p,
            "at_me" => Source::AtMe,
            _ => return Err("unsupported legacy source".into()),
        };
        let cursor = timestamp(
            legacy
                .get("source_watermarks")
                .and_then(|v| v.get(key))
                .ok_or("missing source watermark")?,
        )?;
        if sources
            .insert(
                key.to_string(),
                SourceState {
                    source,
                    cursor_ms: cursor,
                    fault: None,
                },
            )
            .is_some()
        {
            return Err("duplicate source".into());
        }
    }
    for thread in legacy
        .get("tracked_threads")
        .and_then(Value::as_array)
        .ok_or("missing tracked_threads")?
    {
        let id = required_string(thread.get("thread_id"), "thread_id")?;
        let cursor = timestamp(
            thread
                .get("last_read_message_time")
                .ok_or("missing thread cursor")?,
        )?;
        if sources
            .insert(
                format!("thread:{id}"),
                SourceState {
                    source: Source::Thread { thread_id: id },
                    cursor_ms: cursor,
                    fault: None,
                },
            )
            .is_some()
        {
            return Err("duplicate thread".into());
        }
    }
    let mut seen = BTreeSet::new();
    for key in [
        "notified_message_ids",
        "handled_message_ids",
        "acknowledged_in_chat_message_ids",
    ] {
        for id in legacy
            .get(key)
            .and_then(Value::as_array)
            .ok_or_else(|| format!("missing {key}"))?
        {
            seen.insert(required_string(Some(id), key)?);
        }
    }
    for pending in legacy
        .get("pending_items")
        .and_then(Value::as_array)
        .ok_or("missing pending_items")?
    {
        let id = required_string(pending.get("message_id"), "pending message_id")?;
        if pending.get("suppress_repeat") == Some(&json!(true)) {
            seen.insert(id);
        }
    }
    if sources.len() > 128 {
        return Err("at most 128 sources and tracked threads are supported".into());
    }
    let state = State {
        revision: 1,
        legacy,
        sources,
        inbox: BTreeMap::new(),
        seen,
        next_batch: 1,
        last_scan_ms: None,
        interval_ms: minutes * 60_000,
        overlap_ms: overlap * 60_000,
        due: BTreeMap::new(),
        reminder_updates: BTreeMap::new(),
    };
    check_size(&state)?;
    Ok(state)
}
fn required_string(v: Option<&Value>, name: &str) -> Result<String, String> {
    v.and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 256)
        .map(str::to_string)
        .ok_or_else(|| format!("missing {name}"))
}
async fn import(config: &Config) -> Result<State, String> {
    let mut file = tokio::fs::File::open(&config.import_state_path)
        .await
        .map_err(|_| "cannot read configured legacy state")?
        .take((MAX_STATE_BYTES + 1) as u64);
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .await
        .map_err(|_| "cannot read configured legacy state")?;
    if bytes.len() > MAX_STATE_BYTES {
        return Err("legacy state exceeds 1 MiB".into());
    }
    initialize(
        config,
        serde_json::from_slice(&bytes).map_err(|_| "invalid legacy state JSON")?,
    )
}
pub fn check_size(state: &State) -> Result<(), String> {
    if serde_json::to_vec(state)
        .map_err(|_| "cannot encode inspection state")?
        .len()
        > MAX_STATE_BYTES
    {
        return Err("feishu-watch state exceeds 1 MiB; nothing was committed; operator reconciliation required".into());
    }
    Ok(())
}
/// Complete empty rounds persist checkpoints without a Planner event. Incomplete sources never
/// move; successful siblings are independent. The adapter commits inbox and wake atomically.
pub async fn poll<S: Storage, R: Reader>(
    storage: &S,
    reader: &R,
    config: &Config,
    now_ms: i64,
) -> Result<(), String> {
    config.validate()?;
    let Some(mut state) = storage.load().await? else {
        let mut state = import(config).await?;
        state.inbox.insert(
            "import".into(),
            Batch {
                id: "import".into(),
                created_at_ms: now_ms,
                messages: BTreeMap::new(),
                settlement: None,
            },
        );
        check_size(&state)?;
        return storage
            .commit(
                None,
                state,
                Some(Wake {
                    key: "import".into(),
                    text: include_str!("prompts/inspection-import.md")
                        .trim_end()
                        .into(),
                }),
            )
            .await;
    };
    let expected = state.revision;
    let due = fire_due(&mut state, now_ms);
    let scan_due = state
        .last_scan_ms
        .is_none_or(|last| now_ms >= last.saturating_add(state.interval_ms));
    if !scan_due && due.is_empty() {
        return Ok(());
    }
    let mut messages = BTreeMap::new();
    let mut transitions = Vec::new();
    if scan_due {
        let identity = reader.verify_identity(config).await;
        for (key, source) in &mut state.sources {
            let since = source.cursor_ms.saturating_sub(state.overlap_ms).max(0);
            let outcome = if let Err(error) = identity {
                Err(error)
            } else if now_ms < source.cursor_ms {
                Err(ReadError::Malformed)
            } else {
                reader.read(config, &source.source, since, now_ms).await
            };
            match outcome {
                Ok(batch) => {
                    if source.fault.take().is_some() {
                        transitions.push(format!("{key}: recovered"));
                    }
                    for message in batch {
                        if message.at_ms >= since
                            && message.at_ms <= now_ms
                            && message.sender_id != config.self_open_id
                            && !state.seen.contains(&message.id)
                        {
                            messages.entry(message.id.clone()).or_insert(message);
                        }
                    }
                    source.cursor_ms = now_ms;
                }
                Err(fault) => {
                    if source.fault != Some(fault) {
                        transitions.push(format!("{key}: {fault:?}"));
                    }
                    source.fault = Some(fault);
                }
            }
        }
        state.last_scan_ms = Some(now_ms);
    }
    let mut reasons = Vec::new();
    if !messages.is_empty() {
        let id = format!("batch-{:020}", state.next_batch);
        state.next_batch = state
            .next_batch
            .checked_add(1)
            .ok_or("batch counter exhausted")?;
        state.seen.extend(messages.keys().cloned());
        reasons.push(format!(
            "{} new messages in durable batch {id}",
            messages.len()
        ));
        state.inbox.insert(
            id.clone(),
            Batch {
                id,
                created_at_ms: now_ms,
                messages,
                settlement: None,
            },
        );
    }
    reasons.extend(transitions);
    reasons.extend(due);
    state.revision = expected.checked_add(1).ok_or("revision exhausted")?;
    let wake = (!reasons.is_empty()).then(|| Wake {
        key: format!("revision:{}", state.revision),
        text: format!(
            include_str!("prompts/inspection-wake.md"),
            reasons.join("; ")
        ),
    });
    check_size(&state)?;
    storage.commit(Some(expected), state, wake).await
}
fn fire_due(state: &mut State, now_ms: i64) -> Vec<String> {
    let mut reasons = Vec::new();
    for deadline in state
        .due
        .values_mut()
        .filter(|d| !d.fired && d.at_ms <= now_ms)
    {
        deadline.fired = true;
        reasons.push(format!("reminder due: {}", deadline.key));
    }
    reasons
}
pub fn inbox_page(
    state: &State,
    batch_id: &str,
    after_id: Option<&str>,
    limit: usize,
) -> Result<InboxPage, String> {
    if !(1..=25).contains(&limit) {
        return Err("limit must be 1..25".into());
    }
    let batch = state.inbox.get(batch_id).ok_or("unknown batch")?;
    if after_id.is_some_and(|id| !batch.messages.contains_key(id)) {
        return Err("unknown message cursor".into());
    }
    let mut messages = Vec::new();
    let mut bytes = 0;
    let mut more = false;
    for message in batch
        .messages
        .values()
        .filter(|m| after_id.is_none_or(|id| m.id.as_str() > id))
    {
        let size = serde_json::to_vec(message)
            .map_err(|_| "cannot encode message")?
            .len();
        if size > 40 * 1024 {
            return Err("stored message exceeds page budget".into());
        }
        if messages.len() == limit || bytes + size > 64 * 1024 {
            more = true;
            break;
        }
        bytes += size;
        messages.push(message.clone());
    }
    let next_after = more.then(|| messages.last().expect("positive page size").id.clone());
    Ok(InboxPage {
        batch_id: batch_id.into(),
        messages,
        next_after,
        settlement: batch.settlement.clone(),
    })
}
/// Normalize/validate one reminder command in its owning feature, for both entry points.
fn reminder_command(
    mut deadlines: Vec<Deadline>,
    cancel: &[String],
) -> Result<(Vec<Deadline>, Value), String> {
    if deadlines.len() > 100 || cancel.len() > 100 {
        return Err("at most 100 reminder operations".into());
    }
    let mut keys = BTreeSet::new();
    for d in &mut deadlines {
        if d.key.is_empty()
            || d.key.len() > 128
            || d.note.is_empty()
            || d.note.len() > 2048
            || !keys.insert(d.key.clone())
        {
            return Err("reminder keys and notes must be unique and bounded".into());
        }
        d.fired = false;
    }
    let mut cancelled = BTreeSet::new();
    for key in cancel {
        if key.is_empty() || key.len() > 128 || keys.contains(key) || !cancelled.insert(key) {
            return Err("cancellation keys must be unique, bounded and distinct".into());
        }
    }
    let command = json!({"deadlines":deadlines,"cancel_deadline_keys":cancel});
    if command.to_string().len() > 32 * 1024 {
        return Err("reminder command exceeds 32 KiB".into());
    }
    Ok((deadlines, command))
}
fn apply_reminders(
    state: &mut State,
    deadlines: Vec<Deadline>,
    cancel: Vec<String>,
    now_ms: i64,
) -> Result<(), String> {
    if deadlines.iter().any(|d| d.at_ms <= now_ms) {
        return Err("new deadlines require a future timestamp".into());
    }
    if cancel.iter().any(|key| !state.due.contains_key(key)) {
        return Err("unknown reminder cancellation key".into());
    }
    for key in cancel {
        state.due.remove(&key);
    }
    for deadline in deadlines {
        state.due.insert(deadline.key.clone(), deadline);
    }
    Ok(())
}
/// Classification receipts stay immutable; changing reminders is a separate lifecycle.
pub async fn settle<S: Storage>(
    storage: &S,
    batch_id: &str,
    classification: Value,
    deadlines: Vec<Deadline>,
    cancel_deadline_keys: Vec<String>,
    now_ms: i64,
) -> Result<(), String> {
    if !classification.is_object() {
        return Err("classification must be an object".into());
    }
    let mut state = storage
        .load()
        .await?
        .ok_or("inspection is not initialized")?;
    let expected = state.revision;
    let (deadlines, command) = reminder_command(deadlines, &cancel_deadline_keys)?;
    let receipt = json!({"classification":classification,"deadlines":command["deadlines"],"cancel_deadline_keys":command["cancel_deadline_keys"]});
    if receipt.to_string().len() > 32 * 1024 {
        return Err("settlement exceeds 32 KiB".into());
    }
    let batch = state.inbox.get(batch_id).ok_or("unknown batch")?;
    if let Some(old) = &batch.settlement {
        return if old == &receipt {
            Ok(())
        } else {
            Err("batch already settled with different classification".into())
        };
    }
    apply_reminders(&mut state, deadlines, cancel_deadline_keys, now_ms)?;
    state
        .inbox
        .get_mut(batch_id)
        .expect("checked batch")
        .settlement = Some(receipt);
    state.revision = expected.checked_add(1).ok_or("revision exhausted")?;
    check_size(&state)?;
    storage.commit(Some(expected), state, None).await
}
/// Ordinary user answers and reminder-only wakes need no new message batch.
/// A permanent request receipt prevents a lost response from rearming a fired reminder.
pub async fn change_reminders<S: Storage>(
    storage: &S,
    request_id: &str,
    expected_revision: u64,
    deadlines: Vec<Deadline>,
    cancel_deadline_keys: Vec<String>,
    now_ms: i64,
) -> Result<(), String> {
    if request_id.is_empty() || request_id.len() > 128 {
        return Err("request_id must be 1..128 bytes".into());
    }
    let mut state = storage
        .load()
        .await?
        .ok_or("inspection is not initialized")?;
    let (deadlines, command) = reminder_command(deadlines, &cancel_deadline_keys)?;
    if let Some(old) = state.reminder_updates.get(request_id) {
        return if old == &command {
            Ok(())
        } else {
            Err("request_id already used for another reminder command".into())
        };
    }
    if state.revision != expected_revision {
        return Err("inspection revision changed; reread status".into());
    }
    apply_reminders(&mut state, deadlines, cancel_deadline_keys, now_ms)?;
    state.reminder_updates.insert(request_id.into(), command);
    state.revision = expected_revision
        .checked_add(1)
        .ok_or("revision exhausted")?;
    check_size(&state)?;
    storage.commit(Some(expected_revision), state, None).await
}
