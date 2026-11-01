use super::*;
use sqlx::sqlite::SqlitePoolOptions;
use std::future::poll_fn;
use std::time::Duration;
use tokio::sync::{Notify, oneshot};
use tokio::time::timeout;

const LIMIT: Duration = Duration::from_secs(5);

async fn manager() -> Arc<WriteManager> {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let manager = Arc::new(WriteManager::new(pool));
    manager
        .execute(sqlx::query("CREATE TABLE probe(value TEXT NOT NULL)"))
        .await
        .unwrap();
    manager
}

async fn values(manager: &WriteManager) -> Vec<String> {
    manager
        .autocommit(|connection| {
            Box::pin(async {
                sqlx::query_scalar("SELECT value FROM probe ORDER BY rowid")
                    .fetch_all(&mut **connection)
                    .await
            })
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn nested_alias_transaction_and_autocommit_reject_before_queueing() {
    let manager = manager().await;
    timeout(
        LIMIT,
        manager.with_writer(|session| {
            Box::pin(async {
                let tx = session.begin().await.unwrap();
                let nested = manager
                    .with_writer(|other| {
                        Box::pin(async {
                            other.begin().await.unwrap().commit().await.unwrap();
                        })
                    })
                    .await;
                assert!(matches!(nested, Err(NestedWrite)));
                let nested = manager
                    .execute(sqlx::query("INSERT INTO probe VALUES('nested')"))
                    .await;
                assert!(matches!(nested, Err(TruthError::NestedWrite(_))));
                tx.rollback().await.unwrap();
                // A completed transaction releases the marker even while the operation continues.
                manager
                    .execute(sqlx::query("INSERT INTO probe VALUES('after rollback')"))
                    .await
                    .unwrap();
            })
        }),
    )
    .await
    .expect("nested writes must reject before an acquisition timeout")
    .unwrap();
    assert_eq!(values(&manager).await, ["after rollback"]);
}

#[tokio::test]
async fn sibling_operation_futures_on_one_task_queue_without_false_reentry() {
    let manager = manager().await;
    let entered = Arc::new(Notify::new());
    let queued = Arc::new(Notify::new());
    let first = manager.with_writer(|session| {
        Box::pin(async {
            let mut tx = session.begin().await.unwrap();
            sqlx::query("INSERT INTO probe VALUES('first')")
                .execute(&mut **tx)
                .await
                .unwrap();
            entered.notify_one();
            queued.notified().await;
            tx.commit().await.unwrap();
        })
    });
    let second = async {
        entered.notified().await;
        manager
            .with_writer(|session| {
                Box::pin(async {
                    let begin = session.begin();
                    tokio::pin!(begin);
                    let mut notified = false;
                    let mut tx = poll_fn(|cx| {
                        let polled = begin.as_mut().poll(cx);
                        if !notified {
                            assert!(polled.is_pending(), "first operation must own the writer");
                            queued.notify_one();
                            notified = true;
                        }
                        polled
                    })
                    .await
                    .unwrap();
                    sqlx::query("INSERT INTO probe VALUES('second')")
                        .execute(&mut **tx)
                        .await
                        .unwrap();
                    tx.commit().await.unwrap();
                })
            })
            .await
            .unwrap();
    };
    let (first, ()) = timeout(LIMIT, async { tokio::join!(first, second) })
        .await
        .unwrap();
    first.unwrap();
    assert_eq!(values(&manager).await, ["first", "second"]);
}

#[tokio::test]
async fn propagated_child_writer_rejects_parent_dependency() {
    let manager = manager().await;
    timeout(
        LIMIT,
        manager.with_writer(|session| {
            Box::pin(async {
                let tx = session.begin().await.unwrap();
                let alias = manager.clone();
                let child = tokio::spawn(inherit_write_scope(async move {
                    alias
                        .execute(sqlx::query("INSERT INTO probe VALUES('child')"))
                        .await
                }));
                assert!(matches!(
                    child.await.unwrap(),
                    Err(TruthError::NestedWrite(_))
                ));
                tx.rollback().await.unwrap();
            })
        }),
    )
    .await
    .expect("an awaited child must reject instead of waiting on its parent")
    .unwrap();
    assert!(values(&manager).await.is_empty());
}

#[tokio::test]
async fn distinct_managers_can_write_within_one_operation() {
    let first = manager().await;
    let second = manager().await;
    first
        .with_writer(|session| {
            Box::pin(async {
                let tx = session.begin().await.unwrap();
                second
                    .execute(sqlx::query("INSERT INTO probe VALUES('independent')"))
                    .await
                    .unwrap();
                tx.rollback().await.unwrap();
            })
        })
        .await
        .unwrap();
    assert_eq!(values(&second).await, ["independent"]);
}

#[tokio::test]
async fn cancellation_rolls_back_and_releases_scope_and_admission() {
    let manager = manager().await;
    let alias = manager.clone();
    let (entered, reached) = oneshot::channel();
    let operation = tokio::spawn(async move {
        alias
            .with_writer(|session| {
                Box::pin(async move {
                    let mut tx = session.begin().await.unwrap();
                    sqlx::query("INSERT INTO probe VALUES('cancelled')")
                        .execute(&mut **tx)
                        .await
                        .unwrap();
                    entered.send(()).unwrap();
                    std::future::pending::<()>().await;
                    tx.commit().await.unwrap();
                })
            })
            .await
            .unwrap();
    });
    reached.await.unwrap();
    operation.abort();
    assert!(operation.await.unwrap_err().is_cancelled());
    timeout(
        LIMIT,
        manager.execute(sqlx::query("INSERT INTO probe VALUES('survivor')")),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(values(&manager).await, ["survivor"]);
}

#[tokio::test]
async fn active_drop_and_callback_error_preserve_rollback_and_session_reuse() {
    let manager = manager().await;
    let result: Result<()> = manager
        .with_writer(|session| {
            Box::pin(async {
                let mut tx = session.begin().await?;
                sqlx::query("INSERT INTO probe VALUES('dropped')")
                    .execute(&mut **tx)
                    .await?;
                drop(tx);
                let tx = session.begin_once().await?;
                tx.rollback().await?;
                Err(TruthError::Forbidden("fixture rejection".into()))
            })
        })
        .await
        .unwrap();
    assert!(matches!(result, Err(TruthError::Forbidden(_))));
    manager
        .execute(sqlx::query("INSERT INTO probe VALUES('after error')"))
        .await
        .unwrap();
    assert_eq!(values(&manager).await, ["after error"]);
}

#[tokio::test]
async fn session_from_an_earlier_root_cannot_begin_under_an_active_alias() {
    let manager = manager().await;
    timeout(
        LIMIT,
        manager.with_writer(|outer| {
            Box::pin(async {
                manager
                    .with_writer(|inner| {
                        Box::pin(async {
                            let tx = inner.begin().await.unwrap();
                            assert!(matches!(
                                outer.begin().await,
                                Err(TruthError::NestedWrite(_))
                            ));
                            tx.rollback().await.unwrap();
                        })
                    })
                    .await
                    .unwrap();
            })
        }),
    )
    .await
    .expect("preexisting sessions must reject an active alias before queueing")
    .unwrap();
}

#[tokio::test]
async fn inherited_context_does_not_follow_later_transactions_of_the_parent() {
    let manager = manager().await;
    timeout(
        LIMIT,
        manager.with_writer(|session| {
            Box::pin(async {
                let first = session.begin().await.unwrap();
                let alias = manager.clone();
                let child = inherit_write_scope(async move {
                    alias
                        .execute(sqlx::query(
                            "INSERT INTO probe VALUES('later independent root')",
                        ))
                        .await
                });
                first.rollback().await.unwrap();
                let second = session.begin().await.unwrap();
                tokio::pin!(child);
                assert!(
                    futures::poll!(child.as_mut()).is_pending(),
                    "the completed lease must not reject a child's independent root"
                );
                second.rollback().await.unwrap();
                child.await.unwrap();
            })
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(values(&manager).await, ["later independent root"]);
}

#[test]
fn held_writer_control_retains_admission_across_runtime_shutdown() {
    let original = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (db, held) = original.block_on(async {
        let db = crate::db::sqlite::SqliteDb::connect("sqlite::memory:")
            .await
            .unwrap();
        db.writer()
            .execute(sqlx::query("CREATE TABLE restart_probe(value TEXT)"))
            .await
            .unwrap();
        let held = db.hold_writer_for_test().await.unwrap();
        (db, held)
    });
    drop(original);
    let state = db.writer().pool_state_for_test();
    assert_eq!(
        (state.size, state.idle),
        (1, 0),
        "the writer must remain held after the old runtime exits"
    );
    let restarted = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    restarted.block_on(async {
        drop(held);
        timeout(
            LIMIT,
            db.writer().execute(sqlx::query(
                "INSERT INTO restart_probe VALUES('after restart')",
            )),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM restart_probe")
                .fetch_one(db.read_pool())
                .await
                .unwrap(),
            1
        );
        db.close().await;
    });
}
