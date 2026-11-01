use super::{
    Storage,
    model::*,
    reader::{self, ReadError, Reader},
    service,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Mutex;
const START: i64 = 1_793_484_000_000;
fn config() -> Config {
    Config {
        track_id: "track-fixture".into(),
        cli_path: "/usr/bin/fixture-cli".into(),
        home: "/tmp".into(),
        profile: "fixture".into(),
        self_open_id: "ou_self".into(),
        import_state_path: "/tmp/fixture-state".into(),
    }
}
fn legacy() -> Value {
    json!({"schema_version":1,"enabled":true,"identity":"user","profile":"fixture","self_open_id":"ou_self","interval_minutes":1,"overlap_minutes":5,"monitor_sources":["group","incoming_p2p"],"group":{"chat_id":"oc_fixture"},"source_watermarks":{"group":START,"incoming_p2p":START},"tracked_threads":[{"thread_id":"omt_fixture","last_read_message_time":START-3_600_000}],"notified_message_ids":[],"handled_message_ids":[],"acknowledged_in_chat_message_ids":[],"pending_items":[],"arbitrary_provenance":{"ask":"retain"}})
}
fn message(id: &str, at: i64) -> Value {
    json!({"message_id":id,"create_time":at,"sender":{"id":"ou_other"},"content":"a reply"})
}
fn envelope(messages: Vec<Value>) -> Value {
    json!({"ok":true,"identity":"user","data":{"messages":messages},"meta":{"pagination":{"complete":true}}})
}
struct FakeReader {
    group: Value,
    thread: Value,
    p2p: Value,
    identity: Result<(), ReadError>,
    calls: Mutex<Vec<String>>,
}
impl FakeReader {
    fn empty() -> Self {
        Self {
            group: envelope(vec![]),
            thread: envelope(vec![]),
            p2p: envelope(vec![]),
            identity: Ok(()),
            calls: Mutex::new(vec![]),
        }
    }
}
#[async_trait]
impl Reader for FakeReader {
    async fn verify_identity(&self, _: &Config) -> Result<(), ReadError> {
        self.identity
    }
    async fn read(
        &self,
        _: &Config,
        source: &Source,
        _: i64,
        _: i64,
    ) -> Result<Vec<Message>, ReadError> {
        self.calls.lock().unwrap().push(format!("{source:?}"));
        let value = match source {
            Source::Group { .. } => &self.group,
            Source::Thread { .. } => &self.thread,
            _ => &self.p2p,
        };
        reader::parse_output(&serde_json::to_vec(value).unwrap())
    }
}
struct Memory {
    state: Mutex<Option<State>>,
    wakes: Mutex<Vec<Wake>>,
}
impl Memory {
    fn ready() -> Self {
        Self {
            state: Mutex::new(Some(service::initialize(&config(), legacy()).unwrap())),
            wakes: Mutex::new(vec![]),
        }
    }
    fn get(&self) -> State {
        self.state.lock().unwrap().clone().unwrap()
    }
}
#[async_trait]
impl Storage for Memory {
    async fn load(&self) -> Result<Option<State>, String> {
        Ok(self.state.lock().unwrap().clone())
    }
    async fn commit(
        &self,
        expected: Option<u64>,
        state: State,
        wake: Option<Wake>,
    ) -> Result<(), String> {
        let mut current = self.state.lock().unwrap();
        if current.as_ref().map(|s| s.revision) != expected {
            return Err("conflict".into());
        }
        *current = Some(state);
        if let Some(w) = wake {
            self.wakes.lock().unwrap().push(w);
        }
        Ok(())
    }
}
#[tokio::test]
async fn empty_complete_rounds_update_independent_progress_without_waking() {
    let storage = Memory::ready();
    let reader = FakeReader::empty();
    let before = serde_json::to_vec(&storage.get()).unwrap().len();
    for i in 0..400 {
        service::poll(&storage, &reader, &config(), START + i * 60_000)
            .await
            .unwrap();
    }
    assert!(storage.wakes.lock().unwrap().is_empty());
    assert!(storage.get().inbox.is_empty());
    assert!(
        serde_json::to_vec(&storage.get()).unwrap().len() <= before + 40,
        "quiet history must not accumulate in durable continuation"
    );
    assert!(
        storage
            .get()
            .sources
            .values()
            .all(|s| s.cursor_ms == START + 399 * 60_000)
    );
    assert_eq!(reader.calls.lock().unwrap().len(), 1200);
}

#[tokio::test]
async fn delayed_thread_reply_uses_its_own_cursor_instead_of_newer_group_cursor() {
    let storage = Memory::ready();
    let mut reader = FakeReader::empty();
    reader.thread = envelope(vec![message("om_delayed", START - 1_800_000)]);
    service::poll(&storage, &reader, &config(), START)
        .await
        .unwrap();
    let state = storage.get();
    assert_eq!(state.inbox.len(), 1);
    assert!(
        state
            .inbox
            .values()
            .next()
            .unwrap()
            .messages
            .contains_key("om_delayed")
    );
    assert_eq!(storage.wakes.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn missing_pagination_never_advances_failed_source_but_successful_siblings_advance() {
    let storage = Memory::ready();
    let mut reader = FakeReader::empty();
    reader.group.as_object_mut().unwrap().remove("meta");
    let now = START + 60_000;
    service::poll(&storage, &reader, &config(), now)
        .await
        .unwrap();
    let state = storage.get();
    assert_eq!(state.sources["group"].cursor_ms, START);
    assert_eq!(state.sources["group"].fault, Some(ReadError::Incomplete));
    assert_eq!(state.sources["incoming_p2p"].cursor_ms, now);
    service::poll(&storage, &reader, &config(), now + 60_000)
        .await
        .unwrap();
    assert_eq!(
        storage.wakes.lock().unwrap().len(),
        1,
        "unchanged fault must stay quiet"
    );
    reader.group = envelope(vec![]);
    service::poll(&storage, &reader, &config(), now + 120_000)
        .await
        .unwrap();
    assert_eq!(
        storage.wakes.lock().unwrap().len(),
        2,
        "recovery wakes once"
    );
}
#[tokio::test]
async fn malformed_message_discards_entire_source_and_does_not_commit_partial_inbox() {
    let storage = Memory::ready();
    let mut reader = FakeReader::empty();
    reader.group = envelope(vec![message("good", START), json!({"message_id":"bad"})]);
    service::poll(&storage, &reader, &config(), START + 60_000)
        .await
        .unwrap();
    assert!(storage.get().inbox.is_empty());
    assert_eq!(storage.get().sources["group"].cursor_ms, START);
    assert_eq!(
        storage.get().sources["group"].fault,
        Some(ReadError::Malformed)
    );
}
#[tokio::test]
async fn restart_deduplicates_unsettled_inbox_and_source_overlap() {
    let storage = Memory::ready();
    let mut reader = FakeReader::empty();
    reader.group = envelope(vec![message("same", START)]);
    reader.p2p = reader.group.clone();
    service::poll(&storage, &reader, &config(), START)
        .await
        .unwrap();
    let persisted = serde_json::to_vec(&storage.get()).unwrap();
    let restarted = Memory {
        state: Mutex::new(Some(serde_json::from_slice(&persisted).unwrap())),
        wakes: Mutex::new(vec![]),
    };
    service::poll(&restarted, &reader, &config(), START + 60_000)
        .await
        .unwrap();
    assert!(restarted.wakes.lock().unwrap().is_empty());
    assert_eq!(restarted.get().inbox.len(), 1);
    let state = restarted.get();
    let batch = state.inbox.values().next().unwrap();
    assert_eq!(batch.messages.len(), 1);
    assert!(batch.settlement.is_none());
    assert_eq!(
        state.legacy["arbitrary_provenance"],
        legacy()["arbitrary_provenance"]
    );
}
#[tokio::test]
async fn identity_switch_prevents_reads_and_progress_then_recovers_once() {
    let storage = Memory::ready();
    let mut reader = FakeReader::empty();
    reader.identity = Err(ReadError::IdentityMismatch);
    service::poll(&storage, &reader, &config(), START + 60_000)
        .await
        .unwrap();
    assert!(reader.calls.lock().unwrap().is_empty());
    assert_eq!(storage.get().sources["group"].cursor_ms, START);
    assert_eq!(storage.wakes.lock().unwrap().len(), 1);
    service::poll(&storage, &reader, &config(), START + 120_000)
        .await
        .unwrap();
    assert_eq!(storage.wakes.lock().unwrap().len(), 1);
    reader.identity = Ok(());
    service::poll(&storage, &reader, &config(), START + 180_000)
        .await
        .unwrap();
    assert_eq!(storage.wakes.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn reminder_wakes_between_scans_and_immutable_settlement_retry_does_not_rearm() {
    let storage = Memory::ready();
    let mut reader = FakeReader::empty();
    reader.group = envelope(vec![message("msg", START)]);
    service::poll(&storage, &reader, &config(), START)
        .await
        .unwrap();
    let id = storage.get().inbox.keys().next().unwrap().clone();
    let deadline = Deadline {
        key: "review".into(),
        at_ms: START + 15_000,
        note: "review pending source".into(),
        fired: false,
    };
    service::settle(
        &storage,
        &id,
        json!({"decision":"defer"}),
        vec![deadline.clone()],
        vec![],
        START,
    )
    .await
    .unwrap();
    service::poll(&storage, &reader, &config(), START + 15_000)
        .await
        .unwrap();
    assert_eq!(storage.wakes.lock().unwrap().len(), 2);
    service::settle(
        &storage,
        &id,
        json!({"decision":"defer"}),
        vec![deadline],
        vec![],
        START + 20_000,
    )
    .await
    .unwrap();
    service::poll(&storage, &reader, &config(), START + 30_000)
        .await
        .unwrap();
    assert_eq!(storage.wakes.lock().unwrap().len(), 2);
    assert!(storage.get().due["review"].fired);
    assert_eq!(
        reader.calls.lock().unwrap().len(),
        3,
        "reminders do not force message reads"
    );
}
#[tokio::test]
async fn import_is_bounded_identity_checked_preserves_all_provenance_and_hands_off_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config();
    cfg.import_state_path = dir.path().join("legacy.json");
    let original = legacy();
    std::fs::write(
        &cfg.import_state_path,
        serde_json::to_vec(&original).unwrap(),
    )
    .unwrap();
    let storage = Memory {
        state: Mutex::new(None),
        wakes: Mutex::new(vec![]),
    };
    service::poll(&storage, &FakeReader::empty(), &cfg, START)
        .await
        .unwrap();
    assert_eq!(storage.get().legacy, original);
    assert!(storage.get().inbox.contains_key("import"));
    assert!(
        storage.wakes.lock().unwrap()[0]
            .text
            .contains("coverage is unknown")
    );
    service::poll(&storage, &FakeReader::empty(), &cfg, START)
        .await
        .unwrap();
    assert_eq!(storage.wakes.lock().unwrap().len(), 1);
    cfg.profile = "other".into();
    assert!(service::initialize(&cfg, legacy()).is_err());
}
#[test]
fn missing_incomplete_wrong_identity_and_oversized_outputs_are_rejected() {
    let mut output = envelope(vec![]);
    output["meta"]["pagination"]["complete"] = json!(false);
    assert_eq!(
        reader::parse_output(&serde_json::to_vec(&output).unwrap()).unwrap_err(),
        ReadError::Incomplete
    );
    output["identity"] = json!("bot");
    assert_eq!(
        reader::parse_output(&serde_json::to_vec(&output).unwrap()).unwrap_err(),
        ReadError::Malformed
    );
    assert_eq!(
        reader::parse_output(&vec![b' '; super::MAX_STATE_BYTES + 1]).unwrap_err(),
        ReadError::OutputLimit
    );
}
#[tokio::test]
async fn state_cap_retains_previous_cursor_and_inbox_without_silent_pruning() {
    let storage = Memory::ready();
    let mut reader = FakeReader::empty();
    let mut huge = message("large", START);
    huge["content"] = json!("x".repeat(super::MAX_STATE_BYTES));
    reader.group = envelope(vec![huge]);
    // Reader itself enforces output bounds; source fault persists without a cursor move.
    service::poll(&storage, &reader, &config(), START + 60_000)
        .await
        .unwrap();
    assert_eq!(storage.get().sources["group"].cursor_ms, START);
    assert_eq!(
        storage.get().sources["group"].fault,
        Some(ReadError::OutputLimit)
    );
    let mut state = storage.get();
    state.legacy["large"] = json!("x".repeat(super::MAX_STATE_BYTES));
    assert!(service::check_size(&state).is_err());
}
#[test]
fn stable_batch_pagination_has_no_missing_or_repeated_messages() {
    let mut state = service::initialize(&config(), legacy()).unwrap();
    let messages = reader::parse_output(
        &serde_json::to_vec(&envelope(vec![
            message("a", START),
            message("b", START),
            message("c", START),
        ]))
        .unwrap(),
    )
    .unwrap();
    state.inbox.insert(
        "batch".into(),
        Batch {
            id: "batch".into(),
            created_at_ms: START,
            messages: messages.into_iter().map(|m| (m.id.clone(), m)).collect(),
            settlement: None,
        },
    );
    let first = service::inbox_page(&state, "batch", None, 2).unwrap();
    assert_eq!(
        first
            .messages
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    let second = service::inbox_page(&state, "batch", first.next_after.as_deref(), 2).unwrap();
    assert_eq!(second.messages[0].id, "c");
    assert!(second.next_after.is_none());
    assert!(service::inbox_page(&state, "batch", Some("missing"), 2).is_err());
}
#[test]
fn argv_is_read_only_scoped_and_respects_each_commands_documented_limits() {
    let cfg = config();
    let search = reader::argv(&cfg, &Source::AtMe, START, START + 60_000).unwrap();
    assert!(search.contains(&"--is-at-me".into()));
    assert!(!search.contains(&"--order".into()));
    assert!(search.windows(2).any(|a| a == ["--page-limit", "40"]));
    let thread = reader::argv(
        &cfg,
        &Source::Thread {
            thread_id: "omt_fixture".into(),
        },
        START,
        START + 60_000,
    )
    .unwrap();
    assert!(!thread.contains(&"--start".into()));
    assert!(!thread.iter().any(|x| x.contains("send")));
    assert!(thread.windows(2).any(|a| a == ["--as", "user"]));
    crate::manifest::Manifest::parse(super::MANIFEST).unwrap();
}

#[tokio::test]
async fn later_batch_can_cancel_reminder_and_settlement_retry_remains_idempotent() {
    let storage = Memory::ready();
    let mut reader = FakeReader::empty();
    reader.group = envelope(vec![message("msg", START)]);
    service::poll(&storage, &reader, &config(), START)
        .await
        .unwrap();
    let first = storage.get().inbox.keys().next().unwrap().clone();
    let deadline = Deadline {
        key: "review".into(),
        at_ms: START + 180_000,
        note: "review".into(),
        fired: false,
    };
    service::settle(
        &storage,
        &first,
        json!({"decision":"defer"}),
        vec![deadline],
        vec![],
        START,
    )
    .await
    .unwrap();
    reader.group = envelope(vec![message("reply", START + 60_000)]);
    service::poll(&storage, &reader, &config(), START + 60_000)
        .await
        .unwrap();
    let second = storage.get().inbox.keys().last().unwrap().clone();
    service::settle(
        &storage,
        &second,
        json!({"decision":"resolved"}),
        vec![],
        vec!["review".into()],
        START + 60_000,
    )
    .await
    .unwrap();
    service::settle(
        &storage,
        &second,
        json!({"decision":"resolved"}),
        vec![],
        vec!["review".into()],
        START + 240_000,
    )
    .await
    .unwrap();
    service::poll(&storage, &reader, &config(), START + 240_000)
        .await
        .unwrap();
    assert!(storage.get().due.is_empty());
    assert_eq!(storage.wakes.lock().unwrap().len(), 2);
}

#[test]
fn actual_whoami_contract_requires_available_expected_user() {
    let cfg = config();
    let mut value = json!({"identity":"user","available":true,"onBehalfOf":{"openId":"ou_self"}});
    reader::parse_identity(&serde_json::to_vec(&value).unwrap(), &cfg).unwrap();
    value["onBehalfOf"]["openId"] = json!("ou_changed");
    assert_eq!(
        reader::parse_identity(&serde_json::to_vec(&value).unwrap(), &cfg),
        Err(ReadError::IdentityMismatch)
    );
    value["onBehalfOf"]["openId"] = json!("ou_self");
    value["available"] = json!(false);
    assert_eq!(
        reader::parse_identity(&serde_json::to_vec(&value).unwrap(), &cfg),
        Err(ReadError::IdentityMismatch)
    );
}
#[tokio::test]
async fn aggregate_state_cap_does_not_checkpoint_or_publish_unstored_inbox() {
    let storage = Memory::ready();
    let mut reader = FakeReader::empty();
    let large = |prefix: &str| {
        envelope(
            (0..32)
                .map(|i| {
                    let mut v = message(&format!("{prefix}-{i}"), START);
                    v["content"] = json!("x".repeat(20_000));
                    v
                })
                .collect(),
        )
    };
    reader.group = large("g");
    reader.p2p = large("p");
    let before = serde_json::to_value(storage.get()).unwrap();
    let error = service::poll(&storage, &reader, &config(), START + 60_000)
        .await
        .unwrap_err();
    assert!(error.contains("1 MiB"));
    assert_eq!(serde_json::to_value(storage.get()).unwrap(), before);
    assert!(storage.wakes.lock().unwrap().is_empty());
}
#[cfg(unix)]
#[tokio::test]
async fn production_cli_reader_runs_only_scoped_argv_with_explicit_environment() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config();
    cfg.cli_path = dir.path().join("fixture-cli");
    cfg.home = dir.path().into();
    let script = r#"#!/bin/sh
set -eu
[ "$HOME" = "$PWD" ] || exit 2
[ "$LANG" = "C.UTF-8" ] || exit 3
[ "$LARKSUITE_CLI_NO_UPDATE_NOTIFIER" = "1" ] || exit 4
[ "$LARKSUITE_CLI_NO_SKILLS_NOTIFIER" = "1" ] || exit 5
for key in $(/usr/bin/env | /usr/bin/cut -d= -f1); do
 case "$key" in HOME|PATH|LANG|LARKSUITE_CLI_NO_UPDATE_NOTIFIER|LARKSUITE_CLI_NO_SKILLS_NOTIFIER|PWD) ;; *) exit 6 ;; esac
done
if [ "$1" = "whoami" ]; then
 [ "$2" = "--as" ] && [ "$3" = "user" ] && [ "$4" = "--profile" ] && [ "$5" = "fixture" ] || exit 7
 printf '%s' '{"identity":"user","available":true,"onBehalfOf":{"openId":"ou_self"}}'
else
 [ "$1" = "im" ] && [ "$2" = "+threads-messages-list" ] && [ "$3" = "--thread" ] && [ "$4" = "omt_fixture" ] || exit 8
 printf '%s' '{"ok":true,"identity":"user","data":{"messages":[]},"meta":{"pagination":{"complete":true}}}'
fi
"#;
    std::fs::write(&cfg.cli_path, script).unwrap();
    std::fs::set_permissions(&cfg.cli_path, std::fs::Permissions::from_mode(0o700)).unwrap();
    reader::CliReader.verify_identity(&cfg).await.unwrap();
    assert!(
        reader::CliReader
            .read(
                &cfg,
                &Source::Thread {
                    thread_id: "omt_fixture".into()
                },
                START,
                START + 1
            )
            .await
            .unwrap()
            .is_empty()
    );
}

#[test]
fn import_requires_known_legacy_schema_without_defaulting_missing_version() {
    let mut source = legacy();
    source.as_object_mut().unwrap().remove("schema_version");
    assert!(service::initialize(&config(), source).is_err());
    let mut source = legacy();
    source["schema_version"] = json!(999);
    assert!(service::initialize(&config(), source).is_err());
}

#[tokio::test]
async fn ordinary_user_answer_can_cancel_without_new_message_batch() {
    let storage = Memory::ready();
    let reader = FakeReader::empty();
    let mut state = storage.get();
    state.inbox.insert(
        "import".into(),
        Batch {
            id: "import".into(),
            created_at_ms: START,
            messages: Default::default(),
            settlement: None,
        },
    );
    *storage.state.lock().unwrap() = Some(state);
    let reminder = Deadline {
        key: "review".into(),
        at_ms: START + 120_000,
        note: "original Ask reference".into(),
        fired: false,
    };
    service::settle(
        &storage,
        "import",
        json!({"ask_id":1}),
        vec![reminder],
        vec![],
        START,
    )
    .await
    .unwrap();
    let revision = storage.get().revision;
    service::change_reminders(
        &storage,
        "answered",
        revision,
        vec![],
        vec!["review".into()],
        START + 1,
    )
    .await
    .unwrap();
    service::change_reminders(
        &storage,
        "answered",
        revision,
        vec![],
        vec!["review".into()],
        START + 2,
    )
    .await
    .unwrap();
    service::poll(&storage, &reader, &config(), START + 180_000)
        .await
        .unwrap();
    assert!(storage.get().due.is_empty());
    assert!(storage.wakes.lock().unwrap().is_empty());
    assert_eq!(storage.get().inbox.len(), 1);
}
#[tokio::test]
async fn reminder_only_wake_can_be_deferred_and_retry_does_not_rearm() {
    let storage = Memory::ready();
    let reader = FakeReader::empty();
    let reminder = Deadline {
        key: "review".into(),
        at_ms: START + 10,
        note: "review pending".into(),
        fired: false,
    };
    service::change_reminders(
        &storage,
        "initial",
        storage.get().revision,
        vec![reminder],
        vec![],
        START,
    )
    .await
    .unwrap();
    service::poll(&storage, &reader, &config(), START + 20)
        .await
        .unwrap();
    assert!(storage.get().due["review"].fired);
    let revision = storage.get().revision;
    let later = Deadline {
        key: "review".into(),
        at_ms: START + 100,
        note: "user deferred review".into(),
        fired: false,
    };
    service::change_reminders(
        &storage,
        "defer",
        revision,
        vec![later.clone()],
        vec![],
        START + 30,
    )
    .await
    .unwrap();
    service::poll(&storage, &reader, &config(), START + 110)
        .await
        .unwrap();
    let wakes = storage.wakes.lock().unwrap().len();
    assert_eq!(wakes, 2);
    service::change_reminders(
        &storage,
        "defer",
        revision,
        vec![later],
        vec![],
        START + 120,
    )
    .await
    .unwrap();
    service::poll(&storage, &reader, &config(), START + 130)
        .await
        .unwrap();
    assert_eq!(storage.wakes.lock().unwrap().len(), wakes);
    assert!(
        service::change_reminders(
            &storage,
            "defer",
            revision,
            vec![],
            vec!["review".into()],
            START + 140
        )
        .await
        .is_err()
    );
    assert!(
        service::change_reminders(
            &storage,
            "stale",
            revision,
            vec![],
            vec!["review".into()],
            START + 140
        )
        .await
        .is_err()
    );
}
