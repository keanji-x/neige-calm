use super::*;
use std::future::{Future, poll_fn};
use std::task::Poll;
use tokio::time::timeout;

const LIMIT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn concurrent_and_cancelled_close_wait_for_the_real_memory_anchor_ack() {
    // Reuse the actual production role pools and their parsed memory namespace.
    // The extra native connection becomes this fixture's deliberately blocked anchor.
    let roles = SqliteDb::connect("sqlite::memory:").await.unwrap();
    let mut anchor = SqliteConnection::connect_with(&roles.read_pool().connect_options())
        .await
        .unwrap();
    sqlx::query("CREATE TABLE anchor_close_probe(value INTEGER)")
        .execute(&mut anchor)
        .await
        .unwrap();
    // Isolate anchor close; both ordinary role-pool close acknowledgements are complete.
    roles.close().await;
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut anchor)
        .await
        .unwrap();
    sqlx::query("INSERT INTO anchor_close_probe VALUES(1)")
        .execute(&mut anchor)
        .await
        .unwrap();
    let (entered, waiting) = std::sync::mpsc::sync_channel(1);
    let (release, released) = std::sync::mpsc::channel();
    {
        let mut handle = anchor.lock_handle().await.unwrap();
        handle.set_commit_hook(move || {
            let _ = entered.send(());
            let _ = released.recv();
            true
        });
    }
    let mut commit = Box::pin(sqlx::query("COMMIT").execute(&mut anchor));
    let commit_queued = poll_fn(|cx| Poll::Ready(commit.as_mut().poll(cx).is_pending())).await;
    let entered = tokio::task::spawn_blocking(move || waiting.recv_timeout(LIMIT)).await;
    drop(commit);
    let db = SqliteDb::new(
        roles.read_pool().clone(),
        roles.writer_pool().clone(),
        Some(anchor),
    );
    let clone = db.clone();
    let mut cancelled = Box::pin(db.close());
    let first_pending = poll_fn(|cx| Poll::Ready(cancelled.as_mut().poll(cx).is_pending())).await;
    drop(cancelled);
    let mut second = Box::pin(db.close());
    let second_state = poll_fn(|cx| Poll::Ready(second.as_mut().poll(cx))).await;
    let mut concurrent = Box::pin(clone.close());
    let concurrent_state = poll_fn(|cx| Poll::Ready(concurrent.as_mut().poll(cx))).await;
    let second_pending = second_state.is_pending();
    let concurrent_pending = concurrent_state.is_pending();
    // Always release/drain the real worker before any assertion can panic.
    let _ = release.send(());
    let drained = timeout(LIMIT, async {
        if second_pending {
            second.await;
        }
        if concurrent_pending {
            concurrent.await;
        }
    })
    .await;
    assert!(
        entered.unwrap().is_ok(),
        "native anchor COMMIT hook must be entered"
    );
    assert!(
        commit_queued && first_pending && second_pending && concurrent_pending,
        "every close caller must await the same native anchor close acknowledgement"
    );
    assert!(drained.is_ok());
    assert!(!db.has_memory_cache_anchor());
}
