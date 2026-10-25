//! Ambient environment allowed across the application-to-child boundary.
//! Provider credentials belong in the caller's explicit EnsureProcRequest.envs.
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
    std::env::vars_os()
        .filter(|(name, _)| name.to_str().is_some_and(|name| ALLOWED.contains(&name)))
        .collect()
}

use calm_session::control::execution::{
    AmbientEnvironment, AmbientProxyPolicy, EnvironmentPreflight, ExecutionSpecification,
    proxy::{PROXY_KEYS, proxy_value_is_secret},
};

/// Spawn-only bytes, transported over the anonymous helper pipe, never a journal field.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct LaunchEnvironment(pub Vec<(Vec<u8>, Vec<u8>)>);

fn ambient_values(input: &EnvironmentPreflight) -> anyhow::Result<Vec<(OsString, OsString)>> {
    let AmbientEnvironment::SupervisorAllowed { proxy_policy } = &input.ambient_environment else {
        return Ok(Vec::new());
    };
    input.validate().map_err(anyhow::Error::msg)?;
    let mut values = allowed_environment();
    values.retain(|(key, _)| {
        !input
            .explicit_environment
            .iter()
            .any(|(explicit, _)| key == explicit.as_str())
    });
    match proxy_policy {
        AmbientProxyPolicy::RejectSecret => anyhow::ensure!(
            !values.iter().any(|(key, value)| key
                .to_str()
                .is_some_and(|key| PROXY_KEYS.contains(&key) && proxy_value_is_secret(key, value))),
            "secret ambient proxy refused"
        ),
        AmbientProxyPolicy::Exclude => {
            values.retain(|(key, _)| !key.to_str().is_some_and(|key| PROXY_KEYS.contains(&key)))
        }
    }
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
    let explicit = || {
        specification
            .environment
            .iter()
            .map(|(key, value)| (key.as_bytes().to_vec(), value.as_bytes().to_vec()))
    };
    // Preserve the released envp ordering (including repeated explicit keys) for None.
    if specification.ambient_environment == AmbientEnvironment::None {
        return Ok(LaunchEnvironment(explicit().collect()));
    }
    // Validate exactly the snapshot used for spawn, without mutating the durable specification.
    let mut values = std::collections::BTreeMap::new();
    for (key, value) in ambient_values(&EnvironmentPreflight::from(specification))? {
        values.insert(key.as_bytes().to_vec(), value.as_bytes().to_vec());
    }
    values.extend(explicit());
    Ok(LaunchEnvironment(values.into_iter().collect()))
}
