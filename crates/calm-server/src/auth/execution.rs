//! Execution consumes the same real issuer as the HTTP gate. An audit identity
//! or Actor cannot reconstruct a live request credential.

use axum::{
    extract::FromRequestParts,
    http::{HeaderMap, request::Parts},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::{AuthConfig, AuthState, Principal, SessionAuthority, SessionIdentity, SessionStore};
use crate::error::{CalmError, Result};

#[derive(Clone)]
pub(crate) struct AuthenticatedSession {
    principal: Principal,
    issuer: Arc<Issuer>,
}

enum Issuer {
    Session {
        sessions: SessionStore,
        token: String,
        authority: SessionAuthority,
    },
    Development(Arc<AuthConfig>),
}

impl AuthenticatedSession {
    pub(crate) fn principal(&self) -> &Principal {
        &self.principal
    }

    pub(crate) fn revalidate(&self) -> Result<()> {
        let principal = match self.issuer.as_ref() {
            Issuer::Session {
                sessions,
                token,
                authority,
            } => {
                let session = sessions.get(token).ok_or(CalmError::Unauthorized)?;
                if session.authority != *authority {
                    return Err(CalmError::Unauthorized);
                }
                Principal::from_identity(session.identity, session.session_id)
            }
            Issuer::Development(config) if config.dev_autologin => {
                Principal::from_identity(SessionIdentity::owner(config), "dev-autologin".into())
            }
            Issuer::Development(_) => return Err(CalmError::Unauthorized),
        };
        if principal.user_id != self.principal.user_id
            || principal.session_id != self.principal.session_id
            || principal.role != self.principal.role
            || principal.display_name != self.principal.display_name
        {
            return Err(CalmError::Unauthorized);
        }
        Ok(())
    }

    /// Personal-host credentials are an auth-owner decision, independent of
    /// whether an Area permits collaborating on its content.
    pub(crate) fn accept_personal_environment(&self) -> Result<AcceptedEnvironment> {
        self.revalidate()?;
        let environment = AcceptedEnvironment {
            user_id: self.principal.user_id.clone(),
        };
        environment.require_current_personal_environment()?;
        Ok(environment)
    }
}

/// Persisted only in the hash-bound operation grant. This is not a cookie,
/// current Principal or bearer capability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AcceptedEnvironment {
    user_id: String,
}

impl AcceptedEnvironment {
    pub(crate) fn user_id(&self) -> &str {
        &self.user_id
    }

    /// The current deployment supports execution in its owner's private host
    /// environment. A shared-user environment requires a separate real owner.
    pub(crate) fn require_current_personal_environment(&self) -> Result<()> {
        if self.user_id != super::OWNER_USER_ID {
            return Err(CalmError::Forbidden(
                "this identity is not authorized for the current execution environment".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn authorize_current(&self) -> Result<PersonalEnvironment<'_>> {
        self.require_current_personal_environment()?;
        Ok(PersonalEnvironment(self))
    }
}

pub(crate) struct PersonalEnvironment<'a>(&'a AcceptedEnvironment);

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::Principal {}
    impl Sealed for super::PersonalEnvironment<'_> {}
}

pub(crate) trait AreaSubject: sealed::Sealed {
    fn user_id(&self) -> &str;
    fn deployment_owner(&self) -> bool;
}
impl AreaSubject for Principal {
    fn user_id(&self) -> &str {
        &self.user_id
    }
    fn deployment_owner(&self) -> bool {
        self.is_deployment_owner()
    }
}
impl AreaSubject for PersonalEnvironment<'_> {
    fn user_id(&self) -> &str {
        self.0.user_id()
    }
    fn deployment_owner(&self) -> bool {
        self.user_id() == super::OWNER_USER_ID
    }
}

pub(super) fn resolve(state: &AuthState, headers: &HeaderMap) -> Option<AuthenticatedSession> {
    let (principal, issuer) = if state.config.dev_autologin {
        (
            Principal::from_identity(
                SessionIdentity::owner(&state.config),
                "dev-autologin".into(),
            ),
            Issuer::Development(Arc::clone(&state.config)),
        )
    } else {
        let session =
            super::session_cookies(headers).find_map(|token| state.sessions.get(token))?;
        let issuer = Issuer::Session {
            sessions: state.sessions.clone(),
            token: session.session_id.clone(),
            authority: session.authority,
        };
        (
            Principal::from_identity(session.identity, session.session_id),
            issuer,
        )
    };
    Some(AuthenticatedSession {
        principal,
        issuer: Arc::new(issuer),
    })
}

impl<S: Send + Sync> FromRequestParts<S> for AuthenticatedSession {
    type Rejection = CalmError;
    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self> {
        parts
            .extensions
            .get::<Self>()
            .cloned()
            .ok_or(CalmError::Unauthorized)
    }
}
