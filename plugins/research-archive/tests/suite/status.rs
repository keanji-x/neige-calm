//! `status::read`: the health rules, from a real archive and a real
//! `last_exit.json` in a data dir.

use std::path::Path;

use research_archive::Store;
use research_archive::state::{ClosedDangling, ExitKind, LastExit};
use research_archive::status::{self, ArchiveStatus, Health, Policy};
use research_archive::types::{Finish, Source, UnixMs};

use crate::support::{
    as_of, at, complete_day, endpoint, key, ok, plan_request, populated, raw, window,
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
    assert_eq!(
        (
            run.pairs_planned,
            run.pairs_ok,
            run.pairs_failed,
            run.pairs_untried
        ),
        (13, 13, 0, 0)
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

#[tokio::test]
async fn every_alerting_exit_fails_and_no_other_does() {
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
            ExitKind::Degraded
            | ExitKind::Partial
            | ExitKind::AuthUnavailable
            | ExitKind::Fatal => Health::Failing,
            ExitKind::CompleteWithFailures => Health::Attention,
            _ => Health::Ok,
        };
        assert_eq!(status.health, expected, "{kind:?}: {:?}", status.findings);
    }

    let mut auth = exit(ExitKind::AuthUnavailable, NOW.0 - MINUTE);
    auth.failure = Some("auth:invalid_access_token".into());
    auth.write(dir.path());
    let text = &read(dir.path()).await.findings[0].text;
    assert!(text.contains("ended auth_unavailable (exit 75)"), "{text}");
    assert!(
        text.contains("most frequent failure auth:invalid_access_token"),
        "{text}"
    );
    assert!(text.contains("research-archive auth login"), "{text}");

    let mut partial = exit(ExitKind::Partial, NOW.0 - MINUTE);
    partial.closed_dangling = vec![
        ClosedDangling {
            as_of: "2026-01-01".into(),
            failure: Some("http_status:503".into()),
        },
        ClosedDangling {
            as_of: "2026-01-02".into(),
            failure: None,
        },
    ];
    partial.write(dir.path());
    let text = &read(dir.path()).await.findings[0].text;
    assert!(
        text.contains("runs closed partial at start: 2026-01-01 (http_status:503), 2026-01-02"),
        "{text}"
    );
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
async fn lost_days_need_attention() {
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
    assert_eq!(status.health, Health::Attention);
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
        (7, 1, 0, 6)
    );
    assert_eq!(
        texts(&status),
        [
            (
                Health::Attention,
                "2026-01-04 was missed: no capture ran during its window"
            ),
            (
                Health::Attention,
                "2026-01-03 ended partial: 6 of 7 pairs have no ok"
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
    sqlx::query("PRAGMA user_version = 2")
        .execute(&mut conn)
        .await
        .unwrap();
    drop(conn);
    let status = read(dir.path()).await;
    assert_eq!(status.health, Health::Failing);
    assert!(
        status.findings[0]
            .text
            .starts_with("the archive cannot be read: archive user_version is 2"),
        "{:?}",
        status.findings
    );
    assert!(status.runs.is_empty());
}
