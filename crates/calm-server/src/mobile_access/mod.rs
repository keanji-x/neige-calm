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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Clone)]
pub struct MobileAccess {
    inner: Arc<Inner>,
}

struct Inner {
    pairings: Arc<Mutex<PairingState>>,
    control: tokio::sync::Mutex<funnel::Controller>,
    sessions: SessionStore,
    unavailable: AtomicBool,
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
                unavailable: AtomicBool::new(false),
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

    pub fn mark_unavailable(&self) {
        self.inner.unavailable.store(true, Ordering::Release);
    }

    fn lock(&self) -> Result<MutexGuard<'_, PairingState>> {
        self.inner
            .pairings
            .lock()
            .map_err(|_| CalmError::Internal("Mobile pairing state unavailable".into()))
    }

    pub async fn status(&self) -> Result<MobileStatus> {
        if self.inner.unavailable.load(Ordering::Acquire) {
            return Err(CalmError::BadRequest("Private remote access could not initialize. Local Neige is still available. Check the private state and socket permissions, then restart Neige; the stored enabled state was left untouched.".into()));
        }
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
    async fn mobile_access_unavailable_returns_explicit_settings_error() {
        use axum::{
            body::Body,
            http::{Request, StatusCode, header},
        };
        use tower::ServiceExt;
        let auth = AuthState::new(AuthConfig {
            username: Some("owner".into()),
            password: Some("fixture".into()),
            dev_autologin: false,
            display_name: "Owner".into(),
        });
        auth.mobile.mark_unavailable();
        let session = auth
            .sessions
            .create(crate::auth::SessionAuthority::PasswordLogin);
        let router = routes::management_router()
            .layer(axum::middleware::from_fn_with_state(
                auth.clone(),
                crate::auth::require_session,
            ))
            .with_state(auth);
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/mobile/access")
                    .header(
                        header::COOKIE,
                        format!("{}={session}", crate::auth::SESSION_COOKIE),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        assert!(text.contains("Private remote access could not initialize"));
        assert!(text.contains("Local Neige is still available"));
        assert!(
            !text.contains("desiredEnabled"),
            "unknown intent must not become a fabricated disabled status"
        );
    }

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
