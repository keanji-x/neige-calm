//! Single-use, durable authorization for ephemeral stdin payloads.
use super::*;

#[async_trait]
pub trait ReleaseAuthority: Sync {
    async fn authorize_tx(&self, tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>) -> Result<()>;
}
pub struct NoAuthority;
#[async_trait]
impl ReleaseAuthority for NoAuthority {
    async fn authorize_tx(&self, _: &mut sqlx::Transaction<'_, sqlx::Sqlite>) -> Result<()> {
        Ok(())
    }
}

pub async fn send(
    pool: &SqliteDb,
    engine: &ExecutionEngine,
    session: &str,
    claimed: &Claim,
    payload: &[u8],
    authority: &dyn ReleaseAuthority,
) -> Result<()> {
    let id = execution_id(claimed.record())?;
    tokio::time::timeout(Duration::from_secs(10), async {
        wait_spawned(engine, &id).await?;
        let at = crate::model::now_ms();
        let mut tx = calm_truth::db::sqlite::begin_immediate_tx(pool).await?;
        let current = sql::load(&mut **tx, session)
            .await?
            .filter(|c| {
                c.record().execution_id == id.as_str()
                    && c.record().decision == Decision::Observing
                    && c.record().release == sql::Release::Held
            })
            .ok_or_else(|| conflict("release ownership lost"))?;
        if let Err(error) = authority.authorize_tx(&mut tx).await {
            let stopped =
                request_stop_tx(&mut tx, session, &current, StopReason::ReleaseRefused).await?;
            tx.commit().await?;
            crate::execution_quiesce::drive(Some(engine), stopped);
            return Err(error);
        }
        let sql::Cas::Applied(releasing) =
            sql::advance_release(&mut tx, session, &current, at).await?
        else {
            return Err(conflict("release ownership lost"));
        };
        tx.commit().await?; // authorization must precede the first byte
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point("c4-release-authorized", session).await;
        let mut offset = 0;
        while offset < payload.len() {
            let reply = engine
                .interact(
                    &id,
                    ProcessAction::WriteStdin {
                        bytes: payload[offset..].to_vec(),
                    },
                )
                .await
                .map_err(conflict)?;
            let ExecutionReply::StdinProgress { accepted_bytes } = reply else {
                return Err(conflict("unexpected release response"));
            };
            let accepted = accepted_bytes;
            if accepted > payload.len() - offset {
                return Err(conflict("invalid release write length"));
            }
            offset += accepted;
            if offset < payload.len() {
                tokio::task::yield_now().await;
            }
        }
        let mut tx = calm_truth::db::sqlite::begin_immediate_tx(pool).await?;
        let sql::Cas::Applied(_) = sql::advance_release(&mut tx, session, &releasing, at).await?
        else {
            return Err(conflict("release ownership lost"));
        };
        tx.commit().await?;
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point("c4-release-completed", session).await;
        Ok(())
    })
    .await
    .map_err(conflict)?
    // No retry after transport failure or cancellation. RAII/sweep sends Stop.
}

/// Wait for the Engine's Spawned receipt without releasing a single payload byte.
pub(crate) async fn wait_spawned(engine: &ExecutionEngine, id: &ExecutionId) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let generation = engine.output_generation().await.map_err(conflict)?;
            match engine.output(id, None).await {
                Ok(ExecutionReply::Observation { receipt: Some(_), state: ExecutionState::Spawned, .. }) => break,
                Ok(ExecutionReply::Observation { state: ExecutionState::Exited { .. } | ExecutionState::Drained { .. } | ExecutionState::OriginalGroupEmpty { .. } | ExecutionState::InterruptedQuiescent { .. } | ExecutionState::NoChild { .. } | ExecutionState::Revoked, .. }) => return Err(conflict("provider exited before release")),
                Err(DriveError::Executor(ExecError::Refused(_))) => {},
                Err(error) => return Err(conflict(error)),
                _ => {},
            }
            // Receipt publication need not append stdout or change executor generation.
            tokio::select! { _ = generation.changed() => {}, _ = tokio::time::sleep(Duration::from_millis(10)) => {} }
        }
        Ok(())
    }).await.map_err(conflict)?
}
