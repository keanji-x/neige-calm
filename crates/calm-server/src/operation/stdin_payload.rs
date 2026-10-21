//! Ephemeral stdin release protocol. Never persist payloads or include values in diagnostics.
use std::ffi::OsString;

use crate::error::{CalmError, Result};

const MAX_PAYLOAD_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy)]
pub(crate) enum KeySource {
    Kernel,
    // ACP registration consumes this policy in C4.3.
    #[allow(dead_code)]
    Operator,
}

/// The one predicate for keys interpolated into shell source.
pub(crate) fn validate_key(key: &str, source: KeySource) -> Result<()> {
    let mut bytes = key.bytes();
    let identifier = bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    if !identifier
        || matches!(
            key,
            "IFS" | "ENV" | "PS1" | "PS2" | "PS4" | "OPTIND" | "OPTARG"
        )
        || key.starts_with("neige_")
        || (matches!(source, KeySource::Operator) && (key == "PATH" || key.starts_with("NEIGE_")))
    {
        return Err(CalmError::BadRequest("invalid stdin payload key".into()));
    }
    Ok(())
}

/// A fixed-order table whose keys have passed the shell-source boundary.
/// Private storage prevents callers from bypassing validation.
pub(crate) struct PayloadKeys<'a> {
    keys: &'a [&'a str],
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PayloadError {
    NewlineOrNul,
    TooLarge,
}

impl<'a> PayloadKeys<'a> {
    pub(crate) fn new(keys: &'a [&'a str], source: KeySource) -> Result<Self> {
        for key in keys {
            validate_key(key, source)?;
        }
        Ok(Self { keys })
    }

    /// Build and validate the entire buffer before any transport can write a byte.
    pub(crate) fn render(
        &self,
        mut lookup: impl FnMut(&str) -> Option<OsString>,
    ) -> std::result::Result<Vec<u8>, PayloadError> {
        let mut payload = Vec::new();
        for key in self.keys {
            match lookup(key) {
                None => payload.push(b'0'),
                Some(value) => {
                    let bytes = value.as_encoded_bytes();
                    if bytes.contains(&b'\n') || bytes.contains(&0) {
                        return Err(PayloadError::NewlineOrNul);
                    }
                    if bytes.len() > MAX_PAYLOAD_BYTES {
                        return Err(PayloadError::TooLarge);
                    }
                    payload.push(b'1');
                    payload.extend_from_slice(bytes);
                }
            }
            payload.push(b'\n');
        }
        payload.extend_from_slice(b"go\n");
        if payload.len() > MAX_PAYLOAD_BYTES {
            return Err(PayloadError::TooLarge);
        }
        Ok(payload)
    }

    /// Values are read as data, never inserted into source, sourced, or evaluated.
    pub(crate) fn read_prefix(&self) -> String {
        let mut script = String::new();
        // Retain the original protocol's private variable spellings so extraction
        // also preserves forge's wrapper bytes. They carry no consumer policy.
        for key in self.keys {
            script.push_str(&format!(
                "IFS= read -r neige_forge_credential || exit 75\n\
                 case \"$neige_forge_credential\" in\n\
                 0) unset {key} ;;\n\
                 1*) export {key}=\"${{neige_forge_credential#1}}\" ;;\n\
                 *) exit 75 ;;\n\
                 esac\n"
            ));
        }
        script.push_str("unset neige_forge_credential\n");
        script.push_str("IFS= read -r neige_forge_go || exit 75\n");
        script.push_str("[ \"$neige_forge_go\" = go ] || exit 75\n");
        script.push_str("unset neige_forge_go\n");
        script
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c4_payload_key_rejects_non_identifier() {
        for key in [
            "has space",
            "X-Y",
            "$(touch injected)",
            "X;exit",
            "",
            "1X",
            "é",
        ] {
            for source in [KeySource::Kernel, KeySource::Operator] {
                assert!(PayloadKeys::new(&[key], source).is_err(), "{key}");
            }
        }
    }

    #[test]
    fn c4_payload_key_rejects_reserved_and_internal_names() {
        for key in [
            "IFS",
            "ENV",
            "PS1",
            "PS2",
            "PS4",
            "OPTIND",
            "OPTARG",
            "neige_forge_credential",
            "neige_forge_go",
            "neige_future",
        ] {
            for source in [KeySource::Kernel, KeySource::Operator] {
                assert!(PayloadKeys::new(&[key], source).is_err(), "{key}");
            }
        }
        for key in ["PATH", "NEIGE_X", "NEIGE_MCP_TOKEN"] {
            assert!(
                PayloadKeys::new(&[key], KeySource::Operator).is_err(),
                "{key}"
            );
            assert!(PayloadKeys::new(&[key], KeySource::Kernel).is_ok(), "{key}");
        }
        for key in ["X", "_", "lower_case2", "HTTP_PROXY", "no_proxy", "neigeX"] {
            assert!(
                PayloadKeys::new(&[key], KeySource::Operator).is_ok(),
                "{key}"
            );
        }
    }

    #[test]
    fn stdin_payload_encodes_fixed_order_and_presence() {
        let keys = PayloadKeys::new(&["UNSET", "EMPTY", "VALUE"], KeySource::Kernel).unwrap();
        let bytes = keys
            .render(|key| match key {
                "EMPTY" => Some("".into()),
                "VALUE" => Some(" \\ '$();\t".into()),
                _ => None,
            })
            .unwrap();
        assert_eq!(bytes, b"0\n1\n1 \\ '$();\t\ngo\n");
    }

    #[test]
    fn stdin_payload_size_includes_framing() {
        let keys = PayloadKeys::new(&["TOKEN"], KeySource::Kernel).unwrap();
        assert_eq!(
            keys.render(|_| Some("x".repeat(MAX_PAYLOAD_BYTES - 5).into()))
                .unwrap()
                .len(),
            MAX_PAYLOAD_BYTES
        );
        assert_eq!(
            keys.render(|_| Some("x".repeat(MAX_PAYLOAD_BYTES - 4).into())),
            Err(PayloadError::TooLarge)
        );
    }
}
