//! The shared daemon's observed authentication failure, separate from Neige login.
use crate::codex_appserver::Notification;
use provider::codex::AuthenticationFailure;

pub(crate) const SIGN_IN_REQUIRED: &str =
    "Codex sign-in needs renewal. Sign in again for this server, then retry.";

#[derive(Default)]
pub(crate) struct CodexAuthentication {
    problem: std::sync::Mutex<Option<AuthenticationFailure>>,
}

impl CodexAuthentication {
    pub(crate) fn record(&self, message: &str) {
        if let Some(problem) = AuthenticationFailure::from_message(message) {
            *self
                .problem
                .lock()
                .expect("codex authentication mutex poisoned") = Some(problem);
        }
    }

    pub(crate) fn problem(&self) -> Option<AuthenticationFailure> {
        *self
            .problem
            .lock()
            .expect("codex authentication mutex poisoned")
    }

    pub(crate) fn observe(&self, notification: &Notification) {
        match notification {
            Notification::TurnCompleted { turn, .. } => {
                if let Some(message) = turn
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(serde_json::Value::as_str)
                {
                    self.record(message);
                }
            }
            Notification::Other { method, params } if method == "error" => {
                if let Some(message) = params
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(serde_json::Value::as_str)
                {
                    self.record(message);
                }
            }
            Notification::Other { method, params }
                if method == "account/login/completed"
                    && params.get("success").and_then(serde_json::Value::as_bool) == Some(true)
                    && params.get("error").is_some_and(serde_json::Value::is_null) =>
            {
                *self
                    .problem
                    .lock()
                    .expect("codex authentication mutex poisoned") = None;
            }
            _ => {}
        }
    }
}
