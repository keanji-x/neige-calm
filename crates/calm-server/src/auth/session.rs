use crate::error::{CalmError, Result};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// The application identity verified by a session issuer. Credential provenance
/// is tracked separately in [`SessionAuthority`]. Request headers never supply it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionIdentity {
    pub user_id: String,
    pub display_name: String,
    pub role: String,
}

impl SessionIdentity {
    /// Identity bound to the configured deployment-owner credentials.
    pub fn owner(config: &super::AuthConfig) -> Self {
        Self {
            user_id: super::OWNER_USER_ID.into(),
            display_name: config.display_name.clone(),
            role: super::OWNER_ROLE.into(),
        }
    }
}

/// One active session with a required verified identity and credential provenance.
#[derive(Debug, Clone)]
pub struct Session {
    pub session_id: String,
    pub identity: SessionIdentity,
    pub authority: SessionAuthority,
}

/// Required credential provenance: paired devices never gain local management
/// authority, even if a separate device-registry record is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAuthority {
    PasswordLogin,
    PairedDevice,
}

/// Optional credential backend owned by the feature that issues its sessions.
/// The generic gate only consumes credential lookup and revocation.
pub(crate) trait SessionBackend: std::fmt::Debug + Send + Sync {
    fn get(&self, token: &str) -> Option<Session>;
    fn remove(&self, token: &str) -> Result<()>;
}

/// Process-local sessions with an optional feature-owned credential backend.
#[derive(Debug, Clone, Default)]
pub struct SessionStore {
    inner: Arc<Mutex<HashMap<String, Session>>>,
    backend: Arc<Mutex<Option<Arc<dyn SessionBackend>>>>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint a fresh session, store it, and return the new id. Caller sets the cookie.
    pub fn create(&self, identity: SessionIdentity, authority: SessionAuthority) -> String {
        let id = Uuid::new_v4().to_string();
        let session = Session {
            session_id: id.clone(),
            identity,
            authority,
        };
        // Poisoned-mutex policy: log + recover, never take the whole server down for one bad in-flight request.
        if let Ok(mut guard) = self.inner.lock() {
            guard.insert(id.clone(), session);
        } else {
            tracing::error!("session store mutex poisoned on insert");
        }
        id
    }

    pub(crate) fn set_backend(&self, backend: Arc<dyn SessionBackend>) {
        *self.backend.lock().expect("session backend lock poisoned") = Some(backend);
    }

    pub fn get(&self, id: &str) -> Option<Session> {
        match self.inner.lock() {
            Ok(g) => g
                .get(id)
                .cloned()
                .or_else(|| self.backend.lock().ok()?.as_ref()?.get(id)),
            Err(_) => {
                tracing::error!("session store mutex poisoned on get");
                None
            }
        }
    }

    /// Idempotent — removing an unknown id is a no-op.
    pub fn remove(&self, id: &str) -> Result<()> {
        self.inner
            .lock()
            .map_err(|_| CalmError::Internal("Session lock poisoned".into()))?
            .remove(id);
        if let Some(backend) = self
            .backend
            .lock()
            .map_err(|_| CalmError::Internal("Session backend lock poisoned".into()))?
            .as_ref()
        {
            backend.remove(id)?;
        }
        Ok(())
    }
}
