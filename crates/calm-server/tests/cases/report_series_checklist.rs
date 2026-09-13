//! #1628 S2 — the reply checklist at the kernel boundary (design §6 A12),
//! driven through `resolve` so each rejection is observed as the stored
//! `unavailable` row and its reason.
//!
//! Dates used throughout (all 2026): 09-07 Mon, 09-10 Thu, 09-11 Fri,
//! 09-13 Sun, 09-14 Mon. `T0` is Monday 09-14 12:00Z, so a live cutoff is
//! Sunday 09-13.

#![cfg(unix)]

use calm_server::report_series::ResolveOutcome;
use serde_json::{Value, json};

use crate::report_series_fixture::{FixtureOptions, SOURCE, SeriesFixture, midnight_ms, ok_series};

fn unavailable() -> ResolveOutcome {
    ResolveOutcome::Wrote {
        status: "unavailable".into(),
        pinned: false,
    }
}

fn ok(pinned: bool) -> ResolveOutcome {
    ResolveOutcome::Wrote {
        status: "ok".into(),
        pinned,
    }
}

fn frozen(period: &str, as_of: &str) -> Value {
    json!({ "source": SOURCE, "series": ["US:NVDA"], "range": "3M", "period": period, "as_of": as_of })
}

fn live(period: &str) -> Value {
    json!({ "source": SOURCE, "series": ["US:NVDA"], "range": "3M", "period": period })
}

/// Resolve `block` against `reply`; returns `(outcome, reason)`.
async fn verdict(fx: &SeriesFixture, block: Value, reply: Value) -> (ResolveOutcome, String) {
    let block_id = fx.write_series_block(block).await;
    fx.reply_structured(reply);
    let (_, outcomes) = fx.resolve_block(&block_id).await;
    let row = fx.row(&block_id).await.expect("a row lands either way");
    (outcomes[0].clone(), row.reason.unwrap_or_default())
}

#[tokio::test]
async fn ok_series_with_one_point_is_malformed() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let (outcome, reason) = verdict(
        &fx,
        frozen("day", "2026-09-10"),
        json!({ "series": [ok_series("US:NVDA", "2026-09-11", &[("2026-09-10", 1.0)])] }),
    )
    .await;
    assert_eq!(outcome, unavailable());
    assert!(reason.contains("1 point(s), need at least 2"), "{reason}");
}

#[tokio::test]
async fn reply_with_descending_timestamps_is_unavailable() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let (outcome, reason) = verdict(
        &fx,
        frozen("day", "2026-09-10"),
        json!({ "series": [
            ok_series("US:NVDA", "2026-09-11", &[("2026-09-10", 1.0), ("2026-09-09", 2.0)])
        ]}),
    )
    .await;
    assert_eq!(outcome, unavailable());
    assert!(reason.contains("is not after"), "{reason}");
}

/// A week starting Monday 09-07 ends Sunday 09-13, after a Thursday 09-10
/// cutoff — a half week, refused even though its start is inside the window.
#[tokio::test]
async fn partial_week_is_rejected_at_the_boundary() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let (outcome, reason) = verdict(
        &fx,
        frozen("week", "2026-09-10"),
        json!({ "series": [
            ok_series("US:NVDA", "2026-09-15", &[("2026-08-31", 1.0), ("2026-09-07", 2.0)])
        ]}),
    )
    .await;
    assert_eq!(outcome, unavailable());
    assert!(
        reason.contains("period ending 2026-09-13 is after the cutoff 2026-09-10"),
        "{reason}"
    );
}

/// Frozen daily: the last bar dated exactly `complete_through` has no later
/// bar proving it closed.
#[tokio::test]
async fn bar_at_complete_through_is_rejected() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let (outcome, reason) = verdict(
        &fx,
        frozen("day", "2026-09-11"),
        json!({ "series": [
            ok_series("US:NVDA", "2026-09-11", &[("2026-09-10", 1.0), ("2026-09-11", 2.0)])
        ]}),
    )
    .await;
    assert_eq!(outcome, unavailable());
    assert!(
        reason.contains("period ending 2026-09-11 is not before complete_through 2026-09-11"),
        "{reason}"
    );
}

/// Live daily is the one relaxed branch: a bar dated `complete_through`
/// (= yesterday) is accepted.
#[tokio::test]
async fn live_daily_bar_at_complete_through_is_accepted() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let (outcome, reason) = verdict(
        &fx,
        live("day"),
        json!({ "series": [
            ok_series("US:NVDA", "2026-09-13", &[("2026-09-11", 1.0), ("2026-09-13", 2.0)])
        ]}),
    )
    .await;
    assert_eq!(outcome, ok(false), "{reason}");
    assert_eq!(reason, "");
}

/// Live weekly stays strict: the week 09-07…09-13 ending on
/// `complete_through` 09-13 (= the live cutoff) is refused.
#[tokio::test]
async fn live_week_at_complete_through_is_rejected() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let (outcome, reason) = verdict(
        &fx,
        live("week"),
        json!({ "series": [
            ok_series("US:NVDA", "2026-09-13", &[("2026-08-31", 1.0), ("2026-09-07", 2.0)])
        ]}),
    )
    .await;
    assert_eq!(outcome, unavailable());
    assert!(
        reason.contains("period ending 2026-09-13 is not before complete_through 2026-09-13"),
        "{reason}"
    );
}

/// The remaining rejections, table-driven: each row is `(name, block,
/// reply, reason fragment)`.
#[tokio::test]
async fn reply_checklist_rejects_each_malformed_shape() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let two_good = |complete_through: &str| {
        ok_series(
            "US:NVDA",
            complete_through,
            &[("2026-09-09", 1.0), ("2026-09-10", 2.0)],
        )
    };
    let nine: Vec<Value> = (0..9)
        .map(|i| {
            ok_series(
                &format!("US:A{i}"),
                "2026-09-11",
                &[("2026-09-09", 1.0), ("2026-09-10", 2.0)],
            )
        })
        .collect();
    let mut over_ceiling = Vec::new();
    // 3M/day ceiling is 94; 95 daily bars back from the cutoff.
    for i in (0..95).rev() {
        let day = chrono::NaiveDate::from_ymd_opt(2026, 9, 10).unwrap() - chrono::Days::new(i);
        over_ceiling.push(json!([
            midnight_ms(&day.format("%Y-%m-%d").to_string()),
            1.0
        ]));
    }
    let cases: Vec<(&str, Value, Value, &str)> = vec![
        (
            "nine series for a one-series request",
            frozen("day", "2026-09-10"),
            json!({ "series": nine }),
            "reply has 9 series, request had 1",
        ),
        (
            "asset does not match",
            frozen("day", "2026-09-10"),
            json!({ "series": [ok_series("US:AMD", "2026-09-11", &[("2026-09-09", 1.0), ("2026-09-10", 2.0)])] }),
            "asset `US:AMD` does not match request `US:NVDA`",
        ),
        (
            "ts_ms not a UTC midnight",
            frozen("day", "2026-09-10"),
            json!({ "series": [{
                "asset": "US:NVDA", "status": "ok", "complete_through": "2026-09-11",
                "points": [[midnight_ms("2026-09-09"), 1.0], [midnight_ms("2026-09-10") + 1, 2.0]]
            }]}),
            "is not a UTC midnight",
        ),
        (
            "bar after as_of",
            frozen("day", "2026-09-10"),
            json!({ "series": [ok_series("US:NVDA", "2026-09-14", &[("2026-09-10", 1.0), ("2026-09-11", 2.0)])] }),
            "2026-09-11 is outside [2026-06-10, 2026-09-10]",
        ),
        (
            "bar before start",
            frozen("day", "2026-09-10"),
            json!({ "series": [ok_series("US:NVDA", "2026-09-11", &[("2026-06-09", 1.0), ("2026-09-10", 2.0)])] }),
            "2026-06-09 is outside [2026-06-10, 2026-09-10]",
        ),
        (
            "week not starting on a Monday",
            frozen("week", "2026-09-13"),
            json!({ "series": [ok_series("US:NVDA", "2026-09-15", &[("2026-08-24", 1.0), ("2026-09-01", 2.0)])] }),
            "week period 2026-09-01 does not start on a Monday",
        ),
        (
            "month not starting on the 1st",
            frozen("month", "2026-09-30"),
            json!({ "series": [ok_series("US:NVDA", "2026-10-02", &[("2026-07-01", 1.0), ("2026-08-02", 2.0)])] }),
            "month period 2026-08-02 does not start on the 1st",
        ),
        (
            "month ending after as_of",
            frozen("month", "2026-09-10"),
            json!({ "series": [ok_series("US:NVDA", "2026-10-02", &[("2026-07-01", 1.0), ("2026-09-01", 2.0)])] }),
            "period ending 2026-09-30 is after the cutoff 2026-09-10",
        ),
        (
            "non-finite value",
            frozen("day", "2026-09-10"),
            json!({ "series": [{
                "asset": "US:NVDA", "status": "ok", "complete_through": "2026-09-11",
                "points": [[midnight_ms("2026-09-09"), 1.0], [midnight_ms("2026-09-10"), null]]
            }]}),
            "not a finite number",
        ),
        (
            "too many points for the range",
            frozen("day", "2026-09-10"),
            json!({ "series": [{
                "asset": "US:NVDA", "status": "ok", "complete_through": "2026-09-11",
                "points": over_ceiling
            }]}),
            "95 points exceed the 94 ceiling for 3M/day",
        ),
        (
            "ok without complete_through",
            frozen("day", "2026-09-10"),
            json!({ "series": [{
                "asset": "US:NVDA", "status": "ok",
                "points": [[midnight_ms("2026-09-09"), 1.0], [midnight_ms("2026-09-10"), 2.0]]
            }]}),
            "ok without complete_through",
        ),
        (
            "wrong point width",
            frozen("day", "2026-09-10"),
            json!({ "series": [{
                "asset": "US:NVDA", "status": "ok", "complete_through": "2026-09-11",
                "points": [[midnight_ms("2026-09-09"), 1.0, 2.0], [midnight_ms("2026-09-10"), 2.0, 3.0]]
            }]}),
            "3 entries, expected 2",
        ),
        (
            "unknown status word",
            frozen("day", "2026-09-10"),
            json!({ "series": [{ "asset": "US:NVDA", "status": "partial", "reason": "x" }] }),
            "unknown status `partial`",
        ),
        (
            "non-ok without a reason",
            frozen("day", "2026-09-10"),
            json!({ "series": [{ "asset": "US:NVDA", "status": "unavailable" }] }),
            "`unavailable` without a reason",
        ),
    ];
    let mut reasons = Vec::new();
    for (name, block, reply, fragment) in cases {
        let (outcome, reason) = verdict(&fx, block, reply).await;
        assert_eq!(outcome, unavailable(), "{name}: {reason}");
        assert!(reason.contains(fragment), "{name}: got `{reason}`");
        reasons.push(reason);
    }
    let distinct: std::collections::BTreeSet<&String> = reasons.iter().collect();
    assert_eq!(
        distinct.len(),
        reasons.len(),
        "every rejection names its own cause"
    );

    // And the positive control: the same shape, well-formed, lands `ok`.
    let (outcome, reason) = verdict(
        &fx,
        frozen("day", "2026-09-10"),
        json!({ "series": [two_good("2026-09-11")] }),
    )
    .await;
    assert_eq!(outcome, ok(true), "{reason}");
}
