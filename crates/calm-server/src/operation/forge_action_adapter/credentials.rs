//! Ephemeral release payload. Never store this buffer in operation state or diagnostics.
use std::ffi::OsString;

use tokio::io::{AsyncWrite, AsyncWriteExt};

use super::FORGE_CREDENTIAL_ENV_KEYS;
use crate::error::{CalmError, Result};

use crate::operation::stdin_payload::{KeySource, PayloadError, PayloadKeys};

pub(super) fn payload_keys() -> Result<PayloadKeys<'static>> {
    PayloadKeys::new(FORGE_CREDENTIAL_ENV_KEYS, KeySource::Kernel)
}

pub(crate) fn render_payload(lookup: impl FnMut(&str) -> Option<OsString>) -> Result<Vec<u8>> {
    payload_keys()?.render(lookup).map_err(|error| {
        CalmError::Internal(
            match error {
                PayloadError::NewlineOrNul => "forge release credential contains newline or NUL",
                PayloadError::TooLarge => "forge release credential payload exceeds 16 KiB",
            }
            .into(),
        )
    })
}

/// Validate the entire payload before giving the transport its first byte.
#[doc(hidden)]
pub async fn write_credential_payload(
    stdin: &mut (impl AsyncWrite + Unpin),
    lookup: impl FnMut(&str) -> Option<OsString>,
) -> Result<()> {
    let payload = render_payload(lookup)?;
    stdin
        .write_all(&payload)
        .await
        .map_err(|e| CalmError::Internal(format!("forge release write failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c4_payload_forge_constant_keys_satisfy_predicate() {
        assert!(payload_keys().is_ok());
        for key in FORGE_CREDENTIAL_ENV_KEYS {
            assert!(crate::operation::stdin_payload::validate_key(key, KeySource::Kernel).is_ok());
        }
    }
}
