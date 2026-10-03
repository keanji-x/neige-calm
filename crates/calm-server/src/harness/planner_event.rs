//! The run loop's one input (#1981 S4): what a Planner provider reports about its thread, in
//! neige's terms. Each provider produces it: the Codex arm maps the app-server's notifications in
//! `codex_events`, and a Claude session builds it from its CLI's stream.
//!
//! Item bodies and turn records stay JSON: they are neige's stored planner item schema, written
//! byte for byte into the transcript item rows and the turn-outcome rows.

use serde_json::Value;
use tokio::sync::broadcast::{self, error::RecvError};

use crate::harness::codex_events::CodexEvents;

/// One event about a Planner thread.
#[derive(Debug, Clone)]
pub struct PlannerEvent {
    /// The thread the event is about, `None` when the provider named none. The run loop acts only
    /// on an event whose thread equals its own, so `None` passes only while no thread is known.
    pub thread_id: Option<String>,
    pub kind: PlannerEventKind,
}

/// A closed set: a provider event outside it arrives as [`PlannerEventKind::Ignored`].
#[derive(Debug, Clone)]
pub enum PlannerEventKind {
    /// The thread was created or loaded.
    ThreadStarted,
    /// The thread failed; the turn it ran fails with it.
    ThreadSystemError,
    /// The thread has nothing running.
    ThreadIdle,
    TurnStarted {
        turn_id: String,
    },
    /// `turn` is the turn record stored as its outcome row (`id`, `status`, `error`, ...).
    TurnCompleted {
        turn: Value,
    },
    /// The turn ended at an interrupt without a completion.
    TurnAborted {
        turn_id: String,
    },
    /// `params` is the stored item envelope: `threadId`, `turnId`, `item` and its timestamp.
    Item {
        phase: ItemPhase,
        params: Value,
    },
    /// The running turn's whole plan, superseding the previous one; stored as is.
    PlanUpdated {
        params: Value,
    },
    /// A context-usage reading: `params.tokenUsage` carries `last`, `total` and `modelContextWindow`.
    TokenUsage {
        params: Value,
    },
    /// An approval request. A Planner runs with `approval_policy=never`: it is logged and dropped.
    Approval {
        method: String,
    },
    /// Nothing the run loop acts on, such as a streaming delta or a frame it does not model. It
    /// still passes the thread filter and, like every event, writes the snapshot.
    Ignored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemPhase {
    Started,
    Completed,
}

impl ItemPhase {
    /// The `method` an item row is stored and announced under.
    pub fn method(self) -> &'static str {
        match self {
            ItemPhase::Started => "item/started",
            ItemPhase::Completed => "item/completed",
        }
    }
}

/// One harness's subscription to its provider's events, with `broadcast`'s receive semantics:
/// `Lagged` when it fell behind, `Closed` once the provider's sender is gone.
pub struct PlannerEvents(Source);

enum Source {
    Codex(CodexEvents),
    Claude(broadcast::Receiver<PlannerEvent>),
}

impl From<CodexEvents> for PlannerEvents {
    fn from(events: CodexEvents) -> Self {
        Self(Source::Codex(events))
    }
}

impl From<broadcast::Receiver<PlannerEvent>> for PlannerEvents {
    fn from(events: broadcast::Receiver<PlannerEvent>) -> Self {
        Self(Source::Claude(events))
    }
}

impl PlannerEvents {
    /// Cancel safe, like the `broadcast::Receiver::recv` it wraps: the run loop selects on it.
    pub async fn recv(&mut self) -> Result<PlannerEvent, RecvError> {
        match &mut self.0 {
            Source::Codex(events) => events.recv().await,
            Source::Claude(events) => events.recv().await,
        }
    }
}
