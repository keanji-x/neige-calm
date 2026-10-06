//! Per-peer limit on failed password logins (#2132).
//!
//! Threat: a network neighbour that can reach the login route (LAN, tailnet) guessing the owner
//! password online. After [`FREE_FAILURES`] consecutive failures from one IP, that IP is refused
//! WITHOUT its credentials being evaluated for a window that doubles per further failure from
//! [`FIRST_LOCKOUT`] up to [`MAX_LOCKOUT`]. The cap is deliberately low: a neighbour who fails on
//! purpose locks the owner out of that address for at most a minute, never longer.
//!
//! The key is the host the TCP peer address names, not the address itself: every loopback address
//! (all of 127.0.0.0/8, `::1`, and their `::ffff:` mappings) is one peer, and an IPv6 address is keyed
//! by its /64. Any local process can bind any 127.x source address, and one IPv6 interface can pick
//! any address in its prefix, so keying by address would hand such a peer a fresh budget per address
//! and let [`MAX_PEERS`] of them push its throttled record out.
//!
//! Not covered: state is process-local and in memory (a restart forgets it, there is no persistent
//! lockout). Traffic that arrives through a local reverse proxy
//! (e.g. `tailscale serve` forwarding to the listener) shares the proxy's address, so every client
//! behind it shares one budget: a failing client throttles all of them, the owner included, for at
//! most [`MAX_LOCKOUT`]. Forwarded-for headers are not trusted, since any client can write them.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Consecutive failures from one peer that are answered normally; the next attempt is throttled.
pub const FREE_FAILURES: u32 = 5;
/// Window armed by the [`FREE_FAILURES`]-th failure; each further failure doubles it.
pub const FIRST_LOCKOUT: Duration = Duration::from_secs(2);
/// Upper bound of one window, so a neighbour cannot lock the owner out for long.
pub const MAX_LOCKOUT: Duration = Duration::from_secs(60);
/// A peer with no failure for this long starts again from zero.
pub const FORGET_AFTER: Duration = Duration::from_secs(15 * 60);
/// Most peers tracked at once; past it the stalest record is dropped.
pub const MAX_PEERS: usize = 1024;

/// Source of "now"; production reads the monotonic clock, tests advance their own.
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// What one login attempt came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginAttempt {
    Accepted,
    Rejected,
    /// Refused before the credentials were looked at.
    Throttled {
        retry_after: Duration,
    },
}

#[derive(Debug, Clone, Copy)]
struct Failures {
    count: u32,
    last: Instant,
    locked_until: Option<Instant>,
}

#[derive(Clone)]
pub struct LoginThrottle {
    peers: Arc<Mutex<HashMap<IpAddr, Failures>>>,
    clock: Clock,
}

impl std::fmt::Debug for LoginThrottle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginThrottle")
            .field("tracked_peers", &self.tracked_peers())
            .finish_non_exhaustive()
    }
}

impl Default for LoginThrottle {
    fn default() -> Self {
        Self::with_clock(Arc::new(Instant::now))
    }
}

impl LoginThrottle {
    pub fn with_clock(clock: Clock) -> Self {
        Self {
            peers: Arc::default(),
            clock,
        }
    }

    /// Run `verify` for `peer` unless that peer is in a throttle window. The check, the
    /// verification and the bookkeeping happen under one lock, so a burst of parallel attempts
    /// from one peer cannot all slip in before the first failure is recorded. `verify` runs under
    /// that lock, so it must be cheap: hash before calling this, compare inside.
    pub fn attempt(&self, peer: IpAddr, verify: impl FnOnce() -> bool) -> LoginAttempt {
        let now = (self.clock)();
        let peer = peer_key(peer);
        let mut peers = self.peers.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(until) = peers.get(&peer).and_then(|f| f.locked_until)
            && now < until
        {
            return LoginAttempt::Throttled {
                retry_after: until - now,
            };
        }
        if verify() {
            peers.remove(&peer);
            return LoginAttempt::Accepted;
        }
        record_failure(&mut peers, peer, now);
        LoginAttempt::Rejected
    }

    /// Number of peers currently holding a failure record.
    pub fn tracked_peers(&self) -> usize {
        self.peers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

/// The host an address names: the record key. `::ffff:a.b.c.d` is `a.b.c.d`; every loopback
/// address is `127.0.0.1`; an IPv6 address is its /64.
fn peer_key(peer: IpAddr) -> IpAddr {
    match peer.to_canonical() {
        loopback if loopback.is_loopback() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(v6) => IpAddr::V6(Ipv6Addr::from_bits(v6.to_bits() & (u128::MAX << 64))),
        v4 => v4,
    }
}

fn record_failure(peers: &mut HashMap<IpAddr, Failures>, peer: IpAddr, now: Instant) {
    let fresh = Failures {
        count: 0,
        last: now,
        locked_until: None,
    };
    let stale = |f: &Failures| now.saturating_duration_since(f.last) >= FORGET_AFTER;
    if !peers.contains_key(&peer) && peers.len() >= MAX_PEERS {
        peers.retain(|_, f| !stale(f));
        if peers.len() >= MAX_PEERS
            && let Some(oldest) = peers.iter().min_by_key(|(_, f)| f.last).map(|(ip, _)| *ip)
        {
            peers.remove(&oldest);
        }
    }
    let entry = peers.entry(peer).or_insert(fresh);
    if stale(entry) {
        *entry = fresh;
    }
    entry.count = entry.count.saturating_add(1);
    entry.last = now;
    if let Some(extra) = entry.count.checked_sub(FREE_FAILURES) {
        let window = FIRST_LOCKOUT
            .saturating_mul(1u32 << extra.min(16))
            .min(MAX_LOCKOUT);
        entry.locked_until = Some(now + window);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manual_clock() -> (Clock, Arc<Mutex<Instant>>) {
        let now = Arc::new(Mutex::new(Instant::now()));
        let read = now.clone();
        (Arc::new(move || *read.lock().unwrap()), now)
    }

    #[test]
    fn windows_double_from_the_first_lockout_and_stop_at_the_cap() {
        let (clock, now) = manual_clock();
        let throttle = LoginThrottle::with_clock(clock);
        let peer: IpAddr = "100.64.0.9".parse().unwrap();
        let mut windows = Vec::new();
        for _ in 0..FREE_FAILURES - 1 {
            assert_eq!(throttle.attempt(peer, || false), LoginAttempt::Rejected);
        }
        for _ in 0..8 {
            assert_eq!(throttle.attempt(peer, || false), LoginAttempt::Rejected);
            let LoginAttempt::Throttled { retry_after } = throttle.attempt(peer, || true) else {
                panic!("a failure past the free budget arms a window");
            };
            windows.push(retry_after.as_secs());
            *now.lock().unwrap() += retry_after;
        }
        assert_eq!(windows, [2, 4, 8, 16, 32, 60, 60, 60]);
    }

    #[test]
    fn an_ipv4_mapped_address_shares_its_ipv4_budget() {
        let throttle = LoginThrottle::default();
        for _ in 0..FREE_FAILURES {
            throttle.attempt("::ffff:100.64.0.9".parse().unwrap(), || false);
        }
        assert!(matches!(
            throttle.attempt("100.64.0.9".parse().unwrap(), || true),
            LoginAttempt::Throttled { .. }
        ));
    }

    #[test]
    fn a_quiet_peer_is_forgotten() {
        let (clock, now) = manual_clock();
        let throttle = LoginThrottle::with_clock(clock);
        let peer: IpAddr = "100.64.0.9".parse().unwrap();
        for _ in 0..FREE_FAILURES {
            throttle.attempt(peer, || false);
        }
        *now.lock().unwrap() += FORGET_AFTER;
        assert_eq!(throttle.attempt(peer, || false), LoginAttempt::Rejected);
        assert_eq!(
            throttle.attempt(peer, || true),
            LoginAttempt::Accepted,
            "one failure after a quiet spell starts a new count"
        );
    }
}
