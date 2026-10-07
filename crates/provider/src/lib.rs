//! Agent provider contracts, protocol clients, and worker implementations.
pub mod acp;
pub mod claude;
pub mod codex;
pub mod events;
pub mod held_requests;
mod input;
pub mod worker;
pub use calm_exec::{SpawnCtx, SpawnHandle, WorkerProvider};
pub use calm_types::runtime::AgentProvider;
pub use input::{InputItem, TurnModelSelection};
pub use worker::{
    ClaudeProvider, CodexDaemonProbe, CodexLivenessFacts, CodexProvider, LastTurnFacts,
    TerminalProvider, ThreadStatusLite, TurnStatusLite,
};
