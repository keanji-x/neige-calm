//! Directory-scoped development environments; values are ephemeral launch inputs.
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs, io,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

const LOAD_BOUND: Duration = Duration::from_secs(10);

/// Complete launch environment. Values must not enter logs or persisted metadata.
pub struct Environment(BTreeMap<OsString, OsString>);

impl Environment {
    /// Keep only names admitted by the execution owner, without exposing values.
    pub fn retain(&mut self, mut admitted: impl FnMut(&OsStr) -> bool) {
        self.0.retain(|key, _| admitted(key));
    }

    /// Read one variable without exposing the environment through formatting.
    pub fn get(&self, key: &OsStr) -> Option<&OsStr> {
        self.0.get(key).map(OsString::as_os_str)
    }

    /// Transfer the final environment to its execution owner.
    pub fn into_values(self) -> BTreeMap<OsString, OsString> {
        self.0
    }
}

/// Safe failure categories; external output never appears in these errors.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EnvironmentError {
    #[error("cannot inspect workspace environment source ({0:?})")]
    SourceUnavailable(io::ErrorKind),
    #[error("direnv is required for this workspace environment")]
    MissingDirenv,
    #[error("workspace environment loading failed (exit {0:?})")]
    Failed(Option<i32>),
    #[error("direnv returned an invalid environment")]
    InvalidOutput,
    #[error("workspace environment loading timed out")]
    TimedOut,
    #[error("cannot capture workspace environment ({0:?})")]
    CaptureFailed(io::ErrorKind),
}

/// Resolve an authorized directory's environment over the caller's explicit base.
pub fn resolve(
    source: &Path,
    mut base: BTreeMap<OsString, OsString>,
) -> Result<Environment, EnvironmentError> {
    let source = source
        .canonicalize()
        .map_err(|error| EnvironmentError::SourceUnavailable(error.kind()))?;
    let metadata =
        fs::metadata(&source).map_err(|error| EnvironmentError::SourceUnavailable(error.kind()))?;
    if !metadata.is_dir() {
        return Err(EnvironmentError::SourceUnavailable(
            io::ErrorKind::NotADirectory,
        ));
    }
    let mut found = false;
    for directory in source.ancestors() {
        match fs::symlink_metadata(directory.join(".envrc")) {
            Ok(_) => {
                found = true;
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(EnvironmentError::SourceUnavailable(error.kind())),
        }
    }
    if !found {
        return Ok(Environment(base));
    }

    // Direnv owns discovery, trust, and evaluation. Its output and diagnostics
    // are secret-bearing; only the typed result crosses this boundary.
    let output = crate::bounded_git::output(
        Command::new("direnv")
            .args(["export", "json"])
            .current_dir(&source)
            .env_clear()
            .envs(&base),
        Instant::now() + LOAD_BOUND,
    )
    .map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => EnvironmentError::MissingDirenv,
        io::ErrorKind::TimedOut => EnvironmentError::TimedOut,
        kind => EnvironmentError::CaptureFailed(kind),
    })?;
    if !output.status.success() {
        return Err(EnvironmentError::Failed(output.status.code()));
    }
    if output.stdout.is_empty() {
        return Ok(Environment(base));
    }
    let delta: BTreeMap<String, Option<String>> =
        serde_json::from_slice(&output.stdout).map_err(|_| EnvironmentError::InvalidOutput)?;
    for (key, value) in delta {
        if key.is_empty()
            || key.contains(['=', '\0'])
            || value.as_ref().is_some_and(|value| value.contains('\0'))
        {
            return Err(EnvironmentError::InvalidOutput);
        }
        match value {
            Some(value) => {
                base.insert(key.into(), value.into());
            }
            None => {
                base.remove(OsStr::new(&key));
            }
        }
    }
    Ok(Environment(base))
}

#[cfg(test)]
mod tests;
