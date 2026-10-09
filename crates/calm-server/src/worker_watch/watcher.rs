//! The watcher conversation: created on demand through the production conversation create, then
//! sent one message per quiet episode through the production planner-input send (the
//! `today_summary` precedent). Boot recovery and start-on-send are those paths' own.

use axum::http::{HeaderMap, HeaderValue};
use futures::future::BoxFuture;

use crate::actor::Actor;
use crate::conversation_keys::derive_track_conversation_keys;
use crate::error::Result;
use crate::operation::planner_harness_start_adapter::OpeningBriefing;
use crate::routes::planner_input_send::{SendPlannerInputRequest, send_planner_input_keyed};
use crate::routes::today_summary::create_conflict_is_recoverable;
use crate::routes::track_conversations::{
    NewTrackConversationBody, create_track_conversation_inner,
};
use crate::state::{AppState, CodexShellState, RouteState, WorkerState};
use crate::worker_quiet::QuietWorkerInbox;

/// The `Idempotency-Key` each Track's watcher conversation is derived from. A bare constant, so a
/// Track has exactly one watcher card (`derive_track_conversation_keys`).
pub const WORKER_WATCHER_CONVERSATION_KEY: &str = "worker-watcher";

/// The watcher conversation's first message. Fixed: the create binds its text into the
/// operation's payload hash under the fixed key, so a varying text could poison the key.
const BOOTSTRAP_TEXT: &str = include_str!("../../prompts/terminal/worker-watcher-bootstrap.md");

/// The id of `track_id`'s watcher card, whether or not it exists yet.
pub fn watcher_card_id(track_id: &str) -> String {
    derive_track_conversation_keys(track_id, WORKER_WATCHER_CONVERSATION_KEY).card_id
}

/// Delivers quiet episodes to each Track's watcher conversation over the production routes.
pub struct WorkerWatcher {
    route: RouteState,
    worker: WorkerState,
    codex_shell: CodexShellState,
}

impl WorkerWatcher {
    pub fn new(state: &AppState) -> Self {
        use axum::extract::FromRef;
        Self {
            route: RouteState::from_ref(state),
            worker: WorkerState::from_ref(state),
            codex_shell: CodexShellState::from_ref(state),
        }
    }

    /// `track_id`'s watcher card, created with its fixed first message when absent. A create that
    /// lost a race to the same key continues with the card the winner made.
    async fn ensure_card(&self, track_id: &str) -> Result<String> {
        let card_id = watcher_card_id(track_id);
        if self.route.repo.card_get(&card_id).await?.is_some() {
            return Ok(card_id);
        }
        let mut headers = HeaderMap::new();
        headers.insert(
            "idempotency-key",
            HeaderValue::from_static(WORKER_WATCHER_CONVERSATION_KEY),
        );
        let created = create_track_conversation_inner(
            self.route.clone(),
            self.worker.clone(),
            Actor::server_send(),
            headers,
            track_id.to_string(),
            NewTrackConversationBody {
                side: None,
                text: BOOTSTRAP_TEXT.trim_end().to_string(),
                model: None,
                reasoning_effort: None,
            },
            // Each watch message carries what the watcher needs; no activity briefing.
            OpeningBriefing::CallerSuppliesItsOwn,
        )
        .await;
        if let Err(error) = created {
            let exists = self.route.repo.card_get(&card_id).await?.is_some();
            if !create_conflict_is_recoverable(&error, exists) {
                return Err(error);
            }
        }
        Ok(card_id)
    }

    /// Send `text` to `track_id`'s watcher under `episode_key`: a retry under the key replays the
    /// first send instead of queueing the message twice.
    pub async fn deliver(&self, track_id: &str, episode_key: &str, text: String) -> Result<()> {
        let card_id = self.ensure_card(track_id).await?;
        send_planner_input_keyed(
            &self.route,
            &self.worker,
            &self.codex_shell,
            Actor::server_send(),
            card_id,
            SendPlannerInputRequest {
                text,
                attachments: Vec::new(),
                replaces_turn: None,
            },
            episode_key.to_string(),
        )
        .await
        .map(|_| ())
    }
}

impl QuietWorkerInbox for WorkerWatcher {
    fn deliver<'a>(
        &'a self,
        track_id: &'a str,
        episode_key: &'a str,
        text: String,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(WorkerWatcher::deliver(self, track_id, episode_key, text))
    }
}
