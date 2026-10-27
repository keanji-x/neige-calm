//! Observation of the original owner's Report branch, only in fixture test builds.
//! No sender escapes the owner; dropping a receiver only cancels observation.
use super::*;
use calm_truth::db::TrackEvent;
use tokio::sync::broadcast;

const COMPLETION_CAPACITY: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReportOutcome {
    Written,
    Unchanged,
    NoTrack,
    Failed(String),
}

impl ReportOutcome {
    pub(super) fn from_result(result: &crate::error::Result<Recompute>) -> Self {
        match result {
            Ok(Recompute::Written(_)) => Self::Written,
            Ok(Recompute::Unchanged(_)) => Self::Unchanged,
            Ok(Recompute::NoTrack) => Self::NoTrack,
            Err(error) => Self::Failed(error.to_string()),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ReportCompletion {
    Report {
        env: Box<BroadcastEnvelope>,
        outcome: ReportOutcome,
    },
    InputLagged(u64),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CompletionError {
    Closed,
    InputLagged(u64),
    OutputLagged(u64),
    IdentityConflict,
    NoTrack,
    Failed(String),
}

pub(super) fn channel() -> (
    broadcast::Sender<ReportCompletion>,
    broadcast::Receiver<ReportCompletion>,
) {
    broadcast::channel(COMPLETION_CAPACITY)
}

impl ActivityWake {
    pub(crate) fn subscribe_report_completions_for_test(
        &self,
    ) -> Result<broadcast::Receiver<ReportCompletion>, CompletionError> {
        let sender = self.1.upgrade().ok_or(CompletionError::Closed)?;
        let receiver = sender.subscribe();
        drop(sender); // Never retain this temporary strong reference across a wait.
        Ok(receiver)
    }
}

/// Consume this same pre-subscribed stream across the write join and durable-id lookup.
/// Other Reports cannot complete the target; a conflicting identity for its id is fatal.
pub(crate) async fn wait_for_report(
    receiver: &mut broadcast::Receiver<ReportCompletion>,
    expected: &TrackEvent,
) -> Result<ReportOutcome, CompletionError> {
    assert!(expected.id > 0);
    assert!(matches!(expected.event, Event::TrackReportEdited { .. }));
    loop {
        match receiver.recv().await {
            Err(RecvError::Closed) => return Err(CompletionError::Closed),
            Err(RecvError::Lagged(n)) => return Err(CompletionError::OutputLagged(n)),
            Ok(ReportCompletion::InputLagged(n)) => return Err(CompletionError::InputLagged(n)),
            Ok(ReportCompletion::Report { env, outcome }) => {
                if env.id != expected.id {
                    continue;
                }
                if env.actor != expected.actor
                    || env.scope != expected.scope
                    || serde_json::to_value(&env.event).expect("Event serializes")
                        != serde_json::to_value(&expected.event).expect("Event serializes")
                {
                    return Err(CompletionError::IdentityConflict);
                }
                return match outcome {
                    ReportOutcome::Written | ReportOutcome::Unchanged => Ok(outcome),
                    ReportOutcome::NoTrack => Err(CompletionError::NoTrack),
                    ReportOutcome::Failed(error) => Err(CompletionError::Failed(error)),
                };
            }
        }
    }
}

use crate::db::sqlite::SqlxRepo;
use crate::db::{RepoEventWrite, RepoRead, RepoSyncDomainRaw};
use crate::event::EditAuthor;
use crate::ids::CardId;
use crate::model::{NewArea, NewCard, NewTrack, RequestTheme};
use crate::test_seams::{PausePoint, install_pause_for_test};

struct Rig {
    repo: Arc<SqlxRepo>,
    bus: EventBus,
    write: WriteContext,
    track: String,
    report: CardId,
    _root: tempfile::TempDir,
}

impl Rig {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let repo = Arc::new(
            SqlxRepo::open(&format!(
                "sqlite://{}?mode=rwc",
                root.path().join("activity.db").display()
            ))
            .await
            .unwrap(),
        );
        let area = repo
            .area_create(
                calm_types::organization::LEGACY_OWNER_USER_ID,
                NewArea {
                    name: "activity completion".into(),
                    color: "#000000".into(),
                    sort: None,
                },
            )
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                area_id: area.id,
                title: "original owner".into(),
                cwd: root.path().display().to_string(),
                sort: None,
                template_id: None,
                template_input: None,
                plugin_scope: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let report = repo
            .card_create(NewCard {
                track_id: track.id.clone(),
                title: None,
                kind: "track-report".into(),
                sort: None,
                payload: serde_json::to_value(
                    calm_types::track_report::TrackReportPayload::initial(),
                )
                .unwrap(),
            })
            .await
            .unwrap();
        let write = WriteContext::new(
            repo.card_role_cache().clone(),
            repo.track_area_cache().clone(),
        );
        Self {
            repo,
            write,
            bus: EventBus::new(),
            track: track.id.to_string(),
            report: report.id,
            _root: root,
        }
    }

    fn owner(&self, bus: &EventBus) -> TrackActivityProjector {
        TrackActivityProjector::new(
            self.repo.clone(),
            bus.clone(),
            self.write.clone(),
            HarnessRegistry::new(),
            TerminalRendererRegistry::new(),
        )
        .unwrap()
    }

    async fn report(&self, bus: &EventBus) -> TrackEvent {
        let track = self.repo.track_get(&self.track).await.unwrap().unwrap();
        let id = self
            .repo
            .log_pure_event(
                ActorId::Kernel,
                EventScope::Card {
                    card: self.report.clone(),
                    track: track.id.clone(),
                    area: track.area_id,
                },
                None,
                bus,
                self.repo.card_role_cache(),
                self.repo.track_area_cache(),
                Event::TrackReportEdited {
                    track_id: track.id,
                    card_id: self.report.clone(),
                    author: EditAuthor::Planner,
                    author_plugin_id: None,
                    edit_id: uuid::Uuid::new_v4().to_string(),
                    summary_before: String::new(),
                    summary_after: String::new(),
                    body_before: String::new(),
                    body_after: String::new(),
                    agent_message: None,
                },
            )
            .await
            .unwrap();
        self.repo
            .events_for_track(&self.track, &["track.report_edited"], None)
            .await
            .unwrap()
            .into_iter()
            .find(|event| event.id == id)
            .unwrap()
    }

    async fn overlay_events(&self) -> usize {
        self.repo
            .events_for_track(&self.track, &["overlay.set"], None)
            .await
            .unwrap()
            .len()
    }

    async fn remove_overlay(&self) {
        // The original run may select the Report before its boot tick. Either
        // startup order must leave this fixture with no prior activity overlay.
        let stored = self.repo.overlays_for("track", &self.track).await.unwrap();
        if stored.iter().any(|overlay| {
            overlay.plugin_id == KERNEL_OVERLAY_PLUGIN_ID && overlay.kind == ACTIVITY_OVERLAY_KIND
        }) {
            self.repo
                .overlay_delete(
                    KERNEL_OVERLAY_PLUGIN_ID,
                    "track",
                    &self.track,
                    ACTIVITY_OVERLAY_KIND,
                )
                .await
                .unwrap();
        }
    }
}

fn envelope(event: &TrackEvent) -> BroadcastEnvelope {
    BroadcastEnvelope {
        id: event.id,
        actor: event.actor.clone(),
        scope: event.scope.clone(),
        event: event.event.clone(),
        event_version: crate::event::SYNC_EVENT_VERSION,
    }
}

fn hold(point: &str, track: &str) -> PausePoint {
    let pause = PausePoint::default();
    install_pause_for_test(point, track, pause.clone());
    pause
}

async fn entered(pause: &PausePoint) {
    tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
        .await
        .unwrap();
}

async fn completed(
    rx: &mut broadcast::Receiver<ReportCompletion>,
    edit: &TrackEvent,
) -> Result<ReportOutcome, CompletionError> {
    tokio::time::timeout(Duration::from_secs(5), wait_for_report(rx, edit))
        .await
        .unwrap()
}

// Keeps failure paths from detaching a running owner. stop also proves actual destruction.
struct Running(tokio::task::JoinHandle<()>);
impl Running {
    fn new(owner: TrackActivityProjector) -> Self {
        Self(tokio::spawn(owner.run()))
    }
    async fn stop(mut self) {
        self.0.abort();
        assert!((&mut self.0).await.unwrap_err().is_cancelled());
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[tokio::test]
async fn written_waits_for_recompute_and_commit() {
    let rig = Rig::new().await;
    let owner = rig.owner(&rig.bus);
    let wake = owner.wake();
    let mut rx = wake.subscribe_report_completions_for_test().unwrap();
    let before = hold("activity-report-before-recompute", &rig.track);
    // Emitted before run is ever polled: construction must already subscribe the original bus.
    let edit = rig.report(&rig.bus).await;
    let running = Running::new(owner);
    entered(&before).await;
    rig.remove_overlay().await; // Arrange a real Written even if the boot sweep absorbed this input.
    let count = rig.overlay_events().await;
    let write = hold("activity-overlay-before-commit", &rig.track);
    let completion = wait_for_report(&mut rx, &edit);
    tokio::pin!(completion);
    assert!(futures_util::poll!(completion.as_mut()).is_pending());
    before.release.notify_one();
    entered(&write).await;
    assert!(futures_util::poll!(completion.as_mut()).is_pending());
    assert_eq!(rig.overlay_events().await, count, "upsert is not COMMIT");
    assert!(
        rig.repo
            .overlays_for("track", &rig.track)
            .await
            .unwrap()
            .is_empty()
    );
    write.release.notify_one();
    let outcome = tokio::time::timeout(Duration::from_secs(5), completion)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outcome, ReportOutcome::Written);
    assert_eq!(rig.overlay_events().await, count + 1);
    assert_eq!(
        rig.repo
            .overlays_for("track", &rig.track)
            .await
            .unwrap()
            .len(),
        1
    );
    eprintln!(
        "real run Written for event {}: overlay event committed after pre-commit Pending",
        edit.id
    );
    running.stop().await;
}

#[tokio::test]
async fn unchanged_waits_for_original_report_branch() {
    let rig = Rig::new().await;
    let owner = rig.owner(&rig.bus);
    let mut rx = owner
        .wake()
        .subscribe_report_completions_for_test()
        .unwrap();
    let first_hold = hold("activity-report-before-recompute", &rig.track);
    let first = rig.report(&rig.bus).await;
    let running = Running::new(owner);
    entered(&first_hold).await;
    rig.remove_overlay().await;
    // The preceding original bus branch reads both durable edits. No direct recompute in this test.
    let target = rig.report(&rig.bus).await;
    let target_hold = hold("activity-report-before-recompute", &rig.track);
    first_hold.release.notify_one();
    entered(&target_hold).await;
    assert_eq!(
        completed(&mut rx, &first).await.unwrap(),
        ReportOutcome::Written
    );
    let count = rig.overlay_events().await;
    let completion = wait_for_report(&mut rx, &target);
    tokio::pin!(completion);
    assert!(futures_util::poll!(completion.as_mut()).is_pending());
    target_hold.release.notify_one();
    let outcome = tokio::time::timeout(Duration::from_secs(5), completion)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outcome, ReportOutcome::Unchanged);
    assert_eq!(rig.overlay_events().await, count);
    eprintln!(
        "real run Unchanged for event {} after preceding event {} absorbed the input",
        target.id, first.id
    );
    running.stop().await;
}

#[tokio::test]
async fn no_track_is_not_success() {
    let rig = Rig::new().await;
    let owner = rig.owner(&rig.bus);
    let mut rx = owner
        .wake()
        .subscribe_report_completions_for_test()
        .unwrap();
    let pause = hold("activity-report-before-recompute", &rig.track);
    let edit = rig.report(&rig.bus).await;
    let running = Running::new(owner);
    entered(&pause).await;
    rig.repo.track_delete(&rig.track).await.unwrap();
    pause.release.notify_one();
    assert_eq!(
        completed(&mut rx, &edit).await,
        Err(CompletionError::NoTrack)
    );
    running.stop().await;
}

#[tokio::test]
async fn real_recompute_error_is_not_success() {
    let rig = Rig::new().await;
    let owner = rig.owner(&rig.bus);
    let mut rx = owner
        .wake()
        .subscribe_report_completions_for_test()
        .unwrap();
    let pause = hold("activity-report-before-recompute", &rig.track);
    let edit = rig.report(&rig.bus).await;
    let running = Running::new(owner);
    entered(&pause).await;
    sqlx::query("ALTER TABLE overlays RENAME TO unavailable_overlays")
        .execute(rig.repo.pool())
        .await
        .unwrap();
    pause.release.notify_one();
    let result = completed(&mut rx, &edit).await;
    sqlx::query("ALTER TABLE unavailable_overlays RENAME TO overlays")
        .execute(rig.repo.pool())
        .await
        .unwrap();
    assert!(
        matches!(result, Err(CompletionError::Failed(ref error)) if !error.is_empty()),
        "{result:?}"
    );
    running.stop().await;
}

#[tokio::test]
async fn unrun_owner_and_detached_wakes_close() {
    let rig = Rig::new().await;
    let owner = rig.owner(&rig.bus);
    let wake = owner.wake();
    let clone = wake.clone();
    let mut rx = wake.subscribe_report_completions_for_test().unwrap();
    let mut second = clone.subscribe_report_completions_for_test().unwrap();
    drop(owner);
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap(),
        Err(RecvError::Closed)
    ));
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), second.recv())
            .await
            .unwrap(),
        Err(RecvError::Closed)
    ));
    assert_eq!(
        clone.subscribe_report_completions_for_test().unwrap_err(),
        CompletionError::Closed
    );
    assert_eq!(
        ActivityWake::detached()
            .subscribe_report_completions_for_test()
            .unwrap_err(),
        CompletionError::Closed
    );
}

#[tokio::test]
async fn aborted_run_closes_with_wakes_alive() {
    let rig = Rig::new().await;
    let owner = rig.owner(&rig.bus);
    let wake = owner.wake();
    let clone = wake.clone();
    let mut rx = wake.subscribe_report_completions_for_test().unwrap();
    let pause = hold("activity-report-before-recompute", &rig.track);
    let edit = rig.report(&rig.bus).await;
    let running = Running::new(owner);
    entered(&pause).await;
    running.stop().await;
    assert_eq!(
        completed(&mut rx, &edit).await,
        Err(CompletionError::Closed)
    );
    assert_eq!(
        clone.subscribe_report_completions_for_test().unwrap_err(),
        CompletionError::Closed
    );
}

#[tokio::test]
async fn panicking_run_closes_with_wakes_alive() {
    let rig = Rig::new().await;
    let owner = rig.owner(&rig.bus);
    let wake = owner.wake();
    let mut rx = wake.subscribe_report_completions_for_test().unwrap();
    let pause = hold("activity-report-before-recompute", &rig.track);
    let edit = rig.report(&rig.bus).await;
    let panic = Arc::new(tokio::sync::Notify::new());
    let trigger = panic.clone();
    let mut task = tokio::spawn(async move {
        let run = owner.run();
        let triggered = trigger.notified();
        tokio::pin!(run, triggered);
        // Panic while the polled original run future is still owned here, so its
        // destruction is caused by task unwinding, not a select loser being dropped first.
        futures_util::future::poll_fn(|cx| {
            if std::future::Future::poll(triggered.as_mut(), cx).is_ready() {
                panic!("unwind the active owner future");
            }
            assert!(std::future::Future::poll(run.as_mut(), cx).is_pending());
            std::task::Poll::<()>::Pending
        })
        .await;
    });
    entered(&pause).await;
    panic.notify_one();
    let joined = match tokio::time::timeout(Duration::from_secs(5), &mut task).await {
        Ok(joined) => joined,
        Err(error) => {
            task.abort();
            let _ = task.await;
            panic!("owner panic did not finish: {error}");
        }
    };
    assert!(joined.unwrap_err().is_panic());
    assert_eq!(
        completed(&mut rx, &edit).await,
        Err(CompletionError::Closed)
    );
    assert_eq!(
        wake.subscribe_report_completions_for_test().unwrap_err(),
        CompletionError::Closed
    );
}

#[tokio::test]
async fn completed_before_wait_survives_owner_destruction() {
    let rig = Rig::new().await;
    let other = Rig::new().await;
    let owner = rig.owner(&rig.bus);
    let mut rx = owner
        .wake()
        .subscribe_report_completions_for_test()
        .unwrap();
    let target = rig.report(&rig.bus).await;
    let sentinel = other.report(&rig.bus).await;
    let pause = hold("activity-report-before-recompute", &other.track);
    let running = Running::new(owner);
    entered(&pause).await; // The serial owner has already sent target's completion.
    running.stop().await;
    assert!(matches!(
        completed(&mut rx, &target).await.unwrap(),
        ReportOutcome::Written | ReportOutcome::Unchanged
    ));
    assert_eq!(
        completed(&mut rx, &sentinel).await,
        Err(CompletionError::Closed)
    );
}

#[tokio::test]
async fn different_owner_cannot_complete_same_envelope() {
    let rig = Rig::new().await;
    let other_bus = EventBus::new();
    let first = rig.owner(&rig.bus);
    let second = rig.owner(&other_bus);
    let mut a = first
        .wake()
        .subscribe_report_completions_for_test()
        .unwrap();
    let mut b = second
        .wake()
        .subscribe_report_completions_for_test()
        .unwrap();
    let edit = rig.report(&rig.bus).await;
    other_bus.emit_envelope_for_test(envelope(&edit));
    let running = Running::new(first);
    completed(&mut a, &edit).await.unwrap();
    let waiting = wait_for_report(&mut b, &edit);
    tokio::pin!(waiting);
    assert!(futures_util::poll!(waiting.as_mut()).is_pending());
    running.stop().await;
    let running = Running::new(second);
    tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .unwrap()
        .unwrap();
    running.stop().await;
}

#[tokio::test]
async fn unmatched_completion_stays_pending() {
    let rig = Rig::new().await;
    let other = Rig::new().await;
    let owner = rig.owner(&rig.bus);
    let mut rx = owner
        .wake()
        .subscribe_report_completions_for_test()
        .unwrap();
    let first = rig.report(&rig.bus).await;
    let target = rig.report(&EventBus::new()).await; // Durable, but not delivered to this owner.
    assert_ne!(first.id, target.id);
    other.report(&rig.bus).await;
    let pause = hold("activity-report-before-recompute", &other.track);
    let running = Running::new(owner);
    entered(&pause).await; // The unmatched first completion is buffered on this same receiver.
    let waiting = wait_for_report(&mut rx, &target);
    tokio::pin!(waiting);
    assert!(futures_util::poll!(waiting.as_mut()).is_pending());
    running.stop().await;
    assert_eq!(waiting.await, Err(CompletionError::Closed));
}

#[tokio::test]
async fn same_id_identity_conflict_is_fatal() {
    let rig = Rig::new().await;
    let owner = rig.owner(&rig.bus);
    let mut rx = owner
        .wake()
        .subscribe_report_completions_for_test()
        .unwrap();
    let edit = rig.report(&EventBus::new()).await;
    let mut conflict = envelope(&edit);
    conflict.actor = ActorId::KernelDispatcher;
    rig.bus.emit_envelope_for_test(conflict);
    let running = Running::new(owner);
    assert_eq!(
        completed(&mut rx, &edit).await,
        Err(CompletionError::IdentityConflict)
    );
    running.stop().await;
}

#[tokio::test]
async fn input_lag_is_fatal() {
    let rig = Rig::new().await;
    let owner = rig.owner(&rig.bus);
    let mut rx = owner
        .wake()
        .subscribe_report_completions_for_test()
        .unwrap();
    let pause = hold("activity-report-before-recompute", &rig.track);
    let edit = rig.report(&rig.bus).await;
    // Exceed the real EventBus's bounded queue before the original receiver is polled.
    for _ in 0..2048 {
        rig.bus.emit_envelope_for_test(envelope(&edit));
    }
    let running = Running::new(owner);
    entered(&pause).await;
    assert!(
        matches!(completed(&mut rx, &edit).await, Err(CompletionError::InputLagged(n)) if n > 0)
    );
    running.stop().await;
}

#[tokio::test]
async fn completion_lag_is_fatal() {
    let rig = Rig::new().await;
    let owner = rig.owner(&rig.bus);
    let wake = owner.wake();
    let mut rx = wake.subscribe_report_completions_for_test().unwrap();
    let mut progress = wake.subscribe_report_completions_for_test().unwrap();
    let edit = rig.report(&EventBus::new()).await;
    rig.repo.track_delete(&rig.track).await.unwrap();
    let running = Running::new(owner);
    // Observe each real run return on a second completion receiver, keeping at
    // most one upstream input in flight while the receiver under test falls behind.
    for _ in 0..=COMPLETION_CAPACITY {
        rig.bus.emit_envelope_for_test(envelope(&edit));
        assert_eq!(
            completed(&mut progress, &edit).await,
            Err(CompletionError::NoTrack)
        );
    }
    assert!(
        matches!(completed(&mut rx, &edit).await, Err(CompletionError::OutputLagged(n)) if n > 0)
    );
    running.stop().await;
}
