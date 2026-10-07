//! Confirmed failure, untrusted-generation log report, and an explicit owner retry intent.
use crate::codex_appserver::Notification;
use crate::error::{CalmError, Result};
use provider::codex::AuthenticationFailure;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use utoipa::ToSchema;
#[path = "codex_authentication_store.rs"]
mod store;
use store::{Checkpoint, Evidence, Store};

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
#[derive(Clone, Copy)]
pub(crate) struct AuthenticationStamp {
    generation: u64,
    connection_epoch: u64,
}
struct State {
    saved: Checkpoint,
    turns: HashMap<(String, String), u64>,
    loaded: bool,
    dirty: bool,
    persistence_error: bool,
    stderr_source: Option<std::fs::File>,
    connection_key: Option<usize>,
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
    pub(crate) fn new(path: PathBuf, home: PathBuf, stderr: PathBuf) -> Self {
        Self::build(Some(Store::new(path, home, stderr)))
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
        Self {
            state: Mutex::new(State {
                saved,
                turns: HashMap::new(),
                loaded,
                dirty: false,
                persistence_error,
                stderr_source: None,
                connection_key: None,
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
    pub(crate) fn problem(&self) -> Option<AuthenticationFailure> {
        self.state
            .lock()
            .expect("authentication mutex")
            .saved
            .evidence
            .confirmed()
    }
    pub(crate) fn bind_client(&self, key: usize) -> u64 {
        let mut state = self.state.lock().expect("authentication mutex");
        state.connection_epoch = state.connection_epoch.wrapping_add(1);
        state.connection_key = Some(key);
        state.turns.clear();
        state.connection_epoch
    }
    pub(crate) fn stamp(&self, key: usize) -> Option<AuthenticationStamp> {
        let state = self.state.lock().expect("authentication mutex");
        (state.connection_key == Some(key)).then_some(AuthenticationStamp {
            generation: state.saved.generation,
            connection_epoch: state.connection_epoch,
        })
    }
    pub(crate) fn notice(&self) -> Option<AuthenticationNotice> {
        let state = self.state.lock().expect("authentication mutex");
        let (kind, text) = if state.persistence_error {
            (
                AuthenticationNoticeKind::StateUnavailable,
                STATE_UNAVAILABLE,
            )
        } else if matches!(state.saved.evidence, Evidence::Confirmed { .. }) {
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
    #[cfg(test)]
    pub(crate) fn record(&self, generation: u64, message: &str) {
        let epoch = self
            .state
            .lock()
            .expect("authentication mutex")
            .connection_epoch;
        self.record_stamped(
            Some(AuthenticationStamp {
                generation,
                connection_epoch: epoch,
            }),
            message,
        );
    }
    pub(crate) fn record_stamped(&self, stamp: Option<AuthenticationStamp>, message: &str) {
        let Some(stamp) = stamp else {
            return;
        };
        if let Some(problem) = AuthenticationFailure::from_message(message) {
            let mut state = self.state.lock().expect("authentication mutex");
            if !state.loaded {
                state.saved.reported = true;
                return;
            }
            if stamp.generation == state.saved.generation
                && stamp.connection_epoch == state.connection_epoch
            {
                if Self::confirm(&mut state, problem) {
                    state.dirty = true;
                    self.commit(&mut state);
                }
            }
        }
    }
    fn confirm(state: &mut State, failure: AuthenticationFailure) -> bool {
        let evidence = Evidence::Confirmed { failure };
        if state.saved.evidence == evidence {
            return false;
        }
        state.saved.evidence = evidence;
        state.saved.revision = state.saved.revision.wrapping_add(1);
        true
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
        let Some(failure) = state.saved.evidence.confirmed() else {
            return Err(CalmError::Conflict(
                "There is no confirmed sign-in failure waiting for a retry.".into(),
            ));
        };
        let failed_generation = state.saved.generation;
        state.saved.generation = state.saved.generation.wrapping_add(1);
        state.saved.revision = state.saved.revision.wrapping_add(1);
        state.saved.evidence = Evidence::RetryRequested {
            failure,
            failed_generation,
        };
        state.dirty = true;
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
                    if let Evidence::RetryRequested {
                        failure,
                        failed_generation,
                    } = state.saved.evidence
                    {
                        state.saved.evidence = Evidence::Retried {
                            failure,
                            failed_generation,
                        };
                    }
                    self.clear_report(&mut state);
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
                self.clear_report(&mut state);
            }
            _ => {}
        }
        if state.saved != before {
            state.dirty = true;
            self.commit(&mut state);
        }
    }
    fn clear_report(&self, state: &mut State) {
        state.saved.reported = false;
        if let Some(store) = &self.store {
            match store.boundary(state.stderr_source.as_ref()) {
                Ok(cursor) => state.saved.cursor = cursor,
                Err(e) => {
                    tracing::warn!(error=%e,"could not establish authentication log boundary")
                }
            }
        }
    }
    fn observe_error(
        state: &mut State,
        generation: Option<u64>,
        error: Option<&serde_json::Value>,
    ) {
        if !state.loaded {
            state.saved.reported |= error
                .and_then(|e| e.get("message"))
                .and_then(serde_json::Value::as_str)
                .and_then(AuthenticationFailure::from_message)
                .is_some();
            return;
        }
        let generation = generation.or((state.saved.generation == 0).then_some(0));
        if generation != Some(state.saved.generation) {
            return;
        }
        if let Some(problem) = error
            .and_then(|e| e.get("message"))
            .and_then(serde_json::Value::as_str)
            .and_then(AuthenticationFailure::from_message)
        {
            Self::confirm(state, problem);
        }
    }
    pub(crate) fn poll(&self, source: Option<std::fs::File>) {
        let Some(store) = &self.store else {
            return;
        };
        let mut state = self.state.lock().expect("authentication mutex");
        state.stderr_source = source;
        if !state.loaded {
            if let Ok(mut saved) = store.load() {
                state.turns.clear();
                saved.reported |= state.saved.reported;
                state.saved = saved;
                state.loaded = true;
                state.persistence_error = false;
            } else {
                return;
            }
        }
        let before = state.saved.clone();
        let source = state
            .stderr_source
            .as_ref()
            .and_then(|f| f.try_clone().ok());
        match store.scan(&mut state.saved.cursor, source.as_ref()) {
            Ok(true) => state.saved.reported = true,
            Ok(false) => {}
            Err(e) => tracing::warn!(error=%e,"could not read Codex authentication log evidence"),
        }
        if state.saved.reported != before.reported {
            state.dirty = true;
        }
        if state.dirty {
            self.commit(&mut state);
        }
    }
    fn commit(&self, state: &mut State) {
        if !state.loaded {
            return;
        }
        if let Some(store) = &self.store {
            match store.save(&state.saved) {
                Ok(()) => {
                    state.dirty = false;
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
            state.dirty = false;
        }
    }
}
#[cfg(test)]
#[path = "codex_authentication_tests.rs"]
mod tests;
