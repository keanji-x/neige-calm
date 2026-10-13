//! What `last_exit.json` keeps across endings: a dangling run ended at the
//! start stays on record even when a later step fails, and a completed run
//! clears the alert's dedupe state.

use research_archive::calendar::TradingCalendar;
use research_archive::clock::Clock;
use research_archive::state::ExitKind;
use research_archive::store::PlanRequest;
use research_archive::types::{AsOf, Key, Source, UnixMs};

use crate::archive::{Knobs, t};
use crate::fake::Reply;
use crate::fake_clock::{FakeClock, TimedServer};
use crate::hermetic;
use crate::runner::{DAYS, archive, capture, run_end, tue_start};

/// Tuesday's run is left open by its attempt timeout. The token is then
/// lost, and Wednesday's start ends Tuesday's run `partial` and exits
/// `AUTH_UNAVAILABLE` (the token would need the browser flow): the exit
/// state names the auth failure and still records the dangling run with its
/// most frequent failure.
#[test]
fn a_dangling_run_stays_on_record_when_a_later_step_fails() {
    hermetic::run(async {
        let clock = FakeClock::at(UnixMs(tue_start().0 + 60_000));
        let server = TimedServer::start(clock.clone(), |s, _| match s.path.as_str() {
            "/v1/quote/valuation" => Reply::new(503, "<html>busy</html>"),
            _ => Reply::data(r#"{"v":1}"#),
        })
        .await;
        let knobs = Knobs {
            attempt_cap: 100,
            attempt_timeout_minutes: 1,
            ..Knobs::default()
        };
        let a = archive(&server.server.url, &knobs, clock.now());
        assert_eq!(capture(&a, &clock).await.kind, ExitKind::Resumable);

        std::fs::remove_file(a.data_dir.join("oauth/test-client.json")).unwrap();
        clock.set(t("2026-03-04T23:00:00Z"));
        a.calendar(clock.now(), "2026-02-26", "2026-03-12", DAYS);
        let outcome = capture(&a, &clock).await;
        assert_eq!(outcome.kind, ExitKind::AuthUnavailable, "{outcome:?}");
        let (end, reason, _) = run_end(&a, 1).await.unwrap();
        assert_eq!(
            (end.as_str(), reason.as_deref()),
            ("partial", Some("dangling"))
        );
        let state = a.last_exit();
        assert_eq!(state["kind"], "auth_unavailable");
        assert_eq!(state["code"], 75);
        assert_eq!(state["as_of"], "2026-03-04");
        assert_eq!(
            state["closed_dangling"],
            serde_json::json!([{"as_of": "2026-03-03", "failure": "http_status:503"}])
        );
    })
}

/// A dangling run ended at a start with nothing else to do: `partial`, with
/// that run's most frequent failure.
#[test]
fn a_dangling_only_partial_carries_its_failure() {
    hermetic::run(async {
        let clock = FakeClock::at(UnixMs(tue_start().0 + 60_000));
        let server = TimedServer::start(clock.clone(), |s, _| match s.path.as_str() {
            "/v1/quote/valuation" => Reply::new(503, "<html>busy</html>"),
            _ => Reply::data(r#"{"v":1}"#),
        })
        .await;
        let knobs = Knobs {
            attempt_cap: 100,
            attempt_timeout_minutes: 1,
            ..Knobs::default()
        };
        let a = archive(&server.server.url, &knobs, clock.now());
        assert_eq!(capture(&a, &clock).await.kind, ExitKind::Resumable);
        clock.set(t("2026-03-04T15:00:00Z"));
        a.calendar(clock.now(), "2026-02-26", "2026-03-12", DAYS);
        let outcome = capture(&a, &clock).await;
        assert_eq!(outcome.kind, ExitKind::Partial, "{outcome:?}");
        let state = a.last_exit();
        assert_eq!(state["as_of"], "2026-03-03");
        assert_eq!(state["failure"], "http_status:503");
        assert_eq!(state["closed_dangling"][0]["as_of"], "2026-03-03");
    })
}

/// A run that completes clears what the alert last sent, so a failure after
/// the recovery alerts at once, whatever its kind. Other endings keep it.
#[test]
fn a_completed_run_clears_the_alert_dedupe() {
    hermetic::run(async {
        let clock = FakeClock::at(UnixMs(tue_start().0 + 60_000));
        let server = TimedServer::start(clock.clone(), |_, _| Reply::data(r#"{"v":1}"#)).await;
        let a = archive(&server.server.url, &Knobs::default(), clock.now());
        let sent = a.data_dir.join("notify_state.json");
        std::fs::create_dir_all(&a.data_dir).unwrap();
        std::fs::write(&sent, r#"{"kind":"auth_unavailable","at":1}"#).unwrap();
        clock.set(t("2026-03-03T15:00:00Z"));
        a.calendar(clock.now(), "2026-02-26", "2026-03-12", DAYS);
        assert_eq!(capture(&a, &clock).await.kind, ExitKind::NoWindow);
        assert!(sent.exists());
        clock.set(UnixMs(tue_start().0 + 60_000));
        assert_eq!(capture(&a, &clock).await.kind, ExitKind::Complete);
        assert!(!sent.exists());
    })
}

/// A start that works the run and leaves it open for the next one
/// (`resumable`) is working again too: it clears the dedupe.
#[test]
fn a_resumable_start_clears_the_alert_dedupe() {
    hermetic::run(async {
        let clock = FakeClock::at(UnixMs(tue_start().0 + 60_000));
        let server = TimedServer::start(clock.clone(), |s, _| match s.path.as_str() {
            "/v1/quote/valuation" => Reply::new(503, "<html>busy</html>"),
            _ => Reply::data(r#"{"v":1}"#),
        })
        .await;
        let knobs = Knobs {
            attempt_cap: 100,
            attempt_timeout_minutes: 1,
            ..Knobs::default()
        };
        let a = archive(&server.server.url, &knobs, clock.now());
        let sent = a.data_dir.join("notify_state.json");
        std::fs::create_dir_all(&a.data_dir).unwrap();
        std::fs::write(&sent, r#"{"kind":"auth_unavailable","at":1}"#).unwrap();
        assert_eq!(capture(&a, &clock).await.kind, ExitKind::Resumable);
        assert!(!sent.exists());
    })
}

/// Two runs left open (planned, never worked) are both ended `partial` by
/// the next start, and both are on record, oldest first; the start exits
/// `PARTIAL` for the latest.
#[test]
fn every_dangling_run_is_on_record() {
    hermetic::run(async {
        let clock = FakeClock::at(t("2026-03-04T15:00:00Z"));
        let server = TimedServer::start(clock.clone(), |_, _| Reply::data(r#"{"v":1}"#)).await;
        let a = archive(&server.server.url, &Knobs::default(), clock.now());
        let calendar = TradingCalendar::load(&a.data_dir).unwrap().unwrap();
        let store = a.store().await;
        for (day, started) in [
            ("2026-03-02", "2026-03-02T23:00:00Z"),
            ("2026-03-03", "2026-03-03T23:00:00Z"),
        ] {
            let date = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").unwrap();
            let window_start = chrono::NaiveTime::from_hms_opt(17, 30, 0).unwrap();
            store
                .plan_run(&PlanRequest {
                    source: Source::Longbridge,
                    as_of: AsOf::new(date).unwrap(),
                    window: calendar.window_of(date, window_start).unwrap(),
                    started_at: t(started),
                    config: vec![Key::new("AAPL.US").unwrap()],
                    indexes: vec![],
                })
                .await
                .unwrap();
        }
        store.close().await;
        let outcome = capture(&a, &clock).await;
        assert_eq!(outcome.kind, ExitKind::Partial, "{outcome:?}");
        let state = a.last_exit();
        assert_eq!(state["as_of"], "2026-03-03");
        assert_eq!(
            state["closed_dangling"],
            serde_json::json!([{"as_of": "2026-03-02"}, {"as_of": "2026-03-03"}])
        );
        let (end, ..) = run_end(&a, 1).await.unwrap();
        assert_eq!(end, "partial");
        let (end, ..) = run_end(&a, 2).await.unwrap();
        assert_eq!(end, "partial");
    })
}

/// A lock that cannot be taken for an I/O reason (here `archive.lock` is a
/// directory) is `FATAL`, and the exit state says so, so the alert never
/// reads an older start's state.
#[test]
fn a_lock_io_error_is_fatal_and_on_record() {
    hermetic::run(async {
        let clock = FakeClock::at(UnixMs(tue_start().0 + 60_000));
        let server = TimedServer::start(clock.clone(), |_, _| Reply::data(r#"{"v":1}"#)).await;
        let a = archive(&server.server.url, &Knobs::default(), clock.now());
        std::fs::write(
            a.data_dir.join("last_exit.json"),
            r#"{"code":0,"kind":"complete","at":1}"#,
        )
        .unwrap();
        std::fs::create_dir(a.data_dir.join("archive.lock")).unwrap();
        let outcome = capture(&a, &clock).await;
        assert_eq!(outcome.kind, ExitKind::Fatal, "{outcome:?}");
        let state = a.last_exit();
        assert_eq!(
            (state["kind"].as_str(), state["code"].as_i64()),
            (Some("fatal"), Some(70))
        );
        assert!(server.arrivals().is_empty());
    })
}
