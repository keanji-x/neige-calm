//! IO-free execution contracts shared by the kernel, providers, and truth layer.
//!
//! ```text
//!   calm-server / calm-provider / calm-truth
//!                        │
//!                   calm-exec
//!                        │
//!                   calm-types
//! ```

pub mod flow;
pub mod observation;
pub mod provider;
pub mod reaction;

pub use flow::{FlowRowCtx, WorkerFlowItemSink, WorkerFlowSource};
pub use observation::ObservationSink;
pub use provider::{SpawnCtx, SpawnHandle, WorkerProvider};
pub use reaction::{AgentReactor, DecisionIntent, DecisionSink};
