//! Regression: actual WS dispatch must follow pacing and cancellation, not SDK enqueue time.
use crate::{
    archive::quote,
    fake::{FakeServer, Reply},
    hermetic,
};
use longbridge::oauth::OAuth;
use research_archive::{
    Catalog,
    source::{LongbridgeSource, Rps, Timeouts},
    types::AsOf,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

async fn wait_for(server: &quote::Server, command: u8) {
    let start = Instant::now();
    while !server
        .arrivals
        .lock()
        .unwrap()
        .iter()
        .any(|a| a.command == command)
    {
        assert!(start.elapsed() < Duration::from_secs(5));
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
fn request(symbol: &str) -> Vec<(String, String)> {
    Catalog::compiled()
        .get("daily_candlesticks")
        .unwrap()
        .resolve(symbol, AsOf::parse("2026-10-20").unwrap())
}

#[test]
fn cancellation_drops_owned_transport_without_later_queued_sends() {
    hermetic::run(async {
        for command in [2, 27] {
            let gate = Arc::new(tokio::sync::Semaphore::new(0));
            let socket = quote::start_with_gate(Some((command, gate.clone())));
            let http = FakeServer::start_capture(|_| Reply::data("{}")).await;
            let source = Arc::new(
                LongbridgeSource::new(
                    OAuth::from_token("ap_m_FAKE"),
                    &http.url,
                    Rps::new(20).unwrap(),
                    Timeouts::PRODUCTION,
                )
                .with_quote_ws_url(&socket.url),
            );
            let may_send = Arc::new(AtomicBool::new(true));
            let mut tasks = Vec::new();
            for symbol in ["AAPL.US", "QQQ.US", "SOXX.US", "KBE.US"] {
                let source = source.clone();
                let may_send = may_send.clone();
                tasks.push(tokio::spawn(async move {
                    let endpoint = *Catalog::compiled().get("daily_candlesticks").unwrap();
                    source
                        .fetch_request_if(&endpoint, &request(symbol), || {
                            may_send.load(Ordering::SeqCst)
                        })
                        .await
                }));
            }
            wait_for(&socket, command).await;
            tokio::time::sleep(Duration::from_millis(250)).await;
            may_send.store(false, Ordering::SeqCst);
            for task in &tasks {
                task.abort();
            }
            for task in tasks {
                let _ = task.await;
            }
            let before = socket
                .arrivals
                .lock()
                .unwrap()
                .iter()
                .filter(|a| a.command == 27)
                .count();
            gate.add_permits(1);
            tokio::time::sleep(Duration::from_millis(300)).await;
            let after = socket
                .arrivals
                .lock()
                .unwrap()
                .iter()
                .filter(|a| a.command == 27)
                .count();
            assert_eq!(
                after, before,
                "cancelled business commands reached the wire"
            );
        }
    })
}

#[test]
fn actual_wire_starts_remain_paced_after_a_slow_response() {
    hermetic::run(async {
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let socket = quote::start_with_gate(Some((27, gate.clone())));
        let http = FakeServer::start_capture(|_| Reply::data("{}")).await;
        let source = Arc::new(
            LongbridgeSource::new(
                OAuth::from_token("ap_m_FAKE"),
                &http.url,
                Rps::new(2).unwrap(),
                Timeouts::PRODUCTION,
            )
            .with_quote_ws_url(&socket.url),
        );
        let mut tasks = Vec::new();
        for symbol in ["AAPL.US", "QQQ.US", "SOXX.US", "KBE.US"] {
            let source = source.clone();
            tasks.push(tokio::spawn(async move {
                let endpoint = *Catalog::compiled().get("daily_candlesticks").unwrap();
                let result = source
                    .fetch_request_if(&endpoint, &request(symbol), || true)
                    .await
                    .unwrap();
                assert!(matches!(
                    result.response(),
                    research_archive::types::Response::Body(_)
                ));
            }));
        }
        wait_for(&socket, 27).await;
        tokio::time::sleep(Duration::from_millis(1600)).await;
        gate.add_permits(1);
        for task in tasks {
            task.await.unwrap();
        }
        let starts: Vec<_> = socket
            .arrivals
            .lock()
            .unwrap()
            .iter()
            .filter(|a| a.command == 27)
            .map(|a| a.at)
            .collect();
        assert_eq!(starts.len(), 4);
        for pair in starts.windows(2) {
            assert!(
                pair[1].duration_since(pair[0]) >= Duration::from_millis(450),
                "wire starts burst after queueing"
            );
        }
    })
}
