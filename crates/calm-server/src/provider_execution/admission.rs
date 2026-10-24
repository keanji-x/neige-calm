//! Caller-owned policy and intent construction share the admission writer transaction.
use super::*;

#[async_trait]
pub trait Admission: Send + Sync {
    async fn authorize_tx(&self, tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>) -> Result<()>;
    async fn intent_tx(
        self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    ) -> Result<ExecutionIntent>;
}

/// Planner preparation is process-free and already owns its ephemeral payload.
/// It imposes no additional admission policy.
pub struct PreparedIntent(pub ExecutionIntent);
#[async_trait]
impl Admission for PreparedIntent {
    async fn authorize_tx(&self, _: &mut sqlx::Transaction<'_, sqlx::Sqlite>) -> Result<()> {
        Ok(())
    }
    async fn intent_tx(
        self,
        _: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    ) -> Result<ExecutionIntent> {
        Ok(self.0)
    }
}
