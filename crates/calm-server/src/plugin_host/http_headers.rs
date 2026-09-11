//! Private literal request headers shared by check, install and enable.
use std::collections::{BTreeMap, HashSet};

#[derive(Clone, Default)]
pub struct HttpHeaders(BTreeMap<String, String>);

impl std::fmt::Debug for HttpHeaders {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HttpHeaders(<private>)")
    }
}

pub fn validate_header_names<'a>(names: impl Iterator<Item = &'a str>) -> Result<(), String> {
    let mut seen = HashSet::new();
    for name in names {
        let key = name.to_ascii_lowercase();
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
        {
            return Err("HTTP header names must be valid field names".into());
        }
        if !seen.insert(key.clone()) {
            return Err("HTTP header names must be unique regardless of case".into());
        }
        if matches!(
            key.as_str(),
            "host"
                | "content-length"
                | "transfer-encoding"
                | "connection"
                | "trailer"
                | "te"
                | "upgrade"
                | "proxy-authorization"
                | "proxy-connection"
                | "content-type"
                | "accept"
                | "mcp-session-id"
                | "mcp-protocol-version"
        ) {
            return Err("Transport-managed and proxy headers cannot be configured".into());
        }
    }
    if seen.len() > 32 {
        return Err("At most 32 HTTP headers may be configured".into());
    }
    Ok(())
}

impl HttpHeaders {
    pub fn parse(headers: BTreeMap<String, String>) -> Result<Self, String> {
        validate_header_names(headers.keys().map(String::as_str))?;
        if headers
            .iter()
            .map(|(k, v)| k.len() + v.len())
            .sum::<usize>()
            > 16 * 1024
        {
            return Err("HTTP headers exceed 16 KiB".into());
        }
        for (name, value) in &headers {
            if value.trim() != value {
                return Err(
                    "HTTP header values must not have leading or trailing whitespace".into(),
                );
            }
            if value.contains("${") || value.contains("{{") {
                return Err("HTTP header variables must be resolved before use".into());
            }
            // All custom header values are private. Reuse the transport's
            // existing credential constraints: registering arbitrary short
            // values would rewrite protocol keys and ordinary tool data.
            let private = if name.eq_ignore_ascii_case("authorization") {
                value
                    .split_once(' ')
                    .map_or(value.as_str(), |(_, token)| token)
            } else {
                value.as_str()
            };
            super::HttpCredential::parse(private)
                .map_err(|why| format!("HTTP header value {why}"))?;
            // An Authorization scheme is the sole supported space-separated
            // prefix. Its bytes also need to be a valid HTTP token.
            if name.eq_ignore_ascii_case("authorization")
                && let Some((scheme, _)) = value.split_once(' ')
                && (scheme.is_empty()
                    || !scheme
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)))
            {
                return Err("Authorization scheme must be an HTTP token".into());
            }
        }
        Ok(Self(headers))
    }

    pub fn pairs(&self) -> &BTreeMap<String, String> {
        &self.0
    }

    /// Register both whole values and the credential in Authorization schemes.
    /// Every accepted custom value obeys the same redaction constraints as API keys.
    pub fn private_values(&self) -> Vec<String> {
        let mut values = Vec::new();
        for (name, value) in &self.0 {
            if !value.is_empty() {
                values.push(value.clone());
            }
            if name.eq_ignore_ascii_case("authorization")
                && let Some((_, token)) = value.split_once(' ')
                && !token.trim().is_empty()
            {
                values.push(token.trim().to_string());
            }
        }
        values
    }
}
