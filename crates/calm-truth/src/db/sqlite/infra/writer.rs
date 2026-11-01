//! Scoped ownership of writer admission. Domain composition borrows a transaction;
//! only an operation root receives a session that can begin one.
use std::future::Future;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::panic::Location;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use sqlx::pool::PoolConnection;
use sqlx::{Sqlite, SqliteConnection, SqlitePool, Transaction};

use super::{ObservedTransaction, RetryPolicy, begin_observed};
use crate::error::{Result, TruthError};

type SessionState = Arc<Mutex<Option<Arc<AtomicBool>>>>;

#[derive(Clone)]
enum ScopeState {
    Operation(SessionState),
    Inherited(Arc<AtomicBool>),
}
#[derive(Clone)]
struct Scope {
    manager: Arc<()>,
    active: ScopeState,
}
impl Scope {
    fn active_token(&self) -> Option<Arc<AtomicBool>> {
        let token = match &self.active {
            ScopeState::Operation(state) => state.lock().expect("write scope state").clone(),
            ScopeState::Inherited(token) => Some(token.clone()),
        };
        token.filter(|token| token.load(Ordering::Acquire))
    }
}

tokio::task_local! {
    static WRITE_SCOPES: Vec<Scope>;
}

fn manager_active(manager: &Arc<()>) -> bool {
    WRITE_SCOPES
        .try_with(|scopes| {
            scopes
                .iter()
                .any(|scope| Arc::ptr_eq(&scope.manager, manager) && scope.active_token().is_some())
        })
        .unwrap_or(false)
}

/// A write operation attempted to reacquire its active manager through an alias.
#[derive(Debug, thiserror::Error)]
#[error("nested writer acquisition is forbidden; reuse the active transaction")]
pub struct NestedWrite;

// SQLx-only owners retain their error contract while still rejecting reentry
// before admission. Callers of the manager itself receive the typed error.
impl From<NestedWrite> for sqlx::Error {
    fn from(error: NestedWrite) -> Self {
        Self::Protocol(error.to_string())
    }
}

/// One manager per shared database handle, using the existing fair writer pool.
/// It cannot be cloned or constructed by domain callers.
pub struct WriteManager {
    pub(super) pool: SqlitePool,
    identity: Arc<()>,
}

/// Read-only diagnostics for resource-boundary tests; no pool capability escapes.
#[cfg(any(test, feature = "fixtures"))]
pub struct WriterPoolState {
    pub max_connections: u32,
    pub size: u32,
    pub idle: usize,
    pub closed: bool,
}

impl WriteManager {
    pub(super) fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            identity: Arc::new(()),
        }
    }

    /// Run an operation with an exclusively borrowable writer capability.
    /// The future scope covers all callback awaits, including cancellation.
    /// Separate sibling operation futures get separate scopes even on one task.
    ///
    /// ```no_run
    /// use calm_truth::{TruthError, db::sqlite::WriteManager};
    /// async fn write(manager: &WriteManager) -> Result<(), TruthError> {
    ///     manager.with_writer(|writer| Box::pin(async move {
    ///         let transaction = writer.begin_immediate().await?;
    ///         transaction.commit().await?;
    ///         Ok::<_, TruthError>(())
    ///     })).await??;
    ///     Ok(())
    /// }
    /// ```
    /// The managed transaction cannot escape the operation callback:
    /// ```compile_fail
    /// use calm_truth::db::sqlite::WriteManager;
    /// async fn escape(manager: &WriteManager) {
    ///     let transaction = manager.with_writer(|writer| Box::pin(async move {
    ///         writer.begin_immediate().await.unwrap()
    ///     })).await.unwrap();
    ///     transaction.commit().await.unwrap();
    /// }
    /// ```
    pub async fn with_writer<'m, R, F>(&'m self, f: F) -> std::result::Result<R, NestedWrite>
    where
        F: for<'s> FnOnce(&'s mut WriteSession<'m>) -> BoxFuture<'s, R>,
    {
        if manager_active(&self.identity) {
            return Err(NestedWrite);
        }
        let mut scopes = WRITE_SCOPES.try_with(Clone::clone).unwrap_or_default();
        let active: SessionState = Arc::new(Mutex::new(None));
        let scope = Scope {
            manager: self.identity.clone(),
            active: ScopeState::Operation(active.clone()),
        };
        let mut session = WriteSession {
            manager: self,
            active,
        };
        scopes.push(scope);
        Ok(WRITE_SCOPES
            .scope(scopes, async { f(&mut session).await })
            .await)
    }

    #[cfg(any(test, feature = "fixtures"))]
    pub fn pool_state_for_test(&self) -> WriterPoolState {
        WriterPoolState {
            max_connections: self.pool.options().get_max_connections(),
            size: self.pool.size(),
            idle: self.pool.num_idle(),
            closed: self.pool.is_closed(),
        }
    }

    /// Fault injection closes admission without exposing the underlying pool.
    #[cfg(any(test, feature = "fixtures"))]
    pub async fn close_for_test(&self) -> std::result::Result<(), NestedWrite> {
        if manager_active(&self.identity) {
            return Err(NestedWrite);
        }
        self.pool.close().await;
        Ok(())
    }

    /// Execute one mutation without exposing a reusable pool or connection.
    /// Decoding and SQL execution remain SQLx responsibilities.
    pub async fn execute<'m, 'q: 'm, E>(
        &'m self,
        query: E,
    ) -> Result<sqlx::sqlite::SqliteQueryResult>
    where
        E: sqlx::Execute<'q, Sqlite> + Send + 'q,
    {
        self.autocommit(move |connection| {
            Box::pin(async move { sqlx::Executor::execute(&mut **connection, query).await })
        })
        .await
    }

    /// One autocommit operation through the same scoped admission owner.
    pub async fn autocommit<'m, R, F>(&'m self, f: F) -> Result<R>
    where
        F: for<'c> FnOnce(&'c mut WriteConnection<'m>) -> BoxFuture<'c, sqlx::Result<R>>
            + Send
            + 'm,
    {
        self.with_writer(|session| {
            Box::pin(async move { session.autocommit(f).await.map_err(TruthError::from) })
        })
        .await?
    }
}

/// Capture the current operation context before spawning supported child work.
/// Only owned child work can inherit; a borrowed session must stay in its root.
/// Tokio does not inherit task-local values. A child with this context rejects
/// a writer dependency on its still-active parent instead of queueing behind it.
/// Independent operation roots should not inherit another operation's context.
pub fn inherit_write_scope<F: Future + 'static>(future: F) -> impl Future<Output = F::Output> {
    // Capture this lease, not a session that might begin unrelated transactions later.
    let scopes = WRITE_SCOPES
        .try_with(|scopes| {
            scopes
                .iter()
                .filter_map(|scope| {
                    scope.active_token().map(|token| Scope {
                        manager: scope.manager.clone(),
                        active: ScopeState::Inherited(token),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    async move { WRITE_SCOPES.scope(scopes, future).await }
}

/// An operation-scoped capability. No public constructor or Clone exists.
///
/// A transaction keeps the session mutably borrowed until it is finalized:
/// ```compile_fail
/// use calm_truth::db::sqlite::WriteSession;
/// async fn overlap(session: &mut WriteSession<'_>) {
///     let _first = session.begin_immediate().await.unwrap();
///     let second = session.begin_immediate().await.unwrap();
///     second.commit().await.unwrap();
/// }
/// ```
pub struct WriteSession<'m> {
    manager: &'m WriteManager,
    active: SessionState,
}

impl<'m> WriteSession<'m> {
    /// Preserve bounded BEGIN retry ownership and original caller attribution.
    #[track_caller]
    pub fn begin_immediate(&mut self) -> impl Future<Output = Result<WriteTx<'_>>> + Send + '_ {
        let caller = Location::caller();
        async move {
            self.begin_transaction(caller, RetryPolicy::Bounded)
                .await
                .map_err(Into::into)
        }
    }

    /// One BEGIN attempt; capture/replay owners retain their retry policy.
    #[track_caller]
    pub fn begin_immediate_once(
        &mut self,
    ) -> impl Future<Output = sqlx::Result<WriteTx<'_>>> + Send + '_ {
        let caller = Location::caller();
        async move {
            self.begin_transaction(caller, RetryPolicy::Once)
                .await
                .map_err(Into::into)
        }
    }

    async fn begin_transaction(
        &mut self,
        caller: &'static Location<'static>,
        retry: RetryPolicy,
    ) -> std::result::Result<WriteTx<'_>, BeginError> {
        let active = self.activate()?;
        let transaction = begin_observed(&self.manager.pool, caller, retry).await?;
        Ok(WriteTx {
            transaction: Some(transaction),
            _active: active,
            _session: PhantomData,
        })
    }

    fn activate(&self) -> std::result::Result<Active, NestedWrite> {
        if manager_active(&self.manager.identity) {
            return Err(NestedWrite);
        }
        Ok(Active::new(self.active.clone()))
    }

    /// Exercise real connection recovery while keeping admission and reentry fences.
    #[cfg(any(test, feature = "fixtures"))]
    pub async fn retire_connection_for_test(&mut self) -> sqlx::Result<()> {
        let _active = self.activate().map_err(sqlx::Error::from)?;
        let connection = self.manager.pool.acquire().await?;
        sqlx::Connection::close(connection.detach()).await
    }

    /// Autocommit writes (including VACUUM) share admission and alias enforcement
    /// without introducing a transaction around statements that forbid one.
    pub async fn autocommit<R, E, F>(&mut self, f: F) -> std::result::Result<R, E>
    where
        E: From<sqlx::Error> + From<NestedWrite>,
        F: for<'c> FnOnce(&'c mut WriteConnection<'m>) -> BoxFuture<'c, std::result::Result<R, E>>,
    {
        let _active = self.activate()?;
        let mut connection = WriteConnection {
            connection: self.manager.pool.acquire().await?,
            _manager: self.manager,
        };
        f(&mut connection).await
    }
}

/// A borrowed autocommit connection. Callers cannot obtain or clone its pool.
/// Use its SQLx dereference only for borrowed operations; taking or replacing
/// the underlying resource is unsupported (#3436).
pub struct WriteConnection<'m> {
    connection: PoolConnection<Sqlite>,
    _manager: &'m WriteManager,
}
impl Deref for WriteConnection<'_> {
    type Target = SqliteConnection;
    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}
impl DerefMut for WriteConnection<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.connection
    }
}

enum BeginError {
    Nested(NestedWrite),
    Sqlx(sqlx::Error),
}
impl From<NestedWrite> for BeginError {
    fn from(error: NestedWrite) -> Self {
        Self::Nested(error)
    }
}
impl From<sqlx::Error> for BeginError {
    fn from(error: sqlx::Error) -> Self {
        Self::Sqlx(error)
    }
}
impl From<BeginError> for TruthError {
    fn from(error: BeginError) -> Self {
        match error {
            BeginError::Nested(error) => error.into(),
            BeginError::Sqlx(error) => error.into(),
        }
    }
}
impl From<BeginError> for sqlx::Error {
    fn from(error: BeginError) -> Self {
        match error {
            BeginError::Nested(error) => error.into(),
            BeginError::Sqlx(error) => error,
        }
    }
}

struct Active {
    state: SessionState,
    token: Arc<AtomicBool>,
}
impl Active {
    fn new(state: SessionState) -> Self {
        let token = Arc::new(AtomicBool::new(true));
        *state.lock().expect("write scope state") = Some(token.clone());
        Self { state, token }
    }
}
impl Drop for Active {
    fn drop(&mut self) {
        self.token.store(false, Ordering::Release);
        *self.state.lock().expect("write scope state") = None;
    }
}

/// SQLx owns rollback/commit and its worker barrier. The borrow prevents a
/// second transaction or autocommit lease from this session while one is live.
/// Same-connection savepoints remain SQLx operations, not writer acquisitions.
/// Raw SQLx dereferences support borrowed domain helpers only. Extracting,
/// replacing or returning the underlying SQLx resource is unsupported (#3436).
pub struct WriteTx<'s> {
    // SQLx queues active-drop cleanup before releasing the operation marker.
    transaction: Option<ObservedTransaction<'static>>,
    _active: Active,
    _session: PhantomData<&'s mut ()>,
}
impl WriteTx<'_> {
    pub async fn commit(mut self) -> sqlx::Result<()> {
        self.transaction
            .take()
            .expect("transaction not finalized")
            .commit()
            .await
    }
    pub async fn rollback(mut self) -> sqlx::Result<()> {
        self.transaction
            .take()
            .expect("transaction not finalized")
            .rollback()
            .await
    }
}
impl Drop for WriteTx<'_> {
    fn drop(&mut self) {
        // SQLx queues rollback before Active releases this operation's marker.
        // Drop also keeps the session borrow live until destruction, even when
        // the caller never uses the transaction again (NLL must not end it early).
        drop(self.transaction.take());
    }
}
impl Deref for WriteTx<'_> {
    type Target = Transaction<'static, Sqlite>;
    fn deref(&self) -> &Self::Target {
        self.transaction
            .as_ref()
            .expect("transaction not finalized")
    }
}
impl DerefMut for WriteTx<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.transaction
            .as_mut()
            .expect("transaction not finalized")
    }
}

#[cfg(test)]
mod tests;

#[cfg(any(test, feature = "fixtures"))]
mod fixtures;
#[cfg(any(test, feature = "fixtures"))]
pub use fixtures::HeldWriter;
