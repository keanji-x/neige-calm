//! Process-level tests for the market-data plugin, driven through a fake kernel over the real
//! stdio channel. Every price source is pointed at loopback; `USDT` prices at 1.0 with no request.

use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::{Value, json};

use crate::support::market_plugin::*;
/// The tool itself does not price (an `openWorldHint: true` tool is refused under
/// `approval_policy: "never"`); the write records state and wakes the poll thread.
#[test]
fn setting_a_holding_prices_it_now_and_publishes_to_the_callers_track() {
    let (endpoint, _hits) = price_server("2.5");
    // A long poll interval proves the publish came from the WAKE, not from the interval elapsing.
    let mut kernel = FakeKernel::boot(&endpoint);

    let reply = kernel.set_holding(2, "BTC", 4.0, TRACK);

    assert_ne!(
        reply.pointer("/result/isError"),
        Some(&json!(true)),
        "{reply:#?}"
    );
    assert_eq!(
        kernel.kinds_pushed(),
        vec![
            format!("portfolio.holdings@{TRACK}"),
            format!("portfolio.history@{TRACK}"),
        ],
        "both tables, both on the caller's Track"
    );
    let total = kernel.pushes[0]
        .1
        .pointer("/rows")
        .and_then(Value::as_array)
        .and_then(|rows| rows.last())
        .and_then(|row| row["value"].as_f64());
    assert_eq!(total, Some(10.0), "4 BTC at 2.5 is 10");
    assert_eq!(
        kernel.kv.get("holdings/trk_caller"),
        Some(&json!([{ "asset": "CRYPTO:BTC", "quantity": 4.0 }])),
        "the holding is stored under the caller's Track, spelled as the \
         canonical identity a bare crypto name normalises to"
    );
}

/// The collapse happens across the whole `set` path — load (which normalises), `retain`, then a
/// whole-array overwrite — so it is driven through the real binary.
#[test]
fn a_legacy_bare_holding_is_replaced_not_doubled_by_a_qualified_write() {
    let mut kernel = FakeKernel::boot(DEAD_ENDPOINT);
    // Seeded directly, as a pre-venues install would have left it.
    kernel.kv.insert(
        "holdings/trk_caller".to_string(),
        json!([{ "asset": "BTC", "quantity": 100.0 }]),
    );

    kernel.set_holding(2, "crypto:BTC", 60.0, TRACK);

    assert_eq!(
        kernel.kv.get("holdings/trk_caller"),
        Some(&json!([{ "asset": "CRYPTO:BTC", "quantity": 60.0 }])),
        "one row at the written quantity — two rows here would be 160 BTC"
    );
}

/// `HK:01810` and `HK:1810` are the same shares; the Hong Kong fold lives in `parse_asset`,
/// not only in the URL builder, or both rows would be priced and summed.
#[test]
fn a_padded_hong_kong_holding_is_replaced_not_doubled_by_an_unpadded_write() {
    let mut kernel = FakeKernel::boot(DEAD_ENDPOINT);
    kernel.kv.insert(
        "holdings/trk_caller".to_string(),
        json!([{ "asset": "HK:01810", "quantity": 100.0 }]),
    );

    kernel.set_holding(2, "hk:1810", 60.0, TRACK);

    assert_eq!(
        kernel.kv.get("holdings/trk_caller"),
        Some(&json!([{ "asset": "HK:01810", "quantity": 60.0 }])),
        "one row at the written quantity — two rows here would be 160 shares \
         of Xiaomi priced and totalled as a position nobody holds"
    );
}

/// The row survives only because `CN:600519` still parses: `load_holdings` drops rows it
/// cannot parse and `store_holdings` overwrites the whole array, so only a real round trip
/// shows the composition.
#[test]
fn a_stored_cn_holding_survives_a_write_to_a_different_asset() {
    let mut kernel = FakeKernel::boot(DEAD_ENDPOINT);
    kernel.kv.insert(
        "holdings/trk_caller".to_string(),
        json!([{ "asset": "CN:600519", "quantity": 7.0 }]),
    );

    // The `CN:` row is the bystander the overwrite must not lose.
    kernel.set_holding(2, "SH:600519", 3.0, TRACK);

    assert_eq!(
        kernel.kv.get("holdings/trk_caller"),
        Some(&json!([
            { "asset": "CN:600519", "quantity": 7.0 },
            { "asset": "SH:600519", "quantity": 3.0 },
        ])),
        "the legacy CN row must still be in the store at its original \
         quantity; a parse failure on it would have dropped it from the load \
         and the whole-array overwrite would then have erased it for good"
    );
}

/// Acting on some other Track's portfolio is the failure the `_meta` namespace exists to prevent.
#[test]
fn a_call_without_a_track_is_refused_rather_than_guessed() {
    let mut kernel = FakeKernel::boot(DEAD_ENDPOINT);
    let reply = kernel.call_tool(
        2,
        "market.holdings.set",
        json!({ "asset": "BTC", "quantity": 1 }),
        None,
    );
    assert_eq!(reply.pointer("/result/isError"), Some(&json!(true)));
    assert!(text_of(&reply).contains("carries no Track"), "{reply:#?}");
    kernel.drain();
    assert!(
        kernel.pushes.is_empty() && kernel.kv.is_empty(),
        "nothing may be written or published: {:?}",
        kernel.methods
    );
    assert!(kernel.is_responsive());
}

#[test]
fn holdings_are_per_track() {
    let (endpoint, _hits) = price_server("2");
    let mut kernel = FakeKernel::boot(&endpoint);
    kernel.set_holding(2, "BTC", 1.0, TRACK);
    kernel.set_holding(3, "BTC", 5.0, OTHER_TRACK);

    assert_eq!(
        kernel.kv.get("holdings/trk_caller"),
        Some(&json!([{ "asset": "CRYPTO:BTC", "quantity": 1.0 }]))
    );
    assert_eq!(
        kernel.kv.get("holdings/trk_someone_else"),
        Some(&json!([{ "asset": "CRYPTO:BTC", "quantity": 5.0 }])),
        "the second call must not overwrite the first Track's portfolio"
    );
    // Read per Track rather than in push order: a pass may batch both Tracks.
    assert_eq!(kernel.last_total_for(TRACK), Some(2.0));
    assert_eq!(kernel.last_total_for(OTHER_TRACK), Some(10.0));

    // Storing separately is not the same as reading separately.
    let listed = kernel.call_tool(4, "market.holdings.list", json!({}), Some(TRACK));
    let holdings = listed
        .pointer("/result/structuredContent/holdings")
        .and_then(Value::as_array)
        .expect("holdings")
        .clone();
    assert_eq!(holdings.len(), 1, "{listed:#?}");
    assert_eq!(
        holdings[0]["qty"].as_f64(),
        Some(1.0),
        "reading back the first Track must not see the second Track's quantity"
    );
}

#[test]
fn a_quantity_of_zero_removes_the_holding() {
    let (endpoint, _hits) = price_server("2");
    let mut kernel = FakeKernel::boot(&endpoint);
    kernel.set_holding(2, "BTC", 1.0, TRACK);
    kernel.set_holding(3, "BTC", 0.0, TRACK);
    assert_eq!(kernel.kv.get("holdings/trk_caller"), Some(&json!([])));
}

/// A tool call issues callbacks whose replies arrive on the same stdin the plugin reads; the
/// reply must land far inside the plugin's own 15s callback timeout.
#[test]
fn a_tool_call_is_answered_while_the_reader_keeps_reading() {
    let mut kernel = FakeKernel::boot(DEAD_ENDPOINT);
    let reply = kernel.call_tool(2, "market.holdings.list", json!({}), Some(TRACK));
    assert_eq!(reply.get("id"), Some(&json!(2)), "{reply:#?}");
}

/// The portfolio is deliberately PARTLY priceable: a wholly unpriceable one would leave the
/// total at zero, and a defect that skipped history only on a zero total would survive.
#[test]
fn a_tick_that_cannot_price_everything_writes_no_history_point() {
    let mut kernel = FakeKernel::boot(DEAD_ENDPOINT);
    kernel.set_holding(2, "USDT", 3.0, TRACK);
    let before = kernel.pushes.len();
    kernel.set_holding(3, "BTC", 1.0, TRACK);

    assert_eq!(
        kernel.pushes_since(before),
        vec![format!("portfolio.holdings@{TRACK}")],
        "holdings only — no history read, no history write, no history overlay"
    );
    let rows = kernel.pushes.last().unwrap().1["rows"]
        .as_array()
        .expect("rows")
        .clone();
    let btc = rows
        .iter()
        .find(|row| row["asset"] == json!("BTC"))
        .expect("the unpriceable holding keeps its row");
    assert!(btc["price"].is_null() && btc["value"].is_null(), "{btc}");
    assert_eq!(
        rows.last().unwrap()["value"].as_f64(),
        Some(3.0),
        "the total covers the priced rows only, so this pass is PARTIAL not empty"
    );
    assert!(kernel.is_responsive());
}

/// Publishing an unstored point would show one the next pass silently drops.
#[test]
fn a_history_point_that_cannot_be_stored_is_not_published() {
    let (endpoint, _hits) = price_server("2");
    let mut kernel = FakeKernel::boot(&endpoint);
    kernel.set_holding(2, "BTC", 1.0, TRACK);
    let before = kernel.pushes.len();

    // Refuse only the history write, or the pass never reaches the step this test is about.
    kernel.refuse_kv_set = Some("history/".into());
    kernel.set_holding(3, "ETH", 2.0, TRACK);

    assert_eq!(
        kernel.pushes_since(before),
        vec![format!("portfolio.holdings@{TRACK}")],
        "the refused write must end the pass — no history overlay follows it"
    );
    assert!(kernel.is_responsive());
}

/// The pass lists every Track up front and then prices them one at a time; the fake kernel
/// changes the stored holding after the listing is answered, before the Track is priced.
#[test]
fn a_poll_pass_prices_what_is_stored_now_not_what_it_listed() {
    let (endpoint, _hits) = price_server("2");
    let mut kernel = FakeKernel::boot_polling(&endpoint, 5);

    kernel.set_holding(2, "BTC", 1.0, TRACK);
    let before = kernel.pushes.len();

    // From the next listing onward the store says 5, not 1.
    kernel.mutate_after_list = Some((
        "holdings/trk_caller".to_string(),
        json!([{ "asset": "BTC", "quantity": 5.0 }]),
    ));

    // Wait out one poll pass (5s floor) and service it.
    let mut totals = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while std::time::Instant::now() < deadline && totals.is_empty() {
        if let Ok(frame) = kernel.frames.recv_timeout(Duration::from_secs(8))
            && frame.get("method").is_some()
        {
            kernel.service(&frame);
        }
        totals = kernel.pushes[before..]
            .iter()
            .filter(|(kind, _)| kind.starts_with("portfolio.holdings@"))
            .filter_map(|(_, payload)| {
                payload
                    .pointer("/rows")
                    .and_then(Value::as_array)
                    .and_then(|rows| rows.last())
                    .and_then(|row| row["value"].as_f64())
            })
            .collect();
    }

    assert_eq!(
        totals.first(),
        Some(&10.0),
        "the pass must price the stored 5 BTC (=10), not the 1 BTC it listed"
    );
}

/// One asset is priced once per pass, however many Tracks hold it.
#[test]
fn a_poll_pass_prices_each_asset_once_across_tracks() {
    let (endpoint, hits) = price_server("2");
    let mut kernel = FakeKernel::boot_polling(&endpoint, 5);

    for (id, track) in [(2, TRACK), (3, OTHER_TRACK)] {
        kernel.set_holding(id, "BTC", 1.0, track);
    }
    let before_pass = hits.load(Ordering::SeqCst);

    // Service exactly one background pass: both Tracks, one asset.
    let mut seen_tracks = std::collections::HashSet::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while std::time::Instant::now() < deadline && seen_tracks.len() < 2 {
        if let Ok(frame) = kernel.frames.recv_timeout(Duration::from_secs(8))
            && frame.get("method").is_some()
        {
            {
                let kind = frame
                    .pointer("/params/kind")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let entity = frame
                    .pointer("/params/entity_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                kernel.service(&frame);
                if kind == "portfolio.holdings" {
                    seen_tracks.insert(entity);
                }
            }
        }
    }

    assert_eq!(
        seen_tracks.len(),
        2,
        "both Tracks must be priced in the pass"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst) - before_pass,
        1,
        "BTC must be fetched once for the whole pass, not once per Track"
    );
}

/// Returning early on an empty portfolio would leave a reader who has just sold everything
/// looking at their previous position as current. No history point goes with it.
#[test]
fn removing_the_last_holding_publishes_an_empty_table() {
    let (endpoint, _hits) = price_server("2");
    let mut kernel = FakeKernel::boot(&endpoint);
    kernel.set_holding(2, "BTC", 1.0, TRACK);
    let before = kernel.pushes.len();
    let reply = kernel.set_holding(3, "BTC", 0.0, TRACK);

    assert_ne!(
        reply.pointer("/result/isError"),
        Some(&json!(true)),
        "removing the last holding is a success: {reply:#?}"
    );
    assert_eq!(
        kernel.pushes_since(before),
        vec![format!("portfolio.holdings@{TRACK}")],
        "the emptied table is republished, and no history point goes with it"
    );
    let rows = kernel.pushes.last().unwrap().1["rows"]
        .as_array()
        .expect("rows")
        .clone();
    assert_eq!(rows.len(), 1, "only the Total row remains: {rows:?}");
    assert_eq!(
        rows[0]["value"].as_f64(),
        Some(0.0),
        "an empty portfolio's total really is zero"
    );
    let caption = kernel.pushes.last().unwrap().1["caption"]
        .as_str()
        .expect("caption")
        .to_string();
    assert!(
        caption.contains("this Track holds nothing, so its total is 0"),
        "the caption explains the zero rather than denying there is a total: \
         {caption}"
    );
    assert!(!caption.contains("no total is shown"), "{caption}");
}
/// The last `portfolio.holdings` payload pushed at `track`.
fn last_holdings_table<'a>(kernel: &'a FakeKernel, track: &str) -> &'a Value {
    kernel
        .pushes
        .iter()
        .rev()
        .find(|(kind, _)| kind == &format!("portfolio.holdings@{track}"))
        .map(|(_, payload)| payload)
        .expect("a holdings push")
}

/// The price stays in the currency its market quotes; the total carries it into the settlement
/// currency by a rate this plugin fetched.
#[test]
fn a_hong_kong_holding_is_priced_in_hkd_and_settled_in_usd() {
    let mut kernel = FakeKernel::boot_settling(DEAD_ENDPOINT, &sina_server(), 3600, "USD");

    kernel.set_holding(2, "HK:1810", 100.0, TRACK);

    assert_eq!(
        kernel.kinds_pushed(),
        vec![
            format!("portfolio.holdings@{TRACK}"),
            format!("portfolio.history@{TRACK}"),
        ],
        "a fully-priced, fully-converted tick publishes both tables"
    );
    let table = last_holdings_table(&kernel, TRACK);
    let rows = table["rows"].as_array().expect("rows");
    assert_eq!(
        rows[0]["asset"],
        json!("01810"),
        "the canonical five-digit Hong Kong code, which `HK:1810` folds to"
    );
    assert_eq!(rows[0]["venue"], json!("HK"));
    assert_eq!(
        rows[0]["price"].as_f64(),
        Some(27.48),
        "field 6 of the HK row is the last price; field 3 (28.44) is the \
         previous close: {rows:?}"
    );
    assert_eq!(
        rows[0]["currency"],
        json!("HKD"),
        "the price is not relabelled with the settlement currency"
    );
    assert_eq!(
        rows[0]["rate"].as_f64(),
        Some(0.12755265),
        "field 8 of `fx_shkdusd`: {rows:?}"
    );
    assert_eq!(rows[0]["value"].as_f64(), Some(350.51));
    assert_eq!(rows[1]["value"].as_f64(), Some(350.51), "the Total row");
    assert_eq!(rows[1]["currency"], json!("USD"));
    let caption = table["caption"].as_str().expect("caption");
    assert!(caption.contains("totalled in USD"), "{caption}");
    assert!(
        caption.contains("HKD→USD 0.12755265 (fx_shkdusd@sina 0.12755265)"),
        "the conversion is named, at the rate it used: {caption}",
    );

    // The stored history point carries its own unit.
    let points = kernel
        .kv
        .get("history/trk_caller")
        .and_then(Value::as_array)
        .expect("a history document");
    assert_eq!(points.len(), 1, "{points:?}");
    assert_eq!(points[0]["total"].as_f64(), Some(350.51));
    assert_eq!(
        points[0]["currency"],
        json!("USD"),
        "a point that does not say what it is in cannot be compared to \
         anything later: {points:?}"
    );
}

/// Each row is carried into USD first, so the sum is a sum of like things.
#[test]
fn a_portfolio_across_two_currencies_totals_and_writes_a_history_point() {
    let mut kernel = FakeKernel::boot_settling(DEAD_ENDPOINT, &sina_server(), 3600, "USD");
    kernel.set_holding(2, "USDT", 1.0, TRACK);
    assert_eq!(kernel.last_total_for(TRACK), Some(1.0));
    let before = kernel.pushes.len();

    kernel.set_holding(3, "HK:1810", 100.0, TRACK);

    assert_eq!(
        kernel.pushes_since(before),
        vec![
            format!("portfolio.holdings@{TRACK}"),
            format!("portfolio.history@{TRACK}"),
        ],
        "the history overlay goes out too: this tick HAS a total",
    );
    let table = last_holdings_table(&kernel, TRACK);
    let rows = table["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 3, "both asset rows plus the Total: {rows:?}");
    assert_eq!(rows[0]["currency"], json!("USDT"));
    assert_eq!(rows[1]["currency"], json!("HKD"));
    assert_eq!(
        rows[2]["value"].as_f64(),
        Some(351.51),
        "1 USDT at par plus 2748 HKD at 0.1275526474: {rows:?}"
    );
    assert_eq!(rows[2]["currency"], json!("USD"));
    let caption = table["caption"].as_str().expect("caption");
    assert!(caption.contains("totalled in USD"), "{caption}");
    assert!(
        caption.contains("USDT→USD 1 (USDT taken as 1 USD — assumed, not quoted)"),
        "the one number no source stated is published as an assumption: {caption}",
    );
    assert!(
        caption.contains("HKD→USD 0.12755265 (fx_shkdusd@sina 0.12755265)"),
        "{caption}",
    );

    // Two points now, both labelled.
    let points = kernel
        .kv
        .get("history/trk_caller")
        .and_then(Value::as_array)
        .expect("a history document");
    assert_eq!(points.len(), 2, "{points:?}");
    assert_eq!(points[1]["total"].as_f64(), Some(351.51));
    assert_eq!(points[1]["currency"], json!("USD"));

    let listed = kernel.call_tool(4, "market.holdings.list", json!({}), Some(TRACK));
    let text = text_of(&listed);
    assert!(text.contains("Total 351.51 USD"), "{text}");
    assert!(text.contains("assumed, not quoted"), "{text}");
    assert_eq!(
        listed.pointer("/result/structuredContent/currency"),
        Some(&json!("USD")),
        "{listed:#?}"
    );
    assert!(kernel.is_responsive());
}

/// `complete` is false because ONE holding's rate is missing while the rest sum to a real
/// partial total; prose and `structuredContent` are read out of ONE real reply.
#[test]
fn the_list_tool_says_the_same_thing_in_prose_and_in_structured_content() {
    let mut kernel =
        FakeKernel::boot_settling(DEAD_ENDPOINT, &sina_server_without_rates(), 3600, "CNY");
    // Priced in CNY, settling in CNY: needs no rate, and is counted.
    kernel.set_holding(2, "SH:600519", 2.0, TRACK);
    // Priced in USD with no USD→CNY row anywhere: cannot be converted.
    kernel.set_holding(3, "US:NVDA", 1.0, TRACK);

    let listed = kernel.call_tool(4, "market.holdings.list", json!({}), Some(TRACK));
    let text = text_of(&listed);
    let structured = listed
        .pointer("/result/structuredContent")
        .expect("structuredContent");

    assert_eq!(
        structured["complete"],
        json!(false),
        "one holding could not be converted: {structured:#?}"
    );
    assert_eq!(
        structured["total"].as_f64(),
        Some(2633.88),
        "the payload carries a partial total: {structured:#?}"
    );
    assert_eq!(structured["currency"], json!("CNY"));
    assert!(
        text.contains("Partial total 2633.88 CNY"),
        "the prose has to name the number the payload carries: {text}"
    );
    assert!(
        !text.contains("No total"),
        "2633.88 is in the payload; the prose must not deny it: {text}"
    );
    assert!(
        text.contains("covers only the holdings that both priced and converted"),
        "and it has to say what the number leaves out: {text}"
    );
    assert!(kernel.is_responsive());
}

/// The stock source answers the price and lists no rate row at all (a partial outage); nothing
/// reaches for an older rate to keep the series moving.
#[test]
fn a_holding_whose_rate_is_unavailable_writes_no_history_point() {
    let mut kernel =
        FakeKernel::boot_settling(DEAD_ENDPOINT, &sina_server_without_rates(), 3600, "USD");

    kernel.set_holding(2, "HK:1810", 100.0, TRACK);

    assert_eq!(
        kernel.kinds_pushed(),
        vec![format!("portfolio.holdings@{TRACK}")],
        "holdings only — no history read, no history write, no history overlay"
    );
    let table = last_holdings_table(&kernel, TRACK);
    let rows = table["rows"].as_array().expect("rows");
    assert_eq!(
        rows[0]["price"].as_f64(),
        Some(27.48),
        "the price came back and is kept: {rows:?}"
    );
    assert_eq!(rows[0]["currency"], json!("HKD"));
    assert!(rows[0]["rate"].is_null(), "{rows:?}");
    assert!(rows[0]["value"].is_null(), "{rows:?}");
    assert!(
        rows[1]["currency"].is_null(),
        "the Total is in no currency: {rows:?}"
    );
    assert!(
        rows[1]["value"].is_null(),
        "this holding is worth 2748 HKD; a `0` here would be a wrong number \
         rather than a missing one: {rows:?}"
    );
    let caption = table["caption"].as_str().expect("caption");
    assert!(caption.contains("no total is shown"), "{caption}");
    assert!(
        caption.contains("not one holding could be priced and converted"),
        "{caption}"
    );
    assert!(
        !kernel.kv.contains_key("history/trk_caller"),
        "no point was written: {:?}",
        kernel.kv.get("history/trk_caller"),
    );
    assert!(kernel.is_responsive());
}

/// KNOWN GAP: a legacy `{at, total}` point records no currency, so the change column is blank
/// across that boundary and the row's currency cell is empty rather than borrowing today's.
#[test]
fn a_point_from_before_this_slice_is_not_compared_against_a_new_one() {
    let mut kernel = FakeKernel::boot_settling(DEAD_ENDPOINT, &sina_server(), 3600, "USD");
    // `100.0` in an unknown unit: read as USD, the row below would show a +250.51 move that never happened.
    kernel.kv.insert(
        "history/trk_caller".into(),
        json!([{ "at": "2026-09-06T12:00:00Z", "total": 100.0 }]),
    );

    kernel.set_holding(2, "HK:1810", 100.0, TRACK);

    let points = kernel
        .kv
        .get("history/trk_caller")
        .and_then(Value::as_array)
        .expect("a history document");
    assert_eq!(points.len(), 2, "the old point is kept: {points:?}");
    assert!(
        points[0].get("currency").is_none(),
        "nothing invents a unit for it: {points:?}"
    );
    assert_eq!(points[1]["currency"], json!("USD"));

    let history = kernel
        .pushes
        .iter()
        .rev()
        .find(|(kind, _)| kind == &format!("portfolio.history@{TRACK}"))
        .map(|(_, payload)| payload)
        .expect("a history push");
    let rows = history["rows"].as_array().expect("rows");
    // Newest first.
    assert_eq!(rows[0]["total"].as_f64(), Some(350.51));
    assert_eq!(rows[0]["currency"], json!("USD"));
    assert!(
        rows[0]["change"].is_null(),
        "250.51 would be a move between two different units: {rows:?}"
    );
    assert_eq!(rows[1]["total"].as_f64(), Some(100.0));
    assert!(rows[1]["currency"].is_null(), "{rows:?}");
    let caption = history["caption"].as_str().expect("caption");
    assert!(
        caption.contains("1 point was recorded before this plugin stored a currency"),
        "{caption}"
    );
    assert!(kernel.is_responsive());
}
