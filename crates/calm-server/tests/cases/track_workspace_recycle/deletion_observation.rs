//! Capture real route observations while the actual lifecycle/writer owner is held.
use super::*;
use std::sync::Mutex;
use std::time::Duration;
use tracing::field::{Field, Visit};
use tracing::instrument::WithSubscriber;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;

type Record = BTreeMap<String, Value>;
const LIMIT: Duration = Duration::from_secs(10);
const PARK: Duration = Duration::from_millis(50);

#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<Record>>>, Arc<tokio::sync::Notify>);
struct Fields(Record);
impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0
            .insert(field.name().into(), format!("{value:?}").into());
    }
}
impl<S> Layer<S> for Recorder
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_event(&self, event: &tracing::Event<'_>, context: Context<'_, S>) {
        if !matches!(
            event.metadata().target(),
            "calm_server::track_deletion" | "calm_truth::db::transaction"
        ) {
            return;
        }
        let mut fields = Fields(BTreeMap::new());
        event.record(&mut fields);
        if let Some(scope) = context.event_scope(event) {
            fields.0.insert(
                "span_path".into(),
                json!(scope.from_root().map(|s| s.name()).collect::<Vec<_>>()),
            );
        }
        self.0.lock().unwrap().push(fields.0);
        self.1.notify_one();
    }
}
impl Recorder {
    fn dispatch(&self) -> tracing::Dispatch {
        tracing::Dispatch::new(tracing_subscriber::registry().with(self.clone()))
    }
    fn records(&self) -> Vec<Record> {
        self.0.lock().unwrap().clone()
    }
    fn find(&self, stage: &str, outcome: &str) -> Vec<Record> {
        self.records()
            .into_iter()
            .filter(|r| {
                r.get("stage").and_then(Value::as_str) == Some(stage)
                    && r.get("outcome").and_then(Value::as_str) == Some(outcome)
            })
            .collect()
    }
    async fn wait(&self, stage: &str, outcome: &str) {
        tokio::time::timeout(LIMIT, async {
            loop {
                let notified = self.1.notified();
                if !self.find(stage, outcome).is_empty() {
                    break;
                }
                notified.await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("missing {stage}/{outcome}: {:?}", self.records()));
    }
    fn one(&self, stage: &str, outcome: &str) -> Record {
        let mut rows = self.find(stage, outcome);
        assert_eq!(rows.len(), 1, "{stage}/{outcome}: {:?}", self.records());
        rows.remove(0)
    }
}

#[tokio::test]
async fn track_delete_observation_distinguishes_operation_lock_from_writer_admission() {
    let b = boot().await;
    let area = create_area(&b, "timing").await;
    let (track, _) = managed_track(&b, &area, "wait").await;
    let operation = b.operation_runtime.lock_for_track_delete_for_test().await;
    let logs = Recorder::default();
    let app = b.app.clone();
    let id = track.clone();
    let task = tokio::spawn(
        async move { request(app, "DELETE", &format!("/api/tracks/{id}"), None).await }
            .with_subscriber(logs.dispatch()),
    );
    logs.wait("operation_lock", "begin").await;
    tokio::time::sleep(PARK).await;
    assert!(!task.is_finished());
    assert!(logs.find("operation_lock", "ok").is_empty());
    assert!(logs.find("preflight", "begin").is_empty());
    // Hold the real sole writer before allowing deletion past the lifecycle fence.
    let writer = b.repo.db().writer_pool().acquire().await.unwrap();
    drop(operation);
    logs.wait("reprojection", "begin").await;
    tokio::time::sleep(PARK).await;
    assert!(!task.is_finished());
    assert!(logs.find("reprojection", "ok").is_empty());
    drop(writer);
    let (status, body) = tokio::time::timeout(LIMIT, task).await.unwrap().unwrap();
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    for stage in ["operation_lock", "reprojection"] {
        let row = logs.one(stage, "ok");
        assert!(row["elapsed_ms"].as_f64().unwrap() >= PARK.as_secs_f64() * 500.0);
    }
    let transactions = logs
        .records()
        .into_iter()
        .filter(|r| r.contains_key("acquire_ms"))
        .collect::<Vec<_>>();
    assert!(
        transactions
            .iter()
            .any(|r| r["acquire_ms"].as_f64().unwrap() >= PARK.as_secs_f64() * 500.0)
    );
    assert!(transactions.iter().all(|r| {
        r["span_path"]
            .as_array()
            .unwrap()
            .contains(&json!("track_deletion"))
    }));
    logs.one("owned_workflow", "ok");
    logs.one("request", "ok");
    let stage_totals = logs
        .records()
        .into_iter()
        .filter(|r| {
            r.get("outcome").and_then(Value::as_str) != Some("begin") && r.contains_key("stage")
        })
        .collect::<Vec<_>>();
    eprintln!(
        "track_delete_stage_diagnostic={}",
        json!({"stages": stage_totals, "transactions": transactions})
    );
    assert!(b.repo.track_get(&track).await.unwrap().is_none());
    assert!(trash_entry_for(&b.workspace_root, &track).is_some());
}

#[tokio::test]
async fn track_delete_observation_keeps_owned_completion_after_request_cancellation() {
    let b = boot().await;
    let area = create_area(&b, "timing").await;
    let (track, path) = managed_track(&b, &area, "cancel").await;
    let hook = calm_server::routes::tracks::TrackDeleteCommitHook {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
        panic_after_release: false,
    };
    calm_server::routes::tracks::install_track_delete_commit_hook_for_test(&track, hook.clone());
    let logs = Recorder::default();
    let app = b.app.clone();
    let id = track.clone();
    let task = tokio::spawn(
        async move { request(app, "DELETE", &format!("/api/tracks/{id}"), None).await }
            .with_subscriber(logs.dispatch()),
    );
    tokio::time::timeout(LIMIT, hook.entered.notified())
        .await
        .unwrap();
    assert!(!path.exists());
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    logs.wait("request", "interrupted").await;
    assert!(logs.find("owned_workflow", "interrupted").is_empty());
    hook.release.notify_one();
    logs.wait("owned_workflow", "ok").await;
    let cancelled = logs.one("request", "interrupted");
    let completed = logs.one("owned_workflow", "ok");
    assert_eq!(cancelled["deletion_id"], completed["deletion_id"]);
    assert_eq!(completed["track_id"], track);
    assert!(logs.find("request", "ok").is_empty());
    assert!(b.repo.track_get(&track).await.unwrap().is_none());
    assert!(trash_entry_for(&b.workspace_root, &track).is_some());
}

#[tokio::test]
async fn track_delete_observation_reports_preflight_error_without_owned_work() {
    let b = boot().await;
    let logs = Recorder::default();
    let (status, _) = request(b.app.clone(), "DELETE", "/api/tracks/not-present", None)
        .with_subscriber(logs.dispatch())
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    logs.one("preflight", "error");
    logs.one("request", "error");
    assert!(logs.find("owned_workflow", "begin").is_empty());
}

#[tokio::test]
async fn track_delete_observation_retains_compensation_after_caught_panic() {
    let b = boot().await;
    let area = create_area(&b, "timing").await;
    let (track, path) = managed_track(&b, &area, "panic").await;
    let before = fingerprint(&path);
    let hook = calm_server::routes::tracks::TrackDeleteCommitHook {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
        panic_after_release: true,
    };
    calm_server::routes::tracks::install_track_delete_commit_hook_for_test(&track, hook.clone());
    let logs = Recorder::default();
    let app = b.app.clone();
    let id = track.clone();
    let task = tokio::spawn(
        async move { request(app, "DELETE", &format!("/api/tracks/{id}"), None).await }
            .with_subscriber(logs.dispatch()),
    );
    tokio::time::timeout(LIMIT, hook.entered.notified())
        .await
        .unwrap();
    hook.release.notify_one();
    let (status, _) = tokio::time::timeout(LIMIT, task).await.unwrap().unwrap();
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    logs.one("compensation", "ok");
    logs.one("owned_workflow", "error");
    logs.one("request", "error");
    assert!(b.repo.track_get(&track).await.unwrap().is_some());
    assert!(path.is_dir());
    assert!(diff(&before, &fingerprint(&path)).is_empty());
    assert!(trash_entry_for(&b.workspace_root, &track).is_none());
}
