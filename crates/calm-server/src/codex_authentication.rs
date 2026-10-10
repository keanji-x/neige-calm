//! Confirmed failure, a report whose login generation is unknown, and an explicit owner retry intent.
use crate::codex_appserver::Notification;
use crate::error::{CalmError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use utoipa::ToSchema;
#[path = "codex_authentication_store.rs"]
mod store;
use store::{Checkpoint, Evidence, Store};

/// The `CodexRefused` text a held request answers with while a sign-in failure is confirmed.
pub(crate) const SIGN_IN_REFUSAL: &str = "codex_sign_in_required";
pub(crate) const SIGN_IN_REQUIRED: &str =
    "Codex sign-in needs renewal. Sign in again for this server, then retry.";
const REPORTED: &str = "Codex reported a sign-in renewal error. This does not confirm that your current sign-in has failed.";
const STATE_UNAVAILABLE: &str = "Codex sign-in status could not be saved or restored. Check the server's storage before retrying.";
const RETRY_REQUESTED: &str = "You allowed queued messages to retry with the server's current sign-in. \
    Paused conversations may still need their recovery action.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuthenticationNoticeKind {
    SignInRequired,
    RefreshErrorReported,
    StateUnavailable,
    RetryRequested,
}
impl AuthenticationNoticeKind {
    pub(crate) fn holds_issuance(self) -> bool {
        matches!(self, Self::SignInRequired | Self::StateUnavailable)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuthenticationNotice {
    pub kind: AuthenticationNoticeKind,
    pub text: String,
    /// Opaque durable observation revision; only the owner retry command consumes it.
    pub revision: String,
}
struct State {
    saved: Checkpoint,
    turns: HashMap<(String, String), u64>,
    loaded: bool,
    persisted: Option<Checkpoint>,
    persistence_error: bool,
    connection_epoch: u64,
}
pub(crate) struct CodexAuthentication {
    state: Mutex<State>,
    store: Option<Store>,
}
impl Default for CodexAuthentication {
    fn default() -> Self {
        Self::build(None)
    }
}
impl CodexAuthentication {
    pub(crate) fn new(path: PathBuf, home: PathBuf) -> Self {
        Self::build(Some(Store::new(path, home)))
    }
    fn build(store: Option<Store>) -> Self {
        let result = store
            .as_ref()
            .map_or_else(|| Ok(Checkpoint::empty(PathBuf::new())), Store::load);
        let (saved, loaded, persistence_error) = match result {
            Ok(saved) => (saved, true, false),
            Err(e) => {
                tracing::warn!(error=%e,"authentication checkpoint could not be restored");
                (store.as_ref().expect("store supplied").empty(), false, true)
            }
        };
        let persisted = loaded.then(|| saved.clone());
        Self {
            state: Mutex::new(State {
                saved,
                turns: HashMap::new(),
                loaded,
                persisted,
                persistence_error,
                connection_epoch: 0,
            }),
            store,
        }
    }
    #[cfg(test)]
    pub(crate) fn generation(&self) -> u64 {
        self.state
            .lock()
            .expect("authentication mutex")
            .saved
            .generation
    }
    pub(crate) fn sign_in_failed(&self) -> bool {
        self.state
            .lock()
            .expect("authentication mutex")
            .saved
            .evidence
            == Evidence::Confirmed
    }
    pub(crate) fn bind_client(&self) -> u64 {
        let mut state = self.state.lock().expect("authentication mutex");
        state.connection_epoch = state.connection_epoch.wrapping_add(1);
        state.turns.clear();
        state.connection_epoch
    }
    pub(crate) fn notice(&self) -> Option<AuthenticationNotice> {
        let state = self.state.lock().expect("authentication mutex");
        let (kind, text) = if state.persistence_error {
            (
                AuthenticationNoticeKind::StateUnavailable,
                STATE_UNAVAILABLE,
            )
        } else if state.saved.evidence == Evidence::Confirmed {
            (AuthenticationNoticeKind::SignInRequired, SIGN_IN_REQUIRED)
        } else if matches!(state.saved.evidence, Evidence::RetryRequested { .. }) {
            (AuthenticationNoticeKind::RetryRequested, RETRY_REQUESTED)
        } else if state.saved.reported {
            (AuthenticationNoticeKind::RefreshErrorReported, REPORTED)
        } else {
            return None;
        };
        Some(AuthenticationNotice {
            kind,
            text: text.into(),
            revision: format!("{}:{}", state.saved.revision_scope, state.saved.revision),
        })
    }
    pub(crate) fn hold(&self) -> Option<String> {
        self.notice()
            .filter(|n| n.kind.holds_issuance())
            .map(|n| format!("{} Your message is still queued.", n.text))
    }
    /// A turn of `generation` fails with Codex's sign-in error, as the daemon reports one.
    #[cfg(test)]
    pub(crate) fn record(&self, generation: u64) {
        let epoch = {
            let mut state = self.state.lock().expect("authentication mutex");
            state
                .turns
                .insert(("recorded".into(), "recorded".into()), generation);
            state.connection_epoch
        };
        self.observe_from_connection(
            epoch,
            &Notification::TurnCompleted {
                thread_id: "recorded".into(),
                turn: serde_json::json!({"id":"recorded","status":"failed",
                    "error":{"message":"fixture","codexErrorInfo":"unauthorized"}}),
            },
        );
    }
    fn confirm(state: &mut State) {
        if state.saved.evidence != Evidence::Confirmed {
            state.saved.evidence = Evidence::Confirmed;
            state.saved.revision = state.saved.revision.wrapping_add(1);
        }
    }
    /// Owner authorizes a retry, not a credential repair claim. Persist the intent before opening issuance.
    pub(crate) fn request_retry(&self, expected: &str) -> Result<String> {
        if expected.len() > 128 {
            return Err(CalmError::BadRequest(
                "Invalid sign-in state revision".into(),
            ));
        }
        let mut state = self.state.lock().expect("authentication mutex");
        if !state.loaded || state.persistence_error {
            return Err(CalmError::ServiceUnavailable(STATE_UNAVAILABLE.into()));
        }
        if format!("{}:{}", state.saved.revision_scope, state.saved.revision) != expected {
            return Err(CalmError::Conflict(
                "Codex sign-in state changed; read its current status before retrying.".into(),
            ));
        }
        if state.saved.evidence != Evidence::Confirmed {
            return Err(CalmError::Conflict(
                "There is no confirmed sign-in failure waiting for a retry.".into(),
            ));
        }
        let failed_generation = state.saved.generation;
        state.saved.generation = state.saved.generation.wrapping_add(1);
        state.saved.revision = state.saved.revision.wrapping_add(1);
        state.saved.evidence = Evidence::RetryRequested { failed_generation };
        self.commit(&mut state);
        if state.persistence_error {
            return Err(CalmError::ServiceUnavailable(STATE_UNAVAILABLE.into()));
        }
        Ok(format!(
            "{}:{}",
            state.saved.revision_scope, state.saved.revision
        ))
    }
    #[cfg(any(test, feature = "fixtures"))]
    pub(crate) fn observe(&self, notification: &Notification) {
        let epoch = self
            .state
            .lock()
            .expect("authentication mutex")
            .connection_epoch;
        self.observe_from_connection(epoch, notification);
    }
    pub(crate) fn observe_from_connection(&self, epoch: u64, notification: &Notification) {
        let mut state = self.state.lock().expect("authentication mutex");
        if epoch != state.connection_epoch {
            return;
        }
        let before = state.saved.clone();
        match notification {
            Notification::TurnStarted { thread_id, turn } => {
                if !state.loaded {
                    return;
                }
                if let Some(id) = turn.get("id").and_then(serde_json::Value::as_str) {
                    let generation = state.saved.generation;
                    state
                        .turns
                        .insert((thread_id.clone(), id.into()), generation);
                }
            }
            Notification::TurnCompleted { thread_id, turn } => {
                let generation = turn
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|id| state.turns.remove(&(thread_id.clone(), id.into())));
                Self::observe_error(&mut state, generation, turn.get("error"));
                if generation == Some(state.saved.generation)
                    && turn.get("status").and_then(serde_json::Value::as_str) == Some("completed")
                    && turn.get("error").is_none_or(serde_json::Value::is_null)
                {
                    if let Evidence::RetryRequested { failed_generation } = state.saved.evidence {
                        state.saved.evidence = Evidence::Retried { failed_generation };
                    }
                    state.saved.reported = false;
                }
            }
            Notification::Other { method, params } if method == "error" => {
                let generation = notification
                    .thread_id()
                    .zip(params.get("turnId").and_then(serde_json::Value::as_str))
                    .and_then(|(thread, id)| state.turns.get(&(thread.into(), id.into())).copied());
                Self::observe_error(&mut state, generation, params.get("error"));
            }
            Notification::Other { method, params } if method == "turn/aborted" => {
                if let Some((thread, id)) = notification
                    .thread_id()
                    .zip(crate::shared_codex_appserver::other_turn_id(params))
                {
                    state.turns.remove(&(thread.into(), id.into()));
                }
            }
            Notification::Other { method, params }
                if method == "account/login/completed"
                    && params.get("success").and_then(serde_json::Value::as_bool) == Some(true)
                    && params.get("error").is_some_and(serde_json::Value::is_null) =>
            {
                state.saved.generation = state.saved.generation.wrapping_add(1);
                state.saved.revision = state.saved.revision.wrapping_add(1);
                state.saved.evidence = Evidence::Clear;
                state.loaded = true;
                state.saved.reported = false;
            }
            _ => {}
        }
        if state.saved != before {
            self.commit(&mut state);
        }
    }
    fn observe_error(
        state: &mut State,
        generation: Option<u64>,
        error: Option<&serde_json::Value>,
    ) {
        let sign_in_failure = error.is_some_and(provider::codex::is_sign_in_failure);
        if !state.loaded {
            state.saved.reported |= sign_in_failure;
            return;
        }
        let generation = generation.or((state.saved.generation == 0).then_some(0));
        if generation != Some(state.saved.generation) {
            return;
        }
        if sign_in_failure {
            Self::confirm(state);
        }
    }
    /// Restore a checkpoint that could not be read, then commit what is still unsaved.
    pub(crate) fn poll(&self) {
        let Some(store) = &self.store else {
            return;
        };
        let mut state = self.state.lock().expect("authentication mutex");
        if !state.loaded {
            let Ok(mut saved) = store.load() else {
                return;
            };
            state.persisted = Some(saved.clone());
            state.turns.clear();
            saved.reported |= state.saved.reported;
            state.saved = saved;
            state.loaded = true;
            state.persistence_error = false;
        }
        // The persisted snapshot changes only after an atomic commit, including a restoration merge.
        self.commit(&mut state);
    }
    fn commit(&self, state: &mut State) {
        if !state.loaded {
            return;
        }
        if state.persisted.as_ref() == Some(&state.saved) && !state.persistence_error {
            return;
        }
        if let Some(store) = &self.store {
            match store.save(&state.saved) {
                Ok(()) => {
                    state.persisted = Some(state.saved.clone());
                    state.persistence_error = false;
                }
                Err(e) => {
                    if !state.persistence_error {
                        tracing::warn!(error=%e,"authentication checkpoint could not be saved");
                    }
                    state.persistence_error = true;
                }
            }
        } else {
            state.persisted = Some(state.saved.clone());
        }
    }
}
#[cfg(test)]
mod tests;
