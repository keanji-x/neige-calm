//! The SDK's HTTP client logs full bodies at `info`; our subscriber emits
//! only this crate's target.

use std::{
    io::Write,
    sync::{Arc, Mutex},
};

use longbridge::oauth::OAuth;
use research_archive::{catalog::Catalog, logging, types::Response};
use tracing::Level;

use crate::fake::{FakeServer, Reply};
use crate::hermetic;
use crate::support::key;

const TOKEN: &str = "ap_CANARY-LOGTOKEN-91c2";
const BODY: &str = r#"{"canary":"CANARY-BODY-3f1d"}"#;

/// Log lines written to memory.
#[derive(Clone, Default)]
pub struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Capture {
    pub fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

/// Run one request and one line of our own under `subscriber`.
async fn logged(subscriber: impl tracing::Subscriber + Send + Sync + 'static) {
    let server = FakeServer::start(|_| Reply::data(BODY)).await;
    let source = crate::source::source(&server.url, OAuth::from_token(TOKEN));
    let _guard = tracing::subscriber::set_default(subscriber);
    tracing::info!(target: "research_archive", "own line visible");
    let fetched = source
        .fetch(
            Catalog::compiled().get("consensus").unwrap(),
            &key("AAPL.US"),
        )
        .await;
    assert_eq!(
        fetched.response(),
        &Response::Body(BODY.as_bytes().to_vec())
    );
}

/// Only our own target is emitted, even at `trace`: no SDK, HTTP or
/// server line.
#[test]
fn only_our_own_target_is_logged() {
    hermetic::run(async {
        let capture = Capture::default();
        let writer = capture.clone();
        logged(logging::subscriber(Level::TRACE, move || writer.clone())).await;
        let text = capture.text();
        assert!(text.contains("own line visible"), "{text}");
        for line in text.lines() {
            assert!(line.contains(logging::OWN_TARGET), "{line}");
        }
        assert!(!text.contains("longbridge"), "{text}");
        assert!(!text.contains("CANARY"), "{text}");
    });
}

/// Control: without the filter the same request logs its body at `info`,
/// so the test above is not vacuous.
#[test]
fn without_the_cap_the_sdk_logs_the_body() {
    hermetic::run(async {
        use tracing_subscriber::{filter::Targets, layer::SubscriberExt};

        let capture = Capture::default();
        let writer = capture.clone();
        let uncapped = tracing_subscriber::registry()
            .with(Targets::new().with_default(Level::INFO))
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(move || writer.clone())
                    .with_ansi(false),
            );
        logged(uncapped).await;
        let text = capture.text();
        assert!(text.contains("longbridge_httpcli::request"), "{text}");
        assert!(text.contains("CANARY-BODY-3f1d"), "{text}");
    });
}
