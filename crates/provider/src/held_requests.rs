//! What a provider adapter tells a Planner harness about the provider requests its running turn is
//! paused on (#2348). The harness owns the table these feed (`calm_server::harness::held_requests`);
//! this is the contract an adapter implements, the way [`crate::events`] is the contract for the
//! provider's events.
//!
//! An adapter pushes [`HeldRequestMessage`]s into the harness's one ordered, unbounded channel, in
//! the order it learns them, without blocking. It answers a request through the
//! [`HeldResponder`] it hands over, and refuses it when that responder is dropped unanswered.

use tokio::sync::mpsc;

use calm_types::event::AskQuestion;

/// One provider request, as its adapter names it; unique among the requests the adapter has open.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RequestKey(pub String);

/// One provider connection (a socket, a process): every request it carried ends when it does.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConnectionId(pub String);

/// Answers one paused provider request. `respond` hands over the index of the option the user
/// chose, which the adapter turns into its own wire answer. Dropping a responder without
/// responding refuses the request; that is the adapter's job, in its `Drop`.
pub trait HeldResponder: Send + 'static {
    fn respond(self: Box<Self>, option: usize);
}

/// What an adapter tells the harness about its paused requests, in the order it learns it.
pub enum HeldRequestMessage {
    /// A request the running turn waits on: the user is asked `questions`.
    Open {
        request_key: RequestKey,
        connection: ConnectionId,
        questions: Vec<AskQuestion>,
        responder: Box<dyn HeldResponder>,
    },
    /// The provider settled or cancelled the request itself.
    Gone { request_key: RequestKey },
    /// The connection ended; sent by a guard its reader holds, so an abort sends it too. It says
    /// nothing about the remote request, which a later connection may ask again.
    ConnectionLost { connection: ConnectionId },
}

/// The sending half an adapter holds.
pub type HeldRequestSender = mpsc::UnboundedSender<HeldRequestMessage>;
/// The receiving half the harness's run loop reads.
pub type HeldRequestReceiver = mpsc::UnboundedReceiver<HeldRequestMessage>;

/// One harness's channel.
pub fn channel() -> (HeldRequestSender, HeldRequestReceiver) {
    mpsc::unbounded_channel()
}
