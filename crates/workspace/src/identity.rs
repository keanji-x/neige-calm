//! Persistent workspace names; neither an identity nor display text authorizes IO.

use std::{fmt, path::Component};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Kernel-owned, immutable resource identity. Runtime generations are separate.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct WorkspaceId(String);

impl WorkspaceId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for WorkspaceId {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        if value.len() != 32
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(Error::Internal("invalid workspace identity".into()));
        }
        Ok(Self(value))
    }
}

impl From<WorkspaceId> for String {
    fn from(value: WorkspaceId) -> Self {
        value.0
    }
}

impl fmt::Display for WorkspaceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// UTF-8, normalized relative cwd. The workspace root is `.`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct WorkspaceCwd(String);

impl WorkspaceCwd {
    pub fn root() -> Self {
        Self(".".into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for WorkspaceCwd {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        if value.is_empty() || value.contains('\0') {
            return Err(Error::Internal(
                "workspace cwd must be a nonempty relative path".into(),
            ));
        }
        let mut parts = Vec::new();
        for component in std::path::Path::new(&value).components() {
            match component {
                Component::Normal(part) => parts.push(part.to_str().expect("UTF-8 input")),
                Component::CurDir => {}
                _ => {
                    return Err(Error::Internal(
                        "workspace cwd must remain inside its root".into(),
                    ));
                }
            }
        }
        Ok(Self(if parts.is_empty() {
            ".".into()
        } else {
            parts.join("/")
        }))
    }
}

impl From<WorkspaceCwd> for String {
    fn from(value: WorkspaceCwd) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_identity_validates_names_and_relative_cwd() {
        for cwd in ["/tmp", "..", "a/../b", "a\0b", ""] {
            assert!(
                serde_json::from_value::<WorkspaceCwd>(serde_json::json!(cwd)).is_err(),
                "{cwd:?}"
            );
        }
        let cwd: WorkspaceCwd = serde_json::from_value(serde_json::json!("./src//nested")).unwrap();
        assert_eq!(cwd.as_str(), "src/nested");
        assert!(WorkspaceId::try_from(String::from("other")).is_err());
        let id: WorkspaceId = serde_json::from_value(serde_json::json!("a".repeat(32))).unwrap();
        assert_eq!(id.as_str(), "a".repeat(32));
    }
}
