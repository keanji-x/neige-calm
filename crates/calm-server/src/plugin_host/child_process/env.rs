//! The one base allowlist of the server's environment for a child that runs code the server does
//! not own (repository hooks, filters and drivers; plugin-supplied forge argv). Such a child starts
//! from a cleared environment and inherits only these variables plus its own named additions.

use std::ffi::OsString;

/// Inherited by every allowlisted child: `PATH` resolves binaries, `HOME` locates the user's
/// configuration (git's global config, `gh`'s credentials).
pub(crate) const BASE_INHERITED_ENV: [&str; 2] = ["PATH", "HOME"];

/// The `(key, value)` pairs of [`BASE_INHERITED_ENV`] and then of `extra` that are set in the
/// server's environment; an unset key is left out, never defaulted.
pub(crate) fn inherited_env(extra: &[&'static str]) -> Vec<(&'static str, OsString)> {
    BASE_INHERITED_ENV
        .iter()
        .chain(extra)
        .filter_map(|key| std::env::var_os(key).map(|value| (*key, value)))
        .collect()
}
