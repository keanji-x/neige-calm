//! Actual issuer and its explicit legacy private automation declaration.
use super::*;
use std::sync::Weak;

#[derive(Clone)]
pub(crate) struct LegacyPrivateExecution {
    issuer: Weak<AuthConfig>,
}
impl LegacyPrivateExecution {
    pub(crate) fn current_owner(&self) -> Result<String> {
        let issuer = self.issuer.upgrade().ok_or(CalmError::Unauthorized)?;
        if !issuer.dev_autologin && (issuer.username.is_none() || issuer.password.is_none()) {
            return Err(CalmError::Unauthorized);
        }
        // The actual current password/device/dev issuers all issue this one identity.
        // A future named issuer needs a different explicit execution declaration.
        Ok(SessionIdentity::owner(&issuer).user_id)
    }
}

pub(crate) struct ValidatedApplicationAuth {
    pub state: AuthState,
    pub execution: LegacyPrivateExecution,
}
impl ValidatedApplicationAuth {
    pub(crate) fn from_config(cfg: &Config) -> anyhow::Result<Self> {
        let state = AuthState::from_config(cfg)?;
        let execution = LegacyPrivateExecution {
            issuer: Arc::downgrade(&state.config),
        };
        Ok(Self { state, execution })
    }
}
