use super::*;
use serde_json::json;
use std::io::Write;

const ERROR: &str = "Your access token could not be refreshed because your refresh token was already used. private-fixture-detail";
const LOG: &str = "2026-10-07T01:00:00Z ERROR codex_core::auth: Failed to refresh token: \
    Your access token could not be refreshed because your refresh token was already used.";
fn observer(root: &std::path::Path) -> CodexAuthentication {
    CodexAuthentication::new(
        root.join("checkpoint.json"),
        root.join("codex-home"),
        root.join("stderr.log"),
    )
}
fn login(auth: &CodexAuthentication) {
    auth.observe(&Notification::Other {
        method: "account/login/completed".into(),
        params: json!({"success":true,"error":null}),
    });
}
fn append(root: &std::path::Path, text: &str) {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("stderr.log"))
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
}
fn start(auth: &CodexAuthentication, id: &str) {
    auth.observe(&Notification::TurnStarted {
        thread_id: "thread".into(),
        turn: json!({"id":id}),
    });
}
fn complete(auth: &CodexAuthentication, id: &str, status: &str) {
    auth.observe(&Notification::TurnCompleted {
        thread_id: "thread".into(),
        turn: json!({"id":id,"status":status,"error":null}),
    });
}

#[test]
fn confirmed_failure_and_login_are_atomically_restored_without_native_details() {
    let tmp = tempfile::tempdir().unwrap();
    let auth = observer(tmp.path());
    auth.record(0, ERROR);
    assert_eq!(
        observer(tmp.path()).problem(),
        Some(AuthenticationFailure::Reused)
    );
    let bytes = std::fs::read_to_string(tmp.path().join("checkpoint.json")).unwrap();
    assert!(!bytes.contains("private-fixture-detail") && !bytes.contains("Your access token"));
    login(&auth);
    auth.record(0, ERROR);
    let restored = observer(tmp.path());
    assert_eq!(restored.generation(), 1);
    assert_eq!(restored.problem(), None);
    assert_eq!(restored.hold(), None);
    restored.record(1, ERROR);
    assert_eq!(
        observer(tmp.path()).problem(),
        Some(AuthenticationFailure::Reused)
    );
}

#[test]
fn late_stderr_after_login_is_only_a_report_and_cannot_suspend_requests() {
    let tmp = tempfile::tempdir().unwrap();
    let auth = observer(tmp.path());
    auth.record(0, ERROR);
    login(&auth);
    append(tmp.path(), &format!("{LOG}\n"));
    auth.poll(None);
    assert_eq!(auth.problem(), None);
    assert_eq!(auth.hold(), None);
    assert_eq!(
        auth.notice().unwrap().kind,
        AuthenticationNoticeKind::RefreshErrorReported
    );
    let restored = observer(tmp.path());
    assert_eq!(restored.hold(), None);
    assert_eq!(
        restored.notice().unwrap().kind,
        AuthenticationNoticeKind::RefreshErrorReported
    );
}

#[test]
fn only_successful_current_turn_clears_report_never_confirmed_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let auth = observer(tmp.path());
    start(&auth, "old");
    login(&auth);
    append(tmp.path(), &format!("{LOG}\n"));
    auth.poll(None);
    complete(&auth, "old", "completed");
    assert!(
        auth.notice().is_some(),
        "old completion cannot clear a new report"
    );
    start(&auth, "failed");
    complete(&auth, "failed", "failed");
    assert!(
        auth.notice().is_some(),
        "a failed completion proves no recovery"
    );
    start(&auth, "current");
    complete(&auth, "current", "completed");
    assert_eq!(auth.notice(), None);
    assert_eq!(observer(tmp.path()).notice(), None);
    auth.record(auth.generation(), ERROR);
    start(&auth, "another");
    complete(&auth, "another", "completed");
    assert!(
        auth.hold().is_some(),
        "a usable access token cannot repair a failed refresh credential"
    );
    assert!(observer(tmp.path()).hold().is_some());
}

#[test]
fn log_rotation_truncate_regrow_and_split_records_are_read_without_latching() {
    let tmp = tempfile::tempdir().unwrap();
    let auth = observer(tmp.path());
    append(tmp.path(), "neutral\n");
    auth.poll(None);
    std::fs::rename(tmp.path().join("stderr.log"), tmp.path().join("old.log")).unwrap();
    append(tmp.path(), &LOG[..40]);
    auth.poll(None);
    assert_eq!(auth.notice(), None, "a partial record is not evidence");
    append(tmp.path(), &format!("{}\n", &LOG[40..]));
    auth.poll(None);
    assert!(auth.notice().is_some());
    login(&auth);
    let neutral = "x".repeat(LOG.len() + 20);
    std::fs::write(tmp.path().join("stderr.log"), format!("{neutral}\n")).unwrap();
    auth.poll(None);
    assert_eq!(auth.notice(), None);
    std::fs::write(
        tmp.path().join("stderr.log"),
        format!("{LOG}\n{}\n", "n".repeat(300)),
    )
    .unwrap();
    auth.poll(None);
    assert!(
        auth.notice().is_some(),
        "same-inode truncate/regrow must be detected by its anchor"
    );
    assert_eq!(auth.hold(), None);
}

#[test]
fn login_discards_old_partial_record_and_oversized_records_stay_bounded() {
    let tmp = tempfile::tempdir().unwrap();
    let auth = observer(tmp.path());
    append(tmp.path(), &LOG[..40]);
    auth.poll(None);
    login(&auth);
    append(tmp.path(), &format!("{}\n", &LOG[40..]));
    auth.poll(None);
    assert_eq!(auth.notice(), None);
    append(tmp.path(), &"a".repeat(128 * 1024));
    auth.poll(None);
    auth.poll(None);
    append(tmp.path(), &format!("{LOG}\n{LOG}\n"));
    auth.poll(None);
    assert!(
        auth.notice().is_some(),
        "valid record following a discarded huge line is still read"
    );
    assert_eq!(auth.hold(), None);
}

#[test]
fn malformed_checkpoint_is_visible_and_restoring_storage_releases_maintenance_hold() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("checkpoint.json"), "not json").unwrap();
    let auth = observer(tmp.path());
    assert_eq!(
        auth.notice().unwrap().kind,
        AuthenticationNoticeKind::StateUnavailable
    );
    assert!(auth.hold().is_some());
    std::fs::write(
        tmp.path().join("checkpoint.json"),
        serde_json::to_vec(&store::Checkpoint::empty(tmp.path().join("codex-home"))).unwrap(),
    )
    .unwrap();
    auth.poll(None);
    assert_eq!(auth.hold(), None);
}

#[test]
fn failed_login_checkpoint_write_stays_visible_until_it_retries_successfully() {
    let tmp = tempfile::tempdir().unwrap();
    let auth = observer(tmp.path());
    auth.record(0, ERROR);
    std::fs::remove_file(tmp.path().join("checkpoint.json")).unwrap();
    std::fs::create_dir(tmp.path().join("checkpoint.json")).unwrap();
    login(&auth);
    assert_eq!(
        auth.notice().unwrap().kind,
        AuthenticationNoticeKind::StateUnavailable
    );
    assert!(auth.hold().is_some());
    std::fs::remove_dir(tmp.path().join("checkpoint.json")).unwrap();
    auth.poll(None);
    assert_eq!(auth.hold(), None);
    assert_eq!(observer(tmp.path()).hold(), None);
}

#[test]
fn another_codex_home_does_not_inherit_a_confirmed_failure() {
    let tmp = tempfile::tempdir().unwrap();
    observer(tmp.path()).record(0, ERROR);
    let other = CodexAuthentication::new(
        tmp.path().join("checkpoint.json"),
        tmp.path().join("other-home"),
        tmp.path().join("stderr.log"),
    );
    assert_eq!(other.problem(), None);
    assert_eq!(other.hold(), None);
}

#[test]
fn owner_retry_is_a_durable_cas_intent_and_new_failure_relocks_it() {
    let tmp = tempfile::tempdir().unwrap();
    let auth = observer(tmp.path());
    auth.record(0, ERROR);
    start(&auth, "old");
    let revision = auth.notice().unwrap().revision;
    append(tmp.path(), &format!("{LOG}\n"));
    auth.poll(None);
    assert_eq!(
        auth.notice().unwrap().revision,
        revision,
        "unattributed logs cannot invalidate the confirmed episode's CAS"
    );
    let requested = auth.request_retry(&revision).unwrap();
    assert_ne!(requested, revision);
    assert_eq!(auth.hold(), None);
    assert_eq!(
        auth.notice().unwrap().kind,
        AuthenticationNoticeKind::RetryRequested
    );
    assert!(
        auth.request_retry(&revision).is_err(),
        "lost-response replay cannot rearm twice"
    );
    let restored = observer(tmp.path());
    assert_eq!(
        restored.notice().unwrap().kind,
        AuthenticationNoticeKind::RetryRequested
    );
    assert_eq!(restored.hold(), None);
    assert!(
        std::fs::read_to_string(tmp.path().join("checkpoint.json"))
            .unwrap()
            .contains("failed_generation"),
        "original evidence remains recorded, not falsely marked repaired"
    );
    auth.record(0, ERROR);
    complete(&auth, "old", "completed");
    assert_eq!(
        auth.notice().unwrap().kind,
        AuthenticationNoticeKind::RetryRequested
    );
    auth.record(auth.generation(), ERROR);
    assert!(auth.hold().is_some());
    assert_ne!(auth.notice().unwrap().revision, requested);
    assert!(observer(tmp.path()).hold().is_some());
    assert!(
        auth.request_retry(&revision).is_err(),
        "an old observed failure cannot authorize retry of a new confirmed episode"
    );
    assert!(auth.hold().is_some());
}

#[test]
fn retry_intent_cannot_open_issuance_before_its_checkpoint_commits() {
    let tmp = tempfile::tempdir().unwrap();
    let auth = observer(tmp.path());
    auth.record(0, ERROR);
    let revision = auth.notice().unwrap().revision;
    std::fs::remove_file(tmp.path().join("checkpoint.json")).unwrap();
    std::fs::create_dir(tmp.path().join("checkpoint.json")).unwrap();
    assert!(auth.request_retry(&revision).is_err());
    assert!(auth.hold().is_some());
    std::fs::remove_dir(tmp.path().join("checkpoint.json")).unwrap();
    auth.poll(None);
    assert_eq!(auth.hold(), None);
    assert_eq!(
        observer(tmp.path()).notice().unwrap().kind,
        AuthenticationNoticeKind::RetryRequested
    );
}

#[test]
fn old_connection_cannot_clear_or_reassert_current_authentication_state() {
    let tmp = tempfile::tempdir().unwrap();
    let auth = observer(tmp.path());
    let old_epoch = auth.bind_client(1);
    let old_stamp = auth.stamp(1);
    let current_epoch = auth.bind_client(2);
    assert_eq!(auth.stamp(1).map(|s| s.connection_epoch), None);
    auth.record_stamped(auth.stamp(2), ERROR);
    auth.observe_from_connection(
        old_epoch,
        &Notification::Other {
            method: "account/login/completed".into(),
            params: json!({"success":true,"error":null}),
        },
    );
    assert!(
        auth.hold().is_some(),
        "old daemon's completion is not current login proof"
    );
    auth.observe_from_connection(
        current_epoch,
        &Notification::Other {
            method: "account/login/completed".into(),
            params: json!({"success":true,"error":null}),
        },
    );
    auth.record_stamped(old_stamp, ERROR);
    assert_eq!(auth.hold(), None);
    assert_eq!(observer(tmp.path()).hold(), None);
}
