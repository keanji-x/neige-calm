//! The access region and the process environment the SDK is allowed to see.
//!
//! The SDK reads configuration implicitly (`longbridge/openapi@v5.2.0`):
//!
//! - `HttpClientConfig::from_oauth` and `Config::from_oauth` call
//!   `dotenv::dotenv()` (`rust/crates/httpclient/src/config.rs:86`,
//!   `rust/src/config.rs:269`). dotenv 0.15 looks for `.env` in the working
//!   directory, then in each parent until the root, and loads the first file
//!   it finds into the process environment (`dotenv-0.15.0/src/find.rs:24-51`).
//! - They then read `LONGBRIDGE_*` variables, falling back to `LONGPORT_*`:
//!   `HTTP_URL` (`httpclient/src/config.rs:7-11,88`), and `QUOTE_WS_URL`,
//!   `LOG_PATH`, `PAPERTRADING` and more (`rust/src/config.rs:172-194`).
//! - The OAuth endpoint switches to staging on `LONGBRIDGE_ENV=staging`
//!   (`rust/crates/oauth/src/client.rs:21-32`).
//!
//! None of that may steer the archive, so [`check`] refuses to start while
//! any `LONGBRIDGE_*`/`LONGPORT_*` variable is set or a `.env` would be
//! loaded, and the archive passes every setting explicitly.
//!
//! One input has no explicit setter: whether the OAuth and default hosts are
//! the mainland ones comes only from `LONGBRIDGE_REGION`, else
//! `LONGPORT_REGION`, else a live probe of `geotest.lbkrs.com`
//! (`rust/crates/geo/src/lib.rs:30-64`). The archive therefore sets
//! `LONGBRIDGE_REGION` itself from the typed `access_region`
//! ([`pin_process_region`]) before any thread starts, so the geo probe never
//! runs.

use std::{
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
};

use serde::Deserialize;

/// The variable the SDK's `is_cn()` reads first.
pub const REGION_ENV: &str = "LONGBRIDGE_REGION";

/// The variable prefixes the SDK reads.
const SDK_PREFIXES: [&str; 2] = ["LONGBRIDGE_", "LONGPORT_"];

/// Which Longbridge access point to use. The SDK distinguishes only the
/// mainland one; every other value means the global hosts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccessRegion {
    Global,
    Cn,
}

impl AccessRegion {
    /// The `LONGBRIDGE_REGION` value: `is_cn()` compares it with `CN`,
    /// ignoring case, and treats anything else as global.
    pub fn env_value(self) -> &'static str {
        match self {
            AccessRegion::Global => "global",
            AccessRegion::Cn => "CN",
        }
    }
}

/// Why the process refuses to start.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Refusal {
    /// Names only; a value can be a credential.
    #[error("refusing to start: the SDK would read {} from the environment; unset them", .0.join(", "))]
    SdkVariables(Vec<String>),
    #[error(
        "refusing to start: the SDK would load {0}/.env into the environment; run from another directory"
    )]
    DotEnv(PathBuf),
    #[error("refusing to start: cannot check {0} for a .env file: {1}")]
    Unreadable(PathBuf, String),
}

/// Refuse to run while the SDK could pick up configuration we did not pass:
/// any variable named `LONGBRIDGE_*` or `LONGPORT_*` (including
/// `LONGBRIDGE_REGION`, which the archive sets itself), or a `.env` file that
/// dotenv would load from `cwd` or a parent. A path dotenv cannot stat (other
/// than "not found") is refused too, rather than guessed.
pub fn check(
    vars: impl IntoIterator<Item = (OsString, OsString)>,
    cwd: &Path,
) -> Result<(), Refusal> {
    let mut stray: Vec<String> = vars
        .into_iter()
        .map(|(name, _value)| name.to_string_lossy().into_owned())
        .filter(|name| SDK_PREFIXES.iter().any(|p| name.starts_with(p)))
        .collect();
    if !stray.is_empty() {
        stray.sort();
        return Err(Refusal::SdkVariables(stray));
    }
    for dir in cwd.ancestors() {
        let candidate = dir.join(".env");
        match fs::metadata(&candidate) {
            Ok(meta) if meta.is_file() => return Err(Refusal::DotEnv(dir.to_owned())),
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(Refusal::Unreadable(candidate, err.to_string())),
        }
    }
    Ok(())
}

/// Set `LONGBRIDGE_REGION` for this process.
///
/// # Safety
///
/// `std::env::set_var` is unsound while another thread may read or write the
/// environment. The caller must be single-threaded: call this from a sync
/// `main` after [`check`] and before building the async runtime or spawning
/// any thread. The tracing subscriber spawns no thread.
pub unsafe fn pin_process_region(region: AccessRegion) {
    // SAFETY: the caller guarantees no other thread exists yet.
    unsafe { std::env::set_var(REGION_ENV, region.env_value()) };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs.iter().map(|(k, v)| (k.into(), v.into())).collect()
    }

    #[test]
    fn region_values_match_the_sdk_comparison() {
        assert!(AccessRegion::Cn.env_value().eq_ignore_ascii_case("cn"));
        assert!(!AccessRegion::Global.env_value().eq_ignore_ascii_case("cn"));
    }

    #[test]
    fn every_sdk_variable_is_refused_by_name_only() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(check(vars(&[("PATH", "/bin")]), dir.path()), Ok(()));
        for bad in [
            ("LONGBRIDGE_REGION", "CN"),
            ("LONGBRIDGE_HTTP_URL", "http://x"),
            ("LONGPORT_LOG_PATH", "/var/log/x"),
            ("LONGBRIDGE_ENV", "staging"),
            ("LONGBRIDGE_ACCESS_TOKEN", "CANARY-VALUE"),
        ] {
            let err = check(vars(&[bad]), dir.path()).unwrap_err();
            assert_eq!(err, Refusal::SdkVariables(vec![bad.0.to_owned()]));
            assert!(!err.to_string().contains(bad.1), "{err}");
        }
    }

    #[test]
    fn a_dotenv_file_in_an_ancestor_is_refused_but_a_directory_is_not() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("a/b");
        fs::create_dir_all(&nested).unwrap();
        // dotenv skips a `.env` that is not a file.
        fs::create_dir(nested.join(".env")).unwrap();
        assert_eq!(check([], &nested), Ok(()));
        fs::write(root.path().join(".env"), "LONGBRIDGE_HTTP_URL=x\n").unwrap();
        assert_eq!(
            check([], &nested),
            Err(Refusal::DotEnv(root.path().to_owned()))
        );
    }
}
