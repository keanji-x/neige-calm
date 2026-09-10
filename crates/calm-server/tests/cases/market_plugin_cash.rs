use super::*;

#[test]
fn cash_only_process_records_balance_and_publishes_combined_value() {
    let mut kernel = FakeKernel::boot_settling(DEAD_ENDPOINT, DEAD_ENDPOINT, 3600, "CNY");
    let reply = kernel.call_tool(
        2,
        "market.cash.set",
        json!({"currency":"CNY","amount":100000}),
        Some(TRACK),
    );
    assert_ne!(reply["result"]["isError"], true, "{reply}");
    kernel.drain();
    assert_eq!(
        kernel.kv.get(&format!("cash/{TRACK}")),
        Some(&json!({"version":1,"balances":[{"currency":"CNY","amount":100000.0}]}))
    );
    let allocation = kernel
        .pushes
        .iter()
        .rev()
        .find(|(kind, _)| kind == &format!("portfolio.allocation@{TRACK}"))
        .expect("cash-only allocation");
    assert_eq!(allocation.1["rows"][0]["value"], 100000.0);
    assert_eq!(allocation.1["rows"][1]["value"], 100000.0);
    assert_eq!(
        kernel.kv[&format!("total_history/{TRACK}")][0]["total"],
        100000.0
    );
    assert!(!kernel.kv.contains_key(&format!("holdings/{TRACK}")));
    assert!(!kernel.kv.contains_key(&format!("history/{TRACK}")));
}

fn overlay<'a>(kernel: &'a FakeKernel, kind: &str, track: &str) -> &'a Value {
    &kernel
        .pushes
        .iter()
        .rev()
        .find(|(key, _)| key == &format!("{kind}@{track}"))
        .unwrap_or_else(|| panic!("missing {kind}"))
        .1
}
fn set_cash(kernel: &mut FakeKernel, id: u64, currency: &str, amount: f64) -> Value {
    let reply = kernel.call_tool(
        id,
        "market.cash.set",
        json!({"currency":currency,"amount":amount}),
        Some(TRACK),
    );
    kernel.drain();
    reply
}

#[test]
fn cash_replaces_balance_preserves_other_currency_and_keeps_zero() {
    let mut kernel = FakeKernel::boot_settling(DEAD_ENDPOINT, DEAD_ENDPOINT, 3600, "CNY");
    set_cash(&mut kernel, 2, "CNY", 0.29);
    let reply = set_cash(&mut kernel, 3, "CNY", 0.01);
    assert_ne!(reply["result"]["isError"], true);
    set_cash(&mut kernel, 4, "USD", 0.0);
    set_cash(&mut kernel, 5, "CNY", 0.0);
    let reply = kernel.call_tool(6, "market.cash.list", json!({}), Some(TRACK));
    assert_eq!(
        reply["result"]["structuredContent"]["balances"],
        json!([{"currency":"CNY","amount":0.0},{"currency":"USD","amount":0.0}])
    );
    assert_eq!(
        overlay(&kernel, "portfolio.allocation", TRACK)["rows"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["value"],
        0.0
    );
    assert!(
        overlay(&kernel, "portfolio.cash", TRACK)["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["weight"].is_null())
    );
    assert_eq!(
        kernel.kv[&format!("total_history/{TRACK}")]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["total"],
        0.0
    );
    assert!(!kernel.kv.contains_key(&format!("history/{TRACK}")));
}

#[test]
fn invalid_cash_requests_and_corrupt_documents_do_not_write() {
    let mut kernel = FakeKernel::boot(DEAD_ENDPOINT);
    set_cash(&mut kernel, 2, "USD", 1.2);
    let before = kernel.kv.clone();
    let before_pushes = kernel.pushes.len();
    for (index, args) in [
        json!({"currency":"USD","amount":0.001}),
        json!({"currency":"USD","amount":1.005}),
        json!({"currency":"USD","amount":-1}),
        json!({"currency":"EUR","amount":1}),
        // A decimal STRING is the exact amount channel (the starter tells
        // the assistant to use it), so what fails here is a string that is
        // not a whole-cent decimal — not the fact that it is a string.
        json!({"currency":"USD","amount":"1.005"}),
        json!({"currency":"USD","amount":"1.2 USD"}),
        json!({"currency":"USD","amount":""}),
        json!({"currency":"USD","amount":1e100}),
        json!({"currency":"USD","amount":2,"track_id":OTHER_TRACK}),
        json!({"amount":1}),
    ]
    .into_iter()
    .enumerate()
    {
        let reply = kernel.call_tool(10 + index as u64, "market.cash.set", args, Some(TRACK));
        assert_eq!(reply["result"]["isError"], true, "{reply}");
    }
    assert_eq!(kernel.kv, before);
    assert_eq!(kernel.pushes.len(), before_pushes);
    let reply = kernel.call_tool(
        25,
        "market.cash.set",
        json!({"currency":"USD","amount":2}),
        None,
    );
    assert_eq!(reply["result"]["isError"], true);
    let corrupt = json!({"version":9,"balances":[{"currency":"USD","amount":1}]});
    kernel.kv.insert(format!("cash/{TRACK}"), corrupt.clone());
    let reply = kernel.call_tool(
        26,
        "market.cash.set",
        json!({"currency":"USD","amount":2}),
        Some(TRACK),
    );
    assert_eq!(reply["result"]["isError"], true);
    assert_eq!(kernel.kv[&format!("cash/{TRACK}")], corrupt);
}

#[test]
fn cash_and_securities_have_one_new_total_without_changing_old_security_sources() {
    let (endpoint, _) = price_server("4.637");
    let mut kernel = FakeKernel::boot_settling(&endpoint, DEAD_ENDPOINT, 3600, "USD");
    set_cash(&mut kernel, 2, "USD", 100000.0);
    kernel.set_holding(3, "BTC", 7.0, TRACK);
    assert_eq!(kernel.last_total_for(TRACK), Some(32.46));
    assert_eq!(
        overlay(&kernel, "portfolio.positions", TRACK)["rows"][0]["price"],
        4.637
    );
    let allocated = overlay(&kernel, "portfolio.allocation", TRACK);
    assert_eq!(allocated["rows"][2]["value"], 100032.46);
    let positions = overlay(&kernel, "portfolio.positions", TRACK);
    let cash = overlay(&kernel, "portfolio.cash", TRACK);
    assert!(
        (positions["rows"][0]["weight"].as_f64().unwrap() - 100.0 * 3246.0 / 10003246.0).abs()
            < 1e-12
    );
    assert!(
        (positions["rows"][0]["weight"].as_f64().unwrap()
            + cash["rows"][0]["weight"].as_f64().unwrap()
            - 100.0)
            .abs()
            < 1e-12
    );
    let holdings = kernel.kv[&format!("holdings/{TRACK}")].clone();
    set_cash(&mut kernel, 4, "USD", 80000.0);
    assert_eq!(
        overlay(&kernel, "portfolio.allocation", TRACK)["rows"][2]["value"],
        80032.46
    );
    assert_eq!(kernel.kv[&format!("holdings/{TRACK}")], holdings);
    assert!(
        kernel.kv[&format!("history/{TRACK}")]
            .as_array()
            .unwrap()
            .iter()
            .all(|point| point["total"] == 32.46)
    );
}

#[test]
fn cash_fx_public_cents_define_allocation_total_and_zero_weights() {
    let endpoint = sina_server_with_rows(vec![
        ("fx_susdcny", "0,0,0,0,0,0,0,0,0.6"),
        ("fx_shkdcny", "0,0,0,0,0,0,0,0,0.6"),
    ]);
    let mut kernel = FakeKernel::boot_settling(DEAD_ENDPOINT, &endpoint, 3600, "CNY");
    set_cash(&mut kernel, 2, "USD", 0.01);
    set_cash(&mut kernel, 3, "HKD", 0.01);
    let rows = overlay(&kernel, "portfolio.cash", TRACK)["rows"]
        .as_array()
        .unwrap();
    assert!(
        rows.iter()
            .all(|row| row["value"] == 0.01 && row["weight"] == 50.0),
        "{rows:?}"
    );
    assert_eq!(
        overlay(&kernel, "portfolio.allocation", TRACK)["rows"][2]["value"],
        0.02
    );
    let endpoint = sina_server_with_rows(vec![("fx_susdcny", "0,0,0,0,0,0,0,0,0.4")]);
    let mut tiny = FakeKernel::boot_settling(DEAD_ENDPOINT, &endpoint, 3600, "CNY");
    set_cash(&mut tiny, 2, "USD", 0.01);
    let row = &overlay(&tiny, "portfolio.cash", TRACK)["rows"][0];
    assert_eq!(row["amount"], 0.01);
    assert_eq!(row["value"], 0.0);
    assert!(row["weight"].is_null());
    assert_eq!(
        overlay(&tiny, "portfolio.allocation", TRACK)["rows"][1]["value"],
        0.0
    );
}

#[test]
fn missing_cash_fx_keeps_native_balance_but_never_a_subset_total_or_history() {
    let mut kernel = FakeKernel::boot_settling(DEAD_ENDPOINT, DEAD_ENDPOINT, 3600, "USD");
    kernel.set_holding(2, "USDT", 10.0, TRACK);
    let prior = kernel.kv[&format!("total_history/{TRACK}")].clone();
    set_cash(&mut kernel, 3, "HKD", 100.0);
    assert_eq!(kernel.last_total_for(TRACK), Some(10.0));
    let row = &overlay(&kernel, "portfolio.cash", TRACK)["rows"][0];
    assert_eq!(row["amount"], 100.0);
    assert_eq!(row["currency"], "HKD");
    assert!(row["value"].is_null());
    assert!(row["rate"].is_null());
    let allocation = overlay(&kernel, "portfolio.allocation", TRACK);
    assert!(allocation["rows"][2]["value"].is_null());
    assert!(allocation["rows"][2]["currency"].is_null());
    assert!(overlay(&kernel, "portfolio.positions", TRACK)["rows"][0]["weight"].is_null());
    assert_eq!(kernel.kv[&format!("total_history/{TRACK}")], prior);
}

#[test]
fn each_history_read_failure_preserves_its_series_without_blocking_the_other() {
    for failed in ["history/", "total_history/"] {
        let mut kernel = FakeKernel::boot(DEAD_ENDPOINT);
        kernel.set_holding(2, "USDT", 10.0, TRACK);
        set_cash(&mut kernel, 3, "USD", 5.0);
        let failing_key = format!("{failed}{TRACK}");
        let preserved = kernel.kv[&failing_key].clone();
        let other = format!(
            "{}{TRACK}",
            if failed == "history/" {
                "total_history/"
            } else {
                "history/"
            }
        );
        let other_count = kernel.kv[&other].as_array().unwrap().len();
        kernel.refuse_kv_read = Some(failed.into());
        set_cash(&mut kernel, 4, "USD", 6.0);
        assert_eq!(kernel.kv[&failing_key], preserved);
        assert_eq!(kernel.kv[&other].as_array().unwrap().len(), other_count + 1);
        assert_eq!(
            overlay(&kernel, "portfolio.cash", TRACK)["rows"][0]["amount"],
            6.0
        );
    }
}

#[test]
fn cash_only_key_is_rediscovered_by_polling_and_quota_failure_preserves_it() {
    let mut kernel = FakeKernel::boot_settling(DEAD_ENDPOINT, DEAD_ENDPOINT, 5, "CNY");
    let saved = json!({"version":1,"balances":[{"currency":"CNY","amount":100.0}]});
    kernel.kv.insert(format!("cash/{TRACK}"), saved.clone());
    let until = std::time::Instant::now() + Duration::from_secs(8);
    while std::time::Instant::now() < until
        && !kernel.kv.contains_key(&format!("total_history/{TRACK}"))
    {
        if let Ok(frame) = kernel.frames.recv_timeout(Duration::from_millis(50)) {
            kernel.service(&frame);
        }
    }
    assert_eq!(
        overlay(&kernel, "portfolio.allocation", TRACK)["rows"][1]["value"],
        100.0
    );
    kernel.drain();
    kernel.refuse_kv_set = Some("cash/".into());
    let reply = kernel.call_tool(
        2,
        "market.cash.set",
        json!({"currency":"CNY","amount":200}),
        Some(TRACK),
    );
    assert_eq!(reply["result"]["isError"], true);
    assert_eq!(kernel.kv[&format!("cash/{TRACK}")], saved);
}

#[path = "market_plugin_cash_races.rs"]
mod races;

#[test]
fn a_restarted_process_discovers_persisted_cash_without_any_security() {
    let persisted = {
        let mut first = FakeKernel::boot_settling(DEAD_ENDPOINT, DEAD_ENDPOINT, 3600, "CNY");
        set_cash(&mut first, 2, "CNY", 100.0);
        first.kv.clone()
    };
    let mut restarted = FakeKernel::boot_settling(DEAD_ENDPOINT, DEAD_ENDPOINT, 5, "CNY");
    restarted.kv = persisted.clone();
    let prior = restarted.kv[&format!("total_history/{TRACK}")]
        .as_array()
        .unwrap()
        .len();
    let until = std::time::Instant::now() + Duration::from_secs(8);
    while std::time::Instant::now() < until
        && restarted.kv[&format!("total_history/{TRACK}")]
            .as_array()
            .unwrap()
            .len()
            == prior
    {
        if let Ok(frame) = restarted.frames.recv_timeout(Duration::from_millis(50)) {
            restarted.service(&frame);
        }
    }
    assert_eq!(
        restarted.kv[&format!("cash/{TRACK}")],
        persisted[&format!("cash/{TRACK}")]
    );
    assert!(
        restarted.kv[&format!("total_history/{TRACK}")]
            .as_array()
            .unwrap()
            .len()
            > prior
    );
    assert_eq!(
        overlay(&restarted, "portfolio.cash", TRACK)["rows"][0]["amount"],
        100.0
    );
    assert!(!restarted.kv.contains_key(&format!("holdings/{TRACK}")));
}

#[test]
fn unsupported_settlement_keeps_native_cash_and_refuses_mixed_totals() {
    let mut kernel = FakeKernel::boot_settling(DEAD_ENDPOINT, DEAD_ENDPOINT, 3600, "HKD");
    set_cash(&mut kernel, 2, "USD", 10.0);
    assert_eq!(
        overlay(&kernel, "portfolio.allocation", TRACK)["rows"][1]["currency"],
        "USD"
    );
    let before = kernel.kv[&format!("total_history/{TRACK}")].clone();
    set_cash(&mut kernel, 3, "CNY", 20.0);
    let allocation = overlay(&kernel, "portfolio.allocation", TRACK);
    assert!(allocation["rows"][2]["value"].is_null());
    assert!(allocation["rows"][2]["currency"].is_null());
    assert_eq!(kernel.kv[&format!("total_history/{TRACK}")], before);
    assert!(
        overlay(&kernel, "portfolio.cash", TRACK)["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["weight"].is_null())
    );
}

#[test]
fn malformed_history_never_becomes_an_empty_replacement() {
    let mut kernel = FakeKernel::boot(DEAD_ENDPOINT);
    kernel.set_holding(2, "USDT", 10.0, TRACK);
    for prefix in ["history/", "total_history/"] {
        let bad = json!({"unexpected":"preserve this"});
        let key = format!("{prefix}{TRACK}");
        kernel.kv.insert(key.clone(), bad.clone());
        set_cash(&mut kernel, 3, "USD", 10.0);
        assert_eq!(kernel.kv[&key], bad);
    }
}

#[test]
fn stored_null_cash_is_an_error_not_missing_and_prefix_neighbors_are_not_adopted() {
    let mut kernel = FakeKernel::boot(DEAD_ENDPOINT);
    let key = format!("cash/{TRACK}");
    let neighbor = format!("{key}-neighbor");
    let other = json!({"version":1,"balances":[{"currency":"USD","amount":9.0}]});
    kernel.kv.insert(key.clone(), Value::Null);
    kernel.kv.insert(neighbor.clone(), other.clone());
    let list = kernel.call_tool(2, "market.cash.list", json!({}), Some(TRACK));
    assert_eq!(
        list["result"]["isError"], true,
        "stored null was reported as missing: {list}"
    );
    let set = kernel.call_tool(
        3,
        "market.cash.set",
        json!({"currency":"USD","amount":1}),
        Some(TRACK),
    );
    assert_eq!(
        set["result"]["isError"], true,
        "must not replace malformed null cash: {set}"
    );
    assert_eq!(kernel.kv[&key], Value::Null);
    kernel.kv.remove(&key);
    let missing = kernel.call_tool(4, "market.cash.list", json!({}), Some(TRACK));
    assert_eq!(
        missing["result"]["structuredContent"]["balances"],
        json!([])
    );
    set_cash(&mut kernel, 5, "USD", 0.29);
    assert_eq!(kernel.kv[&neighbor], other);
    assert_eq!(kernel.kv[&key]["balances"][0]["amount"], 0.29);
}

#[test]
fn stored_null_histories_are_preserved_and_never_replaced_by_new_points() {
    let mut kernel = FakeKernel::boot(DEAD_ENDPOINT);
    kernel.set_holding(2, "USDT", 1.0, TRACK);
    for prefix in ["history/", "total_history/"] {
        kernel.kv.insert(format!("{prefix}{TRACK}"), Value::Null);
    }
    let before = kernel.pushes.len();
    set_cash(&mut kernel, 3, "USD", 100.0);
    for prefix in ["history/", "total_history/"] {
        assert_eq!(
            kernel.kv[&format!("{prefix}{TRACK}")],
            Value::Null,
            "{prefix}"
        );
    }
    assert!(
        !kernel.pushes[before..]
            .iter()
            .any(|(kind, _)| kind == &format!("portfolio.history@{TRACK}")
                || kind == &format!("portfolio.total_history@{TRACK}"))
    );
}

#[test]
fn stored_null_securities_never_make_a_complete_cash_inclusive_total() {
    let mut kernel = FakeKernel::boot(DEAD_ENDPOINT);
    set_cash(&mut kernel, 2, "USD", 100.0);
    let history = kernel.kv[&format!("total_history/{TRACK}")].clone();
    kernel.kv.insert(format!("holdings/{TRACK}"), Value::Null);
    set_cash(&mut kernel, 3, "USD", 200.0);
    let allocation = overlay(&kernel, "portfolio.allocation", TRACK);
    assert!(
        allocation["rows"].as_array().unwrap().last().unwrap()["value"].is_null(),
        "{allocation}"
    );
    assert!(overlay(&kernel, "portfolio.cash", TRACK)["rows"][0]["weight"].is_null());
    assert_eq!(kernel.kv[&format!("total_history/{TRACK}")], history);
    assert_eq!(
        kernel.last_total_for(TRACK),
        Some(0.0),
        "legacy security-only tolerance is unchanged"
    );
}

fn raw_numeric_amount_must_be_refused(raw: &str) {
    let mut kernel = FakeKernel::boot(DEAD_ENDPOINT);
    let frame=json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"market.cash.set",
        "arguments":{"currency":"USD","amount":"RAW_DECIMAL_NUMBER"},"_meta":{"dev.neige/track":{"id":TRACK}}}})
        .to_string().replace("\"RAW_DECIMAL_NUMBER\"",raw);
    writeln!(kernel.stdin, "{frame}").unwrap();
    kernel.stdin.flush().unwrap();
    let reply = kernel.drain_until_reply();
    assert_eq!(
        reply["result"]["isError"], true,
        "raw input {raw} was accepted/rounded: {reply}"
    );
    assert!(!kernel.kv.contains_key(&format!("cash/{TRACK}")));
}
#[test]
fn large_raw_cent_input_cannot_round_down_before_validation() {
    raw_numeric_amount_must_be_refused("90071992547409.91");
}
#[test]
fn large_raw_cent_input_cannot_round_up_before_validation() {
    raw_numeric_amount_must_be_refused("70368744177664.01");
}
#[test]
fn large_raw_subcent_input_cannot_become_an_integer_before_validation() {
    raw_numeric_amount_must_be_refused("35184372088832.001");
}

#[test]
fn decimal_string_inputs_accept_cents_zero_and_scientific_notation() {
    let mut kernel = FakeKernel::boot_settling(DEAD_ENDPOINT, DEAD_ENDPOINT, 3600, "CNY");
    for (index, amount) in ["0", "0.01", "0.29", "1.20", "1e2"].into_iter().enumerate() {
        let reply = kernel.call_tool(
            2 + index as u64,
            "market.cash.set",
            json!({"currency":"CNY","amount":amount}),
            Some(TRACK),
        );
        assert_ne!(reply["result"]["isError"], true, "{amount}: {reply}");
        kernel.drain();
    }
    let reply = kernel.call_tool(10, "market.cash.list", json!({}), Some(TRACK));
    assert_eq!(
        reply["result"]["structuredContent"]["balances"][0]["amount"],
        100.0
    );
}
