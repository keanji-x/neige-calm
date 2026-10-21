//! The `read_at` tool through the plugin binary, over a real archive file:
//! the read contract's point-in-time value, the four results, the size cap
//! and the argument checks.

use crate::archive::security_count;
use std::path::Path;

use chrono::{DateTime, FixedOffset, SecondsFormat, Utc};
use research_archive::Store;
use research_archive::catalog::{Catalog, EndpointId};
use research_archive::plugin::point::{MAX_ANSWER_BYTES, MAX_READS, readable};
use research_archive::types::{FailureClass, Finish, Response, UnixMs};
use serde_json::{Value, json};

use crate::plugin::{Kernel, config};
use crate::support::{at, endpoint, failed, key, ok, plan_request};

const NVDA_DAY1: &str = r#"{"list":[{"fiscal_year":2026,"details":[{"key":"eps","estimate":"4.10"}]}],"currency":"USD"}"#;
const NVDA_DAY2: &str = r#"{"list":[{"fiscal_year":2026,"details":[{"key":"eps","estimate":"4.35"}]}],"currency":"USD"}"#;
const EMPTY_SHAPED: &str = r#"{"list":[]}"#;

/// A `rating_detail` body over the cap on its own: a long target series.
fn long_series() -> String {
    let list: Vec<Value> = (0..1_000)
        .map(
            |i| json!({ "timestamp": i.to_string(), "avg_target": "190.25", "date": "2026-10-12" }),
        )
        .collect();
    json!({ "target": { "list": list }, "ccy_symbol": "USD" }).to_string()
}

async fn record(
    store: &Store,
    run: research_archive::types::RunId,
    k: &str,
    e: &str,
    t: UnixMs,
    r: Response,
) {
    store
        .record_observation(run, &key(k), endpoint(e), t, &r)
        .await
        .unwrap();
}

/// Day 1 plans NVDA.US, AAPL.US, BAD.US and the index .SPX.US; days 2 to 4
/// plan NVDA.US. NVDA.US consensus: ok on day 1, ok on day 2, `permanent`
/// on day 3, an empty-shaped ok on day 4; all at offset 100 of the day.
/// AAPL.US consensus has only a failure. BAD.US ratings is not JSON.
/// NVDA.US rating_detail (day 1) is over the cap alone.
async fn archive() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&Store::path(dir.path())).await.unwrap();
    let day1 = store
        .plan_run(&plan_request(
            1,
            &["NVDA.US", "AAPL.US", "BAD.US"],
            &[".SPX.US"],
        ))
        .await
        .unwrap()
        .run_id;
    record(
        &store,
        day1,
        "NVDA.US",
        "consensus",
        at(1, 100),
        ok(NVDA_DAY1.as_bytes()),
    )
    .await;
    record(
        &store,
        day1,
        "NVDA.US",
        "rating_detail",
        at(1, 100),
        ok(long_series().as_bytes()),
    )
    .await;
    record(
        &store,
        day1,
        "AAPL.US",
        "consensus",
        at(1, 100),
        failed(FailureClass::Transient),
    )
    .await;
    record(
        &store,
        day1,
        "BAD.US",
        "ratings",
        at(1, 100),
        ok(b"not json"),
    )
    .await;
    store
        .end_run(day1, at(1, 200), Finish::Partial, None)
        .await
        .unwrap();
    let later = [
        ok(NVDA_DAY2.as_bytes()),
        failed(FailureClass::Permanent),
        ok(EMPTY_SHAPED.as_bytes()),
    ];
    for (n, outcome) in (2..=4).zip(later) {
        let run = store
            .plan_run(&plan_request(n, &["NVDA.US"], &[]))
            .await
            .unwrap()
            .run_id;
        record(&store, run, "NVDA.US", "consensus", at(n, 100), outcome).await;
        store
            .end_run(run, at(n, 200), Finish::Partial, None)
            .await
            .unwrap();
    }
    store.close().await;
    dir
}

fn rfc3339(t: UnixMs) -> String {
    DateTime::<Utc>::from_timestamp_millis(t.0)
        .unwrap()
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn read(symbol: &str, endpoint: &str, pointer: &str) -> Value {
    json!({ "symbol": symbol, "endpoint": endpoint, "pointer": pointer })
}

/// Calls `read_at` and returns its answer, checking that the text carries
/// exactly the structured answer.
fn call(kernel: &mut Kernel, at: &str, reads: Vec<Value>) -> Value {
    let result = kernel.call_tool("read_at", json!({ "at": at, "reads": reads }), None);
    assert_eq!(result.get("isError"), None, "{result}");
    let text: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text, result["structuredContent"]);
    text
}

/// Calls `read_at` expecting a tool error, and returns its text.
fn refused(kernel: &mut Kernel, arguments: Value) -> String {
    let result = kernel.call_tool("read_at", arguments, None);
    assert_eq!(result["isError"], true, "{result}");
    assert_eq!(result.get("structuredContent"), None, "{result}");
    result["content"][0]["text"].as_str().unwrap().to_owned()
}

fn nvda_consensus(kernel: &mut Kernel, t: UnixMs) -> Value {
    call(kernel, &rfc3339(t), vec![read("NVDA.US", "consensus", "")])["reads"][0].clone()
}

fn value(as_of: &str, fetched: UnixMs, body: &str) -> Value {
    json!({
        "symbol": "NVDA.US",
        "endpoint": "consensus",
        "pointer": "",
        "result": "value",
        "as_of": as_of,
        "fetched_at": rfc3339(fetched),
        "value": serde_json::from_str::<Value>(body).unwrap(),
    })
}

#[tokio::test]
async fn the_value_is_the_latest_ok_fetched_by_the_time() {
    let dir = archive().await;
    let mut kernel = Kernel::spawn(config(dir.path()));
    let day1 = value("2026-01-02", at(1, 100), NVDA_DAY1);
    let day2 = value("2026-01-03", at(2, 100), NVDA_DAY2);
    // The fetch time itself sees the row; the millisecond before does not.
    assert_eq!(nvda_consensus(&mut kernel, at(1, 100)), day1);
    assert_eq!(
        nvda_consensus(&mut kernel, at(2, 99)),
        day1,
        "a later row is invisible"
    );
    assert_eq!(nvda_consensus(&mut kernel, at(2, 100)), day2);
    assert_eq!(
        nvda_consensus(&mut kernel, at(3, 500)),
        day2,
        "a failure row never wins"
    );
    assert_eq!(
        nvda_consensus(&mut kernel, at(4, 100)),
        value("2026-01-05", at(4, 100), EMPTY_SHAPED),
        "an empty-shaped body is a value"
    );
    // The same moment written with an offset reads the same row.
    let shanghai = FixedOffset::east_opt(8 * 3_600).unwrap();
    let local = DateTime::<Utc>::from_timestamp_millis(at(2, 100).0)
        .unwrap()
        .with_timezone(&shanghai)
        .to_rfc3339_opts(SecondsFormat::Millis, false);
    assert!(local.ends_with("+08:00"), "{local}");
    let answer = call(&mut kernel, &local, vec![read("NVDA.US", "consensus", "")]);
    assert_eq!(
        answer,
        json!({ "at": rfc3339(at(2, 100)), "reads": [day2] })
    );
}

#[tokio::test]
async fn each_read_says_why_it_has_no_value() {
    let dir = archive().await;
    let mut kernel = Kernel::spawn(config(dir.path()));
    let answer = call(
        &mut kernel,
        &rfc3339(at(4, 1_000)),
        vec![
            read("ZZZZ.US", "consensus", ""),
            // An index symbol is planned only for the index endpoint.
            read(".SPX.US", "consensus", ""),
            read("AAPL.US", "consensus", ""),
            read("NVDA.US", "valuation", ""),
        ],
    );
    let results: Vec<(&str, &str, &str)> = answer["reads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            assert_eq!(r.as_object().unwrap().len(), 4, "{r}");
            (
                r["symbol"].as_str().unwrap(),
                r["endpoint"].as_str().unwrap(),
                r["result"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        results,
        [
            ("ZZZZ.US", "consensus", "not_archived"),
            (".SPX.US", "consensus", "not_archived"),
            ("AAPL.US", "consensus", "no_value"),
            ("NVDA.US", "valuation", "no_value"),
        ]
    );
    // Planned, but before the first fetch: no value yet.
    assert_eq!(nvda_consensus(&mut kernel, at(1, 99))["result"], "no_value");
}

#[tokio::test]
async fn the_pointer_selects_inside_the_body() {
    let dir = archive().await;
    let mut kernel = Kernel::spawn(config(dir.path()));
    let answer = call(
        &mut kernel,
        &rfc3339(at(2, 100)),
        vec![
            read("NVDA.US", "consensus", "/list/0/details/0/estimate"),
            read("NVDA.US", "consensus", "/list/1"),
            read("NVDA.US", "rating_detail", "/ccy_symbol"),
        ],
    );
    assert_eq!(
        answer["reads"],
        json!([
            {
                "symbol": "NVDA.US", "endpoint": "consensus", "pointer": "/list/0/details/0/estimate",
                "result": "value", "as_of": "2026-01-03", "fetched_at": rfc3339(at(2, 100)), "value": "4.35",
            },
            {
                "symbol": "NVDA.US", "endpoint": "consensus", "pointer": "/list/1",
                "result": "pointer_missing", "as_of": "2026-01-03", "fetched_at": rfc3339(at(2, 100)),
            },
            {
                "symbol": "NVDA.US", "endpoint": "rating_detail", "pointer": "/ccy_symbol",
                "result": "value", "as_of": "2026-01-02", "fetched_at": rfc3339(at(1, 100)), "value": "USD",
            },
        ])
    );
}

#[tokio::test]
async fn an_answer_over_the_cap_fails_with_every_size() {
    let dir = archive().await;
    let mut kernel = Kernel::spawn(config(dir.path()));
    assert!(long_series().len() > MAX_ANSWER_BYTES);
    let text = refused(
        &mut kernel,
        json!({
            "at": rfc3339(at(2, 100)),
            "reads": [read("NVDA.US", "consensus", "/currency"), read("NVDA.US", "rating_detail", "")],
        }),
    );
    assert!(
        text.contains(&format!("over the {MAX_ANSWER_BYTES}-byte cap")),
        "{text}"
    );
    let sizes =
        regex::Regex::new(r#"reads\[(\d)\] NVDA\.US (\w+) "([^"]*)": (\d+) bytes"#).unwrap();
    let listed: Vec<(String, String, String, usize)> = sizes
        .captures_iter(&text)
        .map(|c| {
            (
                c[1].to_owned(),
                c[2].to_owned(),
                c[3].to_owned(),
                c[4].parse().unwrap(),
            )
        })
        .collect();
    assert_eq!(listed.len(), 2, "{text}");
    assert_eq!(
        (
            listed[0].0.as_str(),
            listed[0].1.as_str(),
            listed[0].2.as_str()
        ),
        ("0", "consensus", "/currency")
    );
    assert!(listed[0].3 < 200, "{text}");
    assert_eq!(
        (listed[1].1.as_str(), listed[1].2.as_str()),
        ("rating_detail", "")
    );
    assert!(listed[1].3 > long_series().len(), "{text}");
}

#[tokio::test]
async fn a_body_that_is_not_json_fails_the_call() {
    let dir = archive().await;
    let mut kernel = Kernel::spawn(config(dir.path()));
    let text = refused(
        &mut kernel,
        json!({ "at": rfc3339(at(1, 100)), "reads": [read("NVDA.US", "consensus", ""), read("BAD.US", "ratings", "")] }),
    );
    assert!(
        text.starts_with("reads[1] (BAD.US ratings): the body fetched at "),
        "{text}"
    );
    assert!(text.contains("is not JSON"), "{text}");
}

#[tokio::test]
async fn bad_arguments_are_refused_before_any_read() {
    // No archive at all: a checked call would fail on opening it instead.
    let empty = tempfile::tempdir().unwrap();
    let mut kernel = Kernel::spawn(config(empty.path()));
    let good = || read("NVDA.US", "consensus", "");
    let catalog = Catalog::compiled();
    let readable_ids = format!(
        "one of: {}",
        readable(&catalog)
            .map(EndpointId::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    );
    for (arguments, reason) in [
        (
            json!({ "at": "2026-10-13T16:00:00", "reads": [good()] }),
            "RFC 3339 time with an offset",
        ),
        (
            json!({ "at": "yesterday", "reads": [good()] }),
            "RFC 3339 time with an offset",
        ),
        (json!({ "reads": [good()] }), "missing field `at`"),
        (
            json!({ "at": "2026-10-13T16:00:00Z", "reads": [] }),
            "`reads` takes 1 to 12 reads, not 0",
        ),
        (
            json!({ "at": "2026-10-13T16:00:00Z", "reads": vec![good(); MAX_READS + 1] }),
            "not 13",
        ),
        (
            json!({ "at": "2026-10-13T16:00:00Z", "reads": [read("NVDA.US", "index_constituents", "")] }),
            readable_ids.as_str(),
        ),
        (
            json!({ "at": "2026-10-13T16:00:00Z", "reads": [read("NVDA.US", "eps", "")] }),
            readable_ids.as_str(),
        ),
        (
            json!({ "at": "2026-10-13T16:00:00Z", "reads": [{ "symbol": "NVDA.US", "endpoint": "consensus" }] }),
            "missing field `pointer`",
        ),
        (
            json!({ "at": "2026-10-13T16:00:00Z", "reads": [good()], "limit": 1 }),
            "unknown field `limit`",
        ),
        (
            json!({ "at": "2026-10-13T16:00:00Z", "reads": [read("NVDA.US", "consensus", "list")] }),
            "reads[0].pointer \"list\" is not an RFC 6901 JSON pointer",
        ),
        (
            json!({ "at": "2026-10-13T16:00:00Z", "reads": [read("NVDA.US", "consensus", "/a~2b")] }),
            "reads[0].pointer \"/a~2b\" is not an RFC 6901 JSON pointer",
        ),
        (
            json!({ "at": "2026-10-13T16:00:00Z", "reads": [read("NVDA.US", "consensus", "/a~")] }),
            "reads[0].pointer \"/a~\" is not an RFC 6901 JSON pointer",
        ),
        (
            json!({
                "at": "2026-10-13T16:00:00Z",
                "reads": [{ "symbol": "NVDA.US", "endpoint": "consensus", "pointer": "", "extra": 1 }],
            }),
            "unknown field `extra`",
        ),
        (
            json!({ "at": "2026-10-13T16:00:00Z", "reads": [read("NV DA", "consensus", "")] }),
            "reads[0].symbol",
        ),
    ] {
        let text = refused(&mut kernel, arguments.clone());
        assert!(text.contains(reason), "{arguments}: {text}");
    }
    // A checked call does reach the archive.
    let text = refused(
        &mut kernel,
        json!({ "at": "2026-10-13T16:00:00Z", "reads": [good()] }),
    );
    assert!(text.starts_with("the archive cannot be read"), "{text}");
}

/// The manifest's input schema is the plugin's own check: the endpoint enum
/// is the catalog's security endpoints, and the read cap is `MAX_READS`.
#[test]
fn the_manifest_schema_matches_the_catalog() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("manifest.json");
    let manifest: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let tool = manifest["exposes_tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "read_at")
        .unwrap();
    let schema = &tool["input_schema"];
    let reads = &schema["properties"]["reads"];
    let item = &reads["items"];
    let catalog = Catalog::compiled();
    let ids: Vec<&str> = readable(&catalog).map(EndpointId::as_str).collect();
    assert_eq!(ids.len(), security_count() + 1);
    assert_eq!(item["properties"]["endpoint"]["enum"], json!(ids));
    assert_eq!(
        (&reads["minItems"], &reads["maxItems"]),
        (&json!(1), &json!(MAX_READS))
    );
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["required"], json!(["at", "reads"]));
    assert_eq!(item["additionalProperties"], false);
    assert_eq!(item["required"], json!(["endpoint", "pointer"]));
    assert_eq!(
        item["oneOf"],
        json!([
            {"required":["symbol"],"not":{"required":["market"]}},
            {"required":["market"],"not":{"required":["symbol"]}}
        ])
    );
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
}

#[tokio::test]
async fn annual_and_quarterly_bodies_are_independent_point_in_time_values() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&Store::path(dir.path())).await.unwrap();
    let run = store
        .plan_run(&plan_request(1, &["NVDA.US"], &[]))
        .await
        .unwrap();
    record(
        &store,
        run.run_id,
        "NVDA.US",
        "consensus",
        at(1, 100),
        ok(br#"{"current_period":"qf","eps":2}"#),
    )
    .await;
    record(
        &store,
        run.run_id,
        "NVDA.US",
        "consensus_annual",
        at(1, 150),
        ok(br#"{"current_period":"af","eps":10}"#),
    )
    .await;
    store.close().await;
    let mut kernel = Kernel::spawn(config(dir.path()));
    let prior = call(
        &mut kernel,
        &rfc3339(at(1, 149)),
        vec![read("NVDA.US", "consensus_annual", "")],
    );
    assert_eq!(prior["reads"][0]["result"], "no_value");
    let result = call(
        &mut kernel,
        &rfc3339(at(1, 150)),
        vec![
            read("NVDA.US", "consensus", "/eps"),
            read("NVDA.US", "consensus_annual", "/eps"),
        ],
    );
    assert_eq!(result["reads"][0]["value"], 2);
    assert_eq!(result["reads"][1]["value"], 10);
}

#[tokio::test]
async fn market_and_benchmark_reads_keep_point_in_time_and_target_contracts() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&Store::path(dir.path())).await.unwrap();
    let plan = store
        .plan_run(&plan_request(1, &["MSFT.US"], &[]))
        .await
        .unwrap();
    let calendar = endpoint("finance_calendar");
    record(
        &store,
        plan.run_id,
        "market:US",
        "finance_calendar",
        at(1, 20),
        Response::Body(br#"{"days":{"2026-01-02":{"MSFT.US":{"a":{"id":"a"}}}}}"#.to_vec()),
    )
    .await;
    for symbol in ["MSFT.US", "SOXX.US"] {
        record(
            &store,
            plan.run_id,
            symbol,
            "daily_candlesticks",
            at(1, 30),
            Response::Body(
                br#"{"bars":[{"trade_date":"2026-01-02","close":"10.123456789"}]}"#.to_vec(),
            ),
        )
        .await;
    }
    assert!(
        store
            .ever_planned(&crate::support::key("market:US"), calendar)
            .await
            .unwrap()
    );
    store.close().await;
    let mut kernel = Kernel::spawn(config(dir.path()));
    let market =
        json!({"market":"US","endpoint":"finance_calendar","pointer":"/days/2026-01-02/MSFT.US/a"});
    let early = call(&mut kernel, &rfc3339(at(1, 10)), vec![market.clone()]);
    assert_eq!(early["reads"][0]["result"], "no_value");
    let found = call(
        &mut kernel,
        &rfc3339(at(1, 40)),
        vec![
            market,
            read("MSFT.US", "daily_candlesticks", "/bars/0"),
            read("SOXX.US", "daily_candlesticks", "/bars/0"),
        ],
    );
    assert_eq!(found["reads"][0]["market"], "US");
    assert_eq!(found["reads"][0]["value"]["id"], "a");
    assert_eq!(
        found["reads"][1]["value"]["trade_date"],
        found["reads"][2]["value"]["trade_date"]
    );
    assert_eq!(found["reads"][1]["as_of"], "2026-01-02");
    assert_eq!(found["reads"][1]["fetched_at"], rfc3339(at(1, 30)));
    let text = refused(
        &mut kernel,
        json!({"at":rfc3339(at(1,40)),"reads":[read("US","finance_calendar","")]}),
    );
    assert!(text.contains("target kind"));
}
