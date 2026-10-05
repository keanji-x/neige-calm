use crate::auth::SessionStore;
use crate::error::{CalmError, Result};
use calm_types::mobile_access::{
    PairedDevice, PairingClaim, PairingClaimed, PairingRedeem, PendingPair,
};
use rand::{RngCore, rngs::OsRng};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const PAIR_TTL: Duration = Duration::from_secs(180);
const MAX_PENDING: usize = 8;
const MAX_DEVICES: usize = 16;
mod enrollment;
use enrollment::ScanInvitation;

fn secret() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn digest(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

// Owner-side refusals answer 404 or 409, never 401: the browser reads any 401 as a lost session
// and signs the owner out (#2131). Only the phone-side claim and redeem answer 401, because there
// the caller really holds no valid ticket or secret.

fn access_off() -> CalmError {
    CalmError::Conflict("Mobile access is off; enable it first".into())
}

/// The scan slot a create reserved was cancelled, disabled or expired before it was published.
pub(super) fn scan_slot_gone() -> CalmError {
    CalmError::Conflict(
        "This invitation expired or was cancelled before it was ready; create a new one".into(),
    )
}

struct Claim {
    secret_hash: [u8; 32],
    device_name: String,
    code: String,
    approved: bool,
}

struct Invitation {
    id: String,
    ticket_hash: [u8; 32],
    expires: Instant,
    claim: Option<Claim>,
}

struct Device {
    public: PairedDevice,
    session: String,
    /// The legacy pairing this device redeemed, so a repeated approve of it is answered as done.
    pairing: Option<String>,
}

/// Every grant/revoke transition holds this one lock, including session creation.
/// No caller may create a paired session outside `redeem`.
pub(super) struct PairingState {
    pub origin: Option<String>,
    pending: HashMap<String, Invitation>,
    devices: HashMap<String, Device>,
    scan: Option<ScanInvitation>,
    scan_window: Instant,
    scan_requests: u32,
    scan_creates: u32,
    pub connections: CancellationToken,
}

impl Default for PairingState {
    fn default() -> Self {
        Self {
            origin: None,
            pending: HashMap::new(),
            devices: HashMap::new(),
            scan: None,
            scan_window: Instant::now(),
            scan_requests: 0,
            scan_creates: 0,
            connections: CancellationToken::new(),
        }
    }
}

impl PairingState {
    pub fn disable(&mut self, sessions: &SessionStore) {
        self.origin = None;
        self.pending.clear();
        self.scan = None;
        for (_, device) in self.devices.drain() {
            sessions.remove(&device.session);
        }
        self.disconnect();
    }

    fn disconnect(&mut self) {
        self.connections.cancel();
        self.connections = CancellationToken::new();
    }

    pub fn list(&mut self) -> (Vec<PendingPair>, Vec<PairedDevice>) {
        self.expire();
        let pending = self
            .pending
            .values()
            .filter_map(|row| {
                row.claim
                    .as_ref()
                    .filter(|claim| !claim.approved)
                    .map(|claim| PendingPair {
                        id: row.id.clone(),
                        device_name: claim.device_name.clone(),
                        verification_code: claim.code.clone(),
                    })
            })
            .collect();
        (
            pending,
            self.devices
                .values()
                .map(|device| device.public.clone())
                .collect(),
        )
    }

    fn expire(&mut self) {
        self.pending.retain(|_, row| row.expires > Instant::now());
        if self
            .scan
            .as_ref()
            .is_some_and(|row| row.expires <= Instant::now())
        {
            self.scan = None;
        }
    }

    pub fn invite(&mut self) -> Result<(String, String, u64)> {
        self.expire();
        let origin = self.origin.as_ref().ok_or_else(access_off)?;
        // A retry after a lost answer must not leave a second live ticket: like a new scan
        // enrollment, a new invitation replaces the earlier ones no phone has claimed.
        self.pending.retain(|_, row| row.claim.is_some());
        if self.pending.len() >= MAX_PENDING || self.devices.len() >= MAX_DEVICES {
            return Err(CalmError::BadRequest(
                "Pairing limit reached; revoke a device or wait for an invitation to expire".into(),
            ));
        }
        let ticket = secret();
        let id = Uuid::new_v4().to_string();
        let payload = format!("{origin}/mobile/pair#v1.{ticket}");
        self.pending.insert(
            id.clone(),
            Invitation {
                id: id.clone(),
                ticket_hash: digest(&ticket),
                expires: Instant::now() + PAIR_TTL,
                claim: None,
            },
        );
        Ok((id, payload, PAIR_TTL.as_secs()))
    }

    pub fn claim(&mut self, request: PairingClaim) -> Result<PairingClaimed> {
        self.expire();
        if self.origin.is_none() || request.ticket.len() != 64 {
            return Err(CalmError::Unauthorized);
        }
        let name = request.device_name.trim();
        if name.is_empty() || name.len() > 80 || name.chars().any(char::is_control) {
            return Err(CalmError::BadRequest(
                "Device name must contain 1–80 printable bytes".into(),
            ));
        }
        let hash = digest(&request.ticket);
        let row = self
            .pending
            .values_mut()
            .find(|row| row.ticket_hash == hash && row.claim.is_none())
            .ok_or(CalmError::Unauthorized)?;
        let secret = secret();
        let code = format!("{:06}", OsRng.next_u32() % 1_000_000);
        row.claim = Some(Claim {
            secret_hash: digest(&secret),
            device_name: name.into(),
            code: code.clone(),
            approved: false,
        });
        Ok(PairingClaimed {
            id: row.id.clone(),
            secret,
            verification_code: code,
        })
    }

    pub fn approve(&mut self, id: &str) -> Result<()> {
        self.expire();
        if self.origin.is_none() {
            return Err(access_off());
        }
        if self
            .devices
            .values()
            .any(|device| device.pairing.as_deref() == Some(id))
        {
            return Ok(());
        }
        let claim = self
            .pending
            .get_mut(id)
            .and_then(|row| row.claim.as_mut())
            .ok_or_else(|| {
                CalmError::NotFound(
                    "No pending pairing request with this id; it expired or was never claimed"
                        .into(),
                )
            })?;
        claim.approved = true;
        Ok(())
    }

    pub fn redeem(
        &mut self,
        request: PairingRedeem,
        sessions: &SessionStore,
    ) -> Result<Option<String>> {
        self.expire();
        if self.origin.is_none() || request.secret.len() != 64 {
            return Err(CalmError::Unauthorized);
        }
        let row = self
            .pending
            .get(&request.id)
            .ok_or(CalmError::Unauthorized)?;
        let claim = row.claim.as_ref().ok_or(CalmError::Unauthorized)?;
        if claim.secret_hash != digest(&request.secret) {
            return Err(CalmError::Unauthorized);
        }
        if !claim.approved {
            return Ok(None);
        }
        if self.devices.len() >= MAX_DEVICES {
            return Err(CalmError::BadRequest("Device limit reached".into()));
        }
        let name = claim.device_name.clone();
        self.pending.remove(&request.id);
        let session = sessions.create(crate::auth::SessionAuthority::PairedDevice);
        let id = Uuid::new_v4().to_string();
        self.devices.insert(
            id.clone(),
            Device {
                public: PairedDevice {
                    id,
                    device_name: name,
                },
                session: session.clone(),
                pairing: Some(request.id),
            },
        );
        Ok(Some(session))
    }

    pub fn revoke(&mut self, id: &str, sessions: &SessionStore) -> Result<()> {
        let device = self
            .devices
            .remove(id)
            .ok_or_else(|| CalmError::NotFound("No paired device with this id".into()))?;
        sessions.remove(&device.session);
        if self
            .scan
            .as_ref()
            .is_some_and(|row| row.session.as_ref() == Some(&device.session))
        {
            self.scan = None;
        }
        // Close live streams as well as rejecting subsequent requests. Other
        // devices reconnect with their still-valid sessions.
        self.disconnect();
        Ok(())
    }
}

#[cfg(test)]
mod tests;
