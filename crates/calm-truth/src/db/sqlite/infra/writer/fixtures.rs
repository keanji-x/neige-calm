//! Scheduling controls for boundary tests. The held operation remains pinned
//! outside the runtime, so a runtime shutdown cannot release its writer early.
use super::*;
use crate::db::sqlite::SqliteDb;
use futures::future::{Either, select};
use tokio::sync::oneshot;

pub struct HeldWriter {
    operation: BoxFuture<'static, Result<()>>,
    release: oneshot::Sender<()>,
}
impl HeldWriter {
    /// Finish the real callback and wait for its connection to be returned.
    pub async fn release(self) -> Result<()> {
        let Self { operation, release } = self;
        let _ = release.send(());
        operation.await
    }
}

impl SqliteDb {
    /// Hold admission without an SQLite transaction. No pool, connection or
    /// session escapes the manager callback; this control can only release it.
    pub async fn hold_writer_for_test(&self) -> Result<HeldWriter> {
        if manager_active(&self.writer().identity) {
            return Err(NestedWrite.into());
        }
        let db = self.clone();
        let (entered, ready) = oneshot::channel();
        let (release, released) = oneshot::channel();
        let operation: BoxFuture<'static, Result<()>> = Box::pin(inherit_write_scope(async move {
            db.writer()
                .with_writer(|write_session| {
                    Box::pin(async move {
                        write_session
                            .autocommit(|_connection| {
                                Box::pin(async move {
                                    let _ = entered.send(());
                                    let _ = released.await;
                                    Ok::<_, TruthError>(())
                                })
                            })
                            .await
                    })
                })
                .await?
        }));
        match select(operation, ready).await {
            Either::Right((Ok(()), operation)) => Ok(HeldWriter { operation, release }),
            Either::Left((result, _)) => {
                result?;
                Err(TruthError::Internal(
                    "writer hold ended before admission".into(),
                ))
            }
            Either::Right((Err(_), operation)) => {
                operation.await?;
                Err(TruthError::Internal(
                    "writer hold ended before admission".into(),
                ))
            }
        }
    }
}
