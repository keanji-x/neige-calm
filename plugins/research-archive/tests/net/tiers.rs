//! Production capture entry-point regressions for Core health isolation.
use crate::archive::Knobs;
use crate::fake::Reply;
use crate::fake_clock::{FakeClock, TimedServer};
use crate::runner::{DAYS, archive, capture, rows, run_end, tue_end, tue_start};
use crate::{hermetic, support::key};
use research_archive::{
    clock::Clock,
    state::ExitKind,
    status::{self, Health, Policy},
    types::{RunId, UnixMs},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

fn supplemental(path: &str) -> bool {
    matches!(
        path,
        "/v1/quote/financials/earnings-snapshot"
            | "/v1/quote/financial-reports"
            | "/v1/quote/finance_calendar"
            | "/v1/socket/token"
    )
}
async fn health(a: &crate::archive::Archive, now: UnixMs) -> status::ArchiveStatus {
    status::read(
        &a.data_dir,
        now,
        Policy {
            stale_after_ms: 90 * 60_000,
        },
    )
    .await
}

#[test]
fn quota_and_auth_failures_in_supplemental_capture_do_not_alert() {
    hermetic::run(async {
        for code in [403, 401] {
            let clock = FakeClock::at(tue_start());
            let server = TimedServer::start(clock.clone(), move |s, _| {
                if supplemental(&s.path) {
                    Reply::new(code, "denied")
                } else {
                    Reply::data("{}")
                }
            })
            .await;
            let a = archive(
                &server.server.url,
                &Knobs {
                    attempt_cap: 1,
                    ..Knobs::default()
                },
                clock.now(),
            );
            let outcome = capture(&a, &clock).await;
            assert_eq!(outcome.kind, ExitKind::Complete, "{outcome:?}");
            assert_eq!(outcome.code(), 0);
            assert!(outcome.failure.is_none());
            let status = health(&a, clock.now()).await;
            assert_eq!(status.health, Health::Ok, "{:?}", status.findings);
            assert_eq!(status.runs[0].core_outcome, "complete");
            assert_eq!(status.runs[0].core.ok, status.runs[0].core.planned);
            assert!(status.runs[0].supplemental.failed > 0);
            let table = research_archive::plugin::view::table(&status);
            assert!(
                table["rows"][0]["supplemental"]
                    .as_str()
                    .unwrap()
                    .contains("failed")
            );
            calm_types::report_blocks::kinds::validate_inline_table_overlay(&table).unwrap();
        }
    })
}

#[test]
fn supplemental_timeout_resumes_without_refetching_core_and_dangling_does_not_alert() {
    hermetic::run(async {
        let clock = FakeClock::at(tue_start());
        let hang = Arc::new(AtomicBool::new(true));
        let flag = hang.clone();
        let server = TimedServer::start(clock.clone(), move |s, clock| {
            let reply = Reply::data("{}");
            if s.path == "/v1/quote/financial-reports" && flag.load(Ordering::SeqCst) {
                clock.advance(60_000);
                reply.after(Duration::from_secs(3600))
            } else {
                reply
            }
        })
        .await;
        let a = archive(
            &server.server.url,
            &Knobs {
                attempt_timeout_minutes: 1,
                ..Knobs::default()
            },
            clock.now(),
        );
        assert_eq!(capture(&a, &clock).await.kind, ExitKind::Complete);
        assert!(run_end(&a, 1).await.is_none());
        assert_eq!(health(&a, clock.now()).await.health, Health::Ok);
        let core_rows: Vec<_> = rows(&a)
            .await
            .into_iter()
            .filter(|r| {
                !matches!(
                    r.1.as_str(),
                    "daily_candlesticks"
                        | "finance_calendar"
                        | "earnings_snapshot"
                        | "financial_reports"
                )
            })
            .collect();
        assert_eq!(core_rows.len(), 7);
        // Even preflight auth failure on an aux-only resume cannot report Core failing.
        std::fs::remove_file(a.data_dir.join("oauth/test-client.json")).unwrap();
        assert_eq!(capture(&a, &clock).await.kind, ExitKind::Complete);
        assert_eq!(health(&a, clock.now()).await.health, Health::Ok);
        a.token("ap_m_RUNNER-TOKEN", None, 4_102_444_800);
        hang.store(false, Ordering::SeqCst);
        clock.advance(30 * 60_000);
        assert_eq!(capture(&a, &clock).await.kind, ExitKind::Complete);
        assert_eq!(run_end(&a, 1).await.unwrap().0, "complete");
        let after = rows(&a).await;
        for original in &core_rows {
            assert_eq!(
                after
                    .iter()
                    .filter(|r| r.0 == original.0 && r.1 == original.1)
                    .count(),
                1
            );
        }
        // A second day left with only auxiliary work is closed without a false partial alert.
        clock.set(crate::archive::t("2026-03-04T22:30:00Z"));
        hang.store(true, Ordering::SeqCst);
        a.calendar(clock.now(), "2026-02-26", "2026-03-12", DAYS);
        assert_eq!(capture(&a, &clock).await.kind, ExitKind::Complete);
        clock.set(crate::archive::t("2026-03-05T16:00:00Z"));
        a.calendar(clock.now(), "2026-02-26", "2026-03-12", DAYS);
        let closed = capture(&a, &clock).await;
        assert_eq!(closed.kind, ExitKind::NoWindow, "{closed:?}");
        assert!(closed.closed_dangling.is_empty());
        assert_eq!(health(&a, clock.now()).await.health, Health::Ok);
        let store = a.store().await;
        assert!(store.core_complete_at(RunId(2)).await.unwrap().is_some());
        assert!(
            store
                .ever_planned(
                    &key("QQQ.US"),
                    crate::support::endpoint("daily_candlesticks")
                )
                .await
                .unwrap()
        );
    })
}

#[test]
fn a_pending_core_retry_prevents_supplemental_requests() {
    hermetic::run(async {
        let clock = FakeClock::at(UnixMs(tue_end().0 - 90_000));
        let server = TimedServer::start(clock.clone(), |s, _| {
            if s.path == "/v1/quote/forecast-eps" {
                Reply::new(503, "busy")
            } else {
                Reply::data("{}")
            }
        })
        .await;
        let a = archive(
            &server.server.url,
            &Knobs {
                attempt_cap: 10,
                ..Knobs::default()
            },
            clock.now(),
        );
        assert_eq!(capture(&a, &clock).await.kind, ExitKind::Partial);
        assert!(!server.arrivals().iter().any(|r| supplemental(&r.path)));
        assert_eq!(health(&a, clock.now()).await.health, Health::Failing);
    })
}

#[test]
fn deployed_config_without_benchmarks_still_captures_core_and_stock_bars() {
    hermetic::run(async {
        let clock = FakeClock::at(tue_start());
        let server = TimedServer::start(clock.clone(), |_, _| Reply::data("{}")).await;
        let a = archive(&server.server.url, &Knobs::default(), clock.now());
        let text = std::fs::read_to_string(&a.config_path).unwrap();
        let legacy = text
            .lines()
            .filter(|line| !line.starts_with("benchmarks ="))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&a.config_path, legacy).unwrap();
        assert_eq!(capture(&a, &clock).await.kind, ExitKind::Complete);
        let store = a.store().await;
        let plan = store.plan(RunId(1)).await.unwrap();
        assert!(plan.benchmarks.is_empty());
        let coverage = store.coverage(plan.run_id).await.unwrap();
        assert!(
            coverage
                .iter()
                .filter(|p| p.tier == research_archive::catalog::CaptureTier::Core)
                .all(|p| p.has_ok)
        );
        assert!(coverage.iter().any(|p| p.key == key("AAPL.US")
            && p.endpoint.as_str() == "daily_candlesticks"
            && p.has_ok));
        assert!(!coverage.iter().any(|p| p.key == key("QQQ.US")));
    })
}
