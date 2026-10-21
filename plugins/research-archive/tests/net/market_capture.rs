//! New source contracts through real HttpClient and local upstream responses.
use crate::{
    fake::{FakeServer, Reply},
    hermetic,
    source::source,
};
use longbridge::oauth::OAuth;
use research_archive::{
    Catalog,
    types::{AsOf, Response},
};
use serde_json::{Value, json};

fn query() -> Vec<(String, String)> {
    let catalog = Catalog::compiled();
    let e = catalog.get("finance_calendar").unwrap();
    let mut q = e.resolve("market:US", AsOf::parse("2026-10-21").unwrap());
    for (name, value) in &mut q {
        if name == "date" || name == "date_end" {
            *value = "2026-10-21".into();
        }
    }
    q
}
const FIRST: &str = r#"{"next_date":"2026-10-22","extra":0.123456789012345678901,"list":[{"date":"2026-10-21","count":2,"infos":[{"id":"a","symbol":"MSFT.US","market":"US"}]}]}"#;
const LAST: &str = r#"{"next_date":"2026-10-22","list":[{"date":"2026-10-21","count":2,"infos":[{"id":"b","symbol":"JPM.US","market":"US"}]}]}"#;

#[test]
fn market_window_exhausts_same_day_offsets_and_preserves_raw_pages() {
    hermetic::run(async {
        let server = FakeServer::start(|s| {
            let q = url::form_urlencoded::parse(s.query.as_deref().unwrap().as_bytes())
                .collect::<std::collections::BTreeMap<_, _>>();
            assert_eq!(q.get("count").unwrap(), "100");
            assert_eq!(q.get("markets[]").unwrap(), "US");
            Reply::data(if q.get("offset").unwrap() == "0" {
                FIRST
            } else {
                LAST
            })
        })
        .await;
        let source = source(&server.url, OAuth::from_token("ap_m_FAKE"));
        let catalog = Catalog::compiled();
        let endpoint = catalog.get("finance_calendar").unwrap();
        let fetched = source
            .fetch_request_if(endpoint, &query(), || true)
            .await
            .unwrap();
        let Response::Body(raw) = fetched.response() else {
            panic!("complete body expected")
        };
        let value: Value = serde_json::from_slice(raw).unwrap();
        assert_eq!(server.seen().len(), 2);
        assert_eq!(value["pages"][0]["raw_json"], FIRST);
        assert_eq!(value["pages"][1]["raw_json"], LAST);
        assert_eq!(value["pages"][0]["offset"], 0);
        assert_eq!(value["pages"][1]["offset"], 1);
        assert_eq!(value["reported_counts"]["2026-10-21"], 2);
        assert_eq!(value["company_counts"]["2026-10-21"], 2);
        assert_eq!(value["coverage"], "upstream_group_company_dates");
        assert_eq!(value["days"]["2026-10-21"]["MSFT.US"]["a"]["id"], "a");
        assert_eq!(value["days"]["2026-10-21"]["JPM.US"]["b"]["id"], "b");
    })
}

#[test]
fn later_calendar_failure_and_cancellation_publish_no_partial_value() {
    hermetic::run(async {
        let server = FakeServer::start(|s| {
            if s.query.as_deref().unwrap().contains("offset=0") {
                Reply::data(FIRST)
            } else {
                Reply::new(503, "busy")
            }
        })
        .await;
        let source = source(&server.url, OAuth::from_token("ap_m_FAKE"));
        let catalog = Catalog::compiled();
        let endpoint = catalog.get("finance_calendar").unwrap();
        let result = source
            .fetch_request_if(endpoint, &query(), || true)
            .await
            .unwrap();
        assert!(matches!(result.response(), Response::Failed { .. }));
        assert_eq!(server.seen().len(), 2);
        let checks = std::sync::atomic::AtomicUsize::new(0);
        let result = source
            .fetch_request_if(endpoint, &query(), || {
                checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0
            })
            .await;
        assert!(result.is_none());
        assert_eq!(server.seen().len(), 3);
        assert_eq!(checks.load(std::sync::atomic::Ordering::SeqCst), 2);
    })
}

#[test]
fn bar_capture_reads_stock_and_benchmark_with_explicit_dates() {
    hermetic::run(async {
        use crate::{archive::Knobs, fake_clock::FakeClock, runner};
        let clock = FakeClock::at(runner::tue_start());
        clock.advance(60_000);
        let server = FakeServer::start_capture(|s| runner::ok_reply(s, &[])).await;
        let a = runner::archive(
            &server.url,
            &Knobs::default(),
            research_archive::clock::Clock::now(&*clock),
        );
        let outcome = runner::capture(&a, &clock).await;
        assert_eq!(outcome.kind, research_archive::state::ExitKind::Complete);
        let store = a.store().await;
        let e = Catalog::compiled().get("daily_candlesticks").unwrap().id;
        for sym in ["AAPL.US", "SOXX.US"] {
            let row = store
                .read_at(
                    &crate::support::key(sym),
                    e,
                    research_archive::clock::Clock::now(&*clock),
                )
                .await
                .unwrap()
                .unwrap();
            let body: Value = serde_json::from_slice(&row.body).unwrap();
            assert_eq!(body["symbol"], sym);
            assert_eq!(body["bars"][0]["trade_date"], "2026-03-03");
            assert_eq!(body["bars"][0]["close"], json!("11.123456789"));
            assert_eq!(row.as_of.to_string(), "2026-03-03");
        }
    })
}

#[test]
fn normalized_short_pages_do_not_skip_new_companies_or_rewrite_dates() {
    hermetic::run(async {
        let server=FakeServer::start(|s| {
            let q=url::form_urlencoded::parse(s.query.as_deref().unwrap().as_bytes()).collect::<std::collections::BTreeMap<_,_>>();
            let offset=q.get("offset").unwrap().parse::<usize>().unwrap();
            let infos=match offset {
                0|1=>json!([{"id":"a","symbol":"MSFT.US","market":"US","datetime":"1792096200","ext":{"local_date":"2026-10-20"}}]),
                2=>json!([]),
                3=>json!([{"id":"b","symbol":"JPM.US","market":"US"}]),
                _=>panic!("unexpected offset"),
            };
            Reply::data(&json!({"list":[{"date":"2026-10-21","count":4,"infos":infos}],"next_date":"2026-10-22"}).to_string())
        }).await;
        let source = source(&server.url, OAuth::from_token("ap_m_FAKE"));
        let endpoint = *Catalog::compiled().get("finance_calendar").unwrap();
        let result = source
            .fetch_request_if(&endpoint, &query(), || true)
            .await
            .unwrap();
        let Response::Body(raw) = result.response() else {
            panic!("published window expected")
        };
        let body: Value = serde_json::from_slice(raw).unwrap();
        assert_eq!(server.seen().len(), 4);
        assert_eq!(body["company_counts"]["2026-10-21"], 2);
        let event = &body["days"]["2026-10-21"]["MSFT.US"]["a"];
        assert_eq!(event["ext"]["local_date"], "2026-10-20");
        assert_eq!(event["datetime"], "1792096200");
        assert_eq!(body["days"]["2026-10-21"]["JPM.US"]["b"]["id"], "b");
        assert_eq!(body["pages"][2]["offset"], 2);
        assert_eq!(body["reported_counts"]["2026-10-21"], 4);
    })
}

#[test]
fn calendar_crosses_a_hundred_company_boundary() {
    hermetic::run(async {
        let server=FakeServer::start(|s| {
            let q=url::form_urlencoded::parse(s.query.as_deref().unwrap().as_bytes()).collect::<std::collections::BTreeMap<_,_>>();
            let offset=q.get("offset").unwrap().parse::<usize>().unwrap();
            assert!(offset==0 || offset==100);
            let infos:Vec<_>=(offset..(offset+100).min(181)).map(|i|json!({"id":i.to_string(),"symbol":format!("SYM{i}.US"),"market":"US"})).collect();
            Reply::data(&json!({"list":[{"date":"2026-10-21","count":181,"infos":infos}],"next_date":"2026-10-22"}).to_string())
        }).await;
        let source = source(&server.url, OAuth::from_token("ap_m_FAKE"));
        let endpoint = *Catalog::compiled().get("finance_calendar").unwrap();
        let result = source
            .fetch_request_if(&endpoint, &query(), || true)
            .await
            .unwrap();
        let Response::Body(raw) = result.response() else {
            panic!("published window expected")
        };
        let body: Value = serde_json::from_slice(raw).unwrap();
        assert_eq!(server.seen().len(), 2);
        assert_eq!(body["company_counts"]["2026-10-21"], 181);
        assert_eq!(body["days"]["2026-10-21"].as_object().unwrap().len(), 181);
        assert_eq!(body["pages"][1]["offset"], 100);
    })
}
