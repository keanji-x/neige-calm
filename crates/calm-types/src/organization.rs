//! Application identities and Area membership; separate from credential and event roles.

/// Stable identity of the installation's original single user. Used by the
/// explicit migration and deployment-owner bootstrap, never as a creator fallback.
pub const LEGACY_OWNER_USER_ID: &str = "local-owner";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AreaMemberRole {
    Owner,
    Member,
}

/// Closed Area policy: members collaborate; deletion requires the Area owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AreaAccess {
    Collaborate,
    Delete,
}

impl AreaMemberRole {
    pub fn permits(self, access: AreaAccess) -> bool {
        match access {
            AreaAccess::Collaborate => true,
            AreaAccess::Delete => self == Self::Owner,
        }
    }
}
