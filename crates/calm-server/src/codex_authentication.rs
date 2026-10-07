//! Observed provider authentication failure; successful login fences earlier work.
use std::collections::HashMap;

use crate::codex_appserver::Notification;
use provider::codex::AuthenticationFailure;

pub(crate) const SIGN_IN_REQUIRED: &str =
    "Codex sign-in needs renewal. Sign in again for this server, then retry.";

#[derive(Default)]
struct State {
    generation: u64,
    problem: Option<AuthenticationFailure>,
    turns: HashMap<(String, String), u64>,
}

#[derive(Default)]
pub(crate) struct CodexAuthentication {
    state: std::sync::Mutex<State>,
}

impl CodexAuthentication {
    pub(crate) fn generation(&self) -> u64 {
        self.state
            .lock()
            .expect("codex authentication mutex poisoned")
            .generation
    }

    pub(crate) fn record(&self, generation: u64, message: &str) {
        if let Some(problem) = AuthenticationFailure::from_message(message) {
            let mut state = self
                .state
                .lock()
                .expect("codex authentication mutex poisoned");
            if generation == state.generation {
                state.problem = Some(problem);
            }
        }
    }

    pub(crate) fn problem(&self) -> Option<AuthenticationFailure> {
        self.state
            .lock()
            .expect("codex authentication mutex poisoned")
            .problem
    }

    pub(crate) fn observe(&self, notification: &Notification) {
        let mut state = self
            .state
            .lock()
            .expect("codex authentication mutex poisoned");
        match notification {
            Notification::TurnStarted { thread_id, turn } => {
                if let Some(turn_id) = turn.get("id").and_then(serde_json::Value::as_str) {
                    let generation = state.generation;
                    state
                        .turns
                        .insert((thread_id.clone(), turn_id.into()), generation);
                }
            }
            Notification::TurnCompleted { thread_id, turn } => {
                let generation = turn
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|id| state.turns.remove(&(thread_id.clone(), id.into())));
                Self::observe_error(&mut state, generation, turn.get("error"));
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
                state.generation = state.generation.wrapping_add(1);
                state.problem = None;
            }
            _ => {}
        }
    }

    fn observe_error(
        state: &mut State,
        generation: Option<u64>,
        error: Option<&serde_json::Value>,
    ) {
        // Adopted in-flight turns may have started before subscription. Once a login
        // is confirmed, an unattributed old frame is not proof against that login.
        let generation = generation.or((state.generation == 0).then_some(0));
        if generation != Some(state.generation) {
            return;
        }
        if let Some(problem) = error
            .and_then(|error| error.get("message"))
            .and_then(serde_json::Value::as_str)
            .and_then(AuthenticationFailure::from_message)
        {
            state.problem = Some(problem);
        }
    }
}
