//! #1780 preview gateway: a fixed pool of LAN ports, each reverse-proxying to one registered
//! loopback dev server so a report can embed it. The registry is in-memory and keyed
//! `(track_id, key)`; after a restart the agent re-registers.
//!
//! Refusing calm's own listen port as a target is one hop only: a registered dev-calm port, or a
//! dev server proxying to calm, still reaches the loopback-only internal routes (`actor.rs`
//! `require_loopback_connect_info`). Registrants must not register such ports.

pub mod gateway;

use crate::config::Config;
use crate::ids::TrackId;
use std::collections::HashMap;
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Most ports one pool may hold; each is a listener bound for the whole process lifetime.
pub const MAX_POOL_PORTS: u16 = 16;

/// `CALM_PREVIEW_PORTS`, an inclusive range of unprivileged ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewPorts {
    first: u16,
    last: u16,
}

impl PreviewPorts {
    /// Clap value parser for `first-last`.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let (first, last) = raw
            .trim()
            .split_once('-')
            .ok_or_else(|| format!("`{raw}` is not a `first-last` port range"))?;
        let port = |s: &str| {
            s.trim()
                .parse::<u16>()
                .map_err(|_| format!("`{s}` in `{raw}` is not a port"))
        };
        let (first, last) = (port(first)?, port(last)?);
        if first > last {
            return Err(format!("`{raw}` is reversed"));
        }
        if first < 1024 {
            return Err(format!("`{raw}` includes privileged ports (< 1024)"));
        }
        if last - first >= MAX_POOL_PORTS {
            return Err(format!("`{raw}` exceeds {MAX_POOL_PORTS} ports"));
        }
        Ok(Self { first, last })
    }

    pub fn contains(&self, port: u16) -> bool {
        (self.first..=self.last).contains(&port)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PreviewError {
    #[error("preview gateway disabled (CALM_PREVIEW_PORTS is unset)")]
    Disabled,
    #[error("preview pool full ({ports} ports); held by: {holders}")]
    PoolFull { ports: usize, holders: String },
    #[error("target port {port} refused: {reason}")]
    TargetRefused { port: u16, reason: &'static str },
}

#[derive(Debug, Clone)]
pub struct PreviewEntry {
    pub track_id: TrackId,
    pub key: String,
    pub title: String,
    pub target_port: u16,
    /// Cancelled on unregister and on re-register to another target; closes open WS tunnels.
    pub tunnels: CancellationToken,
}

/// Pool port → registration. Disabled is an empty pool, not a separate state.
pub struct PreviewRegistry {
    pool: Vec<u16>,
    /// Never proxy targets: calm's own port (the gateway connects from loopback, which would open
    /// the loopback-only internal routes) and every pool port (a gateway looping into itself).
    refused_targets: Vec<u16>,
    slots: Mutex<HashMap<u16, PreviewEntry>>,
}

impl PreviewRegistry {
    pub fn disabled() -> Self {
        Self {
            pool: Vec::new(),
            refused_targets: Vec::new(),
            slots: Mutex::new(HashMap::new()),
        }
    }

    pub fn new(ports: PreviewPorts, calm_port: u16) -> Self {
        let pool: Vec<u16> = (ports.first..=ports.last).collect();
        let mut refused_targets = pool.clone();
        refused_targets.push(calm_port);
        Self {
            pool,
            refused_targets,
            slots: Mutex::new(HashMap::new()),
        }
    }

    /// Boot construction; refuses a pool that overlaps `CALM_LISTEN` or `CALM_ALLOWED_ORIGIN`
    /// (a preview page on calm's trusted origin would carry calm's cookie authority).
    pub fn from_config(cfg: &Config) -> anyhow::Result<Self> {
        let Some(ports) = cfg.preview_ports else {
            return Ok(Self::disabled());
        };
        let calm_port = listen_port(&cfg.listen)?;
        anyhow::ensure!(
            calm_port != 0,
            "CALM_PREVIEW_PORTS needs a fixed CALM_LISTEN port, not 0"
        );
        anyhow::ensure!(
            !ports.contains(calm_port),
            "CALM_PREVIEW_PORTS contains the CALM_LISTEN port {calm_port}"
        );
        if let Some(origin) = &cfg.allowed_origin
            && let Some(port) = origin_port(origin)
        {
            anyhow::ensure!(
                !ports.contains(port),
                "CALM_ALLOWED_ORIGIN {origin} is on preview pool port {port}"
            );
        }
        Ok(Self::new(ports, calm_port))
    }

    pub fn pool(&self) -> &[u16] {
        &self.pool
    }

    /// Returns the pool port; re-registering an existing `(track_id, key)` keeps its port.
    pub fn register(
        &self,
        track_id: &TrackId,
        key: &str,
        title: &str,
        target_port: u16,
    ) -> Result<u16, PreviewError> {
        if self.pool.is_empty() {
            return Err(PreviewError::Disabled);
        }
        let refused = |reason| PreviewError::TargetRefused {
            port: target_port,
            reason,
        };
        if target_port < 1024 {
            return Err(refused("privileged port"));
        }
        if self.refused_targets.contains(&target_port) {
            return Err(refused("calm's own listen or preview port"));
        }
        let mut slots = self.slots.lock().expect("preview registry poisoned");
        let held = slots
            .iter()
            .find(|(_, e)| e.track_id == *track_id && e.key == key)
            .map(|(port, _)| *port);
        let port = match held.or_else(|| self.pool.iter().copied().find(|p| !slots.contains_key(p)))
        {
            Some(port) => port,
            None => {
                let mut holders: Vec<_> = slots
                    .iter()
                    .map(|(port, e)| format!("{port}: track {} key {}", e.track_id, e.key))
                    .collect();
                holders.sort();
                return Err(PreviewError::PoolFull {
                    ports: self.pool.len(),
                    holders: holders.join(", "),
                });
            }
        };
        let tunnels = match slots.get(&port) {
            Some(old) if old.target_port == target_port => old.tunnels.clone(),
            Some(old) => {
                old.tunnels.cancel();
                CancellationToken::new()
            }
            None => CancellationToken::new(),
        };
        let entry = PreviewEntry {
            track_id: track_id.clone(),
            key: key.to_owned(),
            title: title.to_owned(),
            target_port,
            tunnels,
        };
        slots.insert(port, entry);
        Ok(port)
    }

    /// Frees and returns the port held by `(track_id, key)`, if any.
    pub fn unregister(&self, track_id: &TrackId, key: &str) -> Option<u16> {
        let mut slots = self.slots.lock().expect("preview registry poisoned");
        let port = slots
            .iter()
            .find(|(_, e)| e.track_id == *track_id && e.key == key)
            .map(|(port, _)| *port)?;
        slots.remove(&port)?.tunnels.cancel();
        Some(port)
    }

    /// Frees every registration of a deleted track and closes its tunnels; returns the freed
    /// ports. Called after a track or area delete commits: the track's Planner, the only
    /// unregistering identity, is gone with it.
    pub fn release_track(&self, track_id: &TrackId) -> Vec<u16> {
        let mut slots = self.slots.lock().expect("preview registry poisoned");
        let mut freed = Vec::new();
        slots.retain(|port, e| {
            let keep = e.track_id != *track_id;
            if !keep {
                e.tunnels.cancel();
                freed.push(*port);
            }
            keep
        });
        freed.sort_unstable();
        freed
    }

    pub fn lookup(&self, port: u16) -> Option<PreviewEntry> {
        let slots = self.slots.lock().expect("preview registry poisoned");
        slots.get(&port).cloned()
    }

    /// `(pool port, entry)` for every registration of `track_id`, by port.
    pub fn for_track(&self, track_id: &TrackId) -> Vec<(u16, PreviewEntry)> {
        let slots = self.slots.lock().expect("preview registry poisoned");
        let mut held: Vec<_> = slots
            .iter()
            .filter(|(_, e)| e.track_id == *track_id)
            .map(|(port, e)| (*port, e.clone()))
            .collect();
        held.sort_by_key(|(port, _)| *port);
        held
    }
}

/// Host part of `CALM_LISTEN`, the interface the pool binds on (IPv6 brackets stripped).
pub fn listen_host(listen: &str) -> &str {
    let host = listen.rsplit_once(':').map_or(listen, |(host, _)| host);
    host.strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host)
}

fn listen_port(listen: &str) -> anyhow::Result<u16> {
    listen
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("CALM_LISTEN `{listen}` has no port"))
}

/// Explicit port of a normalized origin; a default port (80/443) can never be in the pool.
fn origin_port(origin: &str) -> Option<u16> {
    let authority = origin.split_once("://").map_or(origin, |(_, rest)| rest);
    let (_, port) = authority.rsplit_once(':')?;
    port.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn track(id: &str) -> TrackId {
        TrackId::from(id)
    }

    #[test]
    fn port_range_parse_accepts_and_rejects() {
        assert_eq!(
            PreviewPorts::parse("4050-4057"),
            Ok(PreviewPorts {
                first: 4050,
                last: 4057
            })
        );
        for bad in ["", "4057-4050", "4050-4066", "1000-1003", "4050", "a-b"] {
            assert!(PreviewPorts::parse(bad).is_err(), "{bad:?} must be refused");
        }
        assert!(
            PreviewPorts::parse("4050-4065").is_ok(),
            "16 ports is the cap"
        );
    }

    fn boot(args: &[&str]) -> anyhow::Result<Vec<u16>> {
        let cfg = Config::parse_from(["calm-server"].iter().chain(args));
        PreviewRegistry::from_config(&cfg).map(|reg| reg.pool().to_vec())
    }

    #[test]
    fn boot_refuses_pool_overlapping_listen_or_allowed_origin() {
        let listen = ["--preview-ports", "4050-4057", "--listen", "0.0.0.0:4052"];
        assert!(
            boot(&listen)
                .unwrap_err()
                .to_string()
                .contains("CALM_LISTEN")
        );
        let origin = [
            "--preview-ports",
            "4050-4057",
            "--allowed-origin",
            "http://localhost:4055",
        ];
        let err = boot(&origin).unwrap_err().to_string();
        assert!(err.contains("CALM_ALLOWED_ORIGIN"), "{err}");
        let ok = [
            "--preview-ports",
            "4050-4057",
            "--allowed-origin",
            "http://localhost:5175",
        ];
        assert_eq!(boot(&ok).unwrap(), (4050..=4057).collect::<Vec<_>>());
        assert!(boot(&[]).unwrap().is_empty());
        let ephemeral = ["--preview-ports", "4050-4057", "--listen", "127.0.0.1:0"];
        assert!(boot(&ephemeral).unwrap_err().to_string().contains("not 0"));
    }

    /// Through the boot path, so the listen port reaching the registry is what is pinned.
    #[test]
    fn booted_registry_refuses_the_listen_port_as_target() {
        let args = ["--preview-ports", "4050-4057", "--listen", "127.0.0.1:4040"];
        let cfg = Config::parse_from(["calm-server"].iter().chain(&args));
        let reg = PreviewRegistry::from_config(&cfg).unwrap();
        assert!(matches!(
            reg.register(&track("t"), "fe", "FE", 4040),
            Err(PreviewError::TargetRefused { port: 4040, .. })
        ));
    }

    #[test]
    fn register_refuses_calm_port_pool_ports_and_privileged_targets() {
        let reg = PreviewRegistry::new(PreviewPorts::parse("4050-4051").unwrap(), 4040);
        for target in [4040, 4050, 4051, 80] {
            assert!(
                matches!(
                    reg.register(&track("t"), "fe", "FE", target),
                    Err(PreviewError::TargetRefused { .. })
                ),
                "target {target} must be refused"
            );
        }
        assert_eq!(reg.register(&track("t"), "fe", "FE", 5173), Ok(4050));
    }

    #[test]
    fn registry_keeps_port_on_reregister_fills_and_frees() {
        let reg = PreviewRegistry::new(PreviewPorts::parse("4050-4051").unwrap(), 4040);
        assert_eq!(reg.register(&track("t1"), "fe", "FE", 5173), Ok(4050));
        assert_eq!(reg.register(&track("t2"), "fe", "FE", 5174), Ok(4051));
        assert_eq!(reg.register(&track("t1"), "fe", "FE v2", 5175), Ok(4050));
        assert_eq!(reg.lookup(4050).unwrap().target_port, 5175);
        let full = reg.register(&track("t1"), "api", "API", 8080).unwrap_err();
        assert_eq!(
            full.to_string(),
            "preview pool full (2 ports); held by: 4050: track t1 key fe, 4051: track t2 key fe"
        );
        assert_eq!(reg.unregister(&track("t2"), "fe"), Some(4051));
        assert!(reg.lookup(4051).is_none());
        assert_eq!(reg.register(&track("t1"), "api", "API", 8080), Ok(4051));
    }

    #[test]
    fn disabled_registry_refuses_registration() {
        let reg = PreviewRegistry::disabled();
        assert_eq!(
            reg.register(&track("t"), "fe", "FE", 5173),
            Err(PreviewError::Disabled)
        );
    }
}
