//! Real provider/stdio barriers: writes stay responsive while pricing, and
//! commits (including errors) serialize with acknowledged balance changes.
use super::*;

fn blocked_prices() -> (String, Receiver<()>, std::sync::mpsc::Sender<()>) {
    blocked_prices_followup("2.5")
}

fn blocked_prices_followup(
    following: &'static str,
) -> (String, Receiver<()>, std::sync::mpsc::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (entered_tx, entered) = channel();
    let (release, release_rx) = channel();
    std::thread::spawn(move || {
        for (index, stream) in listener.incoming().enumerate() {
            let Ok(mut stream) = stream else { return };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut header = Vec::new();
            let mut byte = [0_u8; 1];
            while stream.read_exact(&mut byte).is_ok() {
                header.push(byte[0]);
                if header.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            if index == 0 {
                let _ = entered_tx.send(());
                let _ = release_rx.recv_timeout(Duration::from_secs(15));
            }
            let body = format!(
                r#"{{"price":"{}"}}"#,
                if index == 0 { "2.5" } else { following }
            );
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.flush();
        }
    });
    (format!("http://{address}"), entered, release)
}

fn wait_for_quote(kernel: &mut FakeKernel, entered: &Receiver<()>) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if entered.try_recv().is_ok() {
            return;
        }
        if let Ok(frame) = kernel.frames.recv_timeout(Duration::from_millis(10)) {
            kernel.service(&frame);
        }
    }
    panic!("provider was not reached");
}

#[test]
fn cash_set_during_provider_work_discards_the_old_snapshot() {
    let (endpoint, entered, release) = blocked_prices();
    let mut kernel = FakeKernel::boot_settling(&endpoint, DEAD_ENDPOINT, 3600, "USD");
    let holding = kernel.call_tool(
        2,
        "market.holdings.set",
        json!({"asset":"BTC","quantity":2}),
        Some(TRACK),
    );
    assert_ne!(holding["result"]["isError"], true);
    wait_for_quote(&mut kernel, &entered);
    // This returns before the quote is released. Locking the setter behind
    // REFRESH_LOCK would time out rather than pass this production call.
    let reply = kernel.call_tool(
        3,
        "market.cash.set",
        json!({"currency":"USD","amount":100}),
        Some(TRACK),
    );
    assert_ne!(reply["result"]["isError"], true, "{reply}");
    let after_ack = kernel.pushes.len();
    release.send(()).unwrap();
    kernel.drain();
    let totals = kernel.pushes[after_ack..]
        .iter()
        .filter(|(kind, _)| kind == &format!("portfolio.allocation@{TRACK}"))
        .map(|(_, value)| value["rows"].as_array().unwrap().last().unwrap()["value"].clone())
        .collect::<Vec<_>>();
    assert!(!totals.is_empty());
    assert!(totals.iter().all(|total| *total == 105.0), "{totals:?}");
    assert!(
        kernel.kv[&format!("total_history/{TRACK}")]
            .as_array()
            .unwrap()
            .iter()
            .all(|point| point["total"] == 105.0)
    );
}

#[test]
fn holdings_set_participates_in_the_same_snapshot_barrier() {
    let (endpoint, entered, release) = blocked_prices();
    let mut kernel = FakeKernel::boot_settling(&endpoint, DEAD_ENDPOINT, 3600, "USD");
    set_cash(&mut kernel, 2, "USD", 100.0);
    kernel.call_tool(
        3,
        "market.holdings.set",
        json!({"asset":"BTC","quantity":2}),
        Some(TRACK),
    );
    wait_for_quote(&mut kernel, &entered);
    let reply = kernel.call_tool(
        4,
        "market.holdings.set",
        json!({"asset":"BTC","quantity":4}),
        Some(TRACK),
    );
    assert_ne!(reply["result"]["isError"], true);
    let after_ack = kernel.pushes.len();
    release.send(()).unwrap();
    kernel.drain();
    let totals = kernel.pushes[after_ack..]
        .iter()
        .filter(|(kind, _)| kind == &format!("portfolio.allocation@{TRACK}"))
        .map(|(_, value)| value["rows"].as_array().unwrap().last().unwrap()["value"].clone())
        .collect::<Vec<_>>();
    assert!(!totals.is_empty());
    assert!(totals.iter().all(|total| *total == 110.0), "{totals:?}");
    assert!(
        !kernel.kv[&format!("total_history/{TRACK}")]
            .as_array()
            .unwrap()
            .iter()
            .any(|point| point["total"] == 105.0)
    );
}

fn first_projection(kernel: &mut FakeKernel) -> Value {
    for _ in 0..24 {
        let frame = kernel
            .next_frame()
            .expect("refresh must reach a projection");
        if frame["method"] == "neige.overlay.set" {
            return frame;
        }
        kernel.service(&frame);
    }
    panic!("refresh produced no projection");
}

#[test]
fn a_setter_cannot_acknowledge_through_an_inflight_publication() {
    let mut kernel = FakeKernel::boot_settling(DEAD_ENDPOINT, DEAD_ENDPOINT, 3600, "CNY");
    kernel.call_tool(
        2,
        "market.cash.set",
        json!({"currency":"CNY","amount":100}),
        Some(TRACK),
    );
    let pending = first_projection(&mut kernel);
    kernel.send(
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"market.cash.set",
        "arguments":{"currency":"CNY","amount":200},"_meta":{"dev.neige/track":{"id":TRACK}}}}),
    );
    assert!(
        matches!(
            kernel.frames.recv_timeout(Duration::from_millis(150)),
            Err(RecvTimeoutError::Timeout)
        ),
        "a setter crossed the publication barrier"
    );
    kernel.service(&pending);
    let reply = kernel.drain_until_reply();
    assert_eq!(reply["id"], 3);
    assert_ne!(reply["result"]["isError"], true);
    let after_ack = kernel.pushes.len();
    kernel.drain();
    let rows = kernel.pushes[after_ack..]
        .iter()
        .filter(|(kind, _)| kind == &format!("portfolio.cash@{TRACK}"));
    let values = rows
        .map(|(_, payload)| payload["rows"][0]["amount"].clone())
        .collect::<Vec<_>>();
    assert!(!values.is_empty());
    assert!(values.iter().all(|value| *value == 200.0), "{values:?}");
}

#[test]
fn unchanged_aba_inputs_can_commit_the_original_observation() {
    let (endpoint, entered, release) = blocked_prices_followup("3.5");
    let mut kernel = FakeKernel::boot_settling(&endpoint, DEAD_ENDPOINT, 3600, "USD");
    set_cash(&mut kernel, 2, "USD", 100.0);
    kernel.call_tool(
        3,
        "market.holdings.set",
        json!({"asset":"BTC","quantity":2}),
        Some(TRACK),
    );
    wait_for_quote(&mut kernel, &entered);
    for (id, amount) in [(4, 200), (5, 100)] {
        let reply = kernel.call_tool(
            id,
            "market.cash.set",
            json!({"currency":"USD","amount":amount}),
            Some(TRACK),
        );
        assert_ne!(reply["result"]["isError"], true);
    }
    let after_ack = kernel.pushes.len();
    release.send(()).unwrap();
    kernel.drain();
    let first = kernel.pushes[after_ack..]
        .iter()
        .find(|(kind, _)| kind == &format!("portfolio.allocation@{TRACK}"))
        .unwrap();
    assert_eq!(
        first.1["rows"][2]["value"], 105.0,
        "same canonical inputs may use the original 2.5 quote"
    );
    assert!(
        !kernel.kv[&format!("total_history/{TRACK}")]
            .as_array()
            .unwrap()
            .iter()
            .any(|point| point["total"] == 205.0)
    );
}

#[test]
fn an_error_projection_cannot_overwrite_a_newer_cash_ack() {
    let mut kernel = FakeKernel::boot_settling(DEAD_ENDPOINT, DEAD_ENDPOINT, 3600, "CNY");
    set_cash(&mut kernel, 2, "CNY", 100.0);
    kernel.refuse_kv_get = Some("holdings/".into());
    kernel.call_tool(
        3,
        "market.cash.set",
        json!({"currency":"CNY","amount":150}),
        Some(TRACK),
    );
    let pending = first_projection(&mut kernel);
    assert_eq!(pending["params"]["kind"], "portfolio.allocation");
    assert!(
        pending["params"]["payload"]["caption"]
            .as_str()
            .unwrap()
            .contains("unavailable")
    );
    kernel.refuse_kv_get = None;
    kernel.send(
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"market.cash.set",
        "arguments":{"currency":"CNY","amount":200},"_meta":{"dev.neige/track":{"id":TRACK}}}}),
    );
    assert!(
        matches!(
            kernel.frames.recv_timeout(Duration::from_millis(150)),
            Err(RecvTimeoutError::Timeout)
        ),
        "error publication released the state lock early"
    );
    kernel.service(&pending);
    let reply = kernel.drain_until_reply();
    assert_eq!(reply["id"], 4);
    assert_ne!(reply["result"]["isError"], true);
    let after_ack = kernel.pushes.len();
    kernel.drain();
    let rows = kernel.pushes[after_ack..]
        .iter()
        .filter(|(kind, _)| kind == &format!("portfolio.cash@{TRACK}"))
        .collect::<Vec<_>>();
    assert!(!rows.is_empty());
    for (_, payload) in rows {
        assert_eq!(
            payload["rows"][0]["amount"], 200.0,
            "old error after ACK: {payload}"
        );
    }
}
