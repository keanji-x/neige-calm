//! What `last_exit.json` keeps across endings: a dangling run ended at the
//! start stays on record even when a later step fails, and a completed run
//! clears the alert's dedupe state.

use research_archive::clock::Clock;
use research_archive::state::ExitKind;
use research_archive::types::UnixMs;

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
        assert_eq!(state["closed_dangling"], "2026-03-03");
        assert_eq!(state["closed_dangling_failure"], "http_status:503");
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
        assert_eq!(state["closed_dangling"], "2026-03-03");
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
