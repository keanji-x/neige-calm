//! Caller-owned native admission, shared by native transports.
use crate::error::{CalmError, Result};
use futures_util::future::BoxFuture;
use std::sync::Arc;

/// Retain the caller's error in its owning layer; the provider sees only refusal.
pub(crate) struct NativePromptAdmission<'a> {
    gate: provider::send_boundary::PreSendGate<'a>,
    #[cfg(feature = "fixtures")]
    fixture: Arc<dyn Fn() -> BoxFuture<'a, Result<()>> + Send + Sync + 'a>,
    refusal: Arc<std::sync::Mutex<Option<CalmError>>>,
}

impl<'a> NativePromptAdmission<'a> {
    pub(crate) fn new(
        live_issuer: impl Fn() -> Result<()> + Send + Sync + 'a,
        revalidate: impl Fn() -> BoxFuture<'a, Result<()>> + Send + Sync + 'a,
    ) -> Self {
        // The async owner must finish all reads and its commit before the live
        // issuer is checked. No caller can accidentally reverse this ordering.
        let live_issuer = Arc::new(live_issuer);
        let admission = Arc::new(move || {
            let future = revalidate();
            let live_issuer = live_issuer.clone();
            Box::pin(async move {
                future.await?;
                live_issuer()
            }) as BoxFuture<'a, Result<()>>
        });
        #[cfg(feature = "fixtures")]
        let fixture = admission.clone();
        let refusal = Arc::new(std::sync::Mutex::new(None));
        let captured = refusal.clone();
        let gate = provider::send_boundary::PreSendGate::new(move || {
            let future = admission();
            let captured = captured.clone();
            Box::pin(async move {
                future.await.map_err(|error| {
                    *captured.lock().unwrap() = Some(error);
                    provider::send_boundary::SendRefusal(
                        "caller admission refused before send".into(),
                    )
                })
            })
        });
        Self {
            gate,
            refusal,
            #[cfg(feature = "fixtures")]
            fixture,
        }
    }

    // The fixture branch performs only local simulated effects. Native code
    // has no callable admission API; its only consumer is a provider gate.
    #[cfg(feature = "fixtures")]
    pub(crate) async fn check_fixture(&self) -> Result<()> {
        (self.fixture)().await
    }

    pub(crate) fn gate(&self) -> &provider::send_boundary::PreSendGate<'a> {
        &self.gate
    }

    pub(crate) fn take_refusal(&self) -> Option<CalmError> {
        self.refusal.lock().unwrap().take()
    }

    pub(crate) fn finish<T, E: Into<CalmError>>(
        &self,
        result: std::result::Result<T, E>,
    ) -> Result<T> {
        result.map_err(|error| self.take_refusal().unwrap_or_else(|| error.into()))
    }
}
