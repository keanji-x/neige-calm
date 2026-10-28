//! Ambient environment allowed across the application-to-child boundary.
//! Provider credentials belong in the caller's explicit EnsureProcRequest.envs.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::ffi::OsString;

const ALLOWED: &[&str] = &[
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "PATH",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "LC_COLLATE",
    "LC_NUMERIC",
    "LC_TIME",
    "TZ",
    "TMPDIR",
    "TMP",
    "TEMP",
    "TERM",
    "COLORTERM",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "DBUS_SESSION_BUS_ADDRESS",
    "SSH_AUTH_SOCK",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "CURL_CA_BUNDLE",
    "REQUESTS_CA_BUNDLE",
    "NODE_EXTRA_CA_CERTS",
];

pub fn allowed_environment() -> Vec<(OsString, OsString)> {
    values_of(ALLOWED.iter().copied())
}

/// The keys a supervisor-allowed ambient environment carries under `policy`: the PTY spawn's
/// table, without proxy keys under `Exclude`. Callers that declare `SupervisorAllowed` read this
/// rather than keep a copy of the table.
pub fn ambient_keys(policy: &AmbientProxyPolicy) -> impl Iterator<Item = &'static str> + '_ {
    ALLOWED.iter().copied().filter(move |key| match policy {
        AmbientProxyPolicy::RejectSecret => true,
        AmbientProxyPolicy::Exclude => !PROXY_KEYS.contains(key),
    })
}

fn values_of<'a>(keys: impl Iterator<Item = &'a str>) -> Vec<(OsString, OsString)> {
    let keys: Vec<&str> = keys.collect();
    std::env::vars_os()
        .filter(|(name, _)| name.to_str().is_some_and(|name| keys.contains(&name)))
        .collect()
}

use calm_session::control::execution::{
    AmbientEnvironment, AmbientProxyPolicy, EnvironmentPreflight, ExecutionSpecification,
    proxy::{PROXY_KEYS, proxy_value_is_secret},
};

/// Spawn-only bytes, transported over the anonymous helper pipe, never a journal field.
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct LaunchEnvironment(Vec<(String, String)>);
impl LaunchEnvironment {
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub(crate) fn decode(self) -> anyhow::Result<Vec<(OsString, OsString)>> {
        use std::os::unix::ffi::OsStringExt;
        self.0
            .into_iter()
            .map(|(k, v)| {
                Ok((
                    OsString::from_vec(STANDARD.decode(k)?),
                    OsString::from_vec(STANDARD.decode(v)?),
                ))
            })
            .collect()
    }
}

fn ambient_values(input: &EnvironmentPreflight) -> anyhow::Result<Vec<(OsString, OsString)>> {
    let AmbientEnvironment::SupervisorAllowed { proxy_policy } = &input.ambient_environment else {
        return Ok(Vec::new());
    };
    input.validate().map_err(anyhow::Error::msg)?;
    let mut values = values_of(ambient_keys(proxy_policy));
    values.retain(|(key, _)| {
        !input
            .explicit_environment
            .iter()
            .any(|(explicit, _)| key == explicit.as_str())
    });
    // `Exclude` already left every proxy key out of `values`.
    anyhow::ensure!(
        !values.iter().any(|(key, value)| key
            .to_str()
            .is_some_and(|key| PROXY_KEYS.contains(&key) && proxy_value_is_secret(key, value))),
        "secret ambient proxy refused"
    );
    Ok(values)
}

pub(crate) fn preflight(input: &EnvironmentPreflight) -> anyhow::Result<()> {
    ambient_values(input).map(|_| ())
}

pub(crate) fn launch_environment(
    specification: &ExecutionSpecification,
) -> anyhow::Result<LaunchEnvironment> {
    use std::os::unix::ffi::OsStrExt;
    specification
        .validate_environment()
        .map_err(anyhow::Error::msg)?;
    // Only the ambient delta crosses the helper pipe. Explicit values already live
    // in the specification, preserving None's released helper plan size and envp.
    Ok(LaunchEnvironment(
        ambient_values(&EnvironmentPreflight::from(specification))?
            .into_iter()
            .map(|(k, v)| (STANDARD.encode(k.as_bytes()), STANDARD.encode(v.as_bytes())))
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_environment_ambient_keys_are_the_pty_table() {
        let all: Vec<_> = ambient_keys(&AmbientProxyPolicy::RejectSecret).collect();
        assert_eq!(all, ALLOWED);
        assert!(PROXY_KEYS.iter().all(|key| ALLOWED.contains(key)));
        let excluded: Vec<_> = ambient_keys(&AmbientProxyPolicy::Exclude).collect();
        let expected: Vec<_> = ALLOWED
            .iter()
            .copied()
            .filter(|key| !PROXY_KEYS.contains(key))
            .collect();
        assert_eq!(excluded, expected);
    }
}
