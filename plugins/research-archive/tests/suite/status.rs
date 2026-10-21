//! `status::read`: the health rules, from a real archive and a real
//! `last_exit.json` in a data dir.

use std::path::Path;

use research_archive::Store;
use research_archive::plugin::view;
use research_archive::state::{ClosedDangling, ExitKind, LastExit};
use research_archive::status::{self, ArchiveStatus, Health, Policy};
use research_archive::types::{FailureClass, Finish, Source, UnixMs};

use crate::support::{
    as_of, at, complete_day, endpoint, failed, key, observe_and_freeze, ok, plan_request,
    populated, raw, window,
};

const MINUTE: i64 = 60_000;
const POLICY: Policy = Policy {
    stale_after_ms: 90 * MINUTE,
};
/// "Now" for every test: after the fixture days, so exits are recent.
const NOW: UnixMs = UnixMs(1_800_000_000_000);

fn exit(kind: ExitKind, at: i64) -> LastExit {
    LastExit {
        invocation: None,
        code: kind.code(),
        kind,
        at,
        as_of: Some("2026-01-02".into()),
        failure: None,
        closed_dangling: Vec::new(),
        missed_scan_incomplete: false,
    }
}

/// A data dir whose archive has day 1 complete (13 pairs: one index pair,
/// two securities × six endpoints).
async fn healthy_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&Store::path(dir.path())).await.unwrap();
    complete_day(&store, 1, &["AAPL.US"], ".SPX.US", &["AAPL.US", "MSFT.US"]).await;
    store.close().await;
    exit(ExitKind::Complete, NOW.0 - MINUTE).write(dir.path());
    dir
}

async fn read(dir: &Path) -> ArchiveStatus {
    status::read(dir, NOW, POLICY).await
}

fn texts(status: &ArchiveStatus) -> Vec<(Health, &str)> {
    status
        .findings
        .iter()
        .map(|f| (f.health, f.text.as_str()))
        .collect()
}

#[tokio::test]
async fn a_healthy_archive_is_ok() {
    let dir = healthy_dir().await;
    let status = read(dir.path()).await;
    assert_eq!(status.health, Health::Ok, "{:?}", status.findings);
    assert!(status.findings.is_empty());
    assert_eq!(status.checked_at, NOW.0);
    assert_eq!(status.last_exit.as_ref().unwrap().kind, ExitKind::Complete);
    assert_eq!(status.runs.len(), 1);
    let run = &status.runs[0];
    assert_eq!(run.as_of, "2026-01-02");
    assert_eq!(run.outcome, "complete");
    let bars = run
        .endpoints
        .iter()
        .find(|e| e.endpoint == "daily_candlesticks")
        .unwrap();
    assert_eq!(
        (bars.planned, bars.ok, bars.failed, bars.untried),
        (7, 7, 0, 0)
    );
    let market = run
        .endpoints
        .iter()
        .find(|e| e.endpoint == "finance_calendar")
        .unwrap();
    assert_eq!((market.planned, market.ok), (1, 1));
    assert_eq!(
        (
            run.pairs_planned,
            run.pairs_ok,
            run.pairs_failed,
            run.pairs_untried
        ),
        (27, 27, 0, 0)
    );
    assert_eq!(run.frozen.len(), 1);
    assert_eq!(
        (run.frozen[0].index.as_str(), run.frozen[0].mode),
        (".SPX.US", "observed")
    );
}

#[tokio::test]
async fn nothing_yet_is_failing_and_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let status = read(dir.path()).await;
    assert_eq!(status.health, Health::Failing);
    let found = texts(&status);
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(
        found[0].1.contains("last_exit.json is missing"),
        "{found:?}"
    );
    assert!(found[1].1.contains("archive does not exist"), "{found:?}");
    assert!(status.last_exit.is_none() && status.runs.is_empty());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn an_old_exit_means_the_timer_stopped() {
    let dir = healthy_dir().await;
    exit(ExitKind::NoWindow, NOW.0 - 90 * MINUTE).write(dir.path());
    assert_eq!(read(dir.path()).await.health, Health::Ok);

    exit(ExitKind::NoWindow, NOW.0 - 90 * MINUTE - 1).write(dir.path());
    let status = read(dir.path()).await;
    assert_eq!(status.health, Health::Failing);
    assert!(
        status.findings[0]
            .text
            .contains("the timer is not starting the capture"),
        "{:?}",
        status.findings
    );
}

/// Of the exits, only those the archive cannot show fail: no token, or a
/// fatal error. A day's loss is read from its run, never from the exit.
#[tokio::test]
async fn only_an_exit_the_rows_cannot_show_fails() {
    let dir = healthy_dir().await;
    let all = [
        ExitKind::Complete,
        ExitKind::CompleteWithFailures,
        ExitKind::Degraded,
        ExitKind::AlreadyEnded,
        ExitKind::Resumable,
        ExitKind::NoWindow,
        ExitKind::Busy,
        ExitKind::Partial,
        ExitKind::AuthUnavailable,
        ExitKind::Fatal,
    ];
    for kind in all {
        exit(kind, NOW.0 - MINUTE).write(dir.path());
        let status = read(dir.path()).await;
        let expected = match kind {
            ExitKind::AuthUnavailable | ExitKind::Fatal => Health::Failing,
            _ => Health::Ok,
        };
        assert_eq!(status.health, expected, "{kind:?}: {:?}", status.findings);
    }

    let mut auth = exit(ExitKind::AuthUnavailable, NOW.0 - MINUTE);
    auth.failure = Some("auth:invalid_access_token".into());
    auth.write(dir.path());
    assert_eq!(
        texts(&read(dir.path()).await),
        [(
            Health::Failing,
            "the last capture start ended auth_unavailable (exit 75), trading day 2026-01-02, \
             most frequent failure auth:invalid_access_token. The token is unusable: run \
             `research-archive auth login` on the host."
        )]
    );
}

/// Ends day `n` (`AAPL.US` configured, `.SPX.US` = {AAPL.US, MSFT.US}: 13
/// pairs) as `finish`, with the index pair and the first `oks` of the 12
/// security pairs `ok` and the rest failed.
async fn end_day(dir: &Path, n: i64, oks: usize, finish: Finish) {
    let store = Store::open(&Store::path(dir)).await.unwrap();
    let plan = store
        .plan_run(&plan_request(n, &["AAPL.US"], &[".SPX.US"]))
        .await
        .unwrap();
    observe_and_freeze(
        &store,
        &plan,
        at(n, 10),
        &[(".SPX.US", &["AAPL.US", "MSFT.US"])],
    )
    .await;
    let mut pairs = store.coverage(plan.run_id).await.unwrap();
    pairs.sort_by_key(|p| p.tier != research_archive::catalog::CaptureTier::Core);
    for (i, pair) in pairs.into_iter().filter(|p| !p.has_ok).enumerate() {
        let response = if i < oks {
            ok(b"{}")
        } else {
            failed(FailureClass::Permanent)
        };
        store
            .record_observation(
                plan.run_id,
                &pair.key,
                pair.endpoint,
                at(n, 20 + i as i64),
                &response,
            )
            .await
            .unwrap();
    }
    store
        .end_run(plan.run_id, at(n, 10_000), finish, None)
        .await
        .unwrap();
    store.close().await;
}

const LOST: &str = ". Data of that day is lost for good; check `journalctl --user -u \
                    research-archive-capture` for the cause";

#[tokio::test]
async fn a_degraded_day_fails_until_a_later_day_completes() {
    let dir = healthy_dir().await;
    // 4 of 27 pairs ok: below half.
    end_day(dir.path(), 2, 3, Finish::Complete).await;
    let degraded = "2026-01-03 ended degraded: only 4 of 15 pairs have an ok";
    // The capture's own exit, then later starts that overwrite it.
    for kind in [
        ExitKind::Degraded,
        ExitKind::NoWindow,
        ExitKind::AlreadyEnded,
    ] {
        exit(kind, NOW.0 - MINUTE).write(dir.path());
        let status = read(dir.path()).await;
        assert_eq!(
            texts(&status),
            [(Health::Failing, format!("{degraded}{LOST}").as_str())],
            "{kind:?}"
        );
    }
    // A later day ends complete: the loss stays on record, as attention.
    complete_day_n(dir.path(), 3).await;
    assert_eq!(
        texts(&read(dir.path()).await),
        [(Health::Attention, degraded)]
    );
}

#[tokio::test]
async fn a_partial_latest_day_fails_until_a_later_day_completes() {
    let dir = healthy_dir().await;
    end_day(dir.path(), 2, 2, Finish::Partial).await;
    exit(ExitKind::NoWindow, NOW.0 - MINUTE).write(dir.path());
    let partial = "2026-01-03 ended partial: 12 of 15 pairs have no ok";
    assert_eq!(
        texts(&read(dir.path()).await),
        [(Health::Failing, format!("{partial}{LOST}").as_str())]
    );
    // An open run is not judged: the partial day is still the latest ended.
    let store = Store::open(&Store::path(dir.path())).await.unwrap();
    store
        .plan_run(&plan_request(3, &["AAPL.US"], &[".SPX.US"]))
        .await
        .unwrap();
    store.close().await;
    assert_eq!(read(dir.path()).await.health, Health::Failing);

    complete_day_n(dir.path(), 4).await;
    let status = read(dir.path()).await;
    assert_eq!(texts(&status), [(Health::Attention, partial)]);
}

/// A host outage across two windows: the start after it closes day 2
/// dangling (`partial`, no ok) and marks day 3 missed. That is the loss the
/// plugin exists to report, so it fails until a later day ends complete.
#[tokio::test]
async fn an_outage_fails_until_a_later_day_completes() {
    let dir = healthy_dir().await;
    let store = Store::open(&Store::path(dir.path())).await.unwrap();
    store
        .plan_run(&plan_request(2, &["AAPL.US"], &[".SPX.US"]))
        .await
        .unwrap();
    store.close_dangling_runs(at(3, 0)).await.unwrap();
    store
        .record_missed(Source::Longbridge, as_of(3), window(3), at(4, 0))
        .await
        .unwrap();
    store.close().await;
    let partial = "2026-01-03 ended partial: 8 of 8 pairs have no ok";
    let missed = "2026-01-04 was missed: no capture ran during its window";

    let mut first = exit(ExitKind::Partial, NOW.0 - 2 * MINUTE);
    first.closed_dangling = vec![ClosedDangling {
        as_of: "2026-01-03".into(),
        failure: None,
    }];
    // The start that found the outage, then the next one.
    for last in [first, exit(ExitKind::NoWindow, NOW.0 - MINUTE)] {
        last.write(dir.path());
        assert_eq!(
            texts(&read(dir.path()).await),
            [
                (Health::Failing, format!("{partial}{LOST}").as_str()),
                (Health::Attention, missed),
            ],
            "{:?}",
            last.kind
        );
    }

    complete_day_n(dir.path(), 4).await;
    let status = read(dir.path()).await;
    assert_eq!(status.health, Health::Attention);
    assert_eq!(
        texts(&status),
        [(Health::Attention, missed), (Health::Attention, partial)]
    );
}

#[tokio::test]
async fn complete_with_failures_needs_attention_from_the_rows() {
    let dir = healthy_dir().await;
    end_day(dir.path(), 2, 13, Finish::Complete).await;
    // The exit says nothing of it: the rows do.
    exit(ExitKind::Complete, NOW.0 - MINUTE).write(dir.path());
    let status = read(dir.path()).await;
    assert_eq!(
        texts(&status),
        [(
            Health::Attention,
            "2026-01-03 ended complete with failures: 1 of 15 pairs used up their attempts \
             with no ok"
        )]
    );
    assert_eq!(
        (status.runs[0].pairs_ok, status.runs[0].pairs_failed),
        (14, 13)
    );
}

/// The findings and the table name no time that moves with every start, so
/// a Planner asking "when the reasons change" and the overlay's change-only
/// push see a change only when there is one.
#[tokio::test]
async fn the_status_does_not_move_with_the_exit_time() {
    let dir = healthy_dir().await;
    end_day(dir.path(), 2, 3, Finish::Complete).await;
    let mut seen = Vec::new();
    for at in [NOW.0 - MINUTE, NOW.0 - 2 * MINUTE] {
        let mut last = exit(ExitKind::AuthUnavailable, at);
        last.missed_scan_incomplete = true;
        last.write(dir.path());
        let status = read(dir.path()).await;
        assert_eq!(status.findings.len(), 3, "{:?}", status.findings);
        seen.push((status.findings.clone(), view::table(&status)));
    }
    assert_eq!(seen[0], seen[1]);
}

/// [`complete_day`] on a closed archive in `dir`.
async fn complete_day_n(dir: &Path, n: i64) {
    let store = Store::open(&Store::path(dir)).await.unwrap();
    complete_day(&store, n, &["AAPL.US"], ".SPX.US", &["AAPL.US", "MSFT.US"]).await;
    store.close().await;
}

#[tokio::test]
async fn an_incomplete_missed_scan_needs_attention() {
    let dir = healthy_dir().await;
    let mut last = exit(ExitKind::NoWindow, NOW.0 - MINUTE);
    last.missed_scan_incomplete = true;
    last.write(dir.path());
    let status = read(dir.path()).await;
    assert_eq!(status.health, Health::Attention);
    assert!(
        status.findings[0]
            .text
            .contains("missed days may not be marked")
    );
}

#[tokio::test]
async fn a_missed_day_does_not_demote_a_partial_one() {
    let dir = healthy_dir().await;
    let store = Store::open(&Store::path(dir.path())).await.unwrap();
    // Day 2 ends partial with one ok pair; day 3 is missed.
    let plan = store
        .plan_run(&plan_request(2, &["AAPL.US"], &[".SPX.US"]))
        .await
        .unwrap();
    store
        .record_observation(
            plan.run_id,
            &key("AAPL.US"),
            endpoint("consensus"),
            at(2, 5),
            &ok(b"{}"),
        )
        .await
        .unwrap();
    store
        .end_run(
            plan.run_id,
            at(2, 50),
            Finish::Partial,
            Some("window closed"),
        )
        .await
        .unwrap();
    store
        .record_missed(Source::Longbridge, as_of(3), window(3), at(4, 0))
        .await
        .unwrap();
    store.close().await;

    let status = read(dir.path()).await;
    assert_eq!(status.health, Health::Failing);
    assert_eq!(
        status
            .runs
            .iter()
            .map(|r| (r.as_of.as_str(), r.outcome))
            .collect::<Vec<_>>(),
        [
            ("2026-01-04", "missed"),
            ("2026-01-03", "partial"),
            ("2026-01-02", "complete")
        ]
    );
    // Before the freeze, day 2 plans the index pair and AAPL.US's six.
    let day2 = &status.runs[1];
    assert_eq!(
        (
            day2.pairs_planned,
            day2.pairs_ok,
            day2.pairs_failed,
            day2.pairs_untried
        ),
        (17, 1, 0, 16)
    );
    assert_eq!(
        texts(&status),
        [
            (
                Health::Failing,
                format!("2026-01-03 ended partial: 7 of 8 pairs have no ok{LOST}").as_str()
            ),
            (
                Health::Attention,
                "2026-01-04 was missed: no capture ran during its window"
            ),
        ]
    );
}

#[tokio::test]
async fn a_carried_universe_needs_attention() {
    let dir = healthy_dir().await;
    let store = Store::open(&Store::path(dir.path())).await.unwrap();
    // Day 2 freezes with no constituents observation: carried from day 1.
    let plan = store
        .plan_run(&plan_request(2, &["AAPL.US"], &[".SPX.US"]))
        .await
        .unwrap();
    store
        .freeze_membership(plan.run_id, at(2, 10))
        .await
        .unwrap();
    store.close().await;
    let status = read(dir.path()).await;
    assert_eq!(status.health, Health::Attention);
    assert_eq!(status.runs[0].outcome, "open");
    assert_eq!(
        texts(&status),
        [(
            Health::Attention,
            "2026-01-03: the membership of .SPX.US is carried, not observed"
        )]
    );
}

#[tokio::test]
async fn an_unreadable_archive_is_failing() {
    let (dir, path) = populated().await;
    exit(ExitKind::Complete, NOW.0 - MINUTE).write(dir.path());
    let mut conn = raw(&path, false).await;
    sqlx::query("PRAGMA user_version = 99")
        .execute(&mut conn)
        .await
        .unwrap();
    drop(conn);
    let status = read(dir.path()).await;
    assert_eq!(status.health, Health::Failing);
    assert!(
        status.findings[0]
            .text
            .starts_with("the archive cannot be read: archive user_version is 99"),
        "{:?}",
        status.findings
    );
    assert!(status.runs.is_empty());
}

#[tokio::test]
async fn supplemental_only_failures_do_not_change_core_health() {
    let dir = healthy_dir().await;
    let store = Store::open(&Store::path(dir.path())).await.unwrap();
    let plan = store
        .plan_run(&plan_request(2, &["AAPL.US"], &[]))
        .await
        .unwrap();
    store
        .freeze_membership(plan.run_id, at(2, 2))
        .await
        .unwrap();
    for pair in store.coverage(plan.run_id).await.unwrap() {
        let supplemental = matches!(
            pair.endpoint.as_str(),
            "earnings_snapshot" | "financial_reports" | "finance_calendar" | "daily_candlesticks"
        );
        let response = if supplemental {
            failed(FailureClass::Permanent)
        } else {
            ok(b"{}")
        };
        store
            .record_observation(plan.run_id, &pair.key, pair.endpoint, at(2, 3), &response)
            .await
            .unwrap();
    }
    store
        .end_run(plan.run_id, at(2, 4), Finish::Complete, None)
        .await
        .unwrap();
    store.close().await;
    let status = read(dir.path()).await;
    assert_eq!(
        status.health,
        Health::Ok,
        "auxiliary quota failures must stay in the report: {:?}",
        status.findings
    );
    assert!(status.runs[0].endpoints.iter().any(|e| e.failed > 0));
}
