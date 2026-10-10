//! IO-free execution contracts shared by the kernel, providers, and truth layer.

pub mod flow;
pub mod observation;
pub mod provider;
pub mod reaction;

pub use flow::{
    CaptureBatch, CaptureCheckpoint, CaptureOutcome, CapturePosition, FlowRowCtx,
    WorkerFlowItemSink, WorkerFlowSource,
};
pub use observation::ObservationSink;
pub use provider::{BoundKeys, SpawnCtx, SpawnHandle, TuiInput, WorkerProvider};
pub use reaction::{AgentReactor, DecisionIntent, DecisionSink};
