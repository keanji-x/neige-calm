//! Opt-in public ingress and short-lived, owner-approved mobile pairing.
pub use calm_types::mobile_access::MobileStatus;
pub mod funnel;
mod ingress;
pub mod pairing;
pub mod routes;

use crate::auth::SessionStore;
use crate::error::{CalmError, Result};
use axum::Router;
use pairing::PairingState;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Clone)]
pub struct MobileAccess {
    inner: Arc<Inner>,
}

struct Inner {
    pairings: Arc<Mutex<PairingState>>,
    control: tokio::sync::Mutex<funnel::Controller>,
    sessions: SessionStore,
}

impl std::fmt::Debug for MobileAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MobileAccess").finish_non_exhaustive()
    }
}

impl MobileAccess {
    pub fn new(sessions: SessionStore) -> Self {
        Self {
            inner: Arc::new(Inner {
                pairings: Arc::new(Mutex::new(PairingState::default())),
                control: tokio::sync::Mutex::new(funnel::Controller::default()),
                sessions,
            }),
        }
    }

    /// Retain the router outside the controller to avoid an auth/router cycle.
    pub async fn configure(&self, config: funnel::FunnelConfig, public_router: Arc<Router>) {
        *self.inner.control.lock().await = funnel::Controller::configured(config, public_router);
    }

    fn lock(&self) -> Result<MutexGuard<'_, PairingState>> {
        self.inner
            .pairings
            .lock()
            .map_err(|_| CalmError::Internal("Mobile pairing state unavailable".into()))
    }

    pub async fn status(&self) -> Result<MobileStatus> {
        let available = self.inner.control.lock().await.available();
        let mut state = self.lock()?;
        let public_url = state.origin.clone();
        let (pending, devices) = state.list();
        Ok(MobileStatus {
            available,
            public_url,
            pending,
            devices,
        })
    }

    pub async fn enable(&self) -> Result<()> {
        self.inner
            .control
            .lock()
            .await
            .start(self.inner.pairings.clone(), self.inner.sessions.clone())
            .await
    }

    pub async fn disable(&self) -> Result<()> {
        let mut control = self.inner.control.lock().await;
        self.lock()?.disable(&self.inner.sessions);
        control.stop().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AuthConfig, AuthState};

    #[tokio::test]
    async fn mobile_access_releases_configuration_when_routers_are_dropped() {
        let auth = AuthState::new(AuthConfig {
            username: Some("owner".into()),
            password: Some("fixture".into()),
            dev_autologin: false,
            display_name: "Owner".into(),
        });
        let weak = Arc::downgrade(&auth.mobile.inner);
        let router = Arc::new(crate::auth::session_router().with_state(auth.clone()));
        auth.mobile
            .configure(
                funnel::FunnelConfig {
                    executable: "/fixture/tailscale".into(),
                    socket: "/fixture/socket".into(),
                    https_port: 10000,
                },
                router.clone(),
            )
            .await;
        drop(auth);
        drop(router);
        assert!(
            weak.upgrade().is_none(),
            "configuration must not retain its own auth/router state"
        );
    }
}
