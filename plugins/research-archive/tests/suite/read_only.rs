//! `ReadOnlyStore`: the status plugin's open. It never creates the archive,
//! never changes it, refuses what `Store::open` refuses, and reads while
//! the capture's Store writes.

use research_archive::types::{MembershipMode, RunOutcome, Source};
use research_archive::{OpenError, ReadOnlyStore, Store};

use crate::support::{as_of, complete_day, dump, fresh, plan_request, populated, raw};

#[tokio::test]
async fn a_missing_archive_is_refused_and_not_created() {
    let dir = tempfile::tempdir().unwrap();
    let path = Store::path(dir.path());
    let err = ReadOnlyStore::open(&path).await.unwrap_err();
    assert!(matches!(err, OpenError::Sqlite(_)), "{err:?}");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn reading_leaves_the_archive_as_it_was() {
    let (_dir, path) = populated().await;
    let bytes = std::fs::read(&path).unwrap();
    let mut conn = raw(&path, false).await;
    let before = dump(&mut conn).await;
    drop(conn);

    let store = ReadOnlyStore::open(&path).await.unwrap();
    let runs = store.recent_runs(Source::Longbridge, 7).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].as_of, as_of(1));
    let end = runs[0].end.as_ref().unwrap();
    assert_eq!(end.outcome, RunOutcome::Partial);
    assert_eq!(end.reason.as_deref(), Some("window closed"));
    assert_eq!(runs[0].frozen.len(), 1);
    assert_eq!(runs[0].frozen[0].mode, MembershipMode::Observed);
    store.close().await;

    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let mut conn = raw(&path, false).await;
    assert_eq!(dump(&mut conn).await, before);
    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(mode, "wal");
}

#[tokio::test]
async fn refuses_what_the_capture_refuses() {
    let (_dir, path) = populated().await;
    let mut conn = raw(&path, false).await;
    sqlx::query("PRAGMA user_version = 99")
        .execute(&mut conn)
        .await
        .unwrap();
    drop(conn);
    let err = ReadOnlyStore::open(&path).await.unwrap_err();
    assert!(
        matches!(
            err,
            OpenError::UserVersion {
                found: 99,
                expected: 3
            }
        ),
        "{err:?}"
    );

    let (_dir, path) = populated().await;
    let mut conn = raw(&path, false).await;
    sqlx::query("DROP TRIGGER blob_no_delete")
        .execute(&mut conn)
        .await
        .unwrap();
    drop(conn);
    let err = ReadOnlyStore::open(&path).await.unwrap_err();
    assert!(matches!(err, OpenError::Schema(_)), "{err:?}");
}

#[tokio::test]
async fn reads_while_the_capture_writes() {
    let f = fresh().await;
    complete_day(
        &f.store,
        1,
        &["AAPL.US"],
        ".SPX.US",
        &["AAPL.US", "MSFT.US"],
    )
    .await;
    // The writer stays open, with an open run of day 2.
    f.store
        .plan_run(&plan_request(2, &["AAPL.US"], &[".SPX.US"]))
        .await
        .unwrap();
    let reader = ReadOnlyStore::open(&f.path).await.unwrap();
    let runs = reader.recent_runs(Source::Longbridge, 7).await.unwrap();
    assert_eq!(
        runs.iter().map(|r| r.as_of).collect::<Vec<_>>(),
        [as_of(2), as_of(1)]
    );
    assert!(runs[0].end.is_none() && runs[0].frozen.is_empty());
    assert_eq!(runs[1].end.as_ref().unwrap().outcome, RunOutcome::Complete);
    let limited = reader.recent_runs(Source::Longbridge, 1).await.unwrap();
    assert_eq!(limited.len(), 1);
    reader.close().await;
    // The writer still writes.
    complete_day(&f.store, 3, &["AAPL.US"], ".SPX.US", &["AAPL.US"]).await;
}
