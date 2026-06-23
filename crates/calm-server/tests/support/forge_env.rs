use std::ffi::{OsStr, OsString};
use std::sync::OnceLock;

use tempfile::TempDir;

pub static FORGE_ENV_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

pub struct ForgeTestEnv {
    pub _path_dir: TempDir,
    pub _results_dir: TempDir,
    pub _trusted: EnvGuard,
    pub _results: EnvGuard,
    pub _path: EnvGuard,
}

pub struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    pub fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var_os(key);
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.previous.as_ref() {
            Some(previous) => unsafe { std::env::set_var(self.key, previous) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}
