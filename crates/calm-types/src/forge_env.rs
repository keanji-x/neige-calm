/// The passthrough keys that ARE the operator's forge identity. Also the denylist for `cli_query.env_allow`; only genuine credentials
/// belong here, because a key added to this list retroactively invalidates already-installed manifests on boot re-parse.
pub const FORGE_CREDENTIAL_ENV_KEYS: &[&str] = &[
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "SSH_AUTH_SOCK",
    "GIT_SSH_COMMAND",
];

/// Passthrough keys that are **not** credentials; holding one grants nothing.
pub const FORGE_NONCREDENTIAL_ENV_KEYS: &[&str] = &["GH_HOST", "NO_PROXY", "no_proxy"];

/// Composed rather than re-typed, so there is exactly one place each key lives.
pub fn forge_passthrough_env_keys() -> impl Iterator<Item = &'static str> {
    FORGE_CREDENTIAL_ENV_KEYS
        .iter()
        .chain(FORGE_NONCREDENTIAL_ENV_KEYS.iter())
        .copied()
}
