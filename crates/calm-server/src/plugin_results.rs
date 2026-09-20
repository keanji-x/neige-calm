//! The transient ring of plugin results the Planner's proxy calls produced, so `calm.source.capture` can vouch a source body is exactly what the kernel returned for one `(plugin, tool, args)` call.
//! A new call on the same key replaces the old entry whatever its status; eviction is lazy (on insert and lookup) and the ring is process memory.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::plugin_host::mcp::CallToolResult;

pub const MAX_ENTRIES_PER_TRACK: usize = 64;
pub const MAX_TOTAL_BYTES: usize = 128 * 1024 * 1024;
pub const TTL_MS: i64 = 2 * 60 * 60 * 1000;
/// A result whose joined text exceeds this is recorded as `TooLarge`
/// without its text.
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
/// Canonical args longer than this are not kept; the entry is `TooLarge`
/// and a capture must spell `call.args` out.
pub const MAX_ARGS_BYTES: usize = 64 * 1024;
/// Canonicalization version stored in every source row's `origin.args_canon`.
pub const ARGS_CANON_VERSION: &str = "v1";

/// What one recorded call produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultStatus {
    /// Every `type == "text"` block's `text`, in order, joined by `"\n"`.
    Ok { text: String },
    /// The plugin answered `isError: true`.
    Error,
    /// The reply carried no text block.
    NoText,
    /// The joined text exceeded [`MAX_TEXT_BYTES`], or the canonical args
    /// exceeded [`MAX_ARGS_BYTES`].
    TooLarge,
}

impl ResultStatus {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Ok { .. } => "ok",
            Self::Error => "error",
            Self::NoText => "no_text",
            Self::TooLarge => "too_large",
        }
    }
}

/// A snapshot of one entry, handed out by value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    pub plugin_id: String,
    pub tool_name: String,
    pub args_sha256: String,
    pub status: ResultStatus,
    /// `None` when the args were too large to keep.
    pub args_canonical: Option<String>,
    /// Unix milliseconds.
    pub completed_at: i64,
}

impl Recorded {
    /// `plugin.<id>_<tool>` — the registry name of the routed tool.
    pub fn registry_name(&self) -> String {
        registry_name(&self.plugin_id, &self.tool_name)
    }
}

pub fn registry_name(plugin_id: &str, tool_name: &str) -> String {
    format!("plugin.{plugin_id}_{tool_name}")
}

/// Canonical text of a `tools/call` `arguments` value: `serde_json` compact serialization (keys sorted; `1` and `1.0` differ; no Unicode normalization).
pub fn canonical_args(args: &Value) -> String {
    serde_json::to_string(args).unwrap_or_else(|_| "null".to_string())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// `sha256(canonical_args(args))`, lowercase hex.
pub fn args_sha256(args: &Value) -> String {
    sha256_hex(canonical_args(args).as_bytes())
}

/// The joined length is checked block by block, so no temporary larger than the cap is ever built.
pub fn classify(result: &CallToolResult) -> ResultStatus {
    if result.is_error == Some(true) {
        return ResultStatus::Error;
    }
    let mut text = String::new();
    let mut blocks = 0usize;
    for block in &result.content {
        if block.kind != "text" {
            continue;
        }
        let Some(part) = block.text.as_deref() else {
            continue;
        };
        let separator = usize::from(blocks > 0);
        if text.len() + separator + part.len() > MAX_TEXT_BYTES {
            return ResultStatus::TooLarge;
        }
        if separator == 1 {
            text.push('\n');
        }
        text.push_str(part);
        blocks += 1;
    }
    if blocks == 0 {
        return ResultStatus::NoText;
    }
    ResultStatus::Ok { text }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    plugin_id: String,
    tool_name: String,
    args_sha256: String,
}

#[derive(Debug)]
struct Entry {
    status: ResultStatus,
    args_canonical: Option<String>,
    completed_at: i64,
    /// Completion order, assigned once and never rewritten; distinct from `lru_seq`, which a lookup moves.
    completed_seq: u64,
    /// Position in the process-wide LRU order; rewritten on every touch.
    lru_seq: u64,
    bytes: usize,
}

impl Entry {
    fn snapshot(&self, key: &Key) -> Recorded {
        Recorded {
            plugin_id: key.plugin_id.clone(),
            tool_name: key.tool_name.clone(),
            args_sha256: key.args_sha256.clone(),
            status: self.status.clone(),
            args_canonical: self.args_canonical.clone(),
            completed_at: self.completed_at,
        }
    }
}

#[derive(Default)]
struct Inner {
    tracks: HashMap<String, HashMap<Key, Entry>>,
    /// `lru_seq → (track_id, key)`, least recently used first.
    order: BTreeMap<u64, (String, Key)>,
    /// Feeds `lru_seq` (every insert and touch).
    next_lru_seq: u64,
    /// Feeds `completed_seq` (inserts only).
    next_completed_seq: u64,
    total_bytes: usize,
}

pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

pub struct PluginResults {
    inner: Mutex<Inner>,
    now: Clock,
}

impl std::fmt::Debug for PluginResults {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.lock();
        f.debug_struct("PluginResults")
            .field("tracks", &inner.tracks.len())
            .field("entries", &inner.order.len())
            .field("total_bytes", &inner.total_bytes)
            .finish()
    }
}

impl Default for PluginResults {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginResults {
    pub fn new() -> Self {
        Self::with_clock(Arc::new(crate::model::now_ms))
    }

    /// Test seam: an injectable clock for the TTL rules.
    pub fn with_clock(now: Clock) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            now,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Record one completed proxy call; the caller has already decided it is a Planner's and carries a track.
    pub fn record(
        &self,
        track_id: &str,
        plugin_id: &str,
        tool_name: &str,
        args: &Value,
        result: &CallToolResult,
    ) {
        self.insert(track_id, plugin_id, tool_name, args, |_| classify(result));
    }

    /// Record a proxy call that produced no `CallToolResult` at all: the entry is replaced with `Error` so a later capture cannot pick up the previous body.
    pub fn record_failure(&self, track_id: &str, plugin_id: &str, tool_name: &str, args: &Value) {
        self.insert(track_id, plugin_id, tool_name, args, |_| {
            ResultStatus::Error
        });
    }

    fn insert(
        &self,
        track_id: &str,
        plugin_id: &str,
        tool_name: &str,
        args: &Value,
        status_of: impl FnOnce(&str) -> ResultStatus,
    ) {
        let canonical = canonical_args(args);
        let args_sha256 = sha256_hex(canonical.as_bytes());
        let (status, args_canonical) = if canonical.len() > MAX_ARGS_BYTES {
            (ResultStatus::TooLarge, None)
        } else {
            (status_of(&canonical), Some(canonical))
        };
        let key = Key {
            plugin_id: plugin_id.to_string(),
            tool_name: tool_name.to_string(),
            args_sha256,
        };
        let bytes = match &status {
            ResultStatus::Ok { text } => text.len(),
            _ => 0,
        } + args_canonical.as_ref().map_or(0, String::len);
        let now = (self.now)();
        let mut inner = self.lock();
        inner.evict_expired(now);
        inner.remove(track_id, &key);
        let lru_seq = inner.next_lru_seq;
        inner.next_lru_seq += 1;
        let completed_seq = inner.next_completed_seq;
        inner.next_completed_seq += 1;
        inner
            .order
            .insert(lru_seq, (track_id.to_string(), key.clone()));
        inner.total_bytes += bytes;
        inner
            .tracks
            .entry(track_id.to_string())
            .or_default()
            .insert(
                key,
                Entry {
                    status,
                    args_canonical,
                    completed_at: now,
                    completed_seq,
                    lru_seq,
                    bytes,
                },
            );
        inner.evict_track_overflow(track_id);
        inner.evict_global_overflow();
    }

    /// The entry for exactly this call, if any. A hit is an LRU touch.
    pub fn get(
        &self,
        track_id: &str,
        plugin_id: &str,
        tool_name: &str,
        args_sha256: &str,
    ) -> Option<Recorded> {
        let key = Key {
            plugin_id: plugin_id.to_string(),
            tool_name: tool_name.to_string(),
            args_sha256: args_sha256.to_string(),
        };
        let now = (self.now)();
        let mut inner = self.lock();
        inner.evict_expired(now);
        inner.touch(track_id, &key)
    }

    /// The most recently completed entry for `(plugin, tool)` in this
    /// track, whatever its status. A hit is an LRU touch.
    pub fn latest(&self, track_id: &str, plugin_id: &str, tool_name: &str) -> Option<Recorded> {
        let now = (self.now)();
        let mut inner = self.lock();
        inner.evict_expired(now);
        let key = inner
            .tracks
            .get(track_id)?
            .iter()
            .filter(|(key, _)| key.plugin_id == plugin_id && key.tool_name == tool_name)
            .max_by_key(|(_, entry)| (entry.completed_at, entry.completed_seq))
            .map(|(key, _)| key.clone())?;
        inner.touch(track_id, &key)
    }

    /// Distinct `(plugin_id, tool_name)` pairs with a live entry in this
    /// track, sorted — the candidate pool for resolving a sanitized tool
    /// spelling.
    pub fn recorded_tools(&self, track_id: &str) -> Vec<(String, String)> {
        let now = (self.now)();
        let mut inner = self.lock();
        inner.evict_expired(now);
        let mut tools: Vec<(String, String)> = inner
            .tracks
            .get(track_id)
            .map(|entries| {
                entries
                    .keys()
                    .map(|key| (key.plugin_id.clone(), key.tool_name.clone()))
                    .collect()
            })
            .unwrap_or_default();
        tools.sort();
        tools.dedup();
        tools
    }

    /// Drop every entry of `track_id`.
    pub fn forget_track(&self, track_id: &str) {
        let mut inner = self.lock();
        let Some(entries) = inner.tracks.remove(track_id) else {
            return;
        };
        for entry in entries.values() {
            inner.order.remove(&entry.lru_seq);
            inner.total_bytes = inner.total_bytes.saturating_sub(entry.bytes);
        }
    }

    /// Live entry count for `track_id` (tests).
    pub fn len(&self, track_id: &str) -> usize {
        let now = (self.now)();
        let mut inner = self.lock();
        inner.evict_expired(now);
        inner.tracks.get(track_id).map_or(0, HashMap::len)
    }

    pub fn is_empty(&self, track_id: &str) -> bool {
        self.len(track_id) == 0
    }

    /// Bytes currently accounted (tests).
    pub fn total_bytes(&self) -> usize {
        self.lock().total_bytes
    }
}

impl Inner {
    fn remove(&mut self, track_id: &str, key: &Key) -> Option<Entry> {
        let entries = self.tracks.get_mut(track_id)?;
        let entry = entries.remove(key)?;
        if entries.is_empty() {
            self.tracks.remove(track_id);
        }
        self.order.remove(&entry.lru_seq);
        self.total_bytes = self.total_bytes.saturating_sub(entry.bytes);
        Some(entry)
    }

    /// LRU touch: only `lru_seq` moves; `completed_seq` stays.
    fn touch(&mut self, track_id: &str, key: &Key) -> Option<Recorded> {
        let entries = self.tracks.get_mut(track_id)?;
        let entry = entries.get_mut(key)?;
        let old_seq = entry.lru_seq;
        let lru_seq = self.next_lru_seq;
        self.next_lru_seq += 1;
        entry.lru_seq = lru_seq;
        let snapshot = entry.snapshot(key);
        self.order.remove(&old_seq);
        self.order
            .insert(lru_seq, (track_id.to_string(), key.clone()));
        Some(snapshot)
    }

    fn evict_expired(&mut self, now: i64) {
        let expired: Vec<(String, Key)> = self
            .order
            .values()
            .filter(|(track_id, key)| {
                self.tracks
                    .get(track_id)
                    .and_then(|entries| entries.get(key))
                    .is_some_and(|entry| now - entry.completed_at > TTL_MS)
            })
            .cloned()
            .collect();
        for (track_id, key) in expired {
            self.remove(&track_id, &key);
        }
    }

    fn evict_track_overflow(&mut self, track_id: &str) {
        loop {
            let len = self.tracks.get(track_id).map_or(0, HashMap::len);
            if len <= MAX_ENTRIES_PER_TRACK {
                return;
            }
            let Some(victim) = self
                .order
                .values()
                .find(|(candidate, _)| candidate == track_id)
                .cloned()
            else {
                return;
            };
            self.remove(&victim.0, &victim.1);
        }
    }

    fn evict_global_overflow(&mut self) {
        while self.total_bytes > MAX_TOTAL_BYTES {
            let Some((_, (track_id, key))) = self.order.iter().next() else {
                return;
            };
            let (track_id, key) = (track_id.clone(), key.clone());
            self.remove(&track_id, &key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_host::mcp::ContentBlock;
    use serde_json::json;
    use std::sync::atomic::{AtomicI64, Ordering};

    fn text_block(text: &str) -> ContentBlock {
        ContentBlock {
            kind: "text".into(),
            text: Some(text.into()),
            extra: Default::default(),
        }
    }

    fn ok_result(parts: &[&str]) -> CallToolResult {
        CallToolResult {
            content: parts.iter().map(|p| text_block(p)).collect(),
            is_error: None,
            meta: None,
            structured_content: None,
        }
    }

    fn ring_with_clock() -> (PluginResults, Arc<AtomicI64>) {
        let clock = Arc::new(AtomicI64::new(1_000_000));
        let c = clock.clone();
        let ring = PluginResults::with_clock(Arc::new(move || c.load(Ordering::SeqCst)));
        (ring, clock)
    }

    #[test]
    fn args_hash_is_key_order_independent() {
        let a = json!({ "b": 1, "a": [1, 2] });
        let b: Value = serde_json::from_str(r#"{"a":[1,2],"b":1}"#).unwrap();
        assert_eq!(canonical_args(&a), r#"{"a":[1,2],"b":1}"#);
        assert_eq!(args_sha256(&a), args_sha256(&b));
    }

    #[test]
    fn args_hash_distinguishes_integer_and_float_spellings() {
        let one: Value = serde_json::from_str(r#"{"id":1}"#).unwrap();
        let one_point_zero: Value = serde_json::from_str(r#"{"id":1.0}"#).unwrap();
        assert_eq!(canonical_args(&one), r#"{"id":1}"#);
        assert_eq!(canonical_args(&one_point_zero), r#"{"id":1.0}"#);
        assert_ne!(args_sha256(&one), args_sha256(&one_point_zero));
    }

    #[test]
    fn args_hash_is_array_order_sensitive_even_when_nested() {
        let a = json!({ "ids": [[1, 2], [3]] });
        let b = json!({ "ids": [[2, 1], [3]] });
        assert_ne!(args_sha256(&a), args_sha256(&b));
    }

    #[test]
    fn args_hash_does_not_unicode_normalize() {
        // U+00E9 vs U+0065 U+0301 — canonically equivalent, different bytes.
        let nfc = json!({ "q": "\u{e9}" });
        let nfd = json!({ "q": "e\u{301}" });
        assert_ne!(args_sha256(&nfc), args_sha256(&nfd));
    }

    #[test]
    fn args_hash_of_empty_object_is_the_hash_of_two_braces() {
        assert_eq!(canonical_args(&json!({})), "{}");
        assert_eq!(args_sha256(&json!({})), sha256_hex(b"{}"));
    }

    #[test]
    fn args_hash_of_null_is_the_hash_of_null() {
        assert_eq!(canonical_args(&Value::Null), "null");
        assert_eq!(args_sha256(&Value::Null), sha256_hex(b"null"));
        assert_ne!(args_sha256(&Value::Null), args_sha256(&json!({})));
    }

    #[test]
    fn classify_joins_text_blocks_in_order_with_newlines() {
        let mut result = ok_result(&["第一段", "second"]);
        result.content.insert(
            1,
            ContentBlock {
                kind: "image".into(),
                text: None,
                extra: Default::default(),
            },
        );
        result.structured_content = Some(json!({ "ignored": true }));
        assert_eq!(
            classify(&result),
            ResultStatus::Ok {
                text: "第一段\nsecond".into()
            }
        );
    }

    #[test]
    fn classify_error_no_text_and_too_large() {
        let mut error = ok_result(&["failed"]);
        error.is_error = Some(true);
        assert_eq!(classify(&error), ResultStatus::Error);
        assert_eq!(classify(&ok_result(&[])), ResultStatus::NoText);
        let mut untexted = ok_result(&[]);
        untexted.content.push(ContentBlock {
            kind: "text".into(),
            text: None,
            extra: Default::default(),
        });
        assert_eq!(classify(&untexted), ResultStatus::NoText);
        let huge = "x".repeat(MAX_TEXT_BYTES + 1);
        assert_eq!(classify(&ok_result(&[&huge])), ResultStatus::TooLarge);
        let exact = "x".repeat(MAX_TEXT_BYTES);
        assert!(matches!(
            classify(&ok_result(&[&exact])),
            ResultStatus::Ok { .. }
        ));
    }

    #[test]
    fn same_key_new_call_replaces_the_old_entry_whatever_its_status() {
        let (ring, _) = ring_with_clock();
        let args = json!({ "id": 1 });
        ring.record("t", "p", "tool", &args, &ok_result(&["body"]));
        let first = ring.get("t", "p", "tool", &args_sha256(&args)).unwrap();
        assert_eq!(
            first.status,
            ResultStatus::Ok {
                text: "body".into()
            }
        );
        let mut error = ok_result(&["nope"]);
        error.is_error = Some(true);
        ring.record("t", "p", "tool", &args, &error);
        let second = ring.get("t", "p", "tool", &args_sha256(&args)).unwrap();
        assert_eq!(second.status, ResultStatus::Error);
        assert_eq!(ring.len("t"), 1);
        assert_eq!(ring.total_bytes(), canonical_args(&args).len());
    }

    #[test]
    fn entries_are_track_scoped() {
        let (ring, _) = ring_with_clock();
        let args = json!({ "id": 1 });
        ring.record("t1", "p", "tool", &args, &ok_result(&["body"]));
        assert!(ring.get("t2", "p", "tool", &args_sha256(&args)).is_none());
        assert!(ring.latest("t2", "p", "tool").is_none());
        assert!(ring.recorded_tools("t2").is_empty());
        assert_eq!(ring.recorded_tools("t1"), vec![("p".into(), "tool".into())]);
    }

    #[test]
    fn latest_picks_the_most_recently_completed_entry_for_the_tool() {
        let (ring, clock) = ring_with_clock();
        ring.record("t", "p", "tool", &json!({ "id": 1 }), &ok_result(&["one"]));
        clock.fetch_add(10, Ordering::SeqCst);
        ring.record("t", "p", "tool", &json!({ "id": 2 }), &ok_result(&["two"]));
        clock.fetch_add(10, Ordering::SeqCst);
        ring.record(
            "t",
            "p",
            "other",
            &json!({ "id": 3 }),
            &ok_result(&["three"]),
        );
        let latest = ring.latest("t", "p", "tool").unwrap();
        assert_eq!(latest.args_canonical.as_deref(), Some(r#"{"id":2}"#));
        assert_eq!(latest.status, ResultStatus::Ok { text: "two".into() });
        assert_eq!(latest.registry_name(), "plugin.p_tool");
    }

    /// A single counter serving both completion and LRU order would answer A.
    #[test]
    fn latest_is_completion_order_not_last_access() {
        let (ring, _) = ring_with_clock();
        let a = json!({ "id": "a" });
        let b = json!({ "id": "b" });
        ring.record("t", "p", "tool", &a, &ok_result(&["A"]));
        ring.record("t", "p", "tool", &b, &ok_result(&["B"]));
        assert!(ring.get("t", "p", "tool", &args_sha256(&a)).is_some());
        let latest = ring.latest("t", "p", "tool").unwrap();
        assert_eq!(latest.args_canonical.as_deref(), Some(r#"{"id":"b"}"#));
        // A was touched last, so B is the least recently used and goes first under the per-track cap.
        assert!(ring.get("t", "p", "tool", &args_sha256(&a)).is_some());
        for i in 0..MAX_ENTRIES_PER_TRACK - 1 {
            ring.record("t", "p", "tool", &json!({ "fill": i }), &ok_result(&["x"]));
        }
        assert!(ring.get("t", "p", "tool", &args_sha256(&a)).is_some());
        assert!(ring.get("t", "p", "tool", &args_sha256(&b)).is_none());
    }

    #[test]
    fn record_failure_replaces_a_success_on_the_same_key() {
        let (ring, _) = ring_with_clock();
        let args = json!({ "id": 1 });
        ring.record("t", "p", "tool", &args, &ok_result(&["body"]));
        ring.record_failure("t", "p", "tool", &args);
        let entry = ring.get("t", "p", "tool", &args_sha256(&args)).unwrap();
        assert_eq!(entry.status, ResultStatus::Error);
        assert_eq!(entry.args_canonical.as_deref(), Some(r#"{"id":1}"#));
        assert_eq!(ring.len("t"), 1);
        assert_eq!(ring.total_bytes(), canonical_args(&args).len());
        let latest = ring.latest("t", "p", "tool").unwrap();
        assert_eq!(latest.status, ResultStatus::Error);
    }

    #[test]
    fn classify_stops_at_the_cap_without_joining_the_rest() {
        // Two halves that only exceed the cap once joined with the
        // separator: the second block is refused before it is appended.
        let half = "x".repeat(MAX_TEXT_BYTES / 2);
        let half_less_one = "x".repeat(MAX_TEXT_BYTES / 2 - 1);
        let exact = ok_result(&[&half, &half_less_one]);
        assert!(matches!(classify(&exact), ResultStatus::Ok { .. }));
        assert_eq!(
            classify(&ok_result(&[&half, &half])),
            ResultStatus::TooLarge,
            "the separator byte tips it over"
        );
        // Many small blocks past the cap are refused too.
        let small = "y".repeat(1024);
        let parts: Vec<&str> =
            std::iter::repeat_n(small.as_str(), MAX_TEXT_BYTES / 1024 + 1).collect();
        assert_eq!(classify(&ok_result(&parts)), ResultStatus::TooLarge);
    }

    #[test]
    fn ttl_evicts_lazily_on_lookup_and_insert() {
        let (ring, clock) = ring_with_clock();
        let args = json!({ "id": 1 });
        ring.record("t", "p", "tool", &args, &ok_result(&["body"]));
        clock.fetch_add(TTL_MS, Ordering::SeqCst);
        assert!(ring.get("t", "p", "tool", &args_sha256(&args)).is_some());
        clock.fetch_add(1, Ordering::SeqCst);
        assert!(ring.get("t", "p", "tool", &args_sha256(&args)).is_none());
        assert_eq!(ring.total_bytes(), 0);
        assert!(ring.is_empty("t"));
    }

    #[test]
    fn per_track_cap_evicts_least_recently_used() {
        let (ring, _) = ring_with_clock();
        for i in 0..MAX_ENTRIES_PER_TRACK {
            ring.record("t", "p", "tool", &json!({ "i": i }), &ok_result(&["b"]));
        }
        // Touch entry 0 so it is the most recently used.
        let first = args_sha256(&json!({ "i": 0 }));
        assert!(ring.get("t", "p", "tool", &first).is_some());
        ring.record("t", "p", "tool", &json!({ "i": 999 }), &ok_result(&["b"]));
        assert_eq!(ring.len("t"), MAX_ENTRIES_PER_TRACK);
        assert!(ring.get("t", "p", "tool", &first).is_some());
        assert!(
            ring.get("t", "p", "tool", &args_sha256(&json!({ "i": 1 })))
                .is_none(),
            "entry 1 was the least recently used"
        );
        // Another track is not affected by this track's cap.
        ring.record("u", "p", "tool", &json!({ "i": 0 }), &ok_result(&["b"]));
        assert_eq!(ring.len("u"), 1);
    }

    #[test]
    fn global_byte_cap_evicts_oldest_across_tracks() {
        let (ring, _) = ring_with_clock();
        let big = "x".repeat(MAX_TEXT_BYTES);
        // Four tracks (each under its own 64 cap), 200 distinct calls of
        // ~1 MiB each: the process-wide cap bites after ~127 of them.
        let total_calls = 200usize;
        for i in 0..total_calls {
            ring.record(
                &format!("t{}", i % 4),
                "p",
                "tool",
                &json!({ "i": i }),
                &ok_result(&[&big]),
            );
            assert!(ring.total_bytes() <= MAX_TOTAL_BYTES, "after call {i}");
        }
        let live: usize = (0..4).map(|t| ring.len(&format!("t{t}"))).sum();
        assert!(live < total_calls, "something was evicted: {live}");
        assert!(live >= MAX_TOTAL_BYTES / (MAX_TEXT_BYTES + 16), "{live}");
        assert!(
            ring.get("t0", "p", "tool", &args_sha256(&json!({ "i": 0 })))
                .is_none(),
            "the oldest entry went first"
        );
        assert!(
            ring.get(
                "t3",
                "p",
                "tool",
                &args_sha256(&json!({ "i": total_calls - 1 }))
            )
            .is_some(),
            "the newest entry stays"
        );
    }

    #[test]
    fn oversized_args_record_too_large_without_keeping_them() {
        let (ring, _) = ring_with_clock();
        let args = json!({ "blob": "y".repeat(MAX_ARGS_BYTES) });
        ring.record("t", "p", "tool", &args, &ok_result(&["body"]));
        let entry = ring.get("t", "p", "tool", &args_sha256(&args)).unwrap();
        assert_eq!(entry.status, ResultStatus::TooLarge);
        assert_eq!(entry.args_canonical, None);
        assert_eq!(ring.total_bytes(), 0);
    }

    #[test]
    fn forget_track_drops_only_that_track() {
        let (ring, _) = ring_with_clock();
        ring.record("t", "p", "tool", &json!({}), &ok_result(&["a"]));
        ring.record("u", "p", "tool", &json!({}), &ok_result(&["bb"]));
        ring.forget_track("t");
        assert!(ring.is_empty("t"));
        assert_eq!(ring.len("u"), 1);
        assert_eq!(ring.total_bytes(), 2 + 2);
        ring.forget_track("never-seen");
    }
}
