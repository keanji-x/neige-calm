use super::*;
use crate::db::sqlite::{SqlxRepo, begin_immediate_tx, begin_immediate_tx_once};
use std::collections::BTreeMap;
use std::future::{Future, poll_fn};
use std::sync::{Arc, Mutex};
use tokio::sync::{Notify, oneshot};
use tokio::time::{sleep, timeout};
use tracing::Instrument;
use tracing::field::{Field, Visit};
use tracing::instrument::WithSubscriber;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;

type Record = BTreeMap<String, serde_json::Value>;
const LIMIT: Duration = Duration::from_secs(10);
const PARK: Duration = Duration::from_millis(50);

#[derive(Clone, Default)]
struct Recorder {
    records: Arc<Mutex<Vec<Record>>>,
    retried: Arc<Notify>,
}

struct Fields(Record);
impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
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
    fn on_event(&self, event: &tracing::Event<'_>, ctx: Context<'_, S>) {
        let mut fields = Fields(BTreeMap::new());
        event.record(&mut fields);
        if event.metadata().target() == TARGET {
            assert_eq!(*event.metadata().level(), tracing::Level::DEBUG);
            if let Some(scope) = ctx.event_scope(event) {
                let names: Vec<_> = scope
                    .from_root()
                    .map(|span| span.metadata().name())
                    .collect();
                fields
                    .0
                    .insert("span_path".into(), serde_json::json!(names));
            }
            self.records.lock().unwrap().push(fields.0);
        } else if fields.0.get("message").and_then(serde_json::Value::as_str)
            == Some("sqlite: BEGIN IMMEDIATE hit transient writer contention; retrying")
        {
            self.retried.notify_one();
        }
    }
}
impl Recorder {
    fn dispatch(&self) -> tracing::Dispatch {
        tracing::Dispatch::new(tracing_subscriber::registry().with(self.clone()))
    }
    fn records(&self) -> Vec<Record> {
        self.records.lock().unwrap().clone()
    }
    fn only(&self) -> Record {
        let records = self.records();
        assert_eq!(
            records.len(),
            1,
            "one terminal event per invocation: {records:?}"
        );
        records.into_iter().next().unwrap()
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    repo: SqlxRepo,
}
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("observations.db").display());
        let repo = SqlxRepo::open(&url).await.unwrap();
        sqlx::query("CREATE TABLE observation_probe(value TEXT NOT NULL)")
            .execute(repo.pool())
            .await
            .unwrap();
        Self { _dir: dir, repo }
    }
    async fn values(&self) -> Vec<String> {
        sqlx::query_scalar("SELECT value FROM observation_probe ORDER BY rowid")
            .fetch_all(self.repo.pool())
            .await
            .unwrap()
    }
}

fn elapsed(record: &Record, name: &str) -> f64 {
    record[name].as_f64().unwrap()
}

// Signal only AFTER the real helper future has been polled, avoiding a spawn/start race.
async fn first_polled<F: Future>(future: F, signal: oneshot::Sender<()>) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut signal = Some(signal);
    poll_fn(move |cx| {
        let result = future.as_mut().poll(cx);
        if let Some(signal) = signal.take() {
            let _ = signal.send(());
        }
        result
    })
    .await
}

#[tokio::test]
async fn observation_commit_rollback_and_active_drop_have_one_truthful_terminal_record() {
    let fx = Fixture::new().await;
    let recorder = Recorder::default();
    let dispatch = recorder.dispatch();
    async {
        let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
        sqlx::query("INSERT INTO observation_probe VALUES('private-observation-sentinel')")
            .execute(&mut **tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
        sqlx::query("INSERT INTO observation_probe VALUES('rolled back')")
            .execute(&mut **tx)
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
        sqlx::query("INSERT INTO observation_probe VALUES('dropped')")
            .execute(&mut **tx)
            .await
            .unwrap();
        drop(tx);
    }
    .with_subscriber(dispatch)
    .await;
    assert_eq!(fx.values().await, vec!["private-observation-sentinel"]);
    let records = recorder.records();
    assert_eq!(records.len(), 3);
    assert_eq!(
        records
            .iter()
            .map(|r| r["outcome"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["committed", "rolled_back", "active_dropped"]
    );
    for record in records {
        assert_eq!(record["attempts"], 1);
        assert_eq!(record["begin_attempts"], 1);
        assert!(
            record["source_file"]
                .as_str()
                .unwrap()
                .ends_with("observation_tests.rs")
        );
        assert!(record["source_line"].as_u64().unwrap() > 0);
        for key in [
            "acquire_ms",
            "begin_ms",
            "backoff_ms",
            "active_ms",
            "commit_ms",
            "rollback_ms",
            "total_ms",
        ] {
            assert!(elapsed(&record, key) >= 0.0);
        }
        assert!(
            !serde_json::to_string(&record)
                .unwrap()
                .contains("private-observation-sentinel")
        );
    }
}

#[tokio::test]
async fn observation_pool_wait_is_separate_from_begin_wait() {
    let fx = Fixture::new().await;
    let pool = fx.repo.pool().clone();
    let mut held = Vec::new();
    for _ in 0..pool.options().get_max_connections() {
        held.push(pool.acquire().await.unwrap());
    }
    let recorder = Recorder::default();
    let dispatch = recorder.dispatch();
    let (started, ready) = oneshot::channel();
    let task = tokio::spawn(
        async move {
            first_polled(begin_immediate_tx(&pool), started)
                .await
                .unwrap()
                .commit()
                .await
                .unwrap();
        }
        .with_subscriber(dispatch),
    );
    ready.await.unwrap();
    sleep(PARK).await;
    assert!(
        !task.is_finished(),
        "all pooled connections remain checked out"
    );
    drop(held);
    timeout(LIMIT, task).await.unwrap().unwrap();
    let record = recorder.only();
    assert_eq!(record["outcome"], "committed");
    assert!(elapsed(&record, "acquire_ms") >= PARK.as_secs_f64() * 1000.0);
    assert!(elapsed(&record, "acquire_ms") > elapsed(&record, "begin_ms"));
}

#[tokio::test]
async fn observation_begin_wait_is_separate_when_a_connection_is_available() {
    let fx = Fixture::new().await;
    let holder = begin_immediate_tx(fx.repo.pool()).await.unwrap();
    // Ensure the observed task can acquire an existing connection rather than establish one.
    drop(fx.repo.pool().acquire().await.unwrap());
    let pool = fx.repo.pool().clone();
    let recorder = Recorder::default();
    let dispatch = recorder.dispatch();
    let (started, ready) = oneshot::channel();
    let task = tokio::spawn(
        async move {
            first_polled(begin_immediate_tx(&pool), started)
                .await
                .unwrap()
                .commit()
                .await
                .unwrap();
        }
        .with_subscriber(dispatch),
    );
    ready.await.unwrap();
    sleep(PARK).await;
    assert!(
        !task.is_finished(),
        "the real SQLite writer remains owned by holder"
    );
    holder.commit().await.unwrap();
    timeout(LIMIT, task).await.unwrap().unwrap();
    let record = recorder.only();
    assert_eq!(record["outcome"], "committed");
    assert!(elapsed(&record, "begin_ms") > elapsed(&record, "acquire_ms"));
}

#[tokio::test]
async fn observation_cancelling_pool_wait_records_interruption_without_a_begin() {
    let fx = Fixture::new().await;
    let pool = fx.repo.pool().clone();
    let mut held = Vec::new();
    for _ in 0..pool.options().get_max_connections() {
        held.push(pool.acquire().await.unwrap());
    }
    let recorder = Recorder::default();
    let dispatch = recorder.dispatch();
    let (started, ready) = oneshot::channel();
    let task = tokio::spawn(
        async move {
            let _tx = first_polled(begin_immediate_tx(&pool), started)
                .await
                .unwrap();
        }
        .with_subscriber(dispatch),
    );
    ready.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    drop(held);
    let record = recorder.only();
    assert_eq!(record["outcome"], "acquire_interrupted");
    assert_eq!(record["attempts"], 1);
    assert_eq!(record["begin_attempts"], 0);
    begin_immediate_tx(fx.repo.pool())
        .await
        .unwrap()
        .commit()
        .await
        .unwrap();
}

#[tokio::test]
async fn observation_cancelling_begin_preserves_pool_recovery_and_unknown_outcome() {
    let fx = Fixture::new().await;
    let holder = begin_immediate_tx(fx.repo.pool()).await.unwrap();
    drop(fx.repo.pool().acquire().await.unwrap());
    let pool = fx.repo.pool().clone();
    let recorder = Recorder::default();
    let dispatch = recorder.dispatch();
    let (started, ready) = oneshot::channel();
    let task = tokio::spawn(
        async move {
            let _tx = first_polled(begin_immediate_tx(&pool), started)
                .await
                .unwrap();
        }
        .with_subscriber(dispatch),
    );
    ready.await.unwrap();
    sleep(PARK).await;
    assert!(!task.is_finished());
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    holder.commit().await.unwrap();
    timeout(LIMIT, async {
        begin_immediate_tx(fx.repo.pool())
            .await
            .unwrap()
            .commit()
            .await
            .unwrap();
    })
    .await
    .unwrap();
    let record = recorder.only();
    assert_eq!(record["outcome"], "begin_interrupted");
    assert_eq!(record["begin_attempts"], 1);
}

#[tokio::test]
async fn observation_failed_commit_is_not_recorded_as_a_completed_rollback() {
    let fx = Fixture::new().await;
    sqlx::query("CREATE TABLE observation_parent(id INTEGER PRIMARY KEY)")
        .execute(fx.repo.pool())
        .await
        .unwrap();
    sqlx::query("CREATE TABLE observation_child(parent INTEGER REFERENCES observation_parent(id) DEFERRABLE INITIALLY DEFERRED)")
        .execute(fx.repo.pool()).await.unwrap();
    let recorder = Recorder::default();
    let dispatch = recorder.dispatch();
    async {
        let mut tx = begin_immediate_tx(fx.repo.pool()).await.unwrap();
        sqlx::query("INSERT INTO observation_child VALUES(42)")
            .execute(&mut **tx)
            .await
            .unwrap();
        assert!(tx.commit().await.is_err());
    }
    .with_subscriber(dispatch)
    .await;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM observation_child")
        .fetch_one(fx.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(recorder.only()["outcome"], "commit_error");
    begin_immediate_tx(fx.repo.pool())
        .await
        .unwrap()
        .commit()
        .await
        .unwrap();
}

#[tokio::test]
async fn observation_interrupted_commit_can_persist_and_must_report_unknown() {
    let fx = Fixture::new().await;
    let recorder = Recorder::default();
    let dispatch = recorder.dispatch();
    let mut tx = async { begin_immediate_tx(fx.repo.pool()).await.unwrap() }
        .with_subscriber(dispatch.clone())
        .await;
    sqlx::query("INSERT INTO observation_probe VALUES('committed after cancellation')")
        .execute(&mut **tx)
        .await
        .unwrap();
    let (entered, waiting) = std::sync::mpsc::sync_channel(1);
    let (release, released) = std::sync::mpsc::channel();
    let mut first = true;
    {
        let mut handle = tx.lock_handle().await.unwrap();
        handle.set_commit_hook(move || {
            if first {
                first = false;
                let _ = entered.send(());
                let _ = released.recv();
            }
            true
        });
    }
    let task = tokio::spawn(async move { tx.commit().await }.with_subscriber(dispatch));
    // Wait on a blocking helper, not on the runtime or the SQLite worker being held.
    let entered = tokio::task::spawn_blocking(move || waiting.recv_timeout(LIMIT)).await;
    task.abort();
    let aborted = task.await;
    // Always release and drain BEFORE inspecting any result; an assertion must not strand SQLx.
    let _ = release.send(());
    let drained = timeout(LIMIT, async {
        begin_immediate_tx(fx.repo.pool())
            .await
            .unwrap()
            .commit()
            .await
            .unwrap();
    })
    .await;
    let values = timeout(LIMIT, fx.values()).await;
    assert!(
        drained.is_ok(),
        "subsequent writer confirms finalization drained"
    );
    assert!(
        entered.unwrap().is_ok(),
        "public SQLite commit hook was reached"
    );
    assert!(aborted.unwrap_err().is_cancelled());
    assert_eq!(values.unwrap(), vec!["committed after cancellation"]);
    assert_eq!(recorder.only()["outcome"], "commit_interrupted");
}

#[tokio::test]
async fn observation_acquire_busy_preserves_once_errors_and_bounded_retry_policy() {
    use sqlx::Connection;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
    let dir = tempfile::tempdir().unwrap();
    let options = SqliteConnectOptions::new()
        .filename(dir.path().join("acquire-busy.db"))
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Delete)
        .busy_timeout(Duration::ZERO);
    let mut connection = sqlx::SqliteConnection::connect_with(&options)
        .await
        .unwrap();
    sqlx::query("CREATE TABLE observation_probe(value TEXT)")
        .execute(&mut connection)
        .await
        .unwrap();
    // A fixture-owned raw transaction blocks connection establishment's WAL-mode transition.
    let holder = connection.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_lazy_with(options.journal_mode(SqliteJournalMode::Wal));
    let recorder = Recorder::default();
    let dispatch = recorder.dispatch();
    let error = async {
        match begin_immediate_tx_once(&pool).await {
            Err(error) => error,
            Ok(_) => panic!("WAL-mode connection establishment must be blocked by fixture"),
        }
    }
    .with_subscriber(dispatch.clone())
    .await;
    assert!(crate::db::sqlite::is_sqlite_busy(&error));
    let record = recorder.only();
    assert_eq!(record["outcome"], "acquire_error");
    assert_eq!(record["attempts"], 1);
    assert_eq!(record["begin_attempts"], 0);
    let retry = recorder.retried.clone();
    let next_pool = pool.clone();
    let task = tokio::spawn(
        async move {
            begin_immediate_tx(&next_pool)
                .await
                .unwrap()
                .commit()
                .await
                .unwrap();
        }
        .with_subscriber(dispatch),
    );
    timeout(LIMIT, retry.notified())
        .await
        .expect("bounded policy must retry actual acquire BUSY");
    holder.commit().await.unwrap();
    timeout(LIMIT, task).await.unwrap().unwrap();
    let records = recorder.records();
    assert_eq!(records.len(), 2);
    let record = &records[1];
    assert_eq!(record["outcome"], "committed");
    assert!(record["attempts"].as_u64().unwrap() >= 2);
    assert_eq!(record["begin_attempts"], 1);
    assert!(elapsed(record, "backoff_ms") > 0.0);
    pool.close().await;
}

#[tokio::test]
async fn observation_cancelling_active_body_keeps_rollback_owned_by_sqlx() {
    let fx = Fixture::new().await;
    let pool = fx.repo.pool().clone();
    let recorder = Recorder::default();
    let dispatch = recorder.dispatch();
    let (started, ready) = oneshot::channel();
    let task = tokio::spawn(
        async move {
            let mut tx = begin_immediate_tx(&pool).await.unwrap();
            sqlx::query("INSERT INTO observation_probe VALUES('cancelled body')")
                .execute(&mut **tx)
                .await
                .unwrap();
            started.send(()).unwrap();
            std::future::pending::<()>().await;
            drop(tx);
        }
        .with_subscriber(dispatch),
    );
    ready.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    timeout(LIMIT, async {
        begin_immediate_tx(fx.repo.pool())
            .await
            .unwrap()
            .commit()
            .await
            .unwrap();
    })
    .await
    .unwrap();
    assert!(fx.values().await.is_empty());
    assert_eq!(recorder.only()["outcome"], "active_dropped");
}

#[tokio::test]
async fn observation_keeps_first_poll_context_through_finalization_elsewhere() {
    let fx = Fixture::new().await;
    let recorder = Recorder::default();
    let dispatch = recorder.dispatch();
    // Construction is outside the subscriber/span; execution context belongs to first poll.
    let future = begin_immediate_tx(fx.repo.pool());
    let span =
        tracing::dispatcher::with_default(&dispatch, || tracing::debug_span!("observed_operation"));
    let tx = future
        .instrument(span)
        .with_subscriber(dispatch)
        .await
        .unwrap();
    // Explicit finalization outside the polling context still reaches its original subscriber.
    tx.commit().await.unwrap();
    let record = recorder.only();
    assert_eq!(record["outcome"], "committed");
    assert_eq!(
        record["span_path"],
        serde_json::json!(["observed_operation"])
    );
}
