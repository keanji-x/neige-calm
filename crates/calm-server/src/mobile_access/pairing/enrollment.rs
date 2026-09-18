use super::*;
use calm_types::enrollment::{EnrollmentClaim, EnrollmentClaimed, EnrollmentRedeem};

pub(super) struct ScanInvitation {
    id: String,
    generation: String,
    ticket_hash: [u8; 32],
    pub(super) expires: Instant,
    ready: bool,
    claim: Option<ScanClaim>,
    pub(super) session: Option<String>,
}
struct ScanClaim {
    attempt_id: String,
    secret_hash: [u8; 32],
    claim_id: String,
    device_name: String,
}

fn id_valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}
fn secret_valid(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

impl PairingState {
    fn scan_admit(&mut self, create: bool) -> Result<()> {
        self.expire();
        if self.scan_window.elapsed() >= Duration::from_secs(60) {
            self.scan_window = Instant::now();
            self.scan_requests = 0;
            self.scan_creates = 0;
        }
        let (count, limit) = if create {
            (&mut self.scan_creates, 5)
        } else {
            (&mut self.scan_requests, 120)
        };
        if *count >= limit {
            return Err(CalmError::BadRequest(
                "Scan request limit reached; wait before retrying".into(),
            ));
        }
        *count += 1;
        if self.origin.is_none() {
            return Err(CalmError::Unauthorized);
        }
        Ok(())
    }

    /// Creation is reserved under the grant lock before any external await.
    /// Disable/cancel removes this generation so a late key cannot publish it.
    pub fn begin_scan(&mut self) -> Result<(String, String, String, Option<String>)> {
        self.scan_admit(true)?;
        if self.devices.len() >= MAX_DEVICES {
            return Err(CalmError::BadRequest("Device limit reached".into()));
        }
        let previous = self.scan.take().map(|row| row.id);
        let id = Uuid::new_v4().to_string();
        let generation = Uuid::new_v4().to_string();
        let ticket = secret();
        self.scan = Some(ScanInvitation {
            id: id.clone(),
            generation: generation.clone(),
            ticket_hash: digest(&ticket),
            expires: Instant::now() + Duration::from_secs(30),
            ready: false,
            claim: None,
            session: None,
        });
        Ok((id, generation, ticket, previous))
    }

    pub fn finish_scan(
        &mut self,
        id: &str,
        generation: &str,
        origin: &str,
        ttl: Duration,
    ) -> Result<()> {
        self.expire();
        if self.origin.as_deref() != Some(origin) || ttl.is_zero() || ttl > PAIR_TTL {
            return Err(CalmError::Unauthorized);
        }
        let row = self
            .scan
            .as_mut()
            .filter(|r| r.id == id && r.generation == generation && !r.ready)
            .ok_or(CalmError::Unauthorized)?;
        row.expires = Instant::now() + ttl;
        row.ready = true;
        Ok(())
    }

    pub fn cancel_scan(&mut self, id: &str) {
        if self.scan.as_ref().is_some_and(|r| r.id == id) {
            self.scan = None;
        }
    }

    pub fn claim_scan(&mut self, request: EnrollmentClaim) -> Result<EnrollmentClaimed> {
        self.scan_admit(false)?;
        if !id_valid(&request.enrollment_id)
            || !id_valid(&request.attempt_id)
            || !secret_valid(&request.ticket)
            || !secret_valid(&request.attempt_secret)
        {
            return Err(CalmError::Unauthorized);
        }
        let name = request.device_name.trim();
        if name.is_empty() || name.len() > 80 || name.chars().any(char::is_control) {
            return Err(CalmError::BadRequest(
                "Device name must contain 1-80 printable bytes".into(),
            ));
        }
        let row = self
            .scan
            .as_mut()
            .filter(|r| {
                r.ready && r.id == request.enrollment_id && r.ticket_hash == digest(&request.ticket)
            })
            .ok_or(CalmError::Unauthorized)?;
        if let Some(claim) = &row.claim {
            if claim.attempt_id != request.attempt_id
                || claim.secret_hash != digest(&request.attempt_secret)
            {
                return Err(CalmError::Unauthorized);
            }
        } else {
            row.claim = Some(ScanClaim {
                attempt_id: request.attempt_id.clone(),
                secret_hash: digest(&request.attempt_secret),
                claim_id: Uuid::new_v4().to_string(),
                device_name: name.into(),
            });
        }
        let claim = row.claim.as_ref().ok_or(CalmError::Unauthorized)?;
        Ok(EnrollmentClaimed {
            enrollment_id: row.id.clone(),
            attempt_id: claim.attempt_id.clone(),
            claim_id: claim.claim_id.clone(),
        })
    }

    pub fn redeem_scan(
        &mut self,
        request: &EnrollmentRedeem,
        sessions: &SessionStore,
    ) -> Result<String> {
        self.scan_admit(false)?;
        if !id_valid(&request.enrollment_id)
            || !id_valid(&request.attempt_id)
            || !secret_valid(&request.attempt_secret)
        {
            return Err(CalmError::Unauthorized);
        }
        let row = self
            .scan
            .as_mut()
            .filter(|r| r.ready && r.id == request.enrollment_id)
            .ok_or(CalmError::Unauthorized)?;
        let claim = row
            .claim
            .as_ref()
            .filter(|c| {
                c.attempt_id == request.attempt_id
                    && c.secret_hash == digest(&request.attempt_secret)
            })
            .ok_or(CalmError::Unauthorized)?;
        if let Some(session) = &row.session {
            // A lost response may repeat its cookie, never create a replacement
            // after logout, session expiry, device revocation or host disable.
            return sessions
                .get(session)
                .filter(|s| s.authority == crate::auth::SessionAuthority::PairedDevice)
                .map(|_| session.clone())
                .ok_or(CalmError::Unauthorized);
        }
        if self.devices.len() >= MAX_DEVICES {
            return Err(CalmError::BadRequest("Device limit reached".into()));
        }
        let session = sessions.create(crate::auth::SessionAuthority::PairedDevice);
        let id = Uuid::new_v4().to_string();
        self.devices.insert(
            id.clone(),
            Device {
                public: PairedDevice {
                    id,
                    device_name: claim.device_name.clone(),
                },
                session: session.clone(),
            },
        );
        row.session = Some(session.clone());
        Ok(session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ready(state: &mut PairingState) -> (String, String) {
        let (id, generation, ticket, _) = state.begin_scan().unwrap();
        state
            .finish_scan(
                &id,
                &generation,
                "https://fixture.ts.net",
                Duration::from_secs(180),
            )
            .unwrap();
        (id, ticket)
    }
    fn state() -> PairingState {
        PairingState {
            origin: Some("https://fixture.ts.net".into()),
            ..Default::default()
        }
    }
    fn claim(id: &str, ticket: &str) -> EnrollmentClaim {
        EnrollmentClaim {
            enrollment_id: id.into(),
            ticket: ticket.into(),
            device_name: "Phone".into(),
            attempt_id: "attempt".into(),
            attempt_secret: "a".repeat(64),
        }
    }
    fn redeem(id: &str) -> EnrollmentRedeem {
        EnrollmentRedeem {
            enrollment_id: id.into(),
            attempt_id: "attempt".into(),
            attempt_secret: "a".repeat(64),
        }
    }

    #[test]
    fn scan_v1_and_v2_tickets_are_disjoint() {
        let mut state = state();
        let (id, ticket) = ready(&mut state);
        assert!(
            state
                .claim(PairingClaim {
                    ticket: ticket.clone(),
                    device_name: "Phone".into()
                })
                .is_err()
        );
        state.claim_scan(claim(&id, &ticket)).unwrap();
        assert!(
            state
                .redeem(
                    PairingRedeem {
                        id: id.clone(),
                        secret: "a".repeat(64)
                    },
                    &SessionStore::new()
                )
                .is_err()
        );
        let (legacy, payload, _) = state.invite().unwrap();
        assert!(
            state
                .claim_scan(claim(&id, payload.split("#v1.").nth(1).unwrap()))
                .is_err()
        );
        assert!(
            state
                .claim_scan(claim(&legacy, payload.split("#v1.").nth(1).unwrap()))
                .is_err()
        );
    }
    #[test]
    fn scan_retry_returns_exactly_one_live_session() {
        let mut state = state();
        let sessions = SessionStore::new();
        let (id, ticket) = ready(&mut state);
        let a = state.claim_scan(claim(&id, &ticket)).unwrap();
        let b = state.claim_scan(claim(&id, &ticket)).unwrap();
        assert_eq!(a.claim_id, b.claim_id);
        let mut other = claim(&id, &ticket);
        other.attempt_id = "other".into();
        assert!(state.claim_scan(other).is_err());
        let first = state.redeem_scan(&redeem(&id), &sessions).unwrap();
        let second = state.redeem_scan(&redeem(&id), &sessions).unwrap();
        assert_eq!(first, second);
        assert_eq!(state.devices.len(), 1);
        sessions.remove(&first);
        assert!(state.redeem_scan(&redeem(&id), &sessions).is_err());
        assert_eq!(state.devices.len(), 1);
    }
    #[test]
    fn scan_revoke_disable_and_cancel_fence_redeem() {
        for action in ["revoke", "disable", "cancel"] {
            let mut state = state();
            let sessions = SessionStore::new();
            let (id, ticket) = ready(&mut state);
            state.claim_scan(claim(&id, &ticket)).unwrap();
            let session = state.redeem_scan(&redeem(&id), &sessions).unwrap();
            match action {
                "revoke" => {
                    let device = state.devices.keys().next().unwrap().clone();
                    state.revoke(&device, &sessions).unwrap();
                }
                "disable" => state.disable(&sessions),
                _ => state.cancel_scan(&id),
            }
            state.origin = Some("https://fixture.ts.net".into());
            assert!(state.redeem_scan(&redeem(&id), &sessions).is_err());
            if action != "cancel" {
                assert!(sessions.get(&session).is_none());
            }
        }
    }
    #[test]
    fn scan_late_issuer_cannot_reactivate_cancelled_generation() {
        let mut state = state();
        let (id, generation, _, _) = state.begin_scan().unwrap();
        state.cancel_scan(&id);
        assert!(
            state
                .finish_scan(
                    &id,
                    &generation,
                    "https://fixture.ts.net",
                    Duration::from_secs(100)
                )
                .is_err()
        );
        let (id, generation, _, _) = state.begin_scan().unwrap();
        state.disable(&SessionStore::new());
        state.origin = Some("https://fixture.ts.net".into());
        assert!(
            state
                .finish_scan(
                    &id,
                    &generation,
                    "https://fixture.ts.net",
                    Duration::from_secs(100)
                )
                .is_err()
        );
    }

    #[test]
    fn scan_expiry_limits_and_strict_secrets_are_enforced() {
        let mut state = state();
        let (id, ticket) = ready(&mut state);
        let mut bad = claim(&id, &ticket);
        bad.attempt_secret = "G".repeat(64);
        assert!(state.claim_scan(bad).is_err());
        state.scan.as_mut().unwrap().expires = Instant::now() - Duration::from_secs(1);
        assert!(state.claim_scan(claim(&id, &ticket)).is_err());
        for _ in 1..5 {
            state.begin_scan().unwrap();
        }
        assert!(state.begin_scan().is_err());
    }
}
