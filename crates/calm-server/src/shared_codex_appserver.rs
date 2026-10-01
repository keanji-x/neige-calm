//! Compatibility exports for the managed native service and its DTOs.
pub(crate) use crate::operation::execution_manager::DeletionThreadSeals;
pub use crate::operation::execution_manager::{
    BackoffState, DaemonReadiness, FailureClass, HEAL_SLOW_RETRY_CEILING, NotificationFanout,
    ReplaceOutcome, ReplacePrecondition, SPAWN_ENV_PASSTHROUGH, SharedCodexAppServer,
    SharedDaemonRuntime, SharedDaemonState, SharedDaemonStatus, SharedThreadStartParams,
    ThreadConfig, TurnId, bounded_exponential_backoff, other_thread_id, thread_id_from_started,
};

#[cfg(any(test, feature = "fixtures"))]
pub use crate::operation::execution_manager::drop_spawned_child_guard_for_test;
#[cfg(feature = "fixtures")]
pub use crate::operation::execution_manager::{
    FakeSharedCodexAppServer, StartedThreadParam, SteeredTurnParam, TurnStartReturnHook,
};
