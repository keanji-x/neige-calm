//! The real `chart.series` resolver against the real invest App behind the plugin host: the
//! resolver's call shape (the Track, no caller) is admitted by `series_show`, and its reply passes
//! the kernel's checklist into a stored row.

use calm_server::report_series::{Enqueue, ResolveOutcome};

use super::*;
use crate::report_series_fixture::{FixtureOptions, SeriesFixture, seam_fixture};

const PLUGIN_DIR: &str = "invest";
const SOURCE: &str = "neige://plugin/invest/series_show";

/// `YYYY-MM-DD` of a UTC-midnight `ts_ms`.
fn ymd(ts_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ts_ms)
        .unwrap()
        .date_naive()
        .to_string()
}

#[tokio::test]
async fn chart_series_resolves_through_invest() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let app = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins")
        .join(PLUGIN_DIR)
        .canonicalize()
        .unwrap();
    // The kernel seam (#1628): the SDK transport lists daily bars on exactly the seam reply's US
    // dates, with the seam's probe, so invest must answer the seam's US entry.
    let seam = seam_fixture();
    let nvda = seam["reply"]["series"][0].clone();
    assert_eq!(nvda["asset"], "US:NVDA");
    let bars: Vec<Value> = nvda["points"]
        .as_array()
        .unwrap()
        .iter()
        .map(|point| {
            let close = point[1].to_string();
            json!([
                ymd(point[0].as_i64().unwrap()),
                "1",
                close,
                "0.5",
                close,
                "100"
            ])
        })
        .collect();
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("invest-broker.json"),
        json!({
            "snapshot": {
                "identity": {"account_no": "PAPER123", "account_channel": "lb_papertrading"},
                "cash_usd": "10000", "available_cash_usd": "10000", "positions": {}, "quotes": {},
                "session": {"calendar_date": "2026-09-30", "trading_day": true, "half_day": false,
                            "regular_open_at": "2026-09-30T13:30:00+00:00",
                            "regular_close_at": "2026-09-30T20:00:00+00:00"},
                "market_open": true, "orders": [], "fills": []
            },
            "series": {"NVDA.US": {"complete_through": nvda["complete_through"], "bars": bars}}
        })
        .to_string(),
    )
    .unwrap();
    let manifest_json: Value =
        serde_json::from_str(&recipe_slots::plugin_file(PLUGIN_DIR, "manifest.json")).unwrap();
    let manifest = Manifest::parse(&manifest_json.to_string()).unwrap();
    let id = manifest.id.clone();
    fx.boot
        .repo
        .plugin_install(NewPlugin {
            id: id.clone(),
            version: manifest.version.clone(),
            install_path: app.display().to_string(),
            manifest: manifest_json,
            enabled: true,
            user_config: json!({
                "account_no": "PAPER123", "broker_home": home.path().display().to_string(),
                "portfolio_track_id": fx.track_id(), "instrument_recipe_id": "recipe-instrument",
                "oauth_client_id": "fixture-client",
                "sdk_python_path": app.join("tests/broker_fixture.py").display().to_string(),
                "max_held": 4, "max_watched": 4, "max_weight_bps": 10000, "poll_seconds": 5
            }),
        })
        .await
        .unwrap();
    let guard = fx.plugin_host.try_lock_lifecycle(&id).unwrap();
    fx.plugin_host.registry_insert(&guard, manifest, Some(app));
    drop(guard);
    fx.plugin_host.spawn(&id).await.unwrap();
    wait_for_running(&fx.plugin_host, &id).await;
    // The App checks `deadline_ms` against its own wall clock, so the resolver's clock is real.
    fx.set_clock(now_ms());

    let mut block = seam["block"].clone();
    block["source"] = json!(SOURCE);
    let block_id = fx.write_series_block(block).await;
    let (enqueued, outcomes) = fx.resolve_block(&block_id).await;
    assert_eq!(enqueued, Enqueue::Queued);
    // The HK asset is outside the US-only contract, so the frozen row is not pinned.
    assert_eq!(
        outcomes,
        vec![ResolveOutcome::Wrote {
            status: "ok".into(),
            pinned: false
        }]
    );
    let row = fx.row(&block_id).await.expect("row");
    let data = row.data_json();
    assert_eq!(
        data["series"][0],
        json!({"asset": "US:NVDA", "status": "ok", "points": nvda["points"]}),
        "{data}"
    );
    assert_eq!(data["series"][1]["asset"], "HK:9988", "{data}");
    assert_eq!(data["series"][1]["status"], "unknown_asset", "{data}");
    let summary = row.summary_json();
    assert_eq!(
        summary["series"][0], seam["resolved"]["series"][0],
        "{summary}"
    );
    // The resolver's request reached the SDK transport once: the SDK symbol over the window
    // with its 14-day margin.
    let calls: Vec<Value> = std::fs::read_to_string(home.path().join("invest-calls.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|call| call["method"] == "series")
        .map(|call| call["request"].clone())
        .collect();
    assert_eq!(
        calls,
        vec![json!({"symbols": ["NVDA.US"], "start": "2026-07-27", "end": "2026-09-10"})]
    );
    fx.plugin_host.stop(&id).await.unwrap();
}
