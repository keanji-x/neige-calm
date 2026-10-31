//! Explicit, isolated before/after measurement of the real canonical idle loop.
//! Run only this ignored test in its own nextest process; it installs a global recorder.
use super::*;
use std::sync::Mutex as StdMutex;
use tracing::field::{Field, Visit};
use tracing_subscriber::{Layer, layer::Context, prelude::*};

#[derive(Default, Clone, Debug, serde::Serialize)]
struct WriterSample {
    transactions: u64,
    attempts: u64,
    begin_attempts: u64,
    acquire_errors: u64,
    other_outcomes: u64,
    acquire_ms: f64,
    begin_ms: f64,
    active_ms: f64,
    commit_ms: f64,
    rollback_ms: f64,
    total_ms: f64,
}
impl Visit for WriterSample {
    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "attempts" => self.attempts = value,
            "begin_attempts" => self.begin_attempts = value,
            _ => {}
        }
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        match field.name() {
            "acquire_ms" => self.acquire_ms = value,
            "begin_ms" => self.begin_ms = value,
            "active_ms" => self.active_ms = value,
            "commit_ms" => self.commit_ms = value,
            "rollback_ms" => self.rollback_ms = value,
            "total_ms" => self.total_ms = value,
            _ => {}
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "outcome" {
            match value {
                "committed" | "rolled_back" => {}
                "acquire_error" => self.acquire_errors += 1,
                _ => self.other_outcomes += 1,
            }
        }
    }
    fn record_debug(&mut self, _: &Field, _: &dyn std::fmt::Debug) {}
}
#[derive(Clone, Default)]
struct WriterRecorder(Arc<StdMutex<WriterSample>>);
impl<S: tracing::Subscriber> Layer<S> for WriterRecorder {
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        if event.metadata().target() != "calm_truth::db::transaction" {
            return;
        }
        let mut sample = WriterSample::default();
        event.record(&mut sample);
        let mut total = self.0.lock().unwrap();
        total.transactions += 1;
        total.attempts += sample.attempts;
        total.begin_attempts += sample.begin_attempts;
        total.acquire_errors += sample.acquire_errors;
        total.other_outcomes += sample.other_outcomes;
        total.acquire_ms += sample.acquire_ms;
        total.begin_ms += sample.begin_ms;
        total.active_ms += sample.active_ms;
        total.commit_ms += sample.commit_ms;
        total.rollback_ms += sample.rollback_ms;
        total.total_ms += sample.total_ms;
    }
}

#[tokio::test]
#[ignore = "isolated manual before/after measurement; installs a process-global subscriber"]
async fn canonical_idle_writer_pressure_measurement() {
    let recorder = WriterRecorder::default();
    tracing::subscriber::set_global_default(tracing_subscriber::registry().with(recorder.clone()))
        .expect("run this measurement alone in a nextest test process");
    let fx = Fixture::new().await;
    let (_client, mut peer) = connect(&fx.daemon).await;
    super::control_tests::running(&fx, &mut peer).await;
    let original = fx.frame().await;
    notify_thread(
        &fx.daemon,
        &mut peer,
        original.header.last_thread_id.as_deref().unwrap(),
        "turn/completed",
        json!({"id":original.header.last_turn_id,"status":"completed"}),
    )
    .await;
    fx.wait_phase(HarnessPhaseTag::TurnCompleted).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let snapshot_bytes = serde_json::to_vec(&fx.frame().await).unwrap().len();
    assert!(
        recorder.0.lock().unwrap().attempts > 0,
        "the recorder must capture real setup writer admissions"
    );
    *recorder.0.lock().unwrap() = WriterSample::default();
    let started = Instant::now();
    // Re-observe the exact committed identity through the real public ingress.
    // There is no pending body to execute; this also exercises a reconciliation
    // pass on the new scheduler rather than measuring only a sleeping timer.
    fx.harness
        .observe_canonical_source(fx.message.clone(), fx.source.clone())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let elapsed = started.elapsed();
    let sample = recorder.0.lock().unwrap().clone();
    eprintln!(
        "canonical_idle_writer_pressure={}",
        json!({
            "installed_canonical_loops":1,
            "messages_during_sample":0,
            "snapshot_bytes":snapshot_bytes,
            "counting":"completed caller observations; in-flight work at the boundary is not counted",
            "database":"isolated sqlite::memory: fixture (no physical disk/WAL measurement)",
            "elapsed_ms":elapsed.as_secs_f64()*1000.0,
            "writer":sample,
        })
    );
    peer.close(None).await.unwrap();
    fx.harness.shutdown().await.unwrap();
}
