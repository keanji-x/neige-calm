//! Opt-in public ingress and short-lived, owner-approved mobile pairing.
pub use calm_types::mobile_access::MobileStatus;
pub mod funnel;
mod ingress;
pub mod pairing;
pub mod private_tailnet;
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
    private: tokio::sync::Mutex<Option<private_tailnet::Controller>>,
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
                private: tokio::sync::Mutex::new(None),
            }),
        }
    }

    /// Retain the router outside the controller to avoid an auth/router cycle.
    pub async fn configure(&self, config: funnel::FunnelConfig, public_router: Arc<Router>) {
        *self.inner.control.lock().await = funnel::Controller::configured(config, public_router);
    }

    pub async fn configure_private(
        &self,
        config: private_tailnet::PrivateTailnetConfig,
        router: Arc<Router>,
    ) -> anyhow::Result<private_tailnet::PrivateIngress> {
        let (controller, ingress) = private_tailnet::Controller::configured(
            config,
            router,
            self.inner.pairings.clone(),
            self.inner.sessions.clone(),
        )
        .await?;
        *self.inner.private.lock().await = Some(controller);
        Ok(ingress)
    }

    pub async fn tailnet_action(
        &self,
        action: calm_types::tailnet::TailnetAction,
    ) -> Result<calm_types::tailnet::TailnetResponse> {
        let private = self.inner.private.lock().await;
        let controller = private
            .as_ref()
            .ok_or_else(|| CalmError::BadRequest("Private Tailnet is not configured".into()))?;
        controller.request(action).await
    }

    /// Kernel shutdown revokes its ingress, but does not change app-owned
    /// desiredEnabled or stop the independent Tailnet child.
    pub async fn shutdown(&self) -> Result<()> {
        self.lock()?.disable(&self.inner.sessions);
        self.inner.private.lock().await.take();
        self.inner.control.lock().await.stop().await
    }

    fn lock(&self) -> Result<MutexGuard<'_, PairingState>> {
        self.inner
            .pairings
            .lock()
            .map_err(|_| CalmError::Internal("Mobile pairing state unavailable".into()))
    }

    pub async fn status(&self) -> Result<MobileStatus> {
        let tailnet = if let Some(private) = self.inner.private.lock().await.as_ref() {
            Some(private.status().await?)
        } else {
            None
        };
        let funnel = self.inner.control.lock().await.available();
        let available = funnel || tailnet.is_some();
        let provider = if tailnet.is_some() {
            calm_types::mobile_access::MobileProvider::PrivateTailnet
        } else if funnel {
            calm_types::mobile_access::MobileProvider::Funnel
        } else {
            calm_types::mobile_access::MobileProvider::Unavailable
        };
        let mut state = self.lock()?;
        let public_url = state.origin.clone();
        let (pending, devices) = state.list();
        Ok(MobileStatus {
            provider,
            tailnet,
            available,
            public_url,
            pending,
            devices,
        })
    }

    pub async fn enable(&self) -> Result<()> {
        if let Some(private) = self.inner.private.lock().await.as_ref() {
            private
                .request(calm_types::tailnet::TailnetAction::Enable)
                .await?;
            return Ok(());
        }
        self.inner
            .control
            .lock()
            .await
            .start(self.inner.pairings.clone(), self.inner.sessions.clone())
            .await
    }

    pub async fn disable(&self) -> Result<()> {
        self.lock()?.disable(&self.inner.sessions);
        if let Some(private) = self.inner.private.lock().await.as_ref() {
            private
                .request(calm_types::tailnet::TailnetAction::Disable)
                .await?;
            return Ok(());
        }
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
