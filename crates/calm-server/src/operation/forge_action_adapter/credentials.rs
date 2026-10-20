//! Ephemeral release payload. Never store this buffer in operation state or diagnostics.
use std::ffi::OsString;

use tokio::io::{AsyncWrite, AsyncWriteExt};

use super::FORGE_CREDENTIAL_ENV_KEYS;
use crate::error::{CalmError, Result};

const MAX_PAYLOAD_BYTES: usize = 16 * 1024;

pub(crate) fn render_payload(mut lookup: impl FnMut(&str) -> Option<OsString>) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    for key in FORGE_CREDENTIAL_ENV_KEYS {
        match lookup(key) {
            None => payload.push(b'0'),
            Some(value) => {
                let bytes = value.as_encoded_bytes();
                if bytes.contains(&b'\n') || bytes.contains(&0) {
                    return Err(CalmError::Internal(
                        "forge release credential contains newline or NUL".into(),
                    ));
                }
                if bytes.len() > MAX_PAYLOAD_BYTES {
                    return Err(CalmError::Internal(
                        "forge release credential payload exceeds 16 KiB".into(),
                    ));
                }
                payload.push(b'1');
                payload.extend_from_slice(bytes);
            }
        }
        payload.push(b'\n');
    }
    payload.extend_from_slice(b"go\n");
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(CalmError::Internal(
            "forge release credential payload exceeds 16 KiB".into(),
        ));
    }
    Ok(payload)
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
