use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::sync::OnceLock;

use tempfile::TempDir;

pub static FORGE_ENV_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

/// A test binary's project fixtures share a private direnv trust store, including
/// when two callers or a replacement kernel outlive one another.
pub struct ProjectEnvironment {
    environment: Vec<(&'static str, OsString)>,
    _guards: Vec<EnvGuard>,
}

impl ProjectEnvironment {
    pub fn for_project(root: &Path, body: &str) -> Self {
        let environment = Self::new();
        environment.declare(root, body);
        environment
    }

    pub fn new() -> Self {
        static ROOT: OnceLock<TempDir> = OnceLock::new();
        let root = ROOT.get_or_init(|| tempfile::tempdir().expect("project environment tempdir"));
        let environment = vec![
            ("DIRENV_CONFIG", root.path().join("config").into_os_string()),
            ("XDG_DATA_HOME", root.path().join("data").into_os_string()),
        ];
        let guards = environment
            .iter()
            .map(|(key, value)| EnvGuard::set(key, value))
            .collect();
        Self {
            environment,
            _guards: guards,
        }
    }

    pub fn declare(&self, root: &Path, body: &str) {
        std::fs::write(root.join(".envrc"), body).expect("write fixture project environment");
        let output = std::process::Command::new("direnv")
            .arg("allow")
            .arg(root)
            .envs(self.environment.iter().map(|(key, value)| (*key, value)))
            .output()
            .expect("direnv must be installed for project environment tests");
        assert!(
            output.status.success(),
            "authorize fixture project environment"
        );
    }

    pub fn declare_forge_identity(&self, root: &Path) {
        std::fs::create_dir_all(root.join("gh-config")).unwrap();
        self.declare_values(
            root,
            &[
                ("GH_TOKEN", "synthetic-forge-project"),
                ("GH_CONFIG_DIR", root.join("gh-config").to_str().unwrap()),
            ],
        );
    }

    pub fn declare_values(&self, root: &Path, values: &[(&str, &str)]) {
        let body = values
            .iter()
            .map(|(key, value)| format!("export {key}='{}'\n", value.replace('\'', "'\\''")))
            .collect::<String>();
        self.declare(root, &body);
    }
}

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
