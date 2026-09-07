use uuid::Uuid;

use crate::Role;

/// Server-issued ownership identity. The browser's client ID alone cannot
/// distinguish overlapping connections during reconnect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnerLease {
    client_id: Uuid,
    generation: u64,
}

/// One registry per terminal, serialized by the IO shell's mutex.
/// Claims replace the current lease even when the client ID is unchanged.
#[derive(Default)]
pub struct OwnerRegistry {
    owner: Option<OwnerLease>,
    generation: u64,
}

impl OwnerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// First attachment becomes owner unless explicitly observing. Existing
    /// owners are never replaced by handshake; a takeover requires OwnerClaim.
    pub fn on_attach(&mut self, client_id: Uuid, role_hint: Option<Role>) -> Role {
        if self.owner.is_none()
            && role_hint != Some(Role::Observer)
            && self.claim(client_id).is_some()
        {
            Role::Owner
        } else {
            Role::Observer
        }
    }

    pub fn current_owner(&self) -> Option<Uuid> {
        self.owner.map(|lease| lease.client_id)
    }

    pub fn is_current(&self, lease: OwnerLease) -> bool {
        self.owner == Some(lease)
    }

    pub(crate) fn lease(&self) -> Option<OwnerLease> {
        self.owner
    }

    pub(crate) fn claim(&mut self, client_id: Uuid) -> Option<OwnerLease> {
        self.generation = self.generation.checked_add(1)?;
        let lease = OwnerLease {
            client_id,
            generation: self.generation,
        };
        self.owner = Some(lease);
        Some(lease)
    }

    pub(crate) fn release(&mut self, lease: OwnerLease) -> bool {
        if self.owner == Some(lease) {
            self.owner = None;
            true
        } else {
            false
        }
    }
}
