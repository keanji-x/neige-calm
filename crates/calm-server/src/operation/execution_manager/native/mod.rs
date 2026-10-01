//! Native control transport belongs exclusively to the execution manager.
mod inspection;
pub(super) mod supervisor;
pub(super) mod wire;
pub use inspection::CodexAppServer;
pub(crate) use supervisor::DeletionThreadSeals;
pub use supervisor::*;
pub(crate) use wire::redact_thread_start_config;
pub use wire::{
    AccountRead, ActivePermissionProfile, ClientInfo, CodexConfig, CodexModel,
    CodexReasoningEffortOption, ConfigReadResponse, InitializeResult, InputItem, ModelListPage,
    Notification, NotificationStream, PermissionsChoice, ThreadActiveFlag,
    ThreadLoadedListResponse, ThreadReadResponse, ThreadResult, ThreadStartParams, ThreadStatus,
    ThreadUnsubscribeResponse, ThreadView, TurnStartResult, TurnStatus, TurnSteerResult, TurnView,
};
#[cfg(any(feature = "fixtures", feature = "codex-e2e"))]
mod testing;
#[cfg(any(feature = "fixtures", feature = "codex-e2e"))]
pub use testing::TestingCodexAppServer;

mod session;
pub(crate) use session::{
    authorize_native_session_tx, native_session_presentation, session_preparation_tx,
    stop_managed_native_session,
};
