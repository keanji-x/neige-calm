//! Payload-free lifecycle diagnostics for the real native test peer.
use std::time::Instant;

pub(super) struct Trace {
    test: String,
    run: uuid::Uuid,
    started: Instant,
}

impl Trace {
    pub(super) fn new() -> Self {
        Self {
            test: std::thread::current()
                .name()
                .unwrap_or("unnamed-test")
                .into(),
            run: uuid::Uuid::new_v4(),
            started: Instant::now(),
        }
    }

    fn emit(&self, stage: &str, outcome: &str) {
        eprintln!(
            "native-phase test={} run={} elapsed_us={} stage={} outcome={}",
            self.test,
            self.run,
            self.started.elapsed().as_micros(),
            stage,
            outcome
        );
    }

    pub(super) fn phase(&self, stage: &'static str) -> Phase<'_> {
        self.emit(stage, "enter");
        Phase {
            trace: self,
            stage,
            finished: false,
        }
    }
}

pub(super) struct Phase<'a> {
    trace: &'a Trace,
    stage: &'static str,
    finished: bool,
}

impl Phase<'_> {
    pub(super) fn complete(self) {
        self.finish("complete");
    }

    pub(super) fn finish(mut self, outcome: &str) {
        self.trace.emit(self.stage, outcome);
        self.finished = true;
    }
}

impl Drop for Phase<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.trace.emit(
                self.stage,
                if std::thread::panicking() {
                    "panic"
                } else {
                    "cancelled"
                },
            );
        }
    }
}
