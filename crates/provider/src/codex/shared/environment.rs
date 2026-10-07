//! Explicit launch environment for a shared Codex process.
use std::{ffi::OsStr, path::Path};
use tokio::process::Command;

/// Ambient env keys forwarded verbatim into the spawned shared codex app-server; everything
/// else in the parent env is dropped by `env_clear()`, computed keys (including `PATH`) are set in
/// [`SpawnEnvironment::apply`].
pub const SPAWN_ENV_PASSTHROUGH: &[&str] = &[
    // default-home fallback + `~` expansion in config paths; forwarded to MCP children
    "HOME",
    // codex's own child allow-lists forward these
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "TZ",
    "TMPDIR",
    "TEMP",
    "TMP",
    // reqwest env-proxy autodetect honors these for API traffic
    "NO_PROXY",
    "no_proxy",
    "ALL_PROXY",
    "all_proxy",
    // TLS custom CA (SSL_CERT_DIR unused)
    "CODEX_CA_CERTIFICATE",
    "SSL_CERT_FILE",
    // diagnostics
    "RUST_LOG",
    "LOG_FORMAT",
    "RUST_BACKTRACE",
    // API-key-mode auth fallbacks; prod uses auth.json, kept so an API-key deployment
    // doesn't silently break. Still an allow-list.
    "OPENAI_API_KEY",
    "CODEX_API_KEY",
    "CODEX_ACCESS_TOKEN",
    "OPENAI_ORGANIZATION",
    "OPENAI_PROJECT",
];

/// Host-resolved launch inputs. Application context is supplied by the owning kernel,
/// and secrets must not appear in Debug output (there is intentionally no Debug impl).
pub struct SpawnEnvironment<'a> {
    pub path: &'a OsStr,
    pub home: &'a Path,
    pub http_proxy: Option<&'a str>,
    pub https_proxy: Option<&'a str>,
    pub application_env: &'a [(&'a str, &'a str)],
}
impl SpawnEnvironment<'_> {
    pub fn apply(&self, command: &mut Command) {
        command.env_clear();
        for key in SPAWN_ENV_PASSTHROUGH {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command.env("PATH", self.path).env("CODEX_HOME", self.home);
        // Inputs are already resolved from the same settings snapshot; no ambient fallback.
        for (upper, lower, value) in
            super::resolved_proxy_env_pairs(self.http_proxy, self.https_proxy, |_| None)
        {
            command.env(upper, &value).env(lower, value);
        }
        command.envs(self.application_env.iter().copied());
    }
}
