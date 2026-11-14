//! Live request authentication for canonical input execution. This value never
//! enters an operation, snapshot or receipt; its session lookup is a credential.
use super::*;
#[cfg(all(test, feature = "fixtures"))]
pub(crate) mod tests;

#[derive(Clone)]
pub(crate) struct HumanInputSource(Arc<Issuer>);

enum Issuer {
    Session {
        sessions: SessionStore,
        token: String,
        user_id: String,
        authority: SessionAuthority,
    },
    ConfiguredDev {
        config: Arc<AuthConfig>,
    },
}

impl std::fmt::Debug for HumanInputSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HumanInputSource").finish_non_exhaustive()
    }
}

impl HumanInputSource {
    /// Only the actual deployment issuer may delegate installation resources.
    /// A paired device's owner attribution does not issue management authority.
    pub(crate) fn current_deployment_issuer(&self) -> Result<Principal> {
        match self.0.as_ref() {
            Issuer::Session {
                authority: SessionAuthority::PasswordLogin,
                ..
            }
            | Issuer::ConfiguredDev { .. } => self.current_principal(),
            Issuer::Session {
                authority: SessionAuthority::PairedDevice | SessionAuthority::ShareLink,
                ..
            } => Err(CalmError::Forbidden("deployment issuer required".into())),
        }
    }

    /// Credential classification is captured by authentication. Eviction cannot
    /// turn an in-flight share request into an owner request.
    pub(crate) fn is_share_link(&self) -> bool {
        matches!(
            self.0.as_ref(),
            Issuer::Session {
                authority: SessionAuthority::ShareLink,
                ..
            }
        )
    }

    /// Shared access never supplies a registered Human message author.
    pub(crate) fn current_human_principal(&self) -> Result<Principal> {
        if self.is_share_link() {
            return Err(CalmError::Forbidden(
                "share input requires LinkGuest source".into(),
            ));
        }
        self.current_principal()
    }

    /// Ask the actual issuer again at the protected decision. A recorded author,
    /// cached Principal or the dev session sentinel cannot manufacture a source.
    pub(crate) fn current_principal(&self) -> Result<Principal> {
        match self.0.as_ref() {
            Issuer::Session {
                sessions,
                token,
                user_id,
                authority,
            } => {
                let session = sessions.get(token).ok_or(CalmError::Unauthorized)?;
                if session.identity.user_id != *user_id || session.authority != *authority {
                    return Err(CalmError::Unauthorized);
                }
                Ok(Principal::from_identity(
                    session.identity,
                    session.session_id,
                ))
            }
            Issuer::ConfiguredDev { config } if config.dev_autologin => Ok(dev_principal(config)),
            Issuer::ConfiguredDev { .. } => Err(CalmError::Unauthorized),
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for HumanInputSource {
    type Rejection = CalmError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Self>()
            .cloned()
            .ok_or(CalmError::Unauthorized)
    }
}

pub(super) struct AuthenticatedRequest {
    pub principal: Principal,
    pub input_source: HumanInputSource,
}

fn dev_principal(config: &AuthConfig) -> Principal {
    // Existing whoami/logout compatibility only; this string grants nothing.
    Principal::from_identity(SessionIdentity::owner(config), "dev-autologin".into())
}

pub(super) fn resolve(state: &AuthState, headers: &HeaderMap) -> Option<AuthenticatedRequest> {
    let selected = headers
        .get("x-calm-share-link")
        .map(|h| h.to_str().unwrap_or(""));
    let share_session = if let Some(id) = selected {
        let name = crate::share_access::sessions::cookie_name(id);
        Some(
            crate::share_access::sessions::cookies(headers, &name).find_map(|token| {
                let credential = state.shares.get(token)?;
                if credential.link_id != id {
                    return None;
                }
                state
                    .sessions
                    .get(token)
                    .filter(|s| s.authority == SessionAuthority::ShareLink)
            })?,
        )
    } else {
        None
    };
    let (principal, issuer) = if let Some(session) = share_session {
        let issuer = Issuer::Session {
            sessions: state.sessions.clone(),
            token: session.session_id.clone(),
            user_id: session.identity.user_id.clone(),
            authority: session.authority,
        };
        (
            Principal::from_identity(session.identity, session.session_id),
            issuer,
        )
    } else if state.config.dev_autologin {
        (
            dev_principal(&state.config),
            Issuer::ConfiguredDev {
                config: Arc::clone(&state.config),
            },
        )
    } else {
        let session = session_cookies(headers).find_map(|id| {
            state
                .sessions
                .get(id)
                .filter(|s| s.authority != SessionAuthority::ShareLink)
        })?;
        let issuer = Issuer::Session {
            sessions: state.sessions.clone(),
            token: session.session_id.clone(),
            user_id: session.identity.user_id.clone(),
            authority: session.authority,
        };
        (
            Principal::from_identity(session.identity, session.session_id),
            issuer,
        )
    };
    Some(AuthenticatedRequest {
        principal,
        input_source: HumanInputSource(Arc::new(issuer)),
    })
}
