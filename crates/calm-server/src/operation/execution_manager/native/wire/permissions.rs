//! Typed mutually exclusive native Codex permission selection.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionsChoice {
    SandboxMode(String),
    NamedProfile(String),
}

impl PermissionsChoice {
    pub(super) fn verify_active(&self, result: &ThreadResult) -> Result<()> {
        if let Self::NamedProfile(expected) = self {
            if result
                .active_permission_profile
                .as_ref()
                .map(|profile| profile.id.as_str())
                != Some(expected.as_str())
            {
                return Err(CalmError::CodexAppServer(
                    "provider did not confirm the requested named permissions profile".into(),
                ));
            }
        }
        Ok(())
    }

    pub(super) fn apply_thread(&self, params: &mut Value) {
        match self {
            Self::SandboxMode(mode) => params["sandbox"] = json!(mode),
            Self::NamedProfile(profile) => params["permissions"] = json!(profile),
        }
    }

    pub(super) fn apply_turn(&self, params: &mut Value) -> Result<()> {
        match self {
            Self::NamedProfile(profile) => params["permissions"] = json!(profile),
            Self::SandboxMode(_) => return Err(CalmError::CodexAppServer(
                "turn/start requires a named profile; legacy sandbox modes belong to thread configuration".into(),
            )),
        }
        Ok(())
    }
}
