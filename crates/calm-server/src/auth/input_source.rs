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
    let (principal, issuer) = if state.config.dev_autologin {
        (
            dev_principal(&state.config),
            Issuer::ConfiguredDev {
                config: Arc::clone(&state.config),
            },
        )
    } else {
        let session = session_cookies(headers).find_map(|id| state.sessions.get(id))?;
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
